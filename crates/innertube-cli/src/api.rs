//! `it api` and `it player`: live checks of the anonymous read and navigation methods
//! against real YouTube responses.

use innertube::{ClientConfig, SearchItem, Session};

type BoxErr = Box<dyn std::error::Error>;

/// `it player <video_id> [-o out.json]`: the raw `/player` response, for
/// `scripts/diff_player_responses.py`. A metadata call; it opens no media.
pub async fn player(video_id: &str, out: Option<&str>) -> Result<(), BoxErr> {
    let s = Session::new(ClientConfig::web())?;
    let v = s.player_raw(video_id, &innertube::PlayerOptions::default()).await?;
    match out {
        Some(out) => {
            std::fs::write(out, v.as_bytes())?;
            eprintln!("wrote {out} ({} bytes)", v.len());
        }
        None => println!("{v}"),
    }
    Ok(())
}

/// `it api`: resolve_url, channel, search, next, comments, transcript, live chat and the
/// channel tabs, printing a summary of each.
pub async fn smoke() -> Result<(), BoxErr> {
    let s = Session::new(ClientConfig::web())?;

    println!("── resolve_url(@LinusTechTips) ──");
    let r = s.resolve_url("https://www.youtube.com/@LinusTechTips").await?;
    println!("  browseId={:?} pageType={:?}", r.browse_id, r.page_type);

    if let Some(bid) = &r.browse_id {
        println!("\n── channel({bid}) ──");
        let c = s.channel(bid).await?;
        println!("  name={:?} handle={:?} subs={:?}", c.name, c.handle, c.subscribers);
        println!("  tabs={:?}", c.tabs);
        println!("  landing-tab first: {:?}", c.videos.first());
        let up = s.channel_uploads(bid).await?;
        println!("  uploads (newest-first) first: {:?}", up.items.first());
    }

    println!("\n── search(\"rust programming\") ──");
    let results = s.search("rust programming", None).await?;
    println!("  {} items, continuation={}", results.items.len(), results.continuation.is_some());
    let mut first_video = None;
    for item in results.items.iter().take(6) {
        match item {
            SearchItem::Video { id, title, channel, .. } => {
                println!("  [video]   {id}  {title}  — {}", channel.as_deref().unwrap_or("?"));
                first_video.get_or_insert(id.clone());
            }
            SearchItem::Short { title, reel, .. } => println!("  [short]   {}  {title}", reel.video_id),
            SearchItem::ShortsShelf { title, items } => println!("  [shelf]   {title}: {} shorts", items.len()),
            SearchItem::Channel { id, title, .. } => println!("  [channel] {id}  {title}"),
            SearchItem::Playlist { id, title, .. } => println!("  [playlist]{id}  {title}"),
        }
    }

    if let Some(vid) = &first_video {
        println!("\n── next({vid}) ──");
        let d = s.next(vid).await?;
        println!("  title:   {:?}", d.title);
        println!("  channel: {:?}", d.channel);
        println!("  views:   {:?}", d.view_count);
        println!("  related: {} videos", d.related.len());
        for (i, r) in d.related.iter().enumerate() {
            match r {
                SearchItem::Video { id, title, members_only, .. } => println!(
                    "    [{i}] video {id} mo={members_only} {}",
                    title.chars().take(50).collect::<String>()
                ),
                SearchItem::Channel { id, title, .. } => {
                    println!("    [{i}] channel {id} {:?}", title.chars().take(40).collect::<String>())
                }
                SearchItem::Playlist { id, title, .. } => {
                    println!("    [{i}] playlist {id} {}", title.chars().take(40).collect::<String>())
                }
                SearchItem::Short { reel, .. } => println!("    [{i}] short {}", reel.video_id),
                SearchItem::ShortsShelf { items, .. } => println!("    [{i}] shelf of {} shorts", items.len()),
            }
        }
        println!("  comments_continuation: {}", d.comments_continuation.is_some());

        if d.comments_continuation.is_some() {
            println!("\n── comments({vid}) ──");
            let c = s.comments(vid).await?;
            println!("  {} comments, next page: {}", c.items.len(), c.continuation.is_some());
            for cm in c.items.iter().take(3) {
                let v = if cm.is_verified { " ✓" } else { "" };
                println!(
                    "  {}{v} ({} likes, {} replies): {}",
                    cm.author,
                    cm.like_count.as_deref().unwrap_or("0"),
                    cm.reply_count.as_deref().unwrap_or("0"),
                    cm.text.chars().take(70).collect::<String>(),
                );
            }
        }
    }
    if let Some(vid) = &first_video {
        println!("\n── transcript({vid}) ──");
        let tracks = s.caption_tracks(vid).await?;
        println!("  {} caption tracks: {:?}", tracks.len(), tracks.iter().map(|t| (&t.language_code, t.auto)).collect::<Vec<_>>());
        let cues = s.transcript(vid, Some("en")).await?;
        println!("  {} cues; first 2: {:?}", cues.len(), cues.iter().take(2).map(|c| &c.text).collect::<Vec<_>>());
    }

    // Live chat: find a currently-live stream via the live filter, then read a page.
    println!("\n── live_chat (find a live stream) ──");
    let live = s.search("lofi hip hop radio", Some("EgJAAQ%3D%3D")).await?;
    if let Some(SearchItem::Video { id, title, .. }) = live.items.iter().find(|i| matches!(i, SearchItem::Video{..})) {
        println!("  live: {id} — {title}");
        let lc = s.live_chat(id).await?;
        println!("  {} messages, poll_ms={:?}, next token: {}", lc.messages.len(), lc.poll_ms, lc.continuation.is_some());
        for m in lc.messages.iter().take(4) {
            println!("    {}: {}", m.author, m.text().chars().take(60).collect::<String>());
        }
    }

    // Channel content filters (Videos / Shorts / Live vs uploads = all types).
    if let Some(bid) = &r.browse_id {
        println!("\n── channel filters ({bid}) ──");
        for (name, res) in [
            ("Videos", s.channel_videos(bid).await?),
            ("Shorts", s.channel_shorts(bid).await?),
            ("Live  ", s.channel_live(bid).await?),
        ] {
            println!("  {name}[0]: {:?}", res.items.first().map(|i| match i {
                SearchItem::Video { title, .. } => title.clone(), _ => String::new() }));
        }
    }

    println!("\n✓ read/navigation smoke passed");
    Ok(())
}
