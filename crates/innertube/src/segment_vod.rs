//! [`SegmentVod`]: a VOD session for a consumer that demuxes self-contained segments and
//! orders them by timestamp, the way the web player feeds a `SourceBuffer`.
//!
//! Segments are stored by `(itag, sequence_number)` and kept across seeks. Requests report
//! the stored runs as `buffered_ranges` (REQ-008), so the server serves from the end of the
//! run that holds the playhead.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::client::{Navigation, Session};
use innertube_js::NSigSolver;
use innertube_pot::PoTokenProvider;
use innertube_sabr::{BandwidthEstimator, MediaSegment, SabrClientInfo, SabrEngine, SelectedFormat, VodBufferedRange};

use crate::attest::ReattestOutcome;
use crate::vod_bootstrap::{
    announced_hold_ms, bootstrap_vod, request_gate_ms, Reattester, ReloadFn, VodBootstrap,
    NO_MEDIA_FLOOR_MS,
};
use crate::bootstrap::{bootstrap_player, Bootstrap, StartupTimer};
use crate::{Quality, StreamError};

/// Options for [`SegmentVod::open`].
#[derive(Debug, Clone)]
pub struct SegmentVodOptions {
    /// A stored pick, pinned from the first request; the ladder is always the full one.
    pub quality: Quality,
    /// Where playback starts, in ms.
    pub start_ms: i64,
    /// `audioTrack.id` of a dub, or `None` for the original track.
    pub audio_track: Option<String>,
    /// Stored bytes above which segments far from the playhead are evicted.
    pub max_store_bytes: usize,
    /// The page and endpoint this playback was opened from.
    pub navigation: Navigation,
    /// The consumer decodes AV1, so an av01 ladder may be chosen.
    pub av1: bool,
    /// Fetch one round with media, then wait for [`SegmentVod::activate`].
    pub preload: bool,
    /// The player's size as sent to the server; the top rung's size when `None`.
    pub viewport: Option<(i32, i32)>,
}

impl Default for SegmentVodOptions {
    fn default() -> Self {
        Self {
            quality: Quality::Best,
            start_ms: 0,
            audio_track: None,
            max_store_bytes: 384 << 20,
            navigation: Navigation::default(),
            av1: false,
            preload: false,
            viewport: None,
        }
    }
}

#[derive(Clone)]
struct Stored {
    start_ms: i64,
    duration_ms: i64,
    /// The server sent the duration; otherwise it is estimated from the neighbours.
    timed: bool,
    /// Arrival order: where two rungs cover the same time, the later decision wins.
    arrival: u64,
    bytes: Arc<Vec<u8>>,
}

#[derive(Default)]
struct Store {
    segs: HashMap<i32, BTreeMap<i64, Stored>>,
    inits: HashMap<i32, Arc<Vec<u8>>>,
    bytes: usize,
    arrivals: u64,
    /// Per itag, the latest segment spacing seen: the estimate for an untimed segment.
    spacing: HashMap<i32, i64>,
}

/// The duration assumed for an untimed segment before any spacing is known.
const UNTIMED_SEGMENT_MS: i64 = 5_000;

impl Store {
    /// Whether `seg` would add a sequence this store does not hold yet.
    fn insert_is_new(&self, seg: &MediaSegment) -> bool {
        match (seg.init, seg.seq) {
            (false, Some(seq)) => !self.segs.get(&seg.itag).is_some_and(|m| m.contains_key(&seq)),
            _ => false,
        }
    }

    fn insert(&mut self, seg: MediaSegment) {
        match (seg.init, seg.seq, seg.start_ms) {
            (true, _, _) | (_, None, _) => {
                self.inits.entry(seg.itag).or_insert_with(|| Arc::new(seg.bytes));
            }
            (false, Some(seq), Some(start_ms)) if !seg.bytes.is_empty() => {
                self.arrivals += 1;
                let map = self.segs.entry(seg.itag).or_default();
                // A VOD that was live sends segments without `duration_ms`, as live does (UMP-008).
                // The duration is the start gap to a neighbouring segment.
                if let Some(prev) = map.get_mut(&(seq - 1)).filter(|p| !p.timed && start_ms > p.start_ms) {
                    prev.duration_ms = start_ms - prev.start_ms;
                    self.spacing.insert(seg.itag, prev.duration_ms);
                }
                let duration_ms = seg.duration_ms.unwrap_or_else(|| {
                    map.get(&(seq + 1))
                        .map(|n| n.start_ms - start_ms)
                        .or_else(|| map.get(&(seq - 1)).map(|p| start_ms - p.start_ms))
                        .filter(|d| *d > 0)
                        .or_else(|| self.spacing.get(&seg.itag).copied())
                        .unwrap_or(UNTIMED_SEGMENT_MS)
                });
                let timed = seg.duration_ms.is_some();
                let old = map.insert(
                    seq,
                    Stored { start_ms, duration_ms, timed, arrival: self.arrivals, bytes: Arc::new(seg.bytes) },
                );
                self.bytes += self.segs[&seg.itag][&seq].bytes.len();
                if let Some(old) = old {
                    self.bytes -= old.bytes.len();
                }
            }
            _ => {}
        }
    }

    /// The stored segment of one of `itags` covering `t`: `prefer` when it has one, else
    /// the latest to arrive.
    fn covering(&self, itags: &[i32], t: i64, prefer: Option<i32>) -> Option<(i32, i64, Stored)> {
        let mut best: Option<(i32, i64, Stored)> = None;
        for &itag in itags {
            let Some(map) = self.segs.get(&itag) else { continue };
            let hit = map
                .iter()
                .rev()
                .find(|(_, s)| s.start_ms <= t)
                .filter(|(_, s)| t < s.start_ms + s.duration_ms);
            if let Some((&seq, s)) = hit {
                let better = match &best {
                    None => true,
                    Some((bi, _, bs)) => {
                        if prefer == Some(itag) {
                            true
                        } else if prefer == Some(*bi) {
                            false
                        } else {
                            s.arrival > bs.arrival
                        }
                    }
                };
                if better {
                    best = Some((itag, seq, s.clone()));
                }
            }
        }
        best
    }

    /// How far stored media runs contiguously from `t` across `itags`.
    fn covered_until(&self, itags: &[i32], t: i64) -> i64 {
        let mut at = t;
        while let Some((_, _, s)) = self.covering(itags, at, None) {
            at = s.start_ms + s.duration_ms;
        }
        at
    }

    /// Contiguous runs per itag, as the server reads them.
    fn ranges(&self, itags: &[i32]) -> Vec<VodBufferedRange> {
        let mut out = Vec::new();
        for &itag in itags {
            let Some(map) = self.segs.get(&itag) else { continue };
            let mut run: Option<VodBufferedRange> = None;
            for (&seq, s) in map {
                match run.as_mut() {
                    Some(r) if r.end_segment_index as i64 + 1 == seq => {
                        r.end_segment_index = seq as i32;
                        r.duration_ms = s.start_ms + s.duration_ms - r.start_time_ms;
                    }
                    _ => {
                        out.extend(run.take());
                        run = Some(VodBufferedRange {
                            itag,
                            start_time_ms: s.start_ms,
                            duration_ms: s.duration_ms,
                            start_segment_index: seq as i32,
                            end_segment_index: seq as i32,
                        });
                    }
                }
            }
            out.extend(run);
        }
        out
    }

    /// Drop the stored video of every rung but `keep`. The web player clears its buffer on
    /// a quality pick, so later `buffered_ranges` carry only the picked rung.
    fn retain_video_rung(&mut self, video_itags: &[i32], keep: i32) {
        for itag in video_itags.iter().filter(|&&i| i != keep) {
            if let Some(map) = self.segs.remove(itag) {
                self.bytes -= map.values().map(|s| s.bytes.len()).sum::<usize>();
            }
        }
    }

    /// Drop the segments farthest from the playhead until under `cap`, keeping
    /// `[playhead - 10 s, playhead + keep_ahead]`.
    fn evict(&mut self, playhead: i64, keep_ahead: i64, cap: usize) {
        while self.bytes > cap {
            let far = self
                .segs
                .iter()
                .flat_map(|(&itag, m)| m.iter().map(move |(&seq, s)| (itag, seq, s)))
                .filter(|(_, _, s)| s.start_ms + s.duration_ms < playhead - 10_000 || s.start_ms > playhead + keep_ahead)
                .max_by_key(|(_, _, s)| (s.start_ms - playhead).abs())
                .map(|(itag, seq, _)| (itag, seq));
            let Some((itag, seq)) = far else { break };
            if let Some(s) = self.segs.get_mut(&itag).and_then(|m| m.remove(&seq)) {
                self.bytes -= s.bytes.len();
            }
        }
    }
}

struct Shared {
    store: Mutex<Store>,
    arrived: Condvar,
    wake: tokio::sync::Notify,
    stop: AtomicBool,
    playhead_ms: AtomicI64,
    rate_bits: AtomicU32,
    /// Pinned video itag, or 0 for server-driven quality.
    pin: AtomicI32,
    video_itags: Vec<i32>,
    audio_itag: i32,
    heights: HashMap<i32, i32>,
    duration_ms: i64,
    preload: AtomicBool,
    /// Why the fetch loop gave up, once it has.
    failure: Mutex<Option<Arc<StreamError>>>,
}

impl Shared {
    fn video_choice(&self) -> Vec<i32> {
        match self.pin.load(Ordering::Relaxed) {
            0 => self.video_itags.clone(),
            p => vec![p],
        }
    }
}

/// An open segment VOD session. Dropping it stops the fetch loop.
pub struct SegmentVod {
    shared: Arc<Shared>,
    captions: crate::Captions,
    storyboard: Option<crate::client::Storyboard>,
    watch_details: Option<crate::client::VideoDetails>,
}

impl SegmentVod {
    pub async fn open(
        session: &Session,
        solver: &NSigSolver,
        pot: Arc<dyn PoTokenProvider>,
        video_id: &str,
        opts: &SegmentVodOptions,
    ) -> Result<SegmentVod, StreamError> {
        let mut timer = StartupTimer::new();
        let bootstrap =
            bootstrap_player(session, solver, video_id, &opts.navigation, Some(&pot), &mut timer).await?;
        if bootstrap.player.is_live {
            return Err(StreamError::NotVod);
        }
        Self::from_bootstrap(session, pot, video_id, bootstrap, opts, timer).await
    }

    /// Open a VOD session on a bootstrap whose player response is not live.
    pub(crate) async fn from_bootstrap(
        session: &Session,
        pot: Arc<dyn PoTokenProvider>,
        video_id: &str,
        bootstrap: Bootstrap,
        opts: &SegmentVodOptions,
        mut timer: StartupTimer,
    ) -> Result<SegmentVod, StreamError> {
        let client = session.client().clone();
        let VodBootstrap { player, url, po_token, reattest, reload, watch_details } =
            bootstrap_vod(session, pot, video_id, bootstrap, &mut timer).await?;
        let storyboard = player.storyboard.clone();
        let (codec, ladder) = crate::select::select_video_ladder(&player.formats, crate::Quality::Best, opts.av1)?;
        tracing::debug!("[segvod] video ladder: {codec}, {} rungs", ladder.len());
        let audio =
            crate::select::select_audio_track(&player.formats, false, opts.audio_track.as_deref())?;
        let top = ladder.first().copied().ok_or(StreamError::NoVideoFormats)?;
        let sel = |f: &crate::client::Format, is_video: bool| SelectedFormat {
            itag: f.itag,
            last_modified: f.last_modified,
            is_video,
            xtags: f.xtags.clone(),
        };
        let mut selected: Vec<SelectedFormat> = ladder.iter().map(|f| sel(f, true)).collect();
        selected.push(sel(audio, false));
        let all_formats: Vec<SelectedFormat> =
            player.formats.iter().map(|f| sel(f, f.is_video())).collect();
        let captions = crate::Captions::new(session.clone(), player.captions.clone(), po_token.clone());
        let mut engine =
            SabrEngine::new(player.ustreamer_config.clone(), po_token, selected, &all_formats);
        engine.set_live(false);
        let (vw, vh) = opts.viewport.unwrap_or((top.width.unwrap_or(1280), top.height.unwrap_or(720)));
        engine.set_viewport(vw, vh);
        engine.set_client_info(SabrClientInfo {
            client_name: client.client_name_id,
            client_version: client.client_version.clone(),
            os_name: client.os_name.clone(),
            os_version: client.os_version.clone(),
        });
        engine.set_bandwidth_estimate(crate::bandwidth::load());
        engine.set_collect_segments(true);
        engine.set_vod_buffered_ranges(Vec::new());
        let shared = Arc::new(Shared {
            store: Mutex::new(Store::default()),
            arrived: Condvar::new(),
            wake: tokio::sync::Notify::new(),
            stop: AtomicBool::new(false),
            playhead_ms: AtomicI64::new(opts.start_ms.max(0)),
            rate_bits: AtomicU32::new(1.0f32.to_bits()),
            pin: AtomicI32::new(0),
            video_itags: ladder.iter().map(|f| f.itag).collect(),
            audio_itag: audio.itag,
            heights: ladder.iter().map(|f| (f.itag, f.height.unwrap_or(0))).collect(),
            duration_ms: player.duration_ms.unwrap_or(0),
            preload: AtomicBool::new(opts.preload),
            failure: Mutex::new(None),
        });
        timer.lap("segment VOD bootstrap");
        let vod = SegmentVod { shared, captions, storyboard, watch_details };
        if let crate::Quality::Height(h) = opts.quality {
            vod.pin_height(h);
        }
        let shared = vod.shared.clone();
        tokio::spawn(fetch_loop(
            shared.clone(),
            session.clone(),
            engine,
            url,
            reattest,
            reload,
            opts.max_store_bytes,
        ));
        Ok(vod)
    }

    /// Lets a preloaded session fetch on, on the same SABR session.
    pub fn activate(&self) {
        self.shared.preload.store(false, Ordering::Relaxed);
        self.shared.wake.notify_one();
    }

    pub fn captions(&self) -> &crate::Captions {
        &self.captions
    }

    /// The seek-preview storyboard, when the video has one.
    pub fn storyboard(&self) -> Option<&crate::client::Storyboard> {
        self.storyboard.as_ref()
    }

    /// Watch-page metadata from a `get_watch` bootstrap, when that path was taken and the
    /// response carried it. `None` means the caller falls back to `next()`.
    pub fn watch_details(&self) -> Option<&crate::client::VideoDetails> {
        self.watch_details.as_ref()
    }

    pub fn duration_ms(&self) -> i64 {
        self.shared.duration_ms
    }

    /// Video heights the session can pin, highest first.
    pub fn heights(&self) -> Vec<i32> {
        self.shared.video_itags.iter().filter_map(|i| self.shared.heights.get(i).copied()).collect()
    }

    /// A cursor over one track's segments, starting at `from_ms`.
    pub fn track(&self, video: bool, from_ms: i64) -> SegmentTrack {
        SegmentTrack {
            shared: self.shared.clone(),
            video,
            cursor_ms: from_ms,
            last_itag: None,
            stop: Arc::new(AtomicBool::new(false)),
            pin_miss_since: None,
        }
    }

    /// The consumer's playback position: it anchors what is fetched next.
    pub fn set_playhead_ms(&self, ms: i64) {
        self.shared.playhead_ms.store(ms.max(0), Ordering::Relaxed);
        self.shared.wake.notify_one();
    }

    pub fn set_playback_rate(&self, rate: f32) {
        self.shared.rate_bits.store(rate.clamp(0.0625, 16.0).to_bits(), Ordering::Relaxed);
    }

    /// Pin video to the closest rung at or below `height`; returns its itag.
    pub fn pin_height(&self, height: i32) -> Option<i32> {
        let s = &self.shared;
        let itag = s
            .video_itags
            .iter()
            .copied()
            .filter(|i| s.heights.get(i).copied().unwrap_or(0) <= height)
            .max_by_key(|i| s.heights.get(i).copied().unwrap_or(0))
            .or_else(|| s.video_itags.last().copied())?;
        s.pin.store(itag, Ordering::Relaxed);
        s.wake.notify_one();
        Some(itag)
    }

    pub fn unpin(&self) {
        self.shared.pin.store(0, Ordering::Relaxed);
        self.shared.wake.notify_one();
    }

    /// Why the session stopped fetching, when it gave up on its own (repeated request
    /// failures). `None` while healthy, at the end of the video, or after [`Self::stop`].
    pub fn failure(&self) -> Option<Arc<StreamError>> {
        self.shared.failure.lock().unwrap().clone()
    }

    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake.notify_one();
        self.shared.arrived.notify_all();
    }
}

impl Drop for SegmentVod {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One track's read cursor. [`SegmentTrack::next_piece`] blocks until the segment covering the
/// cursor is stored, and leads each rung with its init.
pub struct SegmentTrack {
    shared: Arc<Shared>,
    video: bool,
    cursor_ms: i64,
    last_itag: Option<i32>,
    stop: Arc<AtomicBool>,
    /// When the pinned rung was first missing at the cursor.
    pin_miss_since: Option<Instant>,
}

/// How long a pinned rung that is being fetched may keep the cursor waiting before
/// stored media of another rung plays instead.
const PIN_WAIT: Duration = Duration::from_secs(3);

/// A track of self-contained segments a demuxer can take in order: VOD or live.
pub trait SegmentSource: Send {
    /// The next piece, or `None` at the end, on stop, or on interrupt.
    fn next_piece(&mut self) -> Option<SegmentPiece>;
    /// Move the cursor to the segment holding `ms`; the next piece re-leads with its init.
    fn reposition(&mut self, ms: i64);
    /// Unblocks a waiting [`SegmentSource::next_piece`] from another thread.
    fn interrupt_handle(&self) -> Arc<AtomicBool>;
    fn cursor_ms(&self) -> i64;
}

impl SegmentSource for SegmentTrack {
    fn next_piece(&mut self) -> Option<SegmentPiece> {
        SegmentTrack::next_piece(self)
    }
    fn reposition(&mut self, ms: i64) {
        SegmentTrack::reposition(self, ms)
    }
    fn interrupt_handle(&self) -> Arc<AtomicBool> {
        SegmentTrack::interrupt_handle(self)
    }
    fn cursor_ms(&self) -> i64 {
        SegmentTrack::cursor_ms(self)
    }
}

/// Bytes for the consumer's demuxer: one segment, led by its rung's init when the rung
/// changed or the cursor moved.
pub struct SegmentPiece {
    pub itag: i32,
    pub start_ms: i64,
    pub bytes: Vec<u8>,
}

impl SegmentTrack {
    /// Unblocks a waiting [`SegmentTrack::next_piece`] from another thread.
    pub fn interrupt_handle(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }

    /// Move the cursor; the next piece re-sends its init.
    pub fn reposition(&mut self, ms: i64) {
        self.cursor_ms = ms.max(0);
        self.last_itag = None;
    }

    pub fn cursor_ms(&self) -> i64 {
        self.cursor_ms
    }

    /// The next piece, or `None` at the end of the video, on stop, or on interrupt.
    pub fn next_piece(&mut self) -> Option<SegmentPiece> {
        let s = self.shared.clone();
        let mut store = s.store.lock().unwrap();
        loop {
            if s.stop.load(Ordering::Relaxed) || self.stop.swap(false, Ordering::Relaxed) {
                return None;
            }
            if s.duration_ms > 0 && self.cursor_ms >= s.duration_ms - 50 {
                return None;
            }
            let itags = if self.video { s.video_choice() } else { vec![s.audio_itag] };
            let prefer = if self.video { self.last_itag } else { None };
            let prefer = match s.pin.load(Ordering::Relaxed) {
                0 => prefer,
                p if self.video => Some(p),
                _ => prefer,
            };
            let pinned = store.covering(&itags, self.cursor_ms, prefer);
            if pinned.is_some() {
                self.pin_miss_since = None;
            }
            let waited = self.pin_miss_since.get_or_insert_with(Instant::now).elapsed();
            let found = pinned.or_else(|| {
                // A pinned rung that has not arrived in time: keep playing what is stored.
                (self.video && waited >= PIN_WAIT)
                    .then(|| store.covering(&s.video_itags, self.cursor_ms, self.last_itag))
                    .flatten()
            });
            // Segment timing and the declared duration disagree by a frame or so at the end.
            if found.is_none() && s.duration_ms > 0 && self.cursor_ms >= s.duration_ms - 1_500 {
                return None;
            }
            if let Some((itag, _, seg)) = found {
                if let Some(init) = store.inits.get(&itag).cloned() {
                    let mut bytes = Vec::with_capacity(seg.bytes.len() + init.len());
                    if self.last_itag != Some(itag) {
                        bytes.extend_from_slice(&init);
                    }
                    bytes.extend_from_slice(&seg.bytes);
                    self.last_itag = Some(itag);
                    self.cursor_ms = seg.start_ms + seg.duration_ms;
                    return Some(SegmentPiece { itag, start_ms: seg.start_ms, bytes });
                }
            }
            let (guard, _) = s.arrived.wait_timeout(store, Duration::from_millis(100)).unwrap();
            store = guard;
        }
    }
}

/// A preload stops after its first round with media until [`SegmentVod::activate`] (SABR-035).
fn preload_waits(preload: bool, held_bytes: usize) -> bool {
    preload && held_bytes > 0
}

/// The last segment can end short of the advertised duration. Media within this many ms of
/// the end counts as complete.
const END_SLACK_MS: i64 = 1_000;

fn buffered_to_end(duration_ms: i64, covered_until_ms: i64) -> bool {
    duration_ms > 0 && covered_until_ms >= duration_ms - END_SLACK_MS
}

/// The estimate to advertise after a round of `bytes` over `secs` that carried `segs` media
/// segments. Rounds without media are server holds (GATE-005) and are not throughput.
fn advertise(bwe: &mut BandwidthEstimator, bytes: usize, segs: usize, secs: f64) -> i64 {
    if segs == 0 {
        return bwe.estimate();
    }
    bwe.observe(bytes, secs)
}

/// Consecutive failed SABR POSTs after which the session gives up.
const MAX_FAILED_POSTS: u32 = 8;

async fn fetch_loop(
    shared: Arc<Shared>,
    session: Session,
    mut engine: SabrEngine,
    mut url: String,
    reattest: Reattester,
    reload: ReloadFn,
    cap: usize,
) {
    let mut applied_pin = 0;
    let mut bwe = BandwidthEstimator::new(crate::bandwidth::load());
    let mut hold_until: Option<Instant> = None;
    let mut round = 0u64;
    let mut failed_posts = 0u32;
    'fetch: while !shared.stop.load(Ordering::Relaxed) {
        let pin = shared.pin.load(Ordering::Relaxed);
        if pin != applied_pin {
            match pin {
                0 => engine.unpin_video_quality(),
                itag => {
                    engine.set_video_quality(itag, shared.heights.get(&itag).copied().unwrap_or(0));
                    shared.store.lock().unwrap().retain_video_rung(&shared.video_itags, itag);
                }
            }
            applied_pin = pin;
        }
        let held = shared.store.lock().unwrap().bytes;
        if preload_waits(shared.preload.load(Ordering::Relaxed), held) {
            let _ = tokio::time::timeout(Duration::from_millis(500), shared.wake.notified()).await;
            continue;
        }
        let playhead = shared.playhead_ms.load(Ordering::Relaxed);
        let gate = request_gate_ms(engine.request_policy().target_audio_readahead_ms.unwrap_or(0));
        let (ahead_v, ahead_a, ranges) = {
            let store = shared.store.lock().unwrap();
            let video = shared.video_choice();
            let mut itags = video.clone();
            itags.push(shared.audio_itag);
            (
                store.covered_until(&video, playhead) - playhead,
                store.covered_until(&[shared.audio_itag], playhead) - playhead,
                store.ranges(&itags),
            )
        };
        let end = buffered_to_end(shared.duration_ms, playhead + ahead_v.min(ahead_a));
        if end || (ahead_v >= gate && ahead_a >= gate) {
            let _ = tokio::time::timeout(Duration::from_millis(500), shared.wake.notified()).await;
            continue;
        }
        if let Some(until) = hold_until {
            let wait = until.saturating_duration_since(Instant::now());
            if !wait.is_zero() {
                let _ = tokio::time::timeout(wait, shared.wake.notified()).await;
                continue;
            }
            hold_until = None;
        }
        engine.set_vod_buffered_ranges(ranges);
        engine.set_playback_rate(f32::from_bits(shared.rate_bits.load(Ordering::Relaxed)));
        let t0 = Instant::now();
        let req = engine.request_bytes(playhead);
        let mut body = match session.post_sabr_stream(&url, req).await {
            Ok(b) => b,
            Err(e) => {
                failed_posts += 1;
                tracing::debug!("[segvod] round {round}: POST failed: {e}");
                if failed_posts >= MAX_FAILED_POSTS {
                    tracing::warn!("[segvod] {failed_posts} POSTs failed in a row; stopping this session");
                    *shared.failure.lock().unwrap() =
                        Some(Arc::new(StreamError::RequestsFailed { attempts: failed_posts, last: e }));
                    shared.stop.store(true, Ordering::Relaxed);
                    shared.arrived.notify_all();
                    break 'fetch;
                }
                tokio::time::sleep(Duration::from_millis(NO_MEDIA_FLOOR_MS)).await;
                continue;
            }
        };
        failed_posts = 0;
        engine.begin_response();
        let (mut segs, mut fresh, mut bytes) = (0usize, 0usize, 0usize);
        let (mut status, mut reload_token, mut redirect, mut seek) = (None, None, None, None);
        loop {
            if shared.stop.load(Ordering::Relaxed) {
                break 'fetch;
            }
            match body.next_chunk().await {
                Ok(Some(chunk)) => {
                    bytes += chunk.len();
                    let ing = engine.ingest_streaming(chunk);
                    status = ing.protection_status.or(status);
                    reload_token = ing.reload_token.or(reload_token);
                    redirect = ing.redirect.or(redirect);
                    seek = ing.sabr_seek_ms.or(seek);
                    if !ing.segments.is_empty() {
                        segs += ing.segments.iter().filter(|s| !s.init).count();
                        let mut store = shared.store.lock().unwrap();
                        fresh += ing.segments.iter().filter(|s| store.insert_is_new(s)).count();
                        for seg in ing.segments {
                            tracing::debug!(
                                "[segvod]   seg itag={} seq={:?} start={:?} dur={:?} init={} {}B",
                                seg.itag, seg.seq, seg.start_ms, seg.duration_ms, seg.init, seg.bytes.len()
                            );
                            store.insert(seg);
                        }
                        drop(store);
                        shared.arrived.notify_all();
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("[segvod] round {round}: response body failed: {e}");
                    break;
                }
            }
        }
        let secs = t0.elapsed().as_secs_f64();
        let bps = advertise(&mut bwe, bytes, segs, secs);
        engine.set_bandwidth_estimate(bps);
        tracing::debug!(
            "[segvod] round {round}: pt={playhead} ahead v={ahead_v} a={ahead_a} gate={gate} POST {:.0}ms {bytes}B segs={segs} bwe={bps} prot={status:?} seek={seek:?}",
            secs * 1000.0
        );
        round += 1;
        if let Some(r) = redirect {
            url = r;
            continue;
        }
        if let Some(token) = reload_token {
            if let Some((fresh, config)) = reload(token).await {
                url = fresh;
                engine.set_ustreamer_config(config);
            }
        }
        if let ReattestOutcome::Reminted { token, .. } = reattest.run(status).await {
            engine.set_po_token(token);
        }
        {
            let mut store = shared.store.lock().unwrap();
            store.evict(shared.playhead_ms.load(Ordering::Relaxed), gate, cap);
        }
        // A round that brought nothing new (none, or only segments already held) waits
        // before asking again.
        if fresh == 0 {
            let ms = announced_hold_ms(engine.request_policy().backoff_time_ms).unwrap_or(NO_MEDIA_FLOOR_MS);
            hold_until = Some(Instant::now() + Duration::from_millis(ms));
        }
    }
    // The next session's cold-start seed, as the web player stores `yt-player-bandwidth`
    // (PLAYER-009).
    if let Some(bps) = bwe.persistable() {
        innertube_sabr::bandwidth_persist::save(bps);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(itag: i32, seq: i64, start: i64, dur: i64) -> MediaSegment {
        MediaSegment { itag, seq: Some(seq), start_ms: Some(start), duration_ms: Some(dur), init: false, bytes: vec![seq as u8] }
    }

    #[test]
    fn buffered_ranges_are_contiguous_runs_of_sequence_numbers() {
        let mut s = Store::default();
        for (seq, start) in [(1, 0), (2, 5_000), (3, 10_000), (7, 30_000), (8, 35_000)] {
            s.insert(seg(244, seq, start, 5_000));
        }
        let r: Vec<_> = s
            .ranges(&[244])
            .into_iter()
            .map(|r| (r.start_segment_index, r.end_segment_index, r.start_time_ms, r.duration_ms))
            .collect();
        assert_eq!(r, vec![(1, 3, 0, 15_000), (7, 8, 30_000, 10_000)]);
    }

    #[test]
    fn rounds_with_media_move_the_advertised_estimate_and_held_rounds_do_not() {
        let mut bwe = BandwidthEstimator::new(460_195);
        assert_eq!(advertise(&mut bwe, 105, 0, 4.0), 460_195);
        assert_eq!(advertise(&mut bwe, 2_000_000, 3, 1.0), 2_000_000);
    }

    #[test]
    fn a_pick_drops_the_other_video_rungs_and_keeps_audio() {
        let mut s = Store::default();
        for seq in 1..=4 {
            s.insert(seg(398, seq, (seq - 1) * 5_000, 5_000));
            s.insert(seg(397, seq, (seq - 1) * 5_000, 5_000));
        }
        s.insert(seg(140, 1, 0, 20_000));
        s.retain_video_rung(&[398, 397], 397);
        let itags: Vec<_> = s.ranges(&[398, 397, 140]).into_iter().map(|r| r.itag).collect();
        assert_eq!(itags, vec![397, 140]);
        assert_eq!(s.bytes, 5);
    }

}
