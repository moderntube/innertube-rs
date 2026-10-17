//! [`LiveSegments`]: a raw live stream's complete segments, keyed by sequence number, for a
//! consumer that orders media by timestamp. Sequence numbers are shared across a stream's
//! formats, so a rung change is the same sequence arriving under another itag. The newer
//! delivery takes over from the cursor on, as an overlapping `SourceBuffer` append does.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use innertube_sabr::MediaSegment;

use crate::segment_vod::{SegmentPiece, SegmentSource};

/// Segments kept, by arrival; the oldest arrivals are dropped, so media fetched behind
/// the head after a DVR seek stays while the edge it left ages out.
const KEEP_ARRIVALS: u64 = 800;
/// How long a cursor waits on a missing sequence while later ones are stored.
const GAP_WAIT: Duration = Duration::from_secs(2);
/// The furthest ahead a cursor skips over a missing sequence.
const GAP_SPAN: i64 = 3;
/// How long a pinned rung that has not arrived may hold the video cursor.
const PIN_WAIT: Duration = Duration::from_secs(3);

#[derive(Clone)]
struct Seg {
    start_ms: i64,
    arrival: u64,
    bytes: Arc<Vec<u8>>,
}

#[derive(Default)]
struct Inner {
    video: BTreeMap<i64, BTreeMap<i32, Seg>>,
    audio: BTreeMap<i64, Seg>,
    arrivals: u64,
    closed: bool,
}

/// The shared store a raw live stream fills and its tracks read.
#[derive(Default)]
pub struct LiveSegments {
    inner: Mutex<Inner>,
    arrived: Condvar,
    /// Pinned video itag, 0 for server-driven quality.
    pin: AtomicI32,
    video_itags: Mutex<Vec<i32>>,
    /// Segments a joining track starts behind the newest: `liveChunkReadahead`, which
    /// base.js holds between the playhead and the head (`FX() = head - readaheadTargetMs`).
    readahead_chunks: AtomicI64,
}

impl LiveSegments {
    pub(crate) fn new(video_itags: Vec<i32>) -> Arc<Self> {
        Arc::new(Self { video_itags: Mutex::new(video_itags), ..Default::default() })
    }

    pub(crate) fn insert(&self, segs: Vec<MediaSegment>) {
        if segs.is_empty() {
            return;
        }
        let video_itags = self.video_itags.lock().unwrap().clone();
        let mut inner = self.inner.lock().unwrap();
        for s in segs {
            let (Some(seq), Some(start_ms)) = (s.seq, s.start_ms) else { continue };
            if s.bytes.is_empty() {
                continue;
            }
            inner.arrivals += 1;
            if let Some(dir) = crate::client::dump::raw_dir("live-segments") {
                let _ = std::fs::write(dir.join(format!("{seq}-{}.seg", s.itag)), &s.bytes);
            }
            let seg = Seg { start_ms, arrival: inner.arrivals, bytes: Arc::new(s.bytes) };
            if video_itags.contains(&s.itag) {
                inner.video.entry(seq).or_default().insert(s.itag, seg);
            } else {
                inner.audio.insert(seq, seg);
            }
        }
        let floor = inner.arrivals.saturating_sub(KEEP_ARRIVALS);
        inner.video.retain(|_, by_itag| {
            by_itag.retain(|_, s| s.arrival > floor);
            !by_itag.is_empty()
        });
        inner.audio.retain(|_, s| s.arrival > floor);
        drop(inner);
        self.arrived.notify_all();
    }

    pub(crate) fn set_readahead_chunks(&self, chunks: i64) {
        self.readahead_chunks.store(chunks.max(0), Ordering::Relaxed);
    }

    pub(crate) fn set_pin(&self, itag: i32) {
        self.pin.store(itag, Ordering::Relaxed);
        self.arrived.notify_all();
    }

    /// The sequence number of the segment holding stream time `ms`, and the newest stored
    /// sequence: from a stored segment when one holds `ms`, otherwise stepped from the
    /// nearest stored one at the stored segments' spacing.
    pub fn sequence_at(&self, ms: i64) -> Option<(i64, i64)> {
        let inner = self.inner.lock().unwrap();
        let starts: Vec<(i64, i64)> = inner.audio.iter().map(|(&q, s)| (q, s.start_ms)).collect();
        let newest = *starts.last().map(|(q, _)| q)?;
        let spacing = {
            let mut d: Vec<i64> =
                starts.windows(2).filter(|p| p[1].0 == p[0].0 + 1).map(|p| p[1].1 - p[0].1).collect();
            d.sort_unstable();
            *d.get(d.len() / 2).filter(|&&v| v > 0)?
        };
        let (rq, rs) = *starts.iter().min_by_key(|(_, st)| (st - ms).abs())?;
        let seq = rq + (ms - rs).div_euclid(spacing);
        Some((seq, newest))
    }

    pub(crate) fn close(&self) {
        self.inner.lock().unwrap().closed = true;
        self.arrived.notify_all();
    }

    /// A cursor over one track, starting at the first sequence it finds.
    pub fn track(self: &Arc<Self>, video: bool) -> LiveSegmentTrack {
        LiveSegmentTrack {
            store: self.clone(),
            video,
            cursor: None,
            want_ms: None,
            last_itag: None,
            stop: Arc::new(AtomicBool::new(false)),
            waiting_since: None,
        }
    }
}

/// One live track's cursor, by sequence number.
pub struct LiveSegmentTrack {
    store: Arc<LiveSegments>,
    video: bool,
    cursor: Option<i64>,
    /// A reposition target no stored segment holds yet.
    want_ms: Option<i64>,
    last_itag: Option<i32>,
    stop: Arc<AtomicBool>,
    waiting_since: Option<Instant>,
}

impl LiveSegmentTrack {
    fn start_of(&self, inner: &Inner, seq: i64) -> Option<i64> {
        if self.video {
            inner.video.get(&seq).and_then(|m| m.values().next()).map(|s| s.start_ms)
        } else {
            inner.audio.get(&seq).map(|s| s.start_ms)
        }
    }

    /// The stored sequence whose segment holds `ms`: its successor starts after `ms`, or,
    /// with no successor stored, `ms` is within one segment duration of its start.
    fn locate(&self, inner: &Inner, seqs: &[i64], ms: i64) -> Option<i64> {
        let dur = seqs
            .windows(2)
            .find(|p| p[1] == p[0] + 1)
            .and_then(|p| Some(self.start_of(inner, p[1])? - self.start_of(inner, p[0])?));
        let seq = *seqs.iter().rev().find(|&&q| self.start_of(inner, q).is_some_and(|s| s <= ms))?;
        let start = self.start_of(inner, seq)?;
        let holds = match self.start_of(inner, seq + 1) {
            Some(next) => next > ms,
            None => dur.is_some_and(|d| ms < start + d),
        };
        holds.then_some(seq)
    }

    fn pick(&mut self, inner: &Inner, seq: i64) -> Option<(i32, Seg)> {
        if !self.video {
            return inner.audio.get(&seq).map(|s| (0, s.clone()));
        }
        let by_itag = inner.video.get(&seq)?;
        let pin = self.store.pin.load(Ordering::Relaxed);
        if pin != 0 {
            if let Some(s) = by_itag.get(&pin) {
                return Some((pin, s.clone()));
            }
            if self.waiting_since.get_or_insert_with(Instant::now).elapsed() < PIN_WAIT {
                return None;
            }
        }
        by_itag.iter().max_by_key(|(_, s)| s.arrival).map(|(&i, s)| (i, s.clone()))
    }
}

impl SegmentSource for LiveSegmentTrack {
    fn next_piece(&mut self) -> Option<SegmentPiece> {
        let store = self.store.clone();
        let mut inner = store.inner.lock().unwrap();
        loop {
            if self.stop.swap(false, Ordering::Relaxed) {
                return None;
            }
            let seqs: Vec<i64> = if self.video {
                inner.video.keys().copied().collect()
            } else {
                inner.audio.keys().copied().collect()
            };
            if let Some(ms) = self.want_ms {
                self.cursor = self.locate(&inner, &seqs, ms);
                if self.cursor.is_some() {
                    self.want_ms = None;
                }
            } else if self.cursor.is_none() {
                let chunks = self.store.readahead_chunks.load(Ordering::Relaxed);
                self.cursor = match (seqs.first(), seqs.last()) {
                    (Some(&first), _) if chunks == 0 => Some(first),
                    (Some(&first), Some(&last)) if last - first + 1 >= chunks => Some(last - chunks + 1),
                    _ => None,
                };
            }
            if let Some(seq) = self.cursor {
                if let Some((itag, seg)) = self.pick(&inner, seq) {
                    self.waiting_since = None;
                    self.cursor = Some(seq + 1);
                    self.last_itag = Some(itag);
                    return Some(SegmentPiece { itag, start_ms: seg.start_ms, bytes: seg.bytes.to_vec() });
                }
                // A sequence the server skipped: move on once later ones have waited.
                let later = seqs.iter().find(|&&s| s > seq && s <= seq + GAP_SPAN).copied();
                let absent = if self.video { !inner.video.contains_key(&seq) } else { !inner.audio.contains_key(&seq) };
                if let (Some(next), true) = (later, absent) {
                    if self.waiting_since.get_or_insert_with(Instant::now).elapsed() >= GAP_WAIT {
                        self.waiting_since = None;
                        self.cursor = Some(next);
                        continue;
                    }
                }
                if seqs.first().is_some_and(|&first| seq < first) {
                    self.cursor = seqs.first().copied();
                    continue;
                }
            }
            if inner.closed {
                return None;
            }
            let (guard, _) = store.arrived.wait_timeout(inner, Duration::from_millis(100)).unwrap();
            inner = guard;
        }
    }

    fn reposition(&mut self, ms: i64) {
        self.cursor = None;
        self.want_ms = Some(ms);
        self.last_itag = None;
        self.waiting_since = None;
    }

    fn interrupt_handle(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }

    fn cursor_ms(&self) -> i64 {
        let inner = self.store.inner.lock().unwrap();
        let Some(seq) = self.cursor else { return self.want_ms.unwrap_or(0) };
        if self.video {
            inner.video.get(&seq).and_then(|m| m.values().next()).map_or(0, |s| s.start_ms)
        } else {
            inner.audio.get(&seq).map_or(0, |s| s.start_ms)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(itag: i32, seq: i64) -> MediaSegment {
        MediaSegment { itag, seq: Some(seq), start_ms: Some(seq * 5_000), duration_ms: None, init: false, bytes: vec![itag as u8, seq as u8] }
    }

    #[test]
    fn a_rung_change_continues_from_the_cursor_without_a_time_gap() {
        let store = LiveSegments::new(vec![243, 302]);
        store.insert((10..14).map(|q| seg(243, q)).collect());
        let mut t = store.track(true);
        assert_eq!(t.next_piece().map(|p| p.itag), Some(243));
        store.insert((11..14).map(|q| seg(302, q)).collect());
        let got: Vec<(i32, i64)> = (0..3).map(|_| t.next_piece().map(|p| (p.itag, p.start_ms)).unwrap()).collect();
        assert_eq!(got, vec![(302, 55_000), (302, 60_000), (302, 65_000)]);
    }

    #[test]
    fn a_seek_lands_on_the_segment_holding_the_target_time() {
        let store = LiveSegments::new(vec![243]);
        store.insert((1..10).map(|q| seg(140, q)).collect());
        let mut t = store.track(false);
        t.reposition(22_000);
        assert_eq!(t.next_piece().map(|p| p.start_ms), Some(20_000));
    }

    #[test]
    fn a_dvr_seek_keeps_the_segments_it_fetched_behind_the_head() {
        let store = LiveSegments::new(vec![243]);
        store.insert((1000..1010).map(|q| seg(140, q)).collect());
        store.insert((10..20).map(|q| seg(140, q)).collect());
        let mut t = store.track(false);
        t.reposition(50_000);
        assert_eq!(t.next_piece().map(|p| p.start_ms), Some(50_000));
    }
}
