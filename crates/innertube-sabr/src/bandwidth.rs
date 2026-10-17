//! Throughput estimator for live ABR, a port of the web player's byterate estimator
//! (PLAYER-008). Pure and deterministic (no time or IO).
//!
//! Traced from base.js `9fc68080`:
//! ```text
//!   Lg = function(e){ e = e.G.uK(); return e>0 ? e : 1 }   // the byterate
//!   BW = function(e,r,w,a=r){ e.G.aK(a, w/r); ... }        // weight=elapsed, value=bytes/elapsed
//!   ... e.policy.dT ? new o0D : e.policy.C ? new NY(17) : this.G = new wz(17,20,.5,.4)
//! ```
//! `wz(seconds=17, resolution=20, center=.5, spread=.4)` is a time-weighted ring: capacity is
//! `17*20 = 340` slots and one sample occupies `round(elapsed*20)` of them, so a round's
//! influence is proportional to how long it took. The emitted value `uK()` is the mean of
//! the 10th, 50th and 90th percentiles.
//!
//! `ClientAbrState.bandwidth_estimate` is the only request field that moves the server's
//! rung choice (REQ-016). The window is bounded in time (17 s), so stale fast samples
//! cannot outlive a throughput drop, and weighting by elapsed time lets a long slow
//! transfer dominate while a 20 ms empty round occupies one slot.


/// Our `yt-player-bandwidth` equivalent: the last session's measured byterate, so a cold
/// start begins from a real measurement instead of nothing.
///
/// base.js keeps this in localStorage as `{byterate, delay, stall, init}` and seeds the ring
/// with `.byterate` (PLAYER-009). We have no localStorage, so it goes in the cache dir next
/// to the n-sig cache. There is one global entry, like the single localStorage key.
pub mod persisted {
    use std::path::PathBuf;

    fn path() -> Option<PathBuf> {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .map(|b| b.join("innertube-rust").join("bandwidth"))
    }

    /// Last measured byterate, or `None` without usable history. The estimator then seeds
    /// at 0 and the first request advertises 1 B/s, as base.js does with no
    /// `yt-player-bandwidth` entry.
    pub fn load() -> Option<i64> {
        let raw = std::fs::read_to_string(path()?).ok()?;
        raw.trim().parse::<i64>().ok().filter(|v| *v > 0)
    }

    /// Best-effort persist; failures are ignored the way a blocked localStorage write is.
    pub fn save(bps: i64) {
        if bps <= 0 {
            return;
        }
        if let Some(p) = path() {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, bps.to_string());
        }
    }
}

/// Our cold-start estimate, about 500 KB/s (4 Mbit/s). The reference seeds from the stored
/// byterate or `navigator.connection.downlink` instead (PLAYER-009).
pub const CONSERVATIVE_COLD_START_BPS: i64 = 500_000;

/// base.js `wz(17, 20, .5, .4)` for the long byterate accumulator `G`: a 17 s window at 20
/// slots per second, read at the mean of the 10th, 50th and 90th percentiles.
const WINDOW_SECS: f64 = 17.0;
const RESOLUTION: f64 = 20.0;
const CENTER: f64 = 0.5;
const SPREAD: f64 = 0.4;

/// base.js `this.WY = new wz(5, 1, .25)`, the short accumulator: 5 slots at 1 slot per
/// second, read at the 25th percentile, no spread. It gets the same `(elapsed,
/// bytes/elapsed)` samples as `G` (via `wn` rather than `BW`).
///
/// `Kb` advertises `max(G, WY)`. WY holds about 5 samples, so it refills within seconds of
/// a link recovering while `G`'s 17 s window is still full of slow samples. Downshift is
/// therefore slow and upshift fast (SABR-032).
const SHORT_WINDOW_SLOTS: f64 = 5.0;
const SHORT_RESOLUTION: f64 = 1.0;
const SHORT_CENTER: f64 = 0.25;

/// base.js `Lg = function(e){ e = e.G.uK(); return e>0 ? e : 1 }`: the only guard on the
/// emitted byterate is "not zero". There is no floor and no ceiling, so the server sees how
/// slow the link is.
const MIN_BPS: i64 = 1;

/// A time-weighted percentile ring, base.js's `wz`.
///
/// `aK(weight, value)` writes `value` into `round(weight * resolution)` consecutive slots
/// (at least 1, at most the whole ring), so weight buys window share. `percentile(p)` is a
/// nearest-rank read over the slots written so far.
struct Wz {
    values: Vec<f64>,
    /// Write cursor.
    cursor: usize,
    /// Set once the ring has wrapped; every slot is valid from then on.
    wrapped: bool,
    resolution: f64,
    center: f64,
    spread: f64,
}

impl Wz {
    fn new(seconds: f64, resolution: f64, center: f64, spread: f64) -> Self {
        let cap = (seconds * resolution).round().max(1.0) as usize;
        Wz { values: vec![0.0; cap], cursor: 0, wrapped: false, resolution, center, spread }
    }

    /// base.js: `aK(e,r){ e=min(A,max(1,round(e*resolution))); if(e+X>=A) J=true;
    /// while(e--){ values[X]=r; X=(X+1)%A } }`
    fn add(&mut self, weight: f64, value: f64) {
        let cap = self.values.len();
        let slots = ((weight * self.resolution).round() as isize).clamp(1, cap as isize) as usize;
        if slots + self.cursor >= cap {
            self.wrapped = true;
        }
        for _ in 0..slots {
            self.values[self.cursor] = value;
            self.cursor = (self.cursor + 1) % cap;
        }
    }

    /// Nearest-rank percentile over the written slots (the whole ring once wrapped).
    fn percentile(&self, p: f64) -> f64 {
        let n = if self.wrapped { self.values.len() } else { self.cursor };
        if n == 0 {
            return 0.0;
        }
        let mut sorted: Vec<f64> = self.values[..n].to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let rank = (p.clamp(0.0, 1.0) * (n - 1) as f64).round() as usize;
        sorted[rank.min(n - 1)]
    }

    /// base.js: `uK(){ return G ? (percentile(C-G)+percentile(C)+percentile(C+G))/3
    ///                          : percentile(C) }`
    fn value(&self) -> f64 {
        if self.spread != 0.0 {
            (self.percentile(self.center - self.spread)
                + self.percentile(self.center)
                + self.percentile(self.center + self.spread))
                / 3.0
        } else {
            self.percentile(self.center)
        }
    }

    fn is_empty(&self) -> bool {
        !self.wrapped && self.cursor == 0
    }
}

/// Tracks per-round throughput and yields the web player's advertised estimate.
///
/// base.js `Kb` (the value that reaches `ClientAbrState.bandwidth_estimate`) is
/// `max(a, B)` where `a` is the long-window byterate `Lg` and `B` is the short-window
/// `WY.uK()`. Two parts of `Kb` are not ported:
///
/// * the stall penalty `a = 1/(U.uK()*policy.L + 1/a)`. We do not model `U` (the stall-rate
///   accumulator); with no stall samples `U.uK()` is 0 and the expression is the identity,
///   so omitting it is exact for the no-stall case. A stalling link is not penalised as
///   YouTube does.
/// * the `policy.G` blend and the `r=true` branch (1 MiB reference-size scaling and the
///   `L.percentile(.98)` interruption discount). The callers we mirror pass `r=false`.
pub struct BandwidthEstimator {
    /// base.js `G`: long window, robust centre.
    ring: Wz,
    /// base.js `WY`: short window, 25th percentile. It lets the estimate recover.
    short: Wz,
    seed_bps: i64,
}

impl BandwidthEstimator {
    /// `seed_bps` is advertised until the first sample lands. Centre and spread are the
    /// reference client's fixed `wz(17,20,.5,.4)`.
    pub fn new(seed_bps: i64) -> Self {
        let mut e = Self {
            ring: Wz::new(WINDOW_SECS, RESOLUTION, CENTER, SPREAD),
            short: Wz::new(SHORT_WINDOW_SLOTS, SHORT_RESOLUTION, SHORT_CENTER, 0.0),
            seed_bps: seed_bps.max(0),
        };
        // base.js seeds the ring itself: `this.G.aK(this.policy.J, r)` with `policy.J ===
        // false`, so `round(false * resolution) == 0` clamps to one slot out of 340. The seed
        // is a single sample that real measurements outvote.
        //
        // `r` is the persisted byterate, else `downlink * 64 * 1024` (PLAYER-009). Our client
        // has no `downlink`, so with no history the seed is 0 and `Lg`'s `e>0?e:1` advertises
        // 1 B/s; the first rung follows that estimate (SABR-033).
        if e.seed_bps > 0 {
            e.ring.add(0.0, e.seed_bps as f64);
            e.short.add(0.0, e.seed_bps as f64);
        }
        e
    }

    /// Byterate to persist as this session's `yt-player-bandwidth` equivalent, so the next
    /// session cold-starts from a real measurement instead of nothing. `None` until a real
    /// sample has landed (a seed-only ring has nothing worth persisting).
    pub fn persistable(&self) -> Option<i64> {
        (!self.ring.is_empty()).then(|| self.estimate())
    }

    /// The current estimate to advertise (bytes/sec).
    pub fn estimate(&self) -> i64 {
        // base.js `Kb`: X = max(Lg(e), WY.uK()). The short window lets the estimate climb
        // back after a throughput drop before the long window has rolled over. An empty ring
        // yields 0, which `Lg`'s `e>0 ? e : 1` turns into 1 B/s.
        let long = if self.ring.is_empty() { 0.0 } else { self.ring.value() };
        let short = if self.short.is_empty() { 0.0 } else { self.short.value() };
        (long.max(short) as i64).max(MIN_BPS)
    }

    /// Feed one round's transfer, as base.js `BW`: weight is the elapsed time and the value
    /// is `bytes/elapsed`. There is no minimum-byte threshold; the time weighting already
    /// reduces a small round to a single slot out of 340.
    pub fn observe(&mut self, bytes: usize, secs: f64) -> i64 {
        // base.js feeds `BW`/`wn` from `z2(e)`, a completed-download handler with that
        // download's own elapsed time and byte count. A poll that moved no data is never
        // sampled. Our loop polls (a gated round is a POST returning 0 B in ~20 ms), so those
        // rounds are excluded. A real 10 KB transfer is a sample. A zero-byte round would take
        // a fifth of the 5-slot short window and let its p25 read off unrelated bursts.
        if secs > 0.0 && bytes > 0 {
            let byterate = bytes as f64 / secs;
            self.ring.add(secs, byterate);
            self.short.add(secs, byterate);
        }
        self.estimate()
    }
}

#[cfg(test)]
mod tests {
    //! Expected behaviour comes from the web player's estimator, base.js `9fc68080`
    //! (PLAYER-008, PLAYER-009).
    use super::*;

    /// `secs` rounds of 1 s, each transferring `bps` bytes. One second buys `RESOLUTION`
    /// slots, so `secs >= WINDOW_SECS` replaces the window.
    fn feed(e: &mut BandwidthEstimator, bps: f64, secs: usize) {
        for _ in 0..secs {
            e.observe(bps as usize, 1.0);
        }
    }

    #[test]
    fn the_seed_is_advertised_until_the_first_sample_and_one_sample_replaces_it() {
        let mut e = BandwidthEstimator::new(250_000);
        assert_eq!(e.estimate(), 250_000);
        e.observe(1_000_000, 1.0);
        assert_eq!(e.estimate(), 1_000_000);
    }

    #[test]
    fn no_history_advertises_one_byte_per_second() {
        let e = BandwidthEstimator::new(0);
        assert_eq!(e.estimate(), MIN_BPS);
        assert_eq!(e.persistable(), None);
    }

    /// The seed occupies one of 340 slots, so a few real rounds outvote it.
    #[test]
    fn real_samples_outvote_the_seed() {
        let mut e = BandwidthEstimator::new(5_000_000);
        feed(&mut e, 200_000.0, 3);
        assert!(e.estimate() < 1_000_000, "got {}", e.estimate());
        assert_eq!(e.persistable(), Some(e.estimate()));
    }

    /// A sample weighs its elapsed time (`G.aK(elapsed, bytes/elapsed)`): one 10 s starved
    /// transfer outweighs four 1 s fast ones. p90 still sees the fast rounds, so the
    /// estimate stays above the starved rate.
    #[test]
    fn a_long_starved_transfer_outweighs_brief_fast_ones() {
        let mut e = BandwidthEstimator::new(250_000);
        for _ in 0..4 {
            e.observe(1_000_000, 1.0);
        }
        e.observe(500_000, 10.0);
        let est = e.estimate();
        assert!(est < 500_000, "got {est}");
        assert!(est > 50_000, "got {est}");
    }

    /// base.js samples completed downloads, so a round that moved no bytes is not a sample.
    #[test]
    fn zero_byte_rounds_are_not_samples_but_small_downloads_are() {
        let mut e = BandwidthEstimator::new(250_000);
        feed(&mut e, 10_000_000.0, WINDOW_SECS as usize);
        let healthy = e.estimate();
        for _ in 0..20 {
            e.observe(0, 0.02);
        }
        assert_eq!(e.estimate(), healthy);

        let mut s = BandwidthEstimator::new(250_000);
        feed(&mut s, 10_000_000.0, WINDOW_SECS as usize);
        feed(&mut s, 10_000.0, WINDOW_SECS as usize);
        assert!(s.estimate() < 100_000, "got {}", s.estimate());
    }

    /// The estimate is the mean of p10, p50 and p90. A quarter of rounds latency-bound pulls
    /// it below the fast rate.
    #[test]
    fn the_estimate_reads_the_centre_of_the_window() {
        let mut e = BandwidthEstimator::new(3_000_000);
        for i in 0..WINDOW_SECS as usize {
            let bps = if i % 4 == 0 { 500_000.0 } else { 10_000_000.0 };
            e.observe(bps as usize, 1.0);
        }
        let est = e.estimate();
        assert!(est < 9_000_000, "got {est}");
        assert!(est > 2_000_000, "got {est}");
    }

    /// `max(long window, short window)`: a 3 s recovery fills the 5-slot short window and
    /// lifts the estimate while the 17 s long window still holds the starved samples (SABR-032).
    #[test]
    fn the_estimate_recovers_within_rounds_of_a_link_returning() {
        let mut e = BandwidthEstimator::new(250_000);
        feed(&mut e, 5_000_000.0, WINDOW_SECS as usize);
        assert!(e.estimate() >= 4_000_000);

        feed(&mut e, 60_000.0, WINDOW_SECS as usize);
        let starved = e.estimate();
        assert!(starved < 200_000, "got {starved}");

        feed(&mut e, 5_000_000.0, 3);
        assert!(e.estimate() > 1_000_000, "got {} (starved {starved})", e.estimate());
    }

    #[test]
    fn the_estimate_follows_a_link_down_to_its_rate() {
        let mut e = BandwidthEstimator::new(3_000_000);
        feed(&mut e, 10_000_000.0, WINDOW_SECS as usize);
        assert!(e.estimate() >= 9_000_000);
        feed(&mut e, 400_000.0, WINDOW_SECS as usize);
        assert!(e.estimate() <= 500_000, "got {}", e.estimate());
    }

    /// base.js guards only against zero (`return e>0 ? e : 1`): 0.1 B/s truncates to 0 and
    /// advertises 1.
    #[test]
    fn a_starved_link_is_advertised_as_starved_and_never_as_zero() {
        let mut e = BandwidthEstimator::new(3_000_000);
        feed(&mut e, 20_000.0, WINDOW_SECS as usize);
        assert!((15_000..=25_000).contains(&e.estimate()), "got {}", e.estimate());

        let mut z = BandwidthEstimator::new(3_000_000);
        for _ in 0..WINDOW_SECS as usize {
            z.observe(1, 10.0);
        }
        assert_eq!(z.estimate(), MIN_BPS);
    }
}
