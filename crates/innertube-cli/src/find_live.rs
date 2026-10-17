//! `it find-live <query>`: searches YouTube with the live filter and prints currently-live
//! video IDs, TAB-separated as `id<TAB>channel<TAB>title`.
//!
//! `sp=EgJAAQ%3D%3D` is YouTube's "Live" search filter (the `&sp=` URL param). `--filter`
//! replaces it, and `--filter none` drops it, so the same lookup can source a long VOD.
use innertube::{ClientConfig, SearchItem, Session};

const LIVE_FILTER: &str = "EgJAAQ%3D%3D";

pub async fn run(query: &str, count: usize, filter: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let filter = match filter {
        None => Some(LIVE_FILTER),
        Some("none") => None,
        Some(f) => Some(f),
    };
    let session = Session::new(ClientConfig::web())?;
    // Anonymous search works, but a visitorData makes results consistent with playback.
    let _ = session.bootstrap_visitor_data().await;

    let results = session.search(query, filter).await?;
    let mut printed = 0;
    for item in &results.items {
        if let SearchItem::Video { id, title, channel, .. } = item {
            println!("{id}\t{}\t{}", channel.as_deref().unwrap_or("?"), title);
            printed += 1;
            if printed >= count {
                break;
            }
        }
    }
    if printed == 0 {
        eprintln!("no live results for {query:?}");
    }
    Ok(())
}
