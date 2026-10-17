//! [`SabrStream`]: the live SABR playback session. [`SabrStream::open`] bootstraps (player,
//! n-sig, PO token, format select, `LiveLane`) and spawns a task that runs the request loop
//! and fills per-track [`TrackBuffer`]s. The consumer reads the tracks through blocking
//! [`MediaReader`]s and polls [`SabrStream::stats`].

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::bootstrap::{bootstrap_player, Bootstrap, StartupTimer, StreamError};
use crate::client::{decode_base64_lenient, Navigation, Session};
use innertube_js::NSigSolver;
use innertube_pot::PoTokenProvider;
use innertube_sabr::{
    BandwidthEstimator, LiveLane, MediaReader, ReaderTracker, SabrClientInfo,
    SelectedFormat, TrackBuffer,
};

mod run_loop;
use run_loop::{run_loop, Recovery, REFERENCE_EDGE_VIEWPORT};

/// Requested video quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quality {
    /// Highest the server will serve for the session's client.
    #[default]
    Best,
    /// Cap at this height (e.g. `Height(1080)`); the ABR ladder stays ≤ it.
    Height(i32),
}

/// Options for [`SabrStream::open`].
#[derive(Debug, Clone)]
pub struct SabrStreamOptions {
    pub quality: Quality,
    /// Stop issuing SABR requests while either track buffer holds more than this many bytes
    /// (backpressure for a paused or slow consumer). Default 64 MiB.
    pub max_buffer_bytes: usize,
    /// Join the live DVR at this stream-absolute position (ms) instead of the live edge.
    /// `None` joins at the edge.
    pub start_ms: Option<i64>,
    /// Dub language by `audioTrack.id` (e.g. `de-DE.10`), from
    /// [`PlayerResponse::audio_tracks`]. `None` plays the default track, as does an id the
    /// video does not offer.
    pub audio_track: Option<String>,
    /// The page and endpoint this stream was opened from.
    pub navigation: Navigation,
    /// The consumer decodes AV1, so an av01 ladder may be chosen.
    pub av1: bool,
}

impl Default for SabrStreamOptions {
    fn default() -> Self {
        Self {
            quality: Quality::Best,
            max_buffer_bytes: 64 * 1024 * 1024,
            start_ms: None,
            audio_track: None,
            navigation: Navigation::default(),
            av1: false,
        }
    }
}

/// What the bootstrap committed to (the chosen formats).
#[derive(Debug, Clone)]
pub struct StreamInfo {
    pub video_itag: i32,
    pub video_height: i32,
    pub audio_itag: i32,
    pub codec: &'static str,
    /// Live seek-preview thumbnails, when the stream publishes them.
    pub live_storyboard: Option<crate::client::LiveStoryboard>,
}

/// Why the engine loop terminated. `None` on [`StreamStats`] means still running.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EndReason {
    /// The consumer called [`SabrStream::stop`] (or dropped the stream).
    Stopped,
    /// The loop gave up after repeated transport failures; the string is the last
    /// underlying error.
    Failed(String),
}

/// Live diagnostics, updated each round; poll via [`SabrStream::stats`].
#[derive(Debug, Clone, Default)]
pub struct StreamStats {
    /// The resume threshold in ms from `PLAYBACK_START_POLICY` (UMP-015, SABR-027): how much
    /// must be buffered before resuming after a rebuffer. `None` until the server sends one;
    /// a consumer then keeps its own default.
    pub resume_min_readahead_ms: Option<i64>,
    pub buffered_audio_ms: i64,
    pub buffered_video_ms: i64,
    pub live_head_ms: Option<i64>,
    /// The live DVR window from `LIVE_METADATA`: the earliest and latest seekable stream
    /// times (ms), once the server has sent them.
    pub live_min_seekable_ms: Option<i64>,
    pub live_max_seekable_ms: Option<i64>,
    pub player_time_ms: i64,
    /// `STREAM_PROTECTION_STATUS`: 1 = attested. `>= 2` means re-attestation needed.
    pub protection_status: Option<i32>,
    pub served_video_itag: Option<i32>,
    pub last_post_secs: f64,
    pub rounds: u64,
    /// Cumulative sequence discontinuities seen (gaps/backward resends).
    pub discontinuities: u64,
    /// Set once the first media bytes have been produced.
    pub started: bool,
    /// Set once the loop terminates; `None` while running. Lets a GUI distinguish a
    /// clean end-of-video from a failure.
    pub ended: Option<EndReason>,
    /// Incremented each time a seek is applied: a discontinuity signal for a consumer
    /// that did not initiate the seek (server `SABR_SEEK`, polling UI).
    pub seek_generation: u64,
    /// Server `SABR_SEEK`s received, and the latest target (stream-absolute ms). The web
    /// player answers each by seeking its media element to the target (base.js `HS`).
    pub sabr_seeks: u64,
    pub sabr_seek_ms: Option<i64>,
    /// How many of the `seek_generation` bumps this client caused.
    ///
    /// A rung change empties the video `buffered_ranges` and the server answers with a
    /// `SABR_SEEK` (SABR-012), which bumps `seek_generation`. A consumer compares the delta
    /// of this counter with the delta of `seek_generation`: when they moved together, the
    /// reposition followed our own request and needs no re-join.
    pub self_repositions: u64,
    /// Cumulative live-stall recoveries: times the loop re-anchored a wedged live
    /// session to the edge after the server stopped serving media (diagnostic).
    pub live_reseeks: u64,
    /// A fresh consumer playhead sits behind the live anchor: the consumer has not followed
    /// the last reposition. Set while the engine is healthy and the decoder is stuck.
    pub consumer_behind_anchor: bool,
}


/// A live SABR playback session. Holds the background engine task; reads come
/// through [`SabrStream::video`]/[`SabrStream::audio`]. Drop or [`SabrStream::stop`]
/// to end it.
pub struct SabrStream {
    audio_buf: TrackBuffer,
    video_buf: TrackBuffer,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<StreamStats>>,
    info: StreamInfo,
    seek_request: Arc<Mutex<Option<i64>>>,
    /// The consumer's real playback position, if it reports one (see
    /// [`SabrStream::set_playhead_ms`]). `(when, stream_ms)`.
    consumer_playhead: Arc<Mutex<Option<(std::time::Instant, i64)>>>,
    /// Consumer playback rate bits (see [`SabrStream::set_playback_rate`]).
    playback_rate: Arc<AtomicU32>,
    /// A pending [`SabrStream::set_quality`] request (target video height), applied by the
    /// engine loop within about one round.
    quality_request: Arc<Mutex<Option<i32>>>,
    /// A pending [`SabrStream::request_reseek_live`]: abandon the current position and
    /// splice the live edge into the existing pipe.
    live_reseek_request: Arc<AtomicBool>,
    unpin_request: Arc<AtomicBool>,
    /// Counts the readers handed out via [`SabrStream::audio`]/[`SabrStream::video`];
    /// dropping the last one tears the session down even if the consumer no longer
    /// holds this handle.
    tracker: Arc<ReaderTracker>,
    /// The engine task, taken and awaited by [`SabrStream::stop`] for a deterministic
    /// teardown. `None` once stopped/joined.
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The `player/heartbeat` keepalive task, when the response carries heartbeat params.
    /// Independent of the media loop; aborted on stop and drop.
    heartbeat: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Raw live segments by sequence number.
    segments: Arc<crate::LiveSegments>,
    watch_details: Option<crate::client::VideoDetails>,
}

impl SabrStream {
    /// Bootstrap a live SABR session for `video_id` and spawn its engine loop.
    /// `solver` descrambles `n`; `pot` mints the GVS PO token.
    pub async fn open(
        session: &Session,
        solver: &NSigSolver,
        pot: Arc<dyn PoTokenProvider>,
        video_id: &str,
        opts: &SabrStreamOptions,
    ) -> Result<SabrStream, StreamError> {
        let bootstrap =
            bootstrap_player(session, solver, video_id, &opts.navigation, Some(&pot), &mut StartupTimer::new())
                .await?;
        if !bootstrap.player.is_live {
            return Err(StreamError::NotLive);
        }
        Self::from_bootstrap(session, pot, video_id, bootstrap, opts).await
    }

    /// Open a live session on a bootstrap whose player response is live.
    pub(crate) async fn from_bootstrap(
        session: &Session,
        pot: Arc<dyn PoTokenProvider>,
        video_id: &str,
        bootstrap: Bootstrap,
        opts: &SabrStreamOptions,
    ) -> Result<SabrStream, StreamError> {
        let client = session.client().clone();
        let Bootstrap { player, url, base_js, base_js_url, visitor_data, bg_challenge, watch_details } = bootstrap;

        // Single-codec video ladder, highest first. It is the full ladder of the chosen family,
        // never truncated by `opts.quality`, so the session can switch to any rung later. One
        // codec family per stream: mixing codecs lets the server switch mid-stream and corrupt
        // the per-track byte stream. `opts.quality` selects the initial rung below; the ladder
        // top fixes the session ceiling (field 72 caps, REQ-012). The audio track is the
        // default one (dubbed videos list many itag-140 formats, SABR-045), AAC-LC (mp4) at
        // the highest bitrate.
        let (codec, ladder) = crate::select::select_video_ladder(&player.formats, Quality::Best, opts.av1)?;
        let audio =
            crate::select::select_audio_track(&player.formats, true, opts.audio_track.as_deref())?;
        // Kept for `set_quality`, which resolves a requested height against the ladder
        // committed at open.
        let ladder_heights: Vec<(i32, i32)> =
            ladder.iter().map(|f| (f.itag, f.height.unwrap_or(0))).collect();
        // The initial rung: the requested height resolved against the ladder (closest at or
        // below; `Best` is the top rung). The session ceiling stays the ladder top.
        let video = match opts.quality {
            Quality::Best => ladder[0],
            Quality::Height(h) => ladder
                .iter()
                .find(|f| f.height.unwrap_or(0) <= h)
                .copied()
                .unwrap_or(*ladder.last().unwrap()),
        };

        // The PO token is minted against this page load's challenge (POT-003).
        let po_token = decode_base64_lenient(
            &pot.get_po_token(video_id, bg_challenge.as_ref()).await?,
        )
        .ok_or(StreamError::BadPoToken)?;

        let sel = |f: &crate::client::Format, is_video: bool| SelectedFormat {
            itag: f.itag,
            last_modified: f.last_modified,
            is_video,
            xtags: f.xtags.clone(),
        };
        // Audio is a single format on live too. A container test (`mp4`) also matches AC-3,
        // EC-3 and DTS formats, and offering them with AAC would switch codecs inside one
        // byte-concatenated track.
        let mut selected = vec![sel(audio, false)];
        selected.extend(ladder.iter().map(|f| sel(f, true)));
        let all_formats =
            player.formats.iter().map(|f| sel(f, f.is_video())).collect::<Vec<_>>();

        let low = *ladder.last().unwrap_or(&video);
        // The session ceiling is `field72.max_resolution`, fixed for the session at the ladder
        // top so every rung stays reachable by a later pick. The manual pick below steers the
        // initial rung. The viewport is the player element's size (REQ-015): the reference main
        // player on a 1080p screen sends 1333x750, and none sends more than 1470 wide.
        // `set_res_ceiling` carries the cap separately.
        let top = ladder[0];
        let mut lane = LiveLane::new(
            player.ustreamer_config.clone(),
            po_token,
            selected,
            &all_formats,
            REFERENCE_EDGE_VIEWPORT,
        );
        lane.set_res_ceiling(top.height.unwrap_or(1080));
        let ci = SabrClientInfo {
            client_name: client.client_name_id,
            client_version: client.client_version.clone(),
            os_name: client.os_name.clone(),
            os_version: client.os_version.clone(),
        };
        lane.engine.set_client_info(ci);
        lane.set_raw(true);
        lane.engine.set_live_segment_ms(player.live_segment_ms.unwrap_or(0));
        // Field 24 sub-field 5: the ladder's lowest video rung (REQ-025).
        lane.set_lane_ref_itag(low.itag);
        // An explicit quality is a stored pick: the manual-pick fields go out from round 1
        // (REQ-018, PLAYER-010). `Best` stays automatic: the full ladder rides in the preferred
        // list and the server climbs from the cold-start bandwidth estimate.
        let segments = crate::LiveSegments::new(
            player.formats.iter().filter(|f| f.is_video()).map(|f| f.itag).collect(),
        );
        if matches!(opts.quality, Quality::Height(_)) {
            lane.set_video_quality(video.itag, video.height.unwrap_or(0));
            segments.set_pin(video.itag);
        }
        // Live-edge chunk count: `liveChunkReadahead` (default 3). The edge read-ahead gate
        // multiplies it by the learned segment duration (SABR-022, SABR-023). base.js sets it
        // to 1 only for LOW or ULTRALOW streams with learned segments of 5 s or more
        // (SABR-022), so a short-segment stream keeps its advertised value.
        let low_latency = player.latency_class.map(|c| c.is_low_latency()).unwrap_or(false)
            || player.is_low_latency_live_stream;
        let chunks = player.live_chunk_readahead.unwrap_or(innertube_sabr::DEFAULT_LIVE_CHUNK_READAHEAD);
        lane.set_live_chunk_readahead(chunks);
        segments.set_readahead_chunks(chunks as i64);
        // Log the applied live configuration: the target is `chunks x segment_duration` and
        // both inputs come from the server.
        if player.is_live {
            tracing::debug!(
                "[live-config] latency={:?} low_latency={} liveChunkReadahead={:?} -> chunks={}",
                player.latency_class, low_latency, player.live_chunk_readahead, chunks
            );
        }

        // DVR join: an explicit `start_ms` joins there instead of the edge sentinel, once;
        // later repositions go to the edge. The run loop skips the snap-to-edge for it.
        if let Some(ms) = opts.start_ms {
            lane.set_live_join_ms(Some(ms));
        }


        // No client-side rung timer on live: the server picks the rung. Requests offer the
        // full ladder and the measured `bandwidth_estimate` (REQ-017, REQ-016), and the server
        // downshifts and upshifts on it (SABR-031, SABR-032).
        //
        // The estimator seeds from the persisted byterate, else 0, which advertises 1 B/s;
        // the first rung follows that estimate (SABR-033). `run_loop` advertises it on round 1.
        let bwe_seed = innertube_sabr::bandwidth_persist::load().unwrap_or(0);
        let bwe = Some(BandwidthEstimator::new(bwe_seed));

        let info = StreamInfo {
            video_itag: video.itag,
            video_height: video.height.unwrap_or(0),
            audio_itag: audio.itag,
            codec,
            live_storyboard: player.live_storyboard.clone(),
        };

        let audio_buf = TrackBuffer::new();
        let video_buf = TrackBuffer::new();
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Mutex::new(StreamStats::default()));
        let seek_request = Arc::new(Mutex::new(None));
        let consumer_playhead: Arc<Mutex<Option<(std::time::Instant, i64)>>> =
            Arc::new(Mutex::new(None));
        let playback_rate = Arc::new(AtomicU32::new((1.0f32).to_bits()));
        let quality_request: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
        let live_reseek_request = Arc::new(AtomicBool::new(false));
        let unpin_request = Arc::new(AtomicBool::new(false));

        let recovery = Recovery {
            pot,
            video_id: video_id.to_string(),
            visitor_data,
            attest: bg_challenge.clone(),
            base_js,
            base_js_url,
        };

        // Spawn the heartbeat when the player response carries heartbeat params. It is
        // independent of the media loop and aborted on stop and drop.
        let heartbeat = crate::spawn_heartbeat(Arc::new(session.clone()), video_id, &player);

        let task = tokio::spawn(run_loop(
            session.clone(),
            lane,
            url,
            recovery,
            opts.max_buffer_bytes,
            opts.start_ms.unwrap_or(0),
            seek_request.clone(),
            consumer_playhead.clone(),
            playback_rate.clone(),
            ladder_heights,
            quality_request.clone(),
            live_reseek_request.clone(),
            unpin_request.clone(),
            audio_buf.clone(),
            video_buf.clone(),
            stop.clone(),
            stats.clone(),
            bwe,
            segments.clone(),
        ));

        let tracker = ReaderTracker::new(stop.clone(), vec![audio_buf.clone(), video_buf.clone()]);

        Ok(SabrStream {
            audio_buf,
            video_buf,
            stop,
            stats,
            info,
            seek_request,
            consumer_playhead,
            playback_rate,
            quality_request,
            live_reseek_request,
            unpin_request,
            tracker,
            task: Mutex::new(Some(task)),
            heartbeat: Mutex::new(heartbeat),
            segments,
            watch_details,
        })
    }

    /// Watch-page metadata carried back by a `get_watch` bootstrap, when that path was
    /// taken and the response had one. `None` means the caller falls back to `next()`.
    pub fn watch_details(&self) -> Option<&crate::client::VideoDetails> {
        self.watch_details.as_ref()
    }

    /// A cursor over one track's raw live segments.
    pub fn segment_track(&self, video: bool) -> crate::LiveSegmentTrack {
        self.segments.track(video)
    }

    /// A blocking reader over the video track (call off the async runtime).
    pub fn video(&self) -> MediaReader {
        self.video_buf.reader_tracked(self.tracker.clone())
    }

    /// A blocking reader over the audio track.
    pub fn audio(&self) -> MediaReader {
        self.audio_buf.reader_tracked(self.tracker.clone())
    }

    /// The committed formats (itags/codec/resolution).
    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    /// The storyboard thumbnail for live stream time `ms`. The newest two sheets' worth of
    /// segments have none yet, so those times return `None`.
    pub fn live_thumbnail_at(&self, ms: i64) -> Option<crate::client::StoryboardThumb> {
        let sb = self.info.live_storyboard.as_ref()?;
        let (seq, newest) = self.segments.sequence_at(ms)?;
        (seq <= newest - 2 * sb.frames_per_sheet()).then(|| sb.thumbnail_for_sequence(seq)).flatten()
    }

    /// Snapshot of live diagnostics.
    pub fn stats(&self) -> StreamStats {
        self.stats.lock().unwrap().clone()
    }

    /// Report the consumer's playback position (stream-absolute ms).
    ///
    /// The live read-ahead gate measures runway from the real playhead. Without a report it
    /// models the playhead as `anchor + elapsed wall time`, which runs behind a decoder and
    /// makes the gate decline fetches the decoder needs.
    ///
    /// Cheap and thread-safe; call it as often as once per frame. A report older than
    /// [`PLAYHEAD_FRESH_MS`] is ignored and the wall-clock model takes over. A consumer with
    /// no decoder never calls it.
    pub fn set_playhead_ms(&self, stream_ms: i64) {
        *self.consumer_playhead.lock().unwrap() =
            Some((std::time::Instant::now(), stream_ms));
    }

    /// Publish the consumer's playback rate (1x, 2x, ...). The rate rides every later request
    /// as `ClientAbrState.playback_rate` (field 35, omitted at 1x, REQ-019), and the
    /// read-ahead gate measures runway in playback seconds (`buffered / rate`), so a faster
    /// consumer fetches deeper. Rates outside 0.0625..=16.0 are clamped. Thread-safe; the
    /// engine loop picks it up within about one round.
    pub fn set_playback_rate(&self, rate: f32) {
        self.playback_rate
            .store(rate.clamp(0.0625, 16.0).to_bits(), Ordering::Relaxed);
    }

    /// The currently published playback rate (for tests / diagnostics).
    pub fn playback_rate(&self) -> f32 {
        f32::from_bits(self.playback_rate.load(Ordering::Relaxed))
    }

    /// Reposition playback to `target_ms`. Thread-safe; returns at once and the engine loop
    /// applies it within about one round. Afterwards the track buffers are cleared and each
    /// track leads with a fresh init segment from the target, so the caller must flush its
    /// decode pipeline. Live: a DVR seek in stream-absolute ms, sent as the next
    /// `player_time`. [`SabrStream::request_reseek_live`] returns to the head.
    pub fn seek(&self, target_ms: i64) {
        *self.seek_request.lock().unwrap() = Some(target_ms.max(0));
    }

    /// In-stream video-quality switch modelled on the WEB client's manual pick (REQ-018): no
    /// teardown and no seek. `player_time` stays continuous and the video buffered state
    /// resets (the reference resets audio too), so the new rung comes from the playhead.
    /// Thread-safe; the engine loop applies it within about one round.
    ///
    /// `height` resolves against the ladder committed at [`SabrStream::open`]: the exact
    /// match, else the closest rung below, else the lowest rung. Live only.
    pub fn set_quality(&self, height: i32) {
        *self.quality_request.lock().unwrap() = Some(height);
    }

    /// Clear a manual rung pick and resume server-driven auto: the full ladder and the
    /// measured `bandwidth_estimate` go out again and the server retakes the rung.
    ///
    /// A UI's "Auto" entry calls this. [`set_quality`](Self::set_quality) with the top height
    /// arms a pick, which collapses `preferred_video_format_ids` to one itag and stops the
    /// server adapting.
    ///
    /// No format changes, so there is no re-join or rebuffer; only the next request's wire
    /// shape changes.
    pub fn unpin_quality(&self) {
        self.unpin_request.store(true, Ordering::Relaxed);
    }

    /// Consumer-initiated live re-edge: abandon the current position and splice the live
    /// edge into the existing pipe, as the engine's own stall recovery does. For a caller
    /// that measured the playhead far behind the head. With 8 to 10 s of consumer cache the
    /// splice lands without a rebuffer. No-op on VOD. Thread-safe.
    pub fn request_reseek_live(&self) {
        self.live_reseek_request.store(true, Ordering::Relaxed);
    }

    /// Signal the engine loop to stop and await its teardown. When it returns, the task has
    /// exited and the track buffers are closed (readers see EOF), so a consumer that re-opens
    /// at once never runs two engines (SABR-056). Idempotent: a second call returns at once.
    pub async fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        // The heartbeat loop has no stop flag; abort it.
        if let Some(h) = self.heartbeat.lock().unwrap().take() {
            h.abort();
        }
        // Take the handle out before awaiting so the lock guard isn't held across the
        // await point (and a concurrent caller doesn't double-await).
        let handle = self.task.lock().unwrap().take();
        if let Some(h) = handle {
            let _ = h.await;
        }
    }
}

impl Drop for SabrStream {
    fn drop(&mut self) {
        // Can't await here, so tear down promptly without joining: flag the task and
        // close the buffers directly so any blocked reader EOFs immediately rather
        // than waiting for the task's next loop check. The task observes the flag and
        // exits on its own. For a deterministic join, call `stop().await` first.
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.heartbeat.lock().unwrap().take() {
            h.abort();
        }
        self.audio_buf.close();
        self.video_buf.close();
    }
}
