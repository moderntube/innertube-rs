//! The sync/async bridge: an async engine task fills [`TrackBuffer`]s; a
//! consumer pulls bytes through a **blocking** [`MediaReader`] on its own thread
//! (e.g. a media player's stream callback, which must never `.await`).
//!
//! Backpressure: the buffer stays bounded *only while the reader keeps up*. The
//! SABR server sends read-ahead-worth of media ahead of `player_time`, but a paused
//! consumer stops draining, so the orchestrator (`run_loop`) gates new requests on
//! `buffered_bytes()` past a high-water mark rather than growing without limit.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

struct Inner {
    data: VecDeque<u8>,
    closed: bool,
}

/// A growable byte channel for one media track. Cheap to clone (shared handle).
#[derive(Clone)]
pub struct TrackBuffer {
    inner: Arc<(Mutex<Inner>, Condvar)>,
}

impl Default for TrackBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl TrackBuffer {
    pub fn new() -> Self {
        Self {
            inner: Arc::new((Mutex::new(Inner { data: VecDeque::new(), closed: false }), Condvar::new())),
        }
    }

    /// Append produced bytes and wake any blocked reader.
    pub fn push(&self, bytes: &[u8]) {
        let (lock, cv) = &*self.inner;
        lock.lock().unwrap().data.extend(bytes);
        cv.notify_all();
    }

    /// Signal end-of-stream; readers drain remaining bytes then see EOF.
    pub fn close(&self) {
        let (lock, cv) = &*self.inner;
        lock.lock().unwrap().closed = true;
        cv.notify_all();
    }

    /// Bytes currently buffered (diagnostics).
    pub fn buffered_bytes(&self) -> usize {
        self.inner.0.lock().unwrap().data.len()
    }

    /// Whether the buffer has been closed (diagnostics): a closed buffer makes
    /// reads return EOF once drained.
    pub fn is_closed(&self) -> bool {
        self.inner.0.lock().unwrap().closed
    }

    /// Drop all queued bytes (a seek discards pre-seek data). Does not wake readers:
    /// a reader blocked on an empty buffer simply keeps waiting for fresh bytes.
    pub fn clear(&self) {
        self.inner.0.lock().unwrap().data.clear();
    }

    /// A blocking reader over this buffer.
    pub fn reader(&self) -> MediaReader {
        MediaReader { buf: self.clone(), tracker: None }
    }

    /// A reader counted by `tracker`: when the last reader it tracks is dropped,
    /// the tracker tears the session down (see [`ReaderTracker`]). Use for the
    /// readers a streaming session hands to a consumer, so dropping them all
    /// signals the producer even if the consumer no longer holds the session.
    pub fn reader_tracked(&self, tracker: Arc<ReaderTracker>) -> MediaReader {
        tracker.live.fetch_add(1, Ordering::AcqRel);
        MediaReader { buf: self.clone(), tracker: Some(tracker) }
    }
}

/// Cross-track reader bookkeeping for a streaming session. Counts the live
/// [`MediaReader`]s handed out via [`TrackBuffer::reader_tracked`]; when the last
/// one is dropped it sets the producer's `stop` flag and closes the tracked
/// buffers, so the engine task winds down and any other blocked reader sees EOF.
///
/// A consumer that drops its readers, for example a media player tearing down its stream
/// callbacks, signals the producer without holding (and dropping) the session handle.
/// Create all readers up front: dropping the last one is the teardown signal, so a reader
/// created after the count hits zero would read from already-closed buffers.
pub struct ReaderTracker {
    live: AtomicUsize,
    stop: Arc<AtomicBool>,
    buffers: Vec<TrackBuffer>,
}

impl ReaderTracker {
    /// `stop` is set and every buffer in `buffers` is closed once the last tracked
    /// reader drops.
    pub fn new(stop: Arc<AtomicBool>, buffers: Vec<TrackBuffer>) -> Arc<Self> {
        Arc::new(Self { live: AtomicUsize::new(0), stop, buffers })
    }
}

/// Blocking [`Read`] over a [`TrackBuffer`]. `read` returns as soon as any bytes
/// are available, blocks the calling thread while empty, and returns `Ok(0)`
/// (EOF) once the buffer is closed and drained. Safe to call off the async
/// runtime (a player's read callback).
///
/// Own the `SabrStream` for as long as any `MediaReader` reads from it: dropping or stopping
/// the stream closes the track buffers, and in-flight `read`s return `Ok(0)` (EOF). A reader
/// from [`TrackBuffer::reader_tracked`] (what `SabrStream` hands out) stops the session when
/// the last such reader drops (see [`ReaderTracker`]), even after the consumer has dropped
/// the stream. A plain [`TrackBuffer::reader`] unblocks only via `close()`/`stop()`.
pub struct MediaReader {
    buf: TrackBuffer,
    tracker: Option<Arc<ReaderTracker>>,
}

impl Drop for MediaReader {
    fn drop(&mut self) {
        if let Some(t) = self.tracker.take() {
            // Last tracked reader out: signal the producer and close the buffers so
            // the engine task winds down and any other blocked reader sees EOF.
            if t.live.fetch_sub(1, Ordering::AcqRel) == 1 {
                t.stop.store(true, Ordering::Relaxed);
                for b in &t.buffers {
                    b.close();
                }
            }
        }
    }
}

impl Read for MediaReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let (lock, cv) = &*self.buf.inner;
        let mut guard = lock.lock().unwrap();
        loop {
            if !guard.data.is_empty() {
                let n = out.len().min(guard.data.len());
                // Copy in at most two memcpys, one per contiguous half of the VecDeque,
                // instead of popping bytes one at a time under the lock.
                let (front, back) = guard.data.as_slices();
                let take_front = front.len().min(n);
                out[..take_front].copy_from_slice(&front[..take_front]);
                if take_front < n {
                    out[take_front..n].copy_from_slice(&back[..n - take_front]);
                }
                guard.data.drain(..n);
                return Ok(n);
            }
            if guard.closed {
                return Ok(0);
            }
            guard = cv.wait(guard).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// The bulk drain must reproduce the input byte-for-byte across arbitrary push and read
    /// interleavings, including when the data wraps the ring buffer and a read spans both
    /// halves.
    #[test]
    fn any_interleaving_of_pushes_and_reads_preserves_every_byte_in_order() {
        let buf = TrackBuffer::new();
        let mut r = buf.reader();
        let (mut expected, mut got) = (Vec::new(), Vec::new());
        let mut val: u8 = 0;
        for round in 0..200usize {
            let push_n = (round * 7) % 50 + 1;
            let chunk: Vec<u8> = (0..push_n)
                .map(|_| {
                    val = val.wrapping_add(1);
                    val
                })
                .collect();
            buf.push(&chunk);
            expected.extend_from_slice(&chunk);

            // Read an odd slice (never more than buffered, so the reader never blocks).
            let avail = buf.buffered_bytes();
            let read_n = ((round * 5) % (avail + 1)).min(avail);
            if read_n > 0 {
                let mut tmp = vec![0u8; read_n];
                let n = r.read(&mut tmp).unwrap();
                got.extend_from_slice(&tmp[..n]);
            }
        }
        buf.close();
        let mut tmp = [0u8; 256];
        loop {
            let n = r.read(&mut tmp).unwrap();
            if n == 0 {
                break;
            }
            got.extend_from_slice(&tmp[..n]);
        }
        assert_eq!(got, expected);
    }

    /// A closed buffer yields its remaining bytes, then EOF.
    #[test]
    fn a_closed_buffer_drains_then_reports_eof() {
        let buf = TrackBuffer::new();
        let mut r = buf.reader();
        buf.push(&[1, 2, 3]);
        buf.close();
        let mut tmp = [0u8; 10];
        assert_eq!(r.read(&mut tmp).unwrap(), 3);
        assert_eq!(&tmp[..3], &[1, 2, 3]);
        assert_eq!(r.read(&mut tmp).unwrap(), 0, "EOF after the drain");
    }
}
