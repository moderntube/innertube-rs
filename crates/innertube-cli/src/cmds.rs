//! The SABR subcommands of `it`: `smoke`, `play` and `stream`. Argument parsing lives in
//! `it.rs`.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use innertube::{
    BgUtilProvider, ClientConfig, NSigSolver, Quality, SabrStream, SabrStreamOptions, SegmentSource, Session,
};
use innertube::{decode_base64_lenient, PlayerOptions, PlayerResponse};
use innertube_js::{extract_player_js_url, extract_signature_timestamp};
use innertube_pot::PoTokenProvider;
use innertube_sabr::{parse, SabrClientInfo, SabrEngine, SelectedFormat};

type BoxErr = Box<dyn std::error::Error>;

/// Read a query param value (`?key=...` / `&key=...`) up to the next separator.
fn query_param(url: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=");
    url.split(['?', '&']).find(|kv| kv.starts_with(&pat)).map(|kv| kv[pat.len()..].to_string())
}

/// Replace the `n` query param in `url` using the solver + player JS (else googlevideo 403s).
async fn solve_url_n(url: &str, base_js: &str, solver: &NSigSolver) -> Result<String, BoxErr> {
    match query_param(url, "n") {
        Some(n) => {
            let solved = solver.solve_n(base_js, std::slice::from_ref(&n)).await?;
            Ok(url.replace(&format!("n={n}"), &format!("n={}", solved[0])))
        }
        None => Ok(url.to_string()),
    }
}

/// `it smoke <video_id>`: one-shot live SABR fetch through `SabrEngine`, reporting MEDIA bytes
/// per itag. Uses the WEB/SSR path, where the server provisions the session ready.
pub async fn smoke(video_id: &str) -> Result<(), BoxErr> {
    let session = Session::new(ClientConfig::web())?;
    let client = session.client().clone();
    let pot = BgUtilProvider::local();

    let watch = session.get_text(&format!("https://www.youtube.com/watch?v={video_id}")).await?;
    let js_url = extract_player_js_url(&watch).ok_or("no player js url")?;
    let base_js = session.get_text(&js_url).await?;
    let sts = extract_signature_timestamp(&base_js);
    eprintln!("player js: {js_url} (sts {sts:?})");

    let player = PlayerResponse::from_watch_html(&watch)
        .ok_or("watch page had no streamable player response")?;
    eprintln!(
        "player (SSR): {} formats, ustreamer config {} bytes",
        player.formats.len(),
        player.ustreamer_config.len()
    );
    for f in &player.formats {
        eprintln!(
            "  itag {:>4}  {:>5}p  {}",
            f.itag,
            f.height.unwrap_or(0),
            f.mime_type,
        );
    }

    let audio = player.formats.iter().find(|f| f.is_audio()).expect("an audio format");
    let video = player.formats.iter().find(|f| f.is_video()).expect("a video format");
    eprintln!("selected: audio itag {}, video itag {}", audio.itag, video.itag);

    let mut sabr_url = player.server_abr_streaming_url.clone();
    if let Some(n) = query_param(&sabr_url, "n") {
        let solved = NSigSolver::new().solve_n(&base_js, std::slice::from_ref(&n)).await?;
        eprintln!("n: {n} -> {}", solved[0]);
        sabr_url = sabr_url.replace(&format!("n={n}"), &format!("n={}", solved[0]));
    }

    let po_token = decode_base64_lenient(&pot.get_po_token(video_id, None).await?).ok_or("bad token b64")?;
    eprintln!("GVS PO token: {} bytes", po_token.len());

    let sel = |f: &innertube::Format, is_video: bool| SelectedFormat {
        itag: f.itag,
        last_modified: f.last_modified,
        is_video,
        xtags: f.xtags.clone(),
    };
    let all_formats = player.formats.iter().map(|f| sel(f, f.is_video())).collect::<Vec<_>>();
    let mut engine = SabrEngine::new(
        player.ustreamer_config.clone(),
        po_token,
        vec![sel(audio, false), sel(video, true)],
        &all_formats,
    );
    engine.set_live(player.is_live);
    engine.set_viewport(video.width.unwrap_or(1280), video.height.unwrap_or(720));
    engine.set_client_info(SabrClientInfo {
        client_name: client.client_name_id,
        client_version: client.client_version.clone(),
        os_name: client.os_name.clone(),
        os_version: client.os_version.clone(),
    });

    let mut totals: BTreeMap<i32, usize> = BTreeMap::new();
    for round in 0..4 {
        // `INNERTUBE_DUMP=raw:<dir>` records these requests like the live run loop's, so a
        // request that works can be byte-diffed against one that does not
        // (`scripts/sabr.py`, `scripts/diff_sabr_requests.py`).
        // `INNERTUBE_SMOKE_EDGE=1` asks for the live edge instead of `player_time = 0`. Without
        // it the request targets the DVR start, where segments are long finalized, so it says
        // nothing about whether the edge serves. `LIVE_EDGE_SEEK_MS` is what the live run loop
        // sends on a (re)join.
        let smoke_pt = if std::env::var("INNERTUBE_SMOKE_EDGE").as_deref() == Ok("1") {
            innertube_sabr::LIVE_EDGE_SEEK_MS
        } else {
            0
        };
        let smoke_req = engine.request_bytes(smoke_pt);
        let response = session.post_sabr(&sabr_url, smoke_req).await?;
        if round == 0 {
            let mut counts: BTreeMap<u32, usize> = BTreeMap::new();
            for p in parse(response.clone()) {
                *counts.entry(p.part_type).or_insert(0) += 1;
            }
            eprintln!("UMP parts by type (round 0): {counts:?}");
        }
        let ingest = engine.ingest(response.clone());
        for (itag, bytes) in &ingest.media {
            *totals.entry(*itag).or_insert(0) += bytes.len();
        }
        eprintln!("round {round}: {} B response, prot={:?}", response.len(), ingest.protection_status);
        if !totals.is_empty() {
            break;
        }
    }
    for (itag, bytes) in &totals {
        eprintln!("itag {itag}: {bytes} media bytes");
    }
    if totals.values().any(|&b| b > 0) {
        eprintln!("\n✅ Milestone A: live SABR media received (via SabrEngine).");
    } else {
        eprintln!("\n⚠️  No MEDIA bytes emitted — inspect parts above.");
    }
    Ok(())
}

/// `it play <video_id> [--secs N]`: sustained SABR loop reporting the realtime ratio; writes
/// tracks to `/tmp/sabr_out.<itag>.mp4`.
pub async fn play(video_id: &str, run_secs: u64) -> Result<(), BoxErr> {
    let session = Session::new(ClientConfig::web())?;
    let pot = BgUtilProvider::local();
    let solver = NSigSolver::new();

    let visitor_data = session.bootstrap_visitor_data().await?.ok_or("no visitorData")?;
    let watch = session.get_text(&format!("https://www.youtube.com/watch?v={video_id}")).await?;
    let base_js = session.get_text(extract_player_js_url(&watch).ok_or("no jsUrl")?.as_str()).await?;
    let sts = extract_signature_timestamp(&base_js);

    let player = session
        .player(
            video_id,
            &PlayerOptions {
                visitor_data: Some(&visitor_data),
                signature_timestamp: sts,
                reload_token: None,
                po_token: None,
                ..Default::default()
            },
        )
        .await?;
    let audio = player.formats.iter().find(|f| f.is_audio()).expect("audio");
    let video = player.formats.iter().find(|f| f.is_video()).expect("video");
    eprintln!("formats: audio itag {}, video itag {} (sts {sts:?})", audio.itag, video.itag);

    let mut url = solve_url_n(&player.server_abr_streaming_url, &base_js, &solver).await?;
    let po_token = decode_base64_lenient(&pot.get_po_token(video_id, None).await?).ok_or("token")?;

    let all_formats: Vec<SelectedFormat> = player
        .formats
        .iter()
        .map(|f| SelectedFormat {
            itag: f.itag,
            last_modified: f.last_modified,
            is_video: f.is_video(),
            xtags: f.xtags.clone(),
        })
        .collect();
    let mut engine = SabrEngine::new(
        player.ustreamer_config.clone(),
        po_token,
        vec![
            SelectedFormat { itag: audio.itag, last_modified: audio.last_modified, is_video: false, xtags: audio.xtags.clone() },
            SelectedFormat { itag: video.itag, last_modified: video.last_modified, is_video: true, xtags: video.xtags.clone() },
        ],
        &all_formats,
    );

    let mut tracks: BTreeMap<i32, Vec<u8>> = BTreeMap::new();
    let start = Instant::now();
    let mut round = 0u32;
    let mut consecutive_errors = 0u32;
    let mut edge: Option<(f64, f64)> = None;
    let mut anchor: Option<(i64, f64)> = None;

    while start.elapsed().as_secs() < run_secs {
        round += 1;
        let wall_ms_now = start.elapsed().as_millis() as f64;
        let player_time_ms = match anchor {
            Some((a, w0)) => a + (wall_ms_now - w0) as i64,
            None => 9_007_199_254_740_991,
        };
        // `INNERTUBE_DUMP=raw:<dir>` records these requests like the live run loop's and
        // `smoke`'s, so this path can be captured next to a mitmproxy recording of YouTube's
        // own client on the same stream at the same moment.
        let play_req = engine.request_bytes(player_time_ms);
        let resp = match session.post_sabr(&url, play_req).await {
            Ok(b) => {
                consecutive_errors = 0;
                b
            }
            Err(e) => {
                consecutive_errors += 1;
                eprintln!("round {round}: POST error ({consecutive_errors}/5): {e}");
                if consecutive_errors >= 5 {
                    eprintln!("giving up after 5 consecutive errors");
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        };
        let ingest = engine.ingest(resp);
        let got: usize = ingest.media.values().map(Vec::len).sum();
        for (itag, bytes) in ingest.media {
            tracks.entry(itag).or_default().extend_from_slice(&bytes);
        }
        if anchor.is_none() {
            if let Some(a) = engine.anchor_ms() {
                anchor = Some((a, start.elapsed().as_millis() as f64));
            }
        }
        if let Some(redirect) = ingest.redirect {
            eprintln!("round {round}: SABR_REDIRECT -> new host");
            url = redirect;
        }
        let wall_ms = start.elapsed().as_millis().max(1) as f64;
        let buffered_ms = engine.buffered_ms() as f64;
        eprintln!(
            "round {round:>3}: +{got:>7} B | buffered {:>7.1}s | wall {:>5.1}s | drain {:.1}x",
            buffered_ms / 1000.0,
            wall_ms / 1000.0,
            buffered_ms / wall_ms,
        );
        if got == 0 {
            edge.get_or_insert((wall_ms, buffered_ms));
            tokio::time::sleep(Duration::from_millis(1000)).await;
        }
    }

    eprintln!("\n=== writing tracks ===");
    for (itag, bytes) in &tracks {
        let path = format!("/tmp/sabr_out.{itag}.mp4");
        std::fs::write(&path, bytes)?;
        eprintln!("itag {itag}: {} bytes -> {path}", bytes.len());
    }
    let wall_ms = start.elapsed().as_millis().max(1) as f64;
    let buffered_ms = engine.buffered_ms() as f64;
    eprintln!(
        "\nfinal: {:.1}s media in {:.1}s wall — overall {:.1}x realtime",
        buffered_ms / 1000.0,
        wall_ms / 1000.0,
        buffered_ms / wall_ms,
    );
    if let Some((edge_wall, edge_buf)) = edge {
        let d_wall = wall_ms - edge_wall;
        if d_wall > 2000.0 {
            eprintln!(
                "live-edge: {:.1}s of new media over {:.1}s at the edge — {:.2}x realtime",
                (buffered_ms - edge_buf) / 1000.0,
                d_wall / 1000.0,
                (buffered_ms - edge_buf) / d_wall,
            );
        }
    }
    Ok(())
}

/// Args for [`stream`], the `it stream` subcommand.
pub struct StreamArgs {
    pub video_id: String,
    /// Run duration; `--play` defaults to unbounded, else 30s.
    pub run_secs: Option<u64>,
    pub play: bool,
    pub quality: Quality,
    /// VOD seek-at-open offset (ms).
    pub start_ms: Option<i64>,
    /// Timed in-stream quality switches: `(at_secs, height)`.
    pub switches: Vec<(u64, i32)>,
}

/// `it stream <video_id> …`: drives the high-level `SabrStream` and writes each track's raw
/// live segments to output. With `--play` the tracks go to named fifos read by `ffmpeg`, which
/// writes one Matroska stream to stdout; otherwise they go to `/tmp` capture files.
pub async fn stream(a: StreamArgs) -> Result<(), BoxErr> {
    let run_secs = a.run_secs.unwrap_or(if a.play { u64::MAX } else { 30 });
    let session = Session::new(ClientConfig::web())?;
    let pot = BgUtilProvider::local();
    let solver = NSigSolver::new();

    let opts = SabrStreamOptions {
        quality: a.quality,
        start_ms: a.start_ms,
        ..Default::default()
    };
    let t0 = Instant::now();
    let stream = SabrStream::open(&session, &solver, std::sync::Arc::new(pot), &a.video_id, &opts).await?;
    let info = stream.info();
    eprintln!(
        "── opened in {:.2}s — video itag {} ({}p, {}), audio itag {} ──",
        t0.elapsed().as_secs_f64(),
        info.video_itag,
        info.video_height,
        info.codec,
        info.audio_itag,
    );
    let (apath, vpath) = if a.play {
        ("/tmp/sabr_live.audio.fifo".to_string(), "/tmp/sabr_live.video.fifo".to_string())
    } else {
        ("/tmp/sabr_live.audio.mp4".to_string(), "/tmp/sabr_live.video.mp4".to_string())
    };
    let mut muxer: Option<Child> = None;
    if a.play {
        for p in [&apath, &vpath] {
            let _ = std::fs::remove_file(p);
            if !Command::new("mkfifo").arg(p).status().map(|s| s.success()).unwrap_or(false) {
                return Err(format!("mkfifo {p} failed (coreutils on PATH?)").into());
            }
        }
        let child = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-fflags", "+genpts"])
            .args(["-i", &apath, "-i", &vpath])
            .args(["-c", "copy", "-f", "matroska", "pipe:1"])
            .stdout(std::process::Stdio::inherit())
            .spawn()
            .map_err(|e| format!("spawn ffmpeg (installed?): {e}"))?;
        eprintln!("muxing live a/v to stdout via ffmpeg (pid {}) — pipe to a player, e.g. `it stream … --play | mpv -`", child.id());
        muxer = Some(child);
    }

    let play = a.play;
    let (audio_track, video_track) = (stream.segment_track(false), stream.segment_track(true));
    let stops = [audio_track.interrupt_handle(), video_track.interrupt_handle()];
    let reader_threads = [("audio", apath.clone(), audio_track), ("video", vpath.clone(), video_track)]
        .map(|(name, path, mut track)| {
            std::thread::spawn(move || -> std::io::Result<u64> {
                let mut sink: Box<dyn Write> = if play {
                    let s = std::fs::OpenOptions::new().write(true).open(&path)?;
                    eprintln!("  ffmpeg opened {name} fifo (connected)");
                    Box::new(s)
                } else {
                    Box::new(File::create(&path)?)
                };
                let mut written = 0u64;
                while let Some(piece) = track.next_piece() {
                    sink.write_all(&piece.bytes)?;
                    written += piece.bytes.len() as u64;
                }
                Ok(written)
            })
        });

    let start = Instant::now();
    let mut last_itag = 0i32;
    let mut switch_applied = vec![false; a.switches.len()];
    while start.elapsed().as_secs() < run_secs {
        for (i, &(at_secs, height)) in a.switches.iter().enumerate() {
            if !switch_applied[i] && start.elapsed().as_secs() >= at_secs {
                switch_applied[i] = true;
                eprintln!("  --switch: forcing quality {height}p at {}s", start.elapsed().as_secs());
                stream.set_quality(height);
            }
        }
        if let Some(p) = muxer.as_mut() {
            if let Ok(Some(status)) = p.try_wait() {
                eprintln!("ffmpeg exited ({status}); stopping");
                break;
            }
        }
        let st = stream.stats();
        if let Some(itag) = st.served_video_itag {
            if itag != last_itag {
                last_itag = itag;
                eprintln!("  serving video itag {itag}");
            }
        }
        let ahead = st.live_head_ms.map_or(f64::NAN, |h| (h - st.player_time_ms) as f64 / 1000.0);
        eprintln!(
            "pos {:.0}s | buf a={:.0}s/v={:.0}s | head {} ahead {:+.1}s | prot {:?} | seqΔ {} | seek#{} | wall {:.1}s | POST {:.2}s",
            st.player_time_ms as f64 / 1000.0,
            st.buffered_audio_ms as f64 / 1000.0,
            st.buffered_video_ms as f64 / 1000.0,
            st.live_head_ms.map_or("?".to_string(), |h| format!("{:.1}s", h as f64 / 1000.0)),
            ahead,
            st.protection_status,
            st.discontinuities,
            st.seek_generation,
            start.elapsed().as_secs_f64(),
            st.last_post_secs,
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    stream.stop().await;
    for s in &stops {
        s.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let [a_bytes, v_bytes] = reader_threads.map(|t| t.join().unwrap().unwrap_or(0));
    if let Some(mut p) = muxer {
        eprintln!("\nfed {a_bytes} B audio + {v_bytes} B video; waiting for ffmpeg to flush…");
        let _ = p.wait();
        let _ = std::fs::remove_file(&apath);
        let _ = std::fs::remove_file(&vpath);
    } else {
        eprintln!("\nwrote {apath} ({a_bytes} B), {vpath} ({v_bytes} B)");
        eprintln!("play:  mpv --audio-file={apath} {vpath}");
    }
    Ok(())
}

/// Parse `--switch "30:720,60:1080"` (malformed pairs skipped).
pub fn parse_switches(s: &str) -> Vec<(u64, i32)> {
    s.split(',')
        .filter_map(|pair| {
            let (t, h) = pair.split_once(':')?;
            Some((t.trim().parse().ok()?, h.trim().parse().ok()?))
        })
        .collect()
}

/// Parse `--quality H|best` (default best on empty/garbage).
pub fn parse_quality(s: &str) -> Quality {
    if s.eq_ignore_ascii_case("best") {
        Quality::Best
    } else {
        s.parse::<i32>().map(Quality::Height).unwrap_or(Quality::Best)
    }
}
