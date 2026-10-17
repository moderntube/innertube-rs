//! `it`: the InnerTube dev CLI. One binary with clap subcommands.
//!
//! `smoke`, `play` and `stream` need a bgutil server on :4416. The other subcommands pick
//! the PO-token provider with `INNERTUBE_POT` (default native). `stream --play` and
//! `audio-window -o *.wav` need ffmpeg.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use innertube_cli::cmds::{self, StreamArgs};
use innertube_cli::{api, audio_window, auth, find_live, shorts, vod};

#[derive(Parser)]
#[command(name = "it", about = "InnerTube SABR dev CLI", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// One-shot live SABR fetch that proves media flows.
    Smoke {
        video_id: String,
    },
    /// Sustained SABR loop; prints the realtime ratio.
    Play {
        video_id: String,
        /// Run duration in seconds.
        #[arg(long, default_value_t = 45)]
        secs: u64,
    },
    /// Drive `SabrStream`; `--play` pipes muxed a/v to stdout for a player.
    Stream(StreamCli),
    /// Open a VOD through `SegmentVod`, seek twice, and write each span to `out_dir`.
    Vod {
        video_id: String,
        out_dir: PathBuf,
        /// First seek target, media time in ms.
        #[arg(long, default_value_t = 300_000)]
        seek_ms: i64,
        /// Second seek target back into stored time, media time in ms.
        #[arg(long, default_value_t = 20_000)]
        back_ms: i64,
    },
    /// Swipe through the Shorts feed: open, preload the next Short, swipe.
    Shorts {
        /// Number of swipes.
        #[arg(long, default_value_t = 3)]
        count: usize,
    },
    /// Fetch a bounded audio-only window of a live DVR or VOD into a file.
    AudioWindow {
        video_id: String,
        /// Window start, media time in ms, or `start`.
        #[arg(long, default_value = "start")]
        from: String,
        /// Window end, media time in ms, or `edge` for the live head.
        #[arg(long, default_value = "edge")]
        to: String,
        /// Output file; `*.wav` is transcoded to 16 kHz mono PCM.
        #[arg(short, long, default_value = "out.m4a")]
        out: String,
    },
    /// Sign in with browser cookies and print the account name.
    Auth {
        /// A Firefox-format `cookies.sqlite`; a LibreWolf profile is found when omitted.
        cookies_db: Option<String>,
    },
    /// Search with the live filter and print `id<TAB>channel<TAB>title` per result.
    FindLive {
        query: String,
        #[arg(long, default_value_t = 3)]
        count: usize,
        /// A replacement `sp` search filter, or `none`.
        #[arg(long)]
        filter: Option<String>,
    },
    /// Exercise the anonymous read and navigation endpoints.
    Api,
    /// Print the raw `/player` response for a video.
    Player {
        video_id: String,
        /// Write to this file instead of stdout.
        #[arg(short, long)]
        out: Option<String>,
    },
}

#[derive(Args)]
struct StreamCli {
    video_id: String,
    /// Run duration; with `--play` the default is unbounded.
    #[arg(long)]
    secs: Option<u64>,
    /// Mux a/v to stdout via ffmpeg (pipe to a player).
    #[arg(long)]
    play: bool,
    /// Target quality: a height (720/1080/…) or `best`.
    #[arg(long, default_value = "best")]
    quality: String,
    /// VOD: open at this offset in seconds (seek-at-open).
    #[arg(long)]
    start: Option<f64>,
    /// Timed in-stream quality switches, e.g. `"30:720,60:1080"`.
    #[arg(long)]
    switch: Option<String>,
}

fn parse_ms(v: &str, keyword: &str) -> Result<Option<i64>, String> {
    if v == keyword {
        return Ok(None);
    }
    v.parse().map(Some).map_err(|_| format!("expected ms or `{keyword}`, got {v:?}"))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    innertube_cli::init_logging();
    let cli = Cli::parse();
    let rt = tokio::runtime::Runtime::new()?;
    match cli.cmd {
        Cmd::Smoke { video_id } => rt.block_on(cmds::smoke(&video_id)),
        Cmd::Play { video_id, secs } => rt.block_on(cmds::play(&video_id, secs)),
        Cmd::Stream(s) => rt.block_on(cmds::stream(StreamArgs {
            video_id: s.video_id,
            run_secs: s.secs,
            play: s.play,
            quality: cmds::parse_quality(&s.quality),
            start_ms: s.start.map(|sec| (sec * 1000.0) as i64),
            switches: s.switch.as_deref().map(cmds::parse_switches).unwrap_or_default(),
        })),
        Cmd::Vod { video_id, out_dir, seek_ms, back_ms } => vod::run(&rt, &video_id, &out_dir, seek_ms, back_ms),
        Cmd::Shorts { count } => shorts::run(&rt, count),
        Cmd::AudioWindow { video_id, from, to, out } => {
            let from_ms = parse_ms(&from, "start")?.unwrap_or(0);
            let to_ms = parse_ms(&to, "edge")?;
            rt.block_on(audio_window::run(&video_id, from_ms, to_ms, &out))
        }
        Cmd::Auth { cookies_db } => rt.block_on(auth::run(cookies_db.as_deref())),
        Cmd::FindLive { query, count, filter } => rt.block_on(find_live::run(&query, count, filter.as_deref())),
        Cmd::Api => rt.block_on(api::smoke()),
        Cmd::Player { video_id, out } => rt.block_on(api::player(&video_id, out.as_deref())),
    }
}
