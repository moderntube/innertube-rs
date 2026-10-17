use super::*;

impl SabrEngine {
    /// The `player_time_ms` to send next, given the caller's playhead (the real playback
    /// position; `None` before the run loop has anchored one).
    ///
    /// Before the first live head is known this returns the [`LIVE_EDGE_SEEK_MS`] sentinel.
    /// Once media is buffered it sends the playhead: the web client's edge `player_time`
    /// follows the playback position, about 4 s behind its buffered frontier (SABR-029).
    /// `buffered_ranges` tell the server what is held, so a position behind the frontier does
    /// not re-deliver. It is clamped to the buffered tail: a position past it would skip the
    /// segments in between and splice a gap into both tracks. With no playhead yet it falls
    /// back to the buffered tail.
    pub fn next_player_time(&self, playhead_ms: Option<i64>) -> i64 {
        if !self.is_live {
            // VOD: sequential from 0, advancing from the buffered tail so each round
            // pulls the next segment. No live head, no edge sentinel.
            return self.buffered_end_ms().unwrap_or(0);
        }
        // A DVR join: the caller asked for a point in the DVR window instead of the edge.
        // Join there while no media is buffered, then follow the frontier.
        let join = |edge: i64| self.live_join_ms.unwrap_or(edge);
        // With nothing buffered, ask for the edge position the server last gave
        // (`last_sabr_seek_ms`), else the live-edge sentinel.
        let cold = || join(self.last_sabr_seek_ms.unwrap_or(LIVE_EDGE_SEEK_MS));
        // base.js `FX()`: the furthest position a request may be made at. On the SABR path it
        // is the server's `LIVE_METADATA.max_seekable_time` (SABR-017); the head plays no part:
        //   FX(){ return this.policy.wT && this.videoData.A ? this.videoData.A.rN
        //                                                   : max(J() - Kl(this), P2()) }
        // `policy.wT` is set for SABR streams.
        let fx = |pt: i64| match self.max_seekable_ms {
            Some(m) => pt.min(m),
            None => pt,
        };
        match self.live_head_ms {
            None => cold(),
            Some(_) => match self.buffered_end_ms() {
                Some(end) => fx(playhead_ms.map_or(end, |p| p.min(end))),
                // No buffer yet: cold start, or a re-join after a SNAP or reseek (which clears
                // progress). Both go through the live-edge sentinel. The web client seeks to
                // `Infinity` on every (re)join and the server places it at the edge (SABR-009);
                // a client-computed `head - distance` moves faster than a cold session can follow.
                None => cold(),
            },
        }
    }

    /// How long (ms) until [`should_request`] could next return true, so the caller can sleep
    /// that long instead of polling on a fixed tick.
    ///
    /// The live gate opens when readahead drains, `frontier - player_time < target`, which
    /// happens as the playhead advances at `rate`. A fixed 250 ms poll would add up to a
    /// quarter of a 1000 ms segment of lateness to every fetch.
    ///
    /// Capped at 100 ms so an imperfect model degrades to a faster poll, not a stall.
    pub fn ms_until_gate_reopen(&self, player_time_ms: i64) -> u64 {
        const MIN_MS: i64 = 5;
        const MAX_MS: i64 = 100;
        if !self.is_live {
            return MIN_MS as u64;
        }
        if self.live_head_ms.is_none() {
            return MIN_MS as u64;
        }
        let mut wait: i64 = 0;
        // The gate has no frontier-vs-head condition, so only readahead draining reopens it.
        // Readahead drains as the playhead advances at `rate`. `buffered_end_ms` is the min
        // across tracks of `first_start + duration`, the quantity `should_request`'s per-kind
        // `unclamped_ahead` minimises over.
        let rate = f64::from(self.effective_rate()).max(0.0625);
        let target = (self.live_buffer_target_ms() as f64) * rate;
        if let Some(frontier) = self.buffered_end_ms() {
            let ahead = (frontier - player_time_ms) as f64;
            if ahead >= target {
                wait = wait.max(((ahead - target) / rate).ceil() as i64);
            }
        }
        wait.clamp(MIN_MS, MAX_MS) as u64
    }

    /// Whether to issue another SABR request now, mirroring the web client's readahead-met
    /// gate (SABR-019). Request only while a track is buffered less than its target readahead
    /// ahead of the playhead; once both audio and video have at least their target buffered
    /// ahead, return false. Without the gate we would keep pulling segments past the server's
    /// `[player_time, player_time + readahead]` window.
    ///
    /// Before any `NEXT_REQUEST_POLICY` has given us targets, allow the request (the web
    /// client's `noreadahead` path); that is the cold-start edge probe.
    pub fn should_request(&self, player_time_ms: i64) -> bool {
        if self.is_live {
            // Live gates on `live_buffer_target_ms`: the server's targets capped by `kV`
            // (SABR-019, SABR-020). Before the first head, the request goes (the edge probe).
            if self.live_head_ms.is_none() {
                return true;
            }
            // There is no frontier-vs-head gate; the web client has none. Its complete set of
            // request gates (base.js 9fc68080, `shouldMakeRequest`) is:
            //
            //   requestRateGate              -> "ratelimited"
            //   both tracks ended            -> "endofstream"
            //   ssdai / ads                  -> "ssdaiblocked" / "waitingforads"
            //   no policy yet                -> ACCEPT (nopolicy)
            //   utc / audio / constraint     -> ACCEPT
            //   no target readahead          -> ACCEPT (noreadahead)
            //   policy.j && loader.N0()      -> ACCEPT (seekToHead)
            //   buffered-ahead vs B          -> DECLINE "readaheadmet"
            //   shorts/vod buffered to end   -> DECLINE
            //
            // None compares the buffered frontier against the live head. The only buffer gate
            // is `readaheadmet`, the target check below (SABR-019). A frontier-vs-head gate
            // would cap the forward buffer at `behind_head - margin`.
            //
            // The permanent edge stall such a gate would guard against is handled as in
            // base.js: the request position is clamped through `FX`, the server's
            // `max_seekable` (see `next_player_time`). That is a ceiling on the position we
            // ask for; what the server sends is unrestricted.
            // Max within the kind (see `kind_end_ms`): a retired rung's frozen progress must
            // not pin this at 0 for the rest of the session.
            let unclamped_ahead =
                |is_video: bool| -> i64 {
                    self.kind_end_ms(is_video).map_or(0, |end| end - player_time_ms)
                };
            // Rate-aware: runway is measured in playback seconds (`buffered / rate`), as base.js
            // `YV`/`Hi` do. At 2x the same content is half the runway, so the gate fetches twice
            // as much content for the same cushion (buffered/rate < target is
            // buffered < target * rate).
            let rate = f64::from(self.effective_rate());
            // The buffer target is base.js `Yyp`'s `B` (SABR-019); the chunk model gives the
            // edge margin.
            let target = (self.live_buffer_target_ms() as f64) * rate;
            return (unclamped_ahead(false) as f64) < target
                || (unclamped_ahead(true) as f64) < target;
        }
        // VOD: honor the server's readahead targets.
        let policy = self.request_policy();
        let (target_audio, target_video) =
            match (policy.target_audio_readahead_ms, policy.target_video_readahead_ms) {
                (Some(a), Some(v)) => (a as i64, v as i64),
                _ => return true,
            };
        // Min buffered-ahead across each kind's received tracks; a kind with no data
        // yet reads as 0 ahead (it needs data → request).
        let buffered_ahead =
            |is_video: bool| -> i64 { self.kind_end_ms(is_video).map_or(0, |end| end - player_time_ms) };
        // Rate-aware: runway in playback seconds (base.js `YV`/`Hi`), so at 2x the same
        // content is half the runway and the gate fetches proportionally more.
        let rate = f64::from(self.effective_rate());
        (buffered_ahead(false) as f64) < target_audio as f64 * rate
            || (buffered_ahead(true) as f64) < target_video as f64 * rate
    }

    /// Consumer playback rate clamped to the representable range; 1.0 until told otherwise.
    /// base.js consumes buffered media at `playbackRate`, so every buffer-time comparison in
    /// the gate is made on `buffered / rate` (base.js `YV`, `Hi`).
    pub(super) fn effective_rate(&self) -> f32 {
        self.playback_rate.clamp(0.0625, 16.0)
    }

    /// How close to the live head a request may go, in ms: `liveChunkReadahead` chunks of the
    /// learned segment duration (SABR-022, SABR-023). [`Self::live_buffer_target_ms`] is the hold.
    pub fn live_target_readahead_ms(&self) -> i64 {
        let seg = self.progress.values().map(|p| p.seg_dur_ms).max().unwrap_or(0);
        self.live_chunk_readahead as i64 * seg.max(LIVE_CHUNK_FLOOR_MS)
    }

    /// base.js `kV` (`readaheadTargetSecs` in the de-obfuscated source): the per-track
    /// readahead ceiling in ms, a byte budget over that track's byterate (SABR-020).
    ///
    /// ```text
    /// kV = (audio ? policy.yg : policy.Om) / (track.eH * policy.yd)   // seconds
    ///      policy.AC > 0 && (kV = min(kV, policy.AC))
    /// ```
    ///
    /// Constants from the policy class `g.Gkp` (base.js 9fc68080): `Om = 20971520` (20 MiB
    /// video), `yg = 8388608` (8 MiB audio), `yd = 1`, `AC = 120` seconds. `eH` is a
    /// byterate in bytes/sec.
    ///
    /// It guards memory. On live it rarely binds: against a 15 s server target it matters
    /// only once video exceeds ~11 Mbps (20971520 / 15 = 1.4 MB/s), and below ~1.4 Mbps it
    /// saturates at the 120 s `AC` cap. Besides the server's targets it is the only bound on
    /// an over-fetch.
    ///
    /// The byterate is measured from the bytes received over the media duration in
    /// `progress`. `None` until enough has arrived to measure one.
    pub(super) fn kv_target_ms(&self, is_video: bool) -> Option<i64> {
        const VIDEO_BUDGET_BYTES: f64 = 20_971_520.0; // policy.Om
        const AUDIO_BUDGET_BYTES: f64 = 8_388_608.0; // policy.yg
        const AC_CAP_MS: i64 = 120_000; // policy.AC = 120 s
        let (bytes, ms) = self
            .progress
            .iter()
            .filter(|(itag, _)| self.kind.get(itag).copied().unwrap_or(false) == is_video)
            .fold((0u64, 0i64), |(b, d), (itag, p)| {
                (b + self.bytes_by_itag.get(itag).copied().unwrap_or(0), d + p.duration_ms())
            });
        if bytes == 0 || ms <= 0 {
            return None;
        }
        let byterate = bytes as f64 / (ms as f64 / 1000.0);
        let budget = if is_video { VIDEO_BUDGET_BYTES } else { AUDIO_BUDGET_BYTES };
        Some(((budget / byterate) * 1000.0).round() as i64).map(|ms| ms.min(AC_CAP_MS))
    }

    pub fn live_buffer_target_ms(&self) -> i64 {
        let pol = self.request_policy();
        // base.js `Yyp` (the web client's `should_request`, SABR-019):
        //   B = min( min(kV(audio)*1000, targetAudio), min(kV(video)*1000, targetVideo) )
        // This is the buffer target, kept separate from the edge-margin chunk model of
        // `live_target_readahead_ms`. The request gate fires while either track is under B.
        let with_kv = |target: Option<i32>, is_video: bool| -> Option<i64> {
            let t = target.filter(|v| *v > 0).map(i64::from)?;
            Some(match self.kv_target_ms(is_video) {
                Some(kv) => t.min(kv),
                None => t,
            })
        };
        match (with_kv(pol.target_audio_readahead_ms, false), with_kv(pol.target_video_readahead_ms, true)) {
            // `B = min(r, a)`: the plain minimum, with no floor.
            (Some(a), Some(v)) => a.min(v),
            // base.js: with no audio or video target the web client fetches
            // (`qd(e,{noreadahead:1}), !0`). An unreachable target makes the gate read
            // "always under", which is the same decision.
            _ => i64::MAX / 4,
        }
    }
}
