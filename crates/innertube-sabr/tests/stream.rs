use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use innertube_sabr::{ReaderTracker, TrackBuffer};

#[test]
fn last_tracked_reader_drop_tears_down_session() {
    let stop = Arc::new(AtomicBool::new(false));
    let audio = TrackBuffer::new();
    let video = TrackBuffer::new();
    let tracker = ReaderTracker::new(stop.clone(), vec![audio.clone(), video.clone()]);

    let a = audio.reader_tracked(tracker.clone());
    let v = video.reader_tracked(tracker.clone());

    // One of two readers is gone and the session keeps running.
    drop(a);
    assert!(!stop.load(Ordering::Relaxed), "stop stays clear while a reader remains");
    assert!(!audio.is_closed() && !video.is_closed(), "buffers stay open while a reader remains");

    // The last reader is gone: stop is set and every tracked buffer is closed.
    drop(v);
    assert!(stop.load(Ordering::Relaxed), "last reader drop sets the stop flag");
    assert!(audio.is_closed() && video.is_closed(), "last reader drop closes the tracked buffers");
}

