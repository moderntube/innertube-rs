//! Bounded audio-only DVR window fetch that writes a whisper-ready file. Thin CLI over
//! [`innertube::fetch_audio_window`].
//!
//! Output is the concatenated self-initializing fMP4 audio segments, a valid `.m4a` that
//! Groq's whisper endpoint accepts directly. With `-o *.wav` the file is transcoded via
//! `ffmpeg` to 16 kHz mono PCM.
//!
//! PO tokens: `INNERTUBE_POT=native|bgutil` (default native).

use std::time::Instant;

use innertube::{fetch_audio_window, ClientConfig, NSigSolver, Session};

/// `it audio-window <video_id>`: writes the audio of `[from_ms, to_ms]` (media time, ms)
/// to `out`. `to_ms = None` is the live head at fetch start.
pub async fn run(video_id: &str, from_ms: i64, to_ms: Option<i64>, out: &str) -> Result<(), Box<dyn std::error::Error>> {
    let session = Session::new(ClientConfig::web())?;
    let pot = crate::pot_from_env();
    let solver = NSigSolver::new();
    let start = Instant::now();

    let mut round = 0u32;
    let mut progress = |end: Option<i64>, target: Option<i64>, total: usize| {
        round += 1;
        eprintln!(
            "[aw] round {round:>3}: buf_end={end:?} target={target:?} total={total}B ({:.1}s wall)",
            start.elapsed().as_secs_f64()
        );
    };
    let win = fetch_audio_window(
        &session,
        &solver,
        &pot,
        video_id,
        from_ms,
        to_ms,
        Some(&mut progress),
    )
    .await?;

    if out.ends_with(".wav") {
        // 16 kHz mono PCM: the format whisper resamples to, and the smallest upload.
        let tmp = format!("{out}.tmp.m4a");
        std::fs::write(&tmp, &win.bytes)?;
        let st = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-i", &tmp, "-ar", "16000", "-ac", "1", out])
            .status()?;
        std::fs::remove_file(&tmp).ok();
        if !st.success() {
            return Err("ffmpeg transcode failed".into());
        }
    } else {
        std::fs::write(out, &win.bytes)?;
    }
    eprintln!(
        "[aw] wrote {out}: {:.1}s of audio over [{}..{}], {} bytes raw, {:.1}s wall ({:.1}x realtime)",
        win.seconds,
        win.from_ms,
        win.to_ms,
        win.bytes.len(),
        start.elapsed().as_secs_f64(),
        win.seconds / start.elapsed().as_secs_f64().max(0.001),
    );
    Ok(())
}
