//! UMP parse never copies a media payload. A counting global allocator wraps the
//! system allocator to measure the bytes allocated per parse.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use innertube_sabr::{parse, write_part, Bytes};

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// `parse` slices UMP part payloads out of the response as zero-copy [`Bytes`] views.
/// A 2 MiB MEDIA part is not copied on the way in; the bytes allocated per call stay
/// independent of the payload size.
#[test]
fn parse_does_not_copy_media_payloads() {
    // A steady-state round: MEDIA_HEADER (20), a 2 MiB MEDIA (21), STREAM_PROTECTION_STATUS (58).
    let media = vec![7u8; 2 * 1024 * 1024];
    let mut buf = Vec::new();
    write_part(&mut buf, 20, &[1u8; 32]);
    write_part(&mut buf, 21, &media);
    write_part(&mut buf, 58, &[2u8; 8]);
    // The hot path hands `parse` the owned HTTP-response `Bytes`.
    let ump = Bytes::from(buf);

    const N: usize = 50;
    let a0 = ALLOCS.load(Ordering::Relaxed);
    let b0 = BYTES.load(Ordering::Relaxed);
    let mut keep = 0usize;
    for _ in 0..N {
        let parts = parse(ump.clone());
        keep = keep.wrapping_add(parts.iter().map(|p| p.payload.len()).sum::<usize>());
    }
    let allocs = (ALLOCS.load(Ordering::Relaxed) - a0) as f64 / N as f64;
    let bytes_per_call = (BYTES.load(Ordering::Relaxed) - b0) / N;
    eprintln!("parse: {allocs:.1} allocs/call, {bytes_per_call} bytes copied/call (kept {keep} B)");

    // The allocation per call is the `Vec<UmpPart>`, a few hundred bytes.
    assert!(bytes_per_call < 4096, "parse allocated {bytes_per_call} B per call");
}
