//! The live engine loop: request, POST, merge, recover, pace.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{EndReason, StreamStats};
use crate::bootstrap::{solve_url_n, StreamError};
use crate::client::{decode_base64_lenient, PlayerOptions, Session};
use innertube_js::{extract_player_js_url, extract_signature_timestamp, NSigSolver};
use innertube_pot::PoTokenProvider;
use innertube_sabr::{BandwidthEstimator, LiveLane, MergeOut, RequestPolicy, TrackBuffer};

/// The advertised viewport (`ClientAbrState` 18/19): the reference watch-page player on a
/// 1080p screen, the most common viewport in the captures. It is the player element's size
/// (REQ-015). No captured POST sends a viewport wider than 1470.
pub(super) const REFERENCE_EDGE_VIEWPORT: (i32, i32) = (1333, 750);

const PLAYHEAD_FRESH_MS: u128 = 3_000;
/// Buffered-ahead below which empty live rounds count toward stall recovery.
const LIVE_STALL_DRY_MS: i64 = 5_000;

/// Everything `run_loop` needs to re-mint a PO token and reload the player session
/// mid-stream (owned, so it lives in the spawned task).
pub(super) struct Recovery {
    pub(super) pot: Arc<dyn PoTokenProvider>,
    pub(super) video_id: String,
    pub(super) visitor_data: String,
    /// The page-bound attestation inputs the session opened with, so a re-mint uses the same
    /// challenge and `ytcfg` (POT-003).
    pub(super) attest: Option<innertube_pot::PageAttestation>,
    /// The player JS, to re-derive `signatureTimestamp` and re-solve `n` after a
    /// `RELOAD_PLAYER_RESPONSE`.
    pub(super) base_js: Option<String>,
    /// The url `base_js` came from (the build id is in the path). The cold-start
    /// re-bootstrap reuses `base_js` when a fresh page names the same build.
    pub(super) base_js_url: Option<String>,
}

/// Re-run `player()` with a reload token and re-solve `n`. Returns the fresh SABR POST url
/// and the rotated ustreamer config (SABR-037).
async fn reload_session(
    session: &Session,
    recovery: &Recovery,
    reload_token: &str,
) -> Result<(String, Vec<u8>), StreamError> {
    let sts = recovery.base_js.as_deref().and_then(extract_signature_timestamp);
    let player = session
        .player(
            &recovery.video_id,
            &PlayerOptions {
                visitor_data: Some(&recovery.visitor_data),
                signature_timestamp: sts,
                reload_token: Some(reload_token),
                ..Default::default()
            },
        )
        .await?;
    let url = match &recovery.base_js {
        Some(js) => solve_url_n(&player.server_abr_streaming_url, js, &NSigSolver::new()).await?,
        None => player.server_abr_streaming_url.clone(),
    };
    // The caller adopts the rotated ustreamer config; the server withholds media until it
    // does (SABR-037).
    Ok((url, player.ustreamer_config.clone()))
}

/// How long a live cold start may stay empty (no media byte yet) before the SABR session
/// is abandoned for a fresh one. Healthy opens deliver media within a few rounds. A session
/// provisioned cold stays empty for about 24 to 40 s whatever we request, while a fresh
/// watch page usually yields a ready one.
const COLD_START_REBOOTSTRAP_MS: u64 = 6_000;

/// Bound on re-bootstrap attempts per session open. Each costs a watch-page fetch and an
/// n-solve, and a stream that stays empty across several fresh sessions is not warming up.
const COLD_START_REBOOTSTRAP_MAX: u32 = 3;

/// Whether a cold start has been empty long enough to re-bootstrap: `elapsed_ms` since the
/// session (or the previous re-bootstrap) started serving nothing, within the attempt budget.
fn should_cold_rebootstrap(elapsed_ms: u64, attempts: u32) -> bool {
    elapsed_ms >= COLD_START_REBOOTSTRAP_MS && attempts < COLD_START_REBOOTSTRAP_MAX
}

/// Cold-start re-bootstrap: fetch a fresh watch page and adopt its SSR-provisioned SABR
/// session (url and ustreamer config), as the `RELOAD_PLAYER_RESPONSE` path does. An SSR
/// session is usually provisioned ready, where a `player()` POST provisions a cold one. The
/// caller gates on `recovery.base_js`. `base_js` is reused when the fresh page names the
/// same player build and refetched, with `recovery` updated, on a build change, so the
/// n-solve matches the url's signing build (PLAYER-004).
async fn rebootstrap_session(
    session: &Session,
    recovery: &mut Recovery,
) -> Result<(String, Vec<u8>), StreamError> {
    let watch = session
        .get_text(&format!("https://www.youtube.com/watch?v={}", recovery.video_id))
        .await?;
    let player = crate::client::PlayerResponse::from_watch_html(&watch)
        .ok_or(StreamError::NoVideoFormats)?;
    let js_url = extract_player_js_url(&watch);
    let url = match (js_url, &recovery.base_js_url, &recovery.base_js) {
        (Some(new), Some(old), Some(js)) if new == *old => {
            solve_url_n(&player.server_abr_streaming_url, js, &NSigSolver::new()).await?
        }
        (Some(new), _, _) => {
            let js = session.get_text(&new).await?;
            let solved =
                solve_url_n(&player.server_abr_streaming_url, &js, &NSigSolver::new()).await?;
            recovery.base_js = Some(js);
            recovery.base_js_url = Some(new);
            solved
        }
        _ => return Err(StreamError::NoPlayerJsUrl),
    };
    Ok((url, player.ustreamer_config.clone()))
}

/// Poll interval (ms) after a live round that carried media. The reference client requests
/// again almost at once; steady-state media rounds announce no backoff (SABR-002). Waiting
/// out the 5 s hint acquires content below realtime and drains the join cushion. The
/// read-ahead gate paces the loop.
const LIVE_MEDIA_ROUND_POLL_MS: u64 = 250;

/// Per-round sleep in ms. Live media rounds re-poll after [`LIVE_MEDIA_ROUND_POLL_MS`].
/// Live empty rounds honor the server's backoff, floored at 1 s, because the next segment
/// is not published yet; during cold start they poll at 500 ms until the first media. A
/// `SABR_SEEK` reply is answered at once. VOD honors the backoff.
fn round_sleep_ms(is_live: bool, got: usize, cold_start: bool, backoff_ms: u64, seek_reply: bool) -> u64 {
    // The reference client answers a SABR_SEEK reply within ~150 ms, whatever its backoff.
    if seek_reply {
        LIVE_MEDIA_ROUND_POLL_MS
    } else if got == 0 {
        if cold_start {
            500
        } else {
            backoff_ms.max(1000)
        }
    } else if is_live {
        LIVE_MEDIA_ROUND_POLL_MS
    } else {
        backoff_ms
    }
}

/// Cold-start snap slack (ms), added to the expected distance behind the head. A
/// live-edge-seek burst whose tail lands further behind is discarded and the session
/// re-joins at the edge.
const LIVE_SNAP_SLACK_MS: i64 = 5_000;

/// Resolve a [`SabrStream::set_quality`] height against the ladder committed at open: the
/// rung with the greatest height at or below `target` (an exact match wins), else the
/// lowest rung. `None` only for an empty ladder.
fn resolve_quality(ladder: &[(i32, i32)], target: i32) -> Option<(i32, i32)> {
    ladder
        .iter()
        .copied()
        .filter(|&(_, h)| h <= target)
        .max_by_key(|&(_, h)| h)
        .or_else(|| ladder.iter().copied().min_by_key(|&(_, h)| h))
}

/// Resolves once a consumer reposition is waiting. A live request in flight is abandoned
/// for it, as the web player aborts its fetch on a seek.
async fn seek_pending(seek: &Mutex<Option<i64>>, reseek: &AtomicBool) {
    while seek.lock().unwrap().is_none() && !reseek.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// `fut`'s output, or `None` when a live reposition arrives first.
async fn unless_seek<F: std::future::Future>(
    fut: F,
    seek: &Mutex<Option<i64>>,
    reseek: &AtomicBool,
) -> Option<F::Output> {
    tokio::select! {
        r = fut => Some(r),
        _ = seek_pending(seek, reseek) => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_loop(
    session: Session,
    mut lane: LiveLane,
    url: String,
    recovery: Recovery,
    max_buffer_bytes: usize,
    start_ms: i64,
    seek_request: Arc<Mutex<Option<i64>>>,
    consumer_playhead: Arc<Mutex<Option<(std::time::Instant, i64)>>>,
    playback_rate: Arc<AtomicU32>,
    ladder_heights: Vec<(i32, i32)>,
    quality_request: Arc<Mutex<Option<i32>>>,
    live_reseek_request: Arc<AtomicBool>,
    unpin_request: Arc<AtomicBool>,
    audio_buf: TrackBuffer,
    video_buf: TrackBuffer,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<StreamStats>>,
    bwe: Option<BandwidthEstimator>,
    segments: Arc<crate::LiveSegments>,
) {
    let edge_url = url;
    // One PO-token re-attestation budget for the whole session (see `crate::attest`).
    // Re-minting on every status >= 2 round presents the server a new attestation each time
    // (POT-007).
    let reattest_budget = crate::attest::ReattestBudget::new();
    let errors = 0u32;
    // Cold-start re-bootstrap state: while no media has arrived, a fresh watch-page
    // bootstrap replaces the session once it has been empty long enough.
    let cold_empty_since: Option<std::time::Instant> = None;
    let rebootstrap_attempts = 0u32;
    // Live-stall recovery state: count consecutive empty rounds, and a cooldown since
    // the last edge re-seek so a recovery is given time to take effect instead of
    // thrashing mpv with back-to-back discontinuities.
    let live_stall_rounds = 0u32;
    // A SABR_SEEK target the next request asks at (single use); the web player seeks to it
    // and reports from there (SABR-011).
    let seek_follow: Option<i64> = None;
    let rounds_since_reseek = u32::MAX;
    // Live read-ahead gate clock: a wall-clock playhead anchored at the first segment's
    // stream time, `(anchor_instant, base_stream_ms)`. The gate measures read-ahead against
    // it, so it tracks playback time even when the live head goes stale.
    let live_anchor: Option<(std::time::Instant, i64)> = None;
    // One-shot: whether the cold-start position is resolved (snapped to the edge, or already
    // near it). A DVR join (explicit start_ms) lands far behind the head on purpose, so its
    // burst is never discarded.
    let live_snapped = start_ms > 0;
    // Session clock: every `[live]` log line carries `t=` seconds since the loop started, so
    // a log lines up with the consumer's own timestamped log.
    let loop_t0 = std::time::Instant::now();
    // Startup gate: playback is released once PLAYBACK_START_POLICY's readahead threshold
    // (part 47, SABR-027) is met for both tracks, not on the first init segment.
    // `start_min_readahead_ms` floors at 250 ms when no policy has been seen.
    let playback_released = false;
    // Wall-clock anchor for the head estimate: (head observed, when).
    let head_anchor: Option<(i64, std::time::Instant)> = None;
    // Advertise the estimator's seed on round 1, before any measurement exists. The engine's
    // own 3 MB/s default would let the server commit the top rung on an unmeasured link.
    // Each round's `observe` then advertises the measured estimate.
    if let Some(est) = &bwe {
        let seed = est.estimate();
        lane.engine.set_bandwidth_estimate(seed);
    }
    // Set when we ask for a rung change; consumed by the SABR_SEEK the server sends in
    // reply, so that bump is reported as self-inflicted rather than a server reposition.
    let self_reposition_pending = false;
    tracing::debug!("[live] rung control: SERVER-DRIVEN (full ladder, no client rung timer)");
    LiveLoop {
        session,
        lane,
        edge_url,
        recovery,
        max_buffer_bytes,
        seek_request,
        consumer_playhead,
        playback_rate,
        ladder_heights,
        quality_request,
        live_reseek_request,
        unpin_request,
        audio_buf,
        video_buf,
        stop,
        stats,
        bwe,
        segments,
        reattest_budget,
        errors,
        cold_empty_since,
        rebootstrap_attempts,
        live_stall_rounds,
        seek_follow,
        rounds_since_reseek,
        live_anchor,
        live_snapped,
        loop_t0,
        playback_released,
        head_anchor,
        self_reposition_pending,
    }
    .run()
    .await
}

/// The live engine loop: the inputs [`run_loop`] receives from `SabrStream::open` and the
/// state it carries from one round to the next.
struct LiveLoop {
    session: Session,
    lane: LiveLane,
    edge_url: String,
    recovery: Recovery,
    max_buffer_bytes: usize,
    seek_request: Arc<Mutex<Option<i64>>>,
    consumer_playhead: Arc<Mutex<Option<(std::time::Instant, i64)>>>,
    playback_rate: Arc<AtomicU32>,
    ladder_heights: Vec<(i32, i32)>,
    quality_request: Arc<Mutex<Option<i32>>>,
    live_reseek_request: Arc<AtomicBool>,
    unpin_request: Arc<AtomicBool>,
    audio_buf: TrackBuffer,
    video_buf: TrackBuffer,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<StreamStats>>,
    bwe: Option<BandwidthEstimator>,
    segments: Arc<crate::LiveSegments>,
    reattest_budget: crate::attest::ReattestBudget,
    errors: u32,
    cold_empty_since: Option<std::time::Instant>,
    rebootstrap_attempts: u32,
    live_stall_rounds: u32,
    seek_follow: Option<i64>,
    rounds_since_reseek: u32,
    live_anchor: Option<(std::time::Instant, i64)>,
    live_snapped: bool,
    loop_t0: std::time::Instant,
    playback_released: bool,
    head_anchor: Option<(i64, std::time::Instant)>,
    self_reposition_pending: bool,
}

/// One answered request: the merged output, its media bytes, the POST time in s, and
/// the gate playhead in stream ms after a pending `SABR_SEEK` target replaced it.
struct Round {
    out: MergeOut,
    got: usize,
    post_secs: f64,
    edge_pt: i64,
}

enum Fetch {
    Round(Round),
    /// The request was abandoned or failed; start the next iteration.
    Retry,
    /// Transport failures hit the limit; `stats.ended` holds the reason.
    Failed,
}

impl LiveLoop {
    async fn run(mut self) {
        while !self.stop.load(Ordering::Relaxed) {
            self.apply_control();
            if self.park_for_backpressure().await {
                continue;
            }
            let (edge_pt, consumer_lagging) = self.playhead();
            let est_head = self.estimate_head();
            if self.gate_closed(edge_pt, est_head).await {
                continue;
            }
            let mut round = match self.fetch_round(edge_pt).await {
                Fetch::Round(round) => round,
                Fetch::Retry => continue,
                Fetch::Failed => break,
            };
            if self.snap_to_edge() {
                continue;
            }
            let pushed = self.apply_round(&mut round);
            self.recover(&round).await;
            self.on_sabr_seek(round.out.sabr_seek_ms);
            let policy = self.publish(&round, consumer_lagging, pushed);
            if self.stats.lock().unwrap().ended.is_some() {
                break;
            }
            if self.recover_stall(&round) {
                continue;
            }
            self.pace(&policy, &round).await;
        }
        self.finish();
    }

    fn apply_control(&mut self) {
        // Apply a pending seek: reposition the engine, drop pre-seek bytes, rebase the
        // playback clock. A live DVR seek is only a new `player_time` on the wire.
        if let Some(target) = self.seek_request.lock().unwrap().take() {
            tracing::debug!("[live] DVR seek to {target} ms");
            self.lane.reseek_live();
            self.lane.set_live_join_ms(Some(target));
            self.live_anchor = None;
            self.live_stall_rounds = 0;
            self.rounds_since_reseek = 0;
            let mut s = self.stats.lock().unwrap();
            s.seek_generation += 1;
            s.self_repositions += 1;
        }

        // Back to Auto: clear the manual pick so the full ladder rides in prefv again and
        // the server resumes picking. Only the wire shape changes: no format change, so the
        // video pipeline is untouched and playback continues uninterrupted.
        if self.unpin_request.swap(false, Ordering::Relaxed) && self.lane.is_video_pinned() {
            self.lane.unpin_video_quality();
            self.segments.set_pin(0);
            tracing::debug!("[live] quality: back to AUTO (full ladder, server-driven)");
        }

        // Consumer-requested live re-edge: the same mechanics as the stall recovery below.
        // Dropping the anchor makes the next request send the LIVE_EDGE_SEEK_MS sentinel
        // (SABR-009); fresh init and segments splice into the pipe and `seek_generation` bumps.
        // The consumer holds cache when it asks, so the splice plays through.
        if self.live_reseek_request.swap(false, Ordering::Relaxed) {
            tracing::info!("[live] consumer re-edge: splicing head into the pipe");
            self.lane.reseek_live();
            self.live_anchor = None;
            let mut s = self.stats.lock().unwrap();
            s.seek_generation += 1;
            s.live_reseeks += 1;
        }

        // Apply a pending in-stream quality switch: a seamless manual pick (REQ-018, video
        // ranges only) with no re-join, no buffer clear and no seek-generation bump. `player_time` stays continuous
        // and only the video track's buffered state resets, so the old rung plays out while the
        // server delivers the new one from the current playhead. Viewport and field 72 caps
        // stay fixed for the session (REQ-012, REQ-015): moving them mid-session leaves the
        // session with no media (SABR-054).
        if let Some(target_h) = self.quality_request.lock().unwrap().take() {
            if let Some((itag, h)) = resolve_quality(&self.ladder_heights, target_h) {
                self.lane.set_video_quality(itag, h);
                self.segments.set_pin(itag);
                self.self_reposition_pending = true;
                tracing::debug!(
                    "[live] QUALITY switch → {h}p (itag {itag}): seamless manual pick (no re-join)"
                );
            }
        }
    }

    /// True after a 100 ms park while either track buffer is over `max_buffer_bytes`.
    async fn park_for_backpressure(&mut self) -> bool {
        // Backpressure: while the consumer has stopped draining (a paused GUI), pull no more
        // media. Park until it catches up or stop is requested.
        if self.audio_buf.buffered_bytes() > self.max_buffer_bytes
            || self.video_buf.buffered_bytes() > self.max_buffer_bytes
        {
            tracing::debug!(
                "[live] BACKPRESSURE park: a={}B v={}B max={}B",
                self.audio_buf.buffered_bytes(),
                self.video_buf.buffered_bytes(),
                self.max_buffer_bytes
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
            return true;
        }
        false
    }

    /// The gate playhead in stream ms, and whether a fresh consumer report sits behind
    /// the live anchor.
    fn playhead(&mut self) -> (i64, bool) {
        // Playhead: a wall-clock clock anchored at the first segment's stream time once a head
        // is known. The read-ahead gate measures against it, and once anchored it is the wire
        // `player_time` (SABR-029). The engine clamps it to the buffered tail.
        let mut consumer_lagging;
        let edge_pt = {
            if self.live_anchor.is_none() && self.lane.engine.live_head_ms().is_some() {
                if let Some(base) = self.lane.engine.anchor_ms() {
                    self.live_anchor = Some((std::time::Instant::now(), base));
                }
            }
            // The consumer's reported position wins when fresh. The wall-clock model runs
            // behind a real decoder, and the gate then declines fetches the decoder needs. A
            // report older than PLAYHEAD_FRESH_MS falls back to the model. A fresh report
            // behind the anchor base means the consumer has not followed the last
            // reposition; it is recorded in `consumer_lagging` and the model takes over.
            let fresh_report = self
                .consumer_playhead
                .lock()
                .unwrap()
                .filter(|(t, _)| t.elapsed().as_millis() < PLAYHEAD_FRESH_MS)
                .map(|(_, ms)| ms);
            consumer_lagging = false;
            let live_anchor = self.live_anchor;
            let reported = fresh_report.filter(|&ms| {
                let follows = live_anchor.is_none_or(|(_, base)| ms >= base);
                consumer_lagging = !follows;
                follows
            });
            match self.live_anchor {
                Some((t0, base)) => {
                    // Live stream times are absolute. A report of 0 (before the live file
                    // loads, or after a teardown) is not believed: `player_time = 0` lands at
                    // the DVR start (SABR-044). A report is accepted only at or after the
                    // point we joined at.
                    //
                    // The fallback model advances at the playback rate: a 2x consumer moves
                    // twice as fast as wall clock, and a 1x model would read half the true
                    // runway and under-fetch. The rate is re-read each iteration.
                    let wall_rate =
                        f64::from(f32::from_bits(self.playback_rate.load(Ordering::Relaxed)));
                    let wall = reported
                        .filter(|&ms| ms >= base)
                        .unwrap_or(base + (t0.elapsed().as_millis() as f64 * wall_rate) as i64);
                    // Never let the gate playhead pass buffered content: a decoder stalls on
                    // underrun, and a runaway playhead would drive read-ahead negative and
                    // spin the gate. In steady state the clamp changes nothing.
                    match self.lane.engine.buffered_end_ms() {
                        Some(end) => wall.min(end),
                        None => wall,
                    }
                }
                None => self.lane.engine.next_player_time(None), // pre-anchor cold-start probe
            }
        };
        (edge_pt, consumer_lagging)
    }

    /// The live head in stream ms, extrapolated at 1x wall-clock from the last observed one.
    fn estimate_head(&mut self) -> Option<i64> {
        // Wall-clock estimate of the live head. The observed head refreshes only on a fetch
        // response, so re-anchor when it advances and extrapolate between at 1x. Without it
        // the gate shuts for good once fetching stops at the edge. Re-anchoring only on a
        // strictly higher head keeps gated iterations extrapolating.
        if let Some(h) = self.lane.engine.live_head_ms() {
            if self.head_anchor.is_none_or(|(ph, _)| h > ph) {
                self.head_anchor = Some((h, std::time::Instant::now()));
            }
        }
        // Extrapolate at 1x wall clock. A request for an unpublished segment can be held about
        // 7.5 s (SABR-041).
        self.head_anchor.map(|(h, t)| {
            let raw = h + t.elapsed().as_millis() as i64;
            raw.max(h)
        })
    }

    /// True after sleeping until the read-ahead gate could reopen, when it is closed.
    async fn gate_closed(&mut self, edge_pt: i64, est_head: Option<i64>) -> bool {
        let lane = &self.lane;
        // Readahead gate: wait for a head before gating.
        let gate_ready = lane.engine.live_head_ms().is_some();
        if gate_ready && !lane.engine.should_request(edge_pt) {
            if tracing::enabled!(tracing::Level::DEBUG) {
                let (ba, bv) = lane.engine.buffered_ms_by_kind();
                let pol = lane.engine.request_policy();
                tracing::debug!(
                    "[live] t={:.3} GATE CLOSED (no fetch): head={:?} est_head={:?} buf_end={:?} frontier={:?} pt={} fwd={:?} buf_a={} buf_v={} edge_margin={} buf_target={} head_minus_maxseek={:?} margin_ok={:?} target_a={:?} target_v={:?}",
                    self.loop_t0.elapsed().as_secs_f64(),
                    lane.engine.live_head_ms(), est_head, lane.engine.buffered_end_ms(),
                    lane.engine.buffered_end_ms(), edge_pt,
                    // The quantity the gate decides on, logged so a reader need not derive
                    // it (buf_a/buf_v are cumulative spans).
                    lane.engine.buffered_end_ms().map(|f| f - edge_pt),
                    ba, bv,
                    lane.engine.live_target_readahead_ms(), lane.engine.live_buffer_target_ms(),
                lane.engine.max_seekable_ms().zip(lane.engine.live_head_ms()).map(|(m, h)| h - m),
                    lane.engine.live_head_ms().zip(lane.engine.buffered_end_ms()).map(|(h, e)| h - e),
                    pol.target_audio_readahead_ms, pol.target_video_readahead_ms
                );
            }
            // Sleep until the gate could reopen.
            let nap = lane.engine.ms_until_gate_reopen(edge_pt);
            tokio::time::sleep(Duration::from_millis(nap)).await;
            return true;
        }
        false
    }

    async fn fetch_round(&mut self, edge_pt: i64) -> Fetch {
        // Sync the current playback rate so (a) field 35 rides live requests, and
        // (b) the gate measures runway in playback seconds.
        let rate = f32::from_bits(self.playback_rate.load(Ordering::Relaxed));
        self.lane.engine.set_playback_rate(rate);

        self.lane.engine.set_keep_ranges_on_seek(self.live_anchor.is_some());
        let edge_pt = match self.seek_follow.take() {
            Some(target) if self.live_anchor.is_some() => target,
            _ => edge_pt,
        };
        let post_t = std::time::Instant::now();
        // Once anchored, the playhead is the wire player_time; before that the engine falls
        // back to the sentinel or the buffered tail.
        let wire_pt = self.lane.engine.next_player_time(self.live_anchor.map(|_| edge_pt));
        let edge_req = self.lane.engine.request_bytes(wire_pt);
        let edge = self.session.post_sabr(&self.edge_url, edge_req);
        let Some(edge_res) = unless_seek(edge, &self.seek_request, &self.live_reseek_request).await else {
            tracing::info!("[live] request abandoned for a reposition");
            return Fetch::Retry;
        };
        let edge_resp = match edge_res {
            Ok(b) => {
                self.errors = 0;
                b
            }
            Err(e) => {
                self.errors += 1;
                if self.errors >= 5 {
                    self.stats.lock().unwrap().ended = Some(EndReason::Failed(e.to_string()));
                    return Fetch::Failed;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                return Fetch::Retry;
            }
        };
        let post_secs = post_t.elapsed().as_secs_f64();
        // Feed this round's throughput (wire bytes over round time) to the estimator and
        // advertise the result on the next request, so the server downshifts on a narrow pipe
        // (REQ-016, SABR-032).
        if let Some(est) = &mut self.bwe {
            let bps = est.observe(edge_resp.len(), post_secs);
            self.lane.engine.set_bandwidth_estimate(bps);
        }
        let out = self.lane.round_merge(edge_resp);
        let got = out.segments.iter().map(|s| s.bytes.len()).sum::<usize>();
        Fetch::Round(Round { out, got, post_secs, edge_pt })
    }

    /// True when the cold-start burst was discarded and the next request re-joins at the edge.
    fn snap_to_edge(&mut self) -> bool {
        // Cold-start snap-to-edge. The first request sends the live-edge sentinel (SABR-009);
        // the server can answer with a media burst whose tail sits far behind the head. Walking
        // forward from it at 1x keeps playback that far behind the edge. The first time the
        // head is known, a burst landing further behind than the expected distance is discarded
        // (`reseek_live` clears progress and keeps the head) and the session re-joins via the
        // sentinel. One-shot; a stream that starts near the edge proceeds normally. The snap is
        // specific to our client: base.js has no counterpart.
        if !self.live_snapped {
            if let Some(head) = self.lane.engine.live_head_ms() {
                // How far behind the head a burst may land before it counts as not the edge.
                // When the server has sent `max_seekable`, anchor to it: it is the request
                // ceiling on SABR live (SABR-017), so a burst ending there is the web client's
                // own position. Without it, use the edge margin of the chunk model. That margin
                // falls back to a 1000 ms chunk floor before a segment duration is learned,
                // which on 5 s segments sits below the server's placement (SABR-016).
                let snap_at = match self.lane.engine.max_seekable_ms() {
                    Some(maxs) if head - maxs > 0 => (head - maxs) + LIVE_SNAP_SLACK_MS,
                    _ => self.lane.engine.live_target_readahead_ms() + LIVE_SNAP_SLACK_MS,
                };
                match self.lane.engine.buffered_end_ms() {
                    Some(end) if head - end > snap_at => {
                        self.lane.reseek_live();
                        self.live_anchor = None;
                        self.live_snapped = true;
                        tracing::debug!(
                            "[live] SNAP to edge: discarded cold-start burst ({}ms behind head); next request re-joins at the edge",
                            head - end
                        );
                        return true; // skip the push; next round re-joins via the sentinel
                    }
                    _ => self.live_snapped = true, // already near the edge / no burst to discard
                }
            }
        }
        false
    }

    /// Hands the round's segments to the consumer and adopts a `SABR_REDIRECT`. Returns
    /// the media bytes released for playback this round.
    fn apply_round(&mut self, round: &mut Round) -> usize {
        let got = round.got;
        let out = &mut round.out;
        self.segments.insert(std::mem::take(&mut out.segments));
        // Startup gate: playback counts as started once the buffered span clears
        // PLAYBACK_START_POLICY's threshold for both tracks (SABR-027).
        let mut pushed = 0usize;
        if !self.playback_released {
            let (buffered_a, buffered_v) = self.lane.engine.buffered_ms_by_kind();
            let threshold = self.lane.engine.start_min_readahead_ms();
            if buffered_a >= threshold && buffered_v >= threshold {
                self.playback_released = true;
                tracing::debug!(
                    "[live] START release: {buffered_a}ms/{buffered_v}ms >= {threshold}ms \
resume_policy={:?}ms",
                    self.lane.engine.resume_min_readahead_ms()
                );
            }
        } else {
            pushed = got;
        }
        if let Some(r) = out.redirect.take() {
            self.edge_url = r;
        }
        pushed
    }

    /// Re-attests, reloads the player response, or re-bootstraps a cold session, as the
    /// round calls for.
    async fn recover(&mut self, round: &Round) {
        let out = &round.out;
        let got = round.got;
        // STREAM_PROTECTION_STATUS >= 2 means the server withholds media until we re-attest
        // (GATE-001). A disputed attestation also retires the cached watch pages, so the next
        // open is not funded from the page that failed. The re-mint below follows the web
        // player's `spsumpreject` flow under a per-session budget (POT-006, POT-007).
        if out.protection_status.is_some_and(|p| p != 1) {
            self.session.watch_pages().clear();
        }
        match self.reattest_budget.on_status(out.protection_status) {
            step @ (crate::attest::Reattest::Soft | crate::attest::Reattest::Hard) => {
                // A plain re-mint reuses the provider's cached attestation; only a `Hard`
                // step (status 3, after the soft one is spent) rebuilds it.
                if step == crate::attest::Reattest::Hard {
                    self.recovery.pot.invalidate_attestation().await;
                }
                if let Ok(tok) = self
                    .recovery
                    .pot
                    .get_po_token(&self.recovery.video_id, self.recovery.attest.as_ref())
                    .await
                {
                    if let Some(bytes) = decode_base64_lenient(&tok) {
                        self.lane.engine.set_po_token(bytes);
                        tracing::debug!(
                            "[live] prot={:?} -> {} PO token",
                            out.protection_status,
                            if step == crate::attest::Reattest::Hard {
                                "invalidated attestation + re-minted"
                            } else {
                                "re-minted"
                            },
                        );
                    }
                }
            }
            crate::attest::Reattest::Nothing => {
                if self.reattest_budget.exhausted() && self.reattest_budget.warn_once() {
                    tracing::warn!(
                        "[live] prot={:?} PERSISTS after a hard re-attest — the PO-token provider (bgutil on :4416) is not producing accepted tokens; restart it",
                        out.protection_status,
                    );
                }
            }
        }
        // RELOAD_PLAYER_RESPONSE: re-run player() with the reload token, re-solve `n`, re-point
        // the POST url and adopt the rotated ustreamer config. The server withholds media until
        // the session is reloaded (SABR-037).
        if let Some(token) = out.reload_token.clone() {
            match reload_session(&self.session, &self.recovery, &token).await {
                Ok((fresh, config)) => {
                    tracing::debug!("[live] RELOAD_PLAYER_RESPONSE — reloaded session, adopted new config");
                    self.edge_url = fresh;
                    self.lane.set_ustreamer_config(config);
                }
                Err(e) => {
                    tracing::debug!("[live] reload failed: {e:?}");
                }
            }
        }

        // Prolonged empty cold start: the SSR session behind this url was provisioned cold and
        // request timing cannot fix it. A fresh watch page usually yields a ready session;
        // adopt its url and config as the reload path does. Needs base_js for `n`.
        if self.recovery.base_js.is_some() {
            let started = self.stats.lock().unwrap().started;
            let due = if !started && got == 0 {
                let since = *self.cold_empty_since.get_or_insert_with(std::time::Instant::now);
                should_cold_rebootstrap(since.elapsed().as_millis() as u64, self.rebootstrap_attempts)
            } else {
                if got > 0 {
                    self.cold_empty_since = None;
                }
                false
            };
            if due {
                self.rebootstrap_attempts += 1;
                match rebootstrap_session(&self.session, &mut self.recovery).await {
                    Ok((fresh, config)) => {
                        tracing::debug!(
                            "[live] cold-start re-bootstrap #{}: fresh SSR session adopted",
                            self.rebootstrap_attempts
                        );
                        self.edge_url = fresh;
                        self.lane.set_ustreamer_config(config);
                        self.cold_empty_since = Some(std::time::Instant::now());
                    }
                    Err(e) => {
                        tracing::debug!("[live] cold-start re-bootstrap failed: {e:?}");
                    }
                }
            }
        }
    }

    fn on_sabr_seek(&mut self, sabr_seek_ms: Option<i64>) {
        // SABR_SEEK: the server repositioned the session to `seek_ms` (SABR-010). The engine
        // has reset its dedup and lead-in and delivered init-led media this round, so the
        // buffers are coherent. The next request asks at the target (SABR-011).
        // `seek_generation` signals the discontinuity to a consumer that did not initiate it.
        if let Some(seek_ms) = sabr_seek_ms {
            self.seek_follow = Some(seek_ms);
            let mut s = self.stats.lock().unwrap();
            s.seek_generation += 1;
            s.sabr_seeks += 1;
            s.sabr_seek_ms = Some(seek_ms);
            // Credit the bump to us when it answers a rung change we asked for. The server
            // replies a round or two later, so the flag persists until then.
            if std::mem::take(&mut self.self_reposition_pending) {
                s.self_repositions += 1;
            }
        }
    }

    /// Updates [`StreamStats`], writes the round's log line and dump record, and returns
    /// the server's request policy for pacing.
    fn publish(&self, round: &Round, consumer_lagging: bool, pushed: usize) -> RequestPolicy {
        let Round { out, got, post_secs, edge_pt } = round;
        let (got, post_secs, edge_pt) = (*got, *post_secs, *edge_pt);
        let lane = &self.lane;
        let live_anchor = self.live_anchor;
        let policy = lane.engine.request_policy();
        let (buf_a, buf_v) = lane.engine.buffered_ms_by_kind();
        {
            let mut s = self.stats.lock().unwrap();
            s.resume_min_readahead_ms = lane.engine.resume_min_readahead_ms();
            s.buffered_audio_ms = buf_a;
            s.buffered_video_ms = buf_v;
            s.live_head_ms = lane.engine.live_head_ms();
            s.live_min_seekable_ms = lane.engine.min_seekable_ms();
            s.live_max_seekable_ms = lane.engine.max_seekable_ms();
            // Publish a playhead a consumer can act on: before the anchor, the sentinel
            // (2^53 - 1) would read as an enormous negative `head - player_time`. Never
            // publish a position past the observed head.
            s.player_time_ms = match lane.engine.live_head_ms() {
                Some(h) => edge_pt.min(h),
                None => edge_pt,
            };
            s.consumer_behind_anchor = consumer_lagging;
            s.protection_status = out.protection_status;
            if let Some(itag) = out.served_video_itag {
                s.served_video_itag = Some(itag);
            }
            s.last_post_secs = post_secs;
            s.rounds += 1;
            s.discontinuities += out.discontinuities.len() as u64;
            // `started` means first media released. The startup gate can hold received bytes,
            // so it follows the bytes pushed this round (`got` counts the received ones).
            if pushed > 0 {
                s.started = true;
            }
            // A SABR_ERROR ends the response (SABR-038); record it and stop.
            if let Some(err) = &out.sabr_error {
                s.ended = Some(EndReason::Failed(format!(
                    "sabr_error type={:?} code={:?}",
                    err.r#type, err.code
                )));
            }
        }
        if tracing::enabled!(tracing::Level::DEBUG) {
            let rounds = self.stats.lock().unwrap().rounds;
            let req_pt = lane.engine.next_player_time(live_anchor.map(|_| edge_pt));
            let bend = lane.engine.buffered_end_ms();
            // `fwd` is the quantity the gate decides on (frontier - playhead). `lag_a` and
            // `lag_v` are how far behind the live head the newest received segment starts,
            // which tells a late request from a late-finalizing server.
            let head_now = lane.engine.live_head_ms();
            let (newest_a, newest_v) = lane.engine.newest_start_ms_by_kind();
            let lag = |newest: Option<i64>| -> Option<i64> { Some(head_now? - newest?) };
            tracing::debug!(
                // `rbuf_a` and `rbuf_v` are bytes in each consumer TrackBuffer, handed to the
                // reader and not yet drained. The engine's `fwd` is a minimum across tracks, so
                // it hides one reader starving while the other is fine.
                "[live] t={:.3} round {rounds}: head={:?} gate_pt={edge_pt} req_pt={req_pt} buf_end={bend:?} fwd={:?} lag_a={:?} lag_v={:?} rbuf_a={} rbuf_v={} buf_a={buf_a} buf_v={buf_v} post={post_secs:.2}s got={got}B bwe={}KB/s prot={:?} served_vitag={:?} viewport={:?} reload={} err={:?}",
                self.loop_t0.elapsed().as_secs_f64(),
                lane.engine.live_head_ms(),
                lane.engine.buffered_end_ms().map(|f| f - edge_pt),
                lag(newest_a),
                lag(newest_v),
                self.audio_buf.buffered_bytes(),
                self.video_buf.buffered_bytes(),
                self.bwe.as_ref().map(|e| e.estimate() / 1024).unwrap_or(-1),
                out.protection_status,
                out.served_video_itag,
                lane.viewport(),
                out.reload_token.is_some(),
                out.sabr_error.as_ref().map(|e| (e.r#type.clone(), e.code)),
            );
        }
        // Per-round summary for the request dump; it holds no secrets. Independent of the log
        // level.
        if crate::client::dump::enabled() {
            let rounds = self.stats.lock().unwrap().rounds;
            crate::client::dump::record(
                "sabr_round",
                serde_json::json!({
                    "round": rounds,
                    "head_ms": lane.engine.live_head_ms(),
                    "gate_pt_ms": edge_pt,
                    "req_pt_ms": lane.engine.next_player_time(live_anchor.map(|_| edge_pt)),
                    "buf_end_ms": lane.engine.buffered_end_ms(),
                    "buf_a_ms": buf_a,
                    "buf_v_ms": buf_v,
                    "post_secs": post_secs,
                    "got_bytes": got,
                    "prot": out.protection_status,
                    "served_vitag": out.served_video_itag,
                    "viewport": [lane.viewport().0, lane.viewport().1],
                    "reload": out.reload_token.is_some(),
                    "discontinuities": out.discontinuities.len(),
                    "sabr_error": out.sabr_error.as_ref().map(|e| serde_json::json!({"type": e.r#type, "code": e.code})),
                    "sabr_seek_ms": out.sabr_seek_ms,
                }),
            );
        }
        policy
    }

    /// True when a wedged live session was re-anchored to the edge.
    fn recover_stall(&mut self, round: &Round) -> bool {
        let (got, edge_pt) = (round.got, round.edge_pt);
        // Live-stall recovery, a backstop for sustained delivery stalls. A wedge pins the
        // buffered tail while the live head advances and the server answers empty rounds
        // indefinitely. A transient dip differs in how far the buffered tail has fallen behind
        // the head (`head - buffered_end`): a wedge drifts tens of seconds back, a brief gap
        // stays within a few. Re-seek only when well behind and dry, since re-seeking a shallow
        // dip clears the buffer and splices a discontinuity for nothing. A cooldown after each
        // re-seek gives the recovery time to land.
        let started = self.stats.lock().unwrap().started;
        if started && got == 0 {
            self.live_stall_rounds += 1;
        } else {
            self.live_stall_rounds = 0;
        }
        self.rounds_since_reseek = self.rounds_since_reseek.saturating_add(1);
        let behind = match (self.lane.engine.live_head_ms(), self.lane.engine.buffered_end_ms()) {
            (Some(h), Some(end)) => h - end,
            _ => 0,
        };
        // Empty rounds with media still buffered ahead are the server pacing a DVR
        // playhead, not a wedge.
        let dry = self.lane.engine.buffered_end_ms().is_none_or(|f| f - edge_pt < LIVE_STALL_DRY_MS);
        if started
            && dry
            && got == 0
            && self.live_stall_rounds >= 3
            && behind > 15_000
            && self.rounds_since_reseek >= 20
        {
            tracing::debug!(
                "[live] STALL recover: re-seek to edge ({behind}ms behind live, {} empty rounds)",
                self.live_stall_rounds
            );
            self.lane.reseek_live();
            // Drop the anchor, as the cold-start snap does. With the anchor kept, the next
            // request sends the pre-seek playhead instead of the `LIVE_EDGE_SEEK_MS` sentinel,
            // and the server answers with no media from then on.
            self.live_anchor = None;
            {
                let mut s = self.stats.lock().unwrap();
                s.seek_generation += 1;
                s.live_reseeks += 1;
            }
            self.live_stall_rounds = 0;
            self.rounds_since_reseek = 0;
            return true;
        }
        false
    }

    async fn pace(&mut self, policy: &RequestPolicy, round: &Round) {
        // Pace the next round (see `round_sleep_ms`). Sleep in short hops so a `stop()` or a
        // pending seek takes effect promptly instead of after a full backoff.
        let backoff_ms = policy.backoff_time_ms.unwrap_or(0).clamp(0, 5000) as u64;
        let cold_start = !self.stats.lock().unwrap().started;
        let mut sleep_ms =
            round_sleep_ms(true, round.got, cold_start, backoff_ms, round.out.sabr_seek_ms.is_some());
        while sleep_ms > 0
            && !self.stop.load(Ordering::Relaxed)
            && self.seek_request.lock().unwrap().is_none()
        {
            let hop = sleep_ms.min(250);
            tokio::time::sleep(Duration::from_millis(hop)).await;
            sleep_ms -= hop;
        }
    }

    fn finish(self) {
        {
            let mut s = self.stats.lock().unwrap();
            if s.ended.is_none() {
                s.ended = Some(EndReason::Stopped);
            }
        }
        // Persist the measured byterate as the next session's cold-start seed, as the web
        // player stores `yt-player-bandwidth` (PLAYER-009).
        if let Some(est) = &self.bwe {
            if let Some(bps) = est.persistable() {
                innertube_sabr::bandwidth_persist::save(bps);
            }
        }
        self.audio_buf.close();
        self.video_buf.close();
        self.segments.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_available_height_resolves_to_its_own_rung() {
        let ladder = [(303, 1080), (302, 720), (244, 480)];
        assert_eq!(resolve_quality(&ladder, 720), Some((302, 720)));
    }

    #[test]
    fn an_unavailable_height_resolves_to_the_closest_rung_below() {
        let ladder = [(303, 1080), (302, 720), (244, 480)];
        assert_eq!(resolve_quality(&ladder, 900), Some((302, 720)));
    }

}
