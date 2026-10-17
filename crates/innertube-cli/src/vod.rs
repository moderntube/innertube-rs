//! `it vod <video_id> <out_dir>`: drives a `SegmentVod` without a player. It opens, reads
//! both tracks for a span, seeks, reads again, seeks back into stored time, and writes each
//! span of each track to a WebM file in `out_dir` for ffprobe.
use std::error::Error;
use std::io::Write;
use std::time::Instant;

use innertube::{ClientConfig, NSigSolver, SegmentTrack, SegmentVod, SegmentVodOptions, Session};

fn read_span(label: &str, vod: &SegmentVod, t: &mut SegmentTrack, until_ms: i64, out: &mut std::fs::File) -> usize {
    let t0 = Instant::now();
    let mut n = 0;
    while t.cursor_ms() < until_ms {
        let Some(p) = t.next_piece() else { break };
        if n == 0 {
            eprintln!("{label}: first piece itag {} at {} ms after {:.0} ms", p.itag, p.start_ms, t0.elapsed().as_secs_f64() * 1000.0);
        }
        vod.set_playhead_ms(p.start_ms);
        out.write_all(&p.bytes).unwrap();
        n += 1;
    }
    eprintln!("{label}: {n} pieces to {} ms in {:.0} ms", t.cursor_ms(), t0.elapsed().as_secs_f64() * 1000.0);
    n
}

/// `seek_ms` and `back_ms` are media time in ms.
pub fn run(
    rt: &tokio::runtime::Runtime,
    video_id: &str,
    dir: &std::path::Path,
    seek_ms: i64,
    back_ms: i64,
) -> Result<(), Box<dyn Error>> {
    std::fs::create_dir_all(dir)?;
    let session = Session::new(ClientConfig::web())?;
    let t0 = Instant::now();
    let pot = crate::pot_from_env();
    let vod = rt.block_on(SegmentVod::open(&session, &NSigSolver::new(), pot, video_id, &SegmentVodOptions::default()))?;
    eprintln!("open {:.0} ms, duration {} ms, heights {:?}", t0.elapsed().as_secs_f64() * 1000.0, vod.duration_ms(), vod.heights());
    let tracks = vod.captions().tracks();
    eprintln!("captions: {:?}", tracks.iter().map(|t| (&t.language_code, t.auto)).collect::<Vec<_>>());
    if !tracks.is_empty() {
        match rt.block_on(vod.captions().fetch(0)) {
            Ok(cues) => eprintln!("captions[0]: {} cues, first {:?}", cues.len(), cues.first()),
            Err(e) => eprintln!("captions[0]: {e}"),
        }
    }
    let _guard = rt.enter();
    for (name, video) in [("video", true), ("audio", false)] {
        let mut out = std::fs::File::create(dir.join(format!("{name}-open.webm")))?;
        let mut t = vod.track(video, 0);
        read_span(&format!("{name} open"), &vod, &mut t, 60_000, &mut out);
        let mut out = std::fs::File::create(dir.join(format!("{name}-seek.webm")))?;
        vod.set_playhead_ms(seek_ms);
        t.reposition(seek_ms);
        read_span(&format!("{name} seek {seek_ms}"), &vod, &mut t, seek_ms + 20_000, &mut out);
        let mut out = std::fs::File::create(dir.join(format!("{name}-back.webm")))?;
        vod.set_playhead_ms(back_ms);
        t.reposition(back_ms);
        read_span(&format!("{name} back {back_ms}"), &vod, &mut t, back_ms + 20_000, &mut out);
    }
    vod.stop();
    Ok(())
}
