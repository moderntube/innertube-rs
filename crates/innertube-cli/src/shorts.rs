//! Swipe through Shorts the way the web client does: enter the feed seedless, show a
//! Short, preload the next one (one SABR round), then swipe and continue it.
//!
//! `it shorts [--count N]` prints, per Short, how long the open took and how long the
//! swipe waited for media.
use std::error::Error;
use std::time::{Duration, Instant};

use innertube::{ClientConfig, Navigation, NSigSolver, ReelEntry, SegmentVod, SegmentVodOptions, Session};

/// The Shorts player's size in the reference capture.
const VIEWPORT: (i32, i32) = (523, 932);

fn open_opts(e: &ReelEntry, preload: bool) -> SegmentVodOptions {
    SegmentVodOptions {
        navigation: Navigation {
            player_params: e.player_params.clone(),
            short: true,
            prefetched_player: e.prefetched.clone(),
            ..Default::default()
        },
        preload,
        viewport: Some(VIEWPORT),
        ..Default::default()
    }
}

/// Blocks until the first video piece is readable; how long that took, or `None` when the
/// session stopped without media.
fn first_media(vod: &SegmentVod) -> Option<Duration> {
    let t0 = Instant::now();
    let mut t = vod.track(true, 0);
    t.next_piece().map(|_| t0.elapsed())
}

fn ms(d: Duration) -> u128 {
    d.as_millis()
}

pub fn run(rt: &tokio::runtime::Runtime, count: usize) -> Result<(), Box<dyn Error>> {
    let _guard = rt.enter();
    let session = Session::new(ClientConfig::web())?;
    let solver = NSigSolver::new();
    let pot = crate::pot_from_env();

    let t0 = Instant::now();
    let start = rt.block_on(session.reel_seedless())?.ok_or("seedless entry returned no Short")?;
    eprintln!("[shorts] seedless entry {} ms: first {}", ms(t0.elapsed()), start.first.video_id);

    let mut current = start.first.clone();
    let t0 = Instant::now();
    let mut vod = rt.block_on(SegmentVod::open(&session, &solver, pot.clone(), &current.video_id, &open_opts(&current, false)))?;
    let overlay = rt.block_on(session.reel_item_watch(&current));
    eprintln!("[shorts] reel_item_watch {}: {:?}", current.video_id, overlay.as_ref().map(|o| (&o.likes, &o.comment_count)));
    if let Some(token) = overlay.as_ref().ok().and_then(|o| o.comments_token.clone()) {
        match rt.block_on(session.reel_comments_page(&token)) {
            Ok(page) => {
                eprintln!("[shorts] comments: {} on the first page, more={}", page.items.len(), page.continuation.is_some());
                if let Some(t) = page.items.iter().find_map(|c| c.replies_token.clone()) {
                    match rt.block_on(session.reel_comments_page(&t)) {
                        Ok(r) => {
                            let levels: Vec<u8> = r.items.iter().map(|c| c.reply_level).collect();
                            eprintln!(
                                "[shorts] replies: {} in the first thread page, levels {levels:?}, more={}",
                                r.items.len(),
                                r.continuation.is_some()
                            );
                        }
                        Err(e) => eprintln!("[shorts] replies failed: {e}"),
                    }
                }
            }
            Err(e) => eprintln!("[shorts] comments failed: {e}"),
        }
    }
    let sts = rt.block_on(innertube::session_signature_timestamp(&session));
    let mut queue = match &start.sequence {
        Some(tok) => rt.block_on(session.reel_watch_sequence(tok, &current.video_id, sts))?.entries,
        None => Vec::new(),
    };
    eprintln!(
        "[shorts] sequence: {} entries, {} prefetched",
        queue.len(),
        queue.iter().filter(|e| e.prefetched.is_some()).count()
    );
    first_media(&vod).ok_or("first Short got no media")?;
    eprintln!("[shorts] #0 {} cold open: media {} ms after the click (sts {sts:?})", current.video_id, ms(t0.elapsed()));

    for n in 1..=count {
        if queue.is_empty() {
            eprintln!("[shorts] sequence exhausted");
            break;
        }
        let next = queue.remove(0);
        let t_pre = Instant::now();
        let pre = rt.block_on(SegmentVod::open(&session, &solver, pot.clone(), &next.video_id, &open_opts(&next, true)))?;
        let source = if next.prefetched.is_some() { "prefetched player" } else { "/player" };
        if first_media(&pre).is_none() {
            eprintln!("[shorts] preload {} ({source}): no media, stopping", next.video_id);
            break;
        }
        eprintln!("[shorts] preload {} ({source}): media {} ms after starting it", next.video_id, ms(t_pre.elapsed()));
        std::thread::sleep(Duration::from_secs(4));

        let t_swipe = Instant::now();
        pre.activate();
        let _ = rt.block_on(session.reel_item_watch(&next));
        let swipe_wait = first_media(&pre).map_or(-1, |d| ms(d) as i64);
        vod.stop();
        eprintln!("[shorts] #{n} swipe to {}: media {} ms after the swipe (read {swipe_wait} ms)", next.video_id, ms(t_swipe.elapsed()));
        std::thread::sleep(Duration::from_secs(3));
        eprintln!("[shorts] #{n} after 3 s: heights {:?}", pre.heights());
        vod = pre;
        current = next;
    }
    eprintln!("[shorts] done on {}", current.video_id);
    vod.stop();
    Ok(())
}
