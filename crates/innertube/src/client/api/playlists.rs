//! Playlists and their contents.

use super::*;

/// Contents of a playlist (`browse VL…`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaylistContents {
    pub title: Option<String>,
    pub items: Vec<SearchItem>,
}

/// A playlist entry (id + title) from a library/list view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playlist {
    pub id: String,
    pub title: String,
    /// First-video thumbnail (mqdefault), when the source lockup carries one.
    pub thumbnail: Option<String>,
}

/// The first video thumbnail inside a lockup's `contentImage`. Playlist and collection
/// lockups embed the lead video's thumbnail; the id comes from the ytimg URL and the result
/// is an `mqdefault` URL. `None` if no ytimg thumbnail is present.
pub(super) fn first_video_thumb(v: &Value) -> Option<String> {
    fn find_id(v: &Value) -> Option<String> {
        if let Some(u) = v.get("url").and_then(Value::as_str) {
            if let Some(rest) = u.split("/vi/").nth(1) {
                let id = rest.split('/').next().unwrap_or("");
                if id.len() == 11 {
                    return Some(id.to_string());
                }
            }
        }
        match v {
            Value::Object(m) => m.values().find_map(find_id),
            Value::Array(a) => a.iter().find_map(find_id),
            _ => None,
        }
    }
    find_id(v).map(|id| format!("https://i.ytimg.com/vi/{id}/mqdefault.jpg"))
}

/// Parse the playlists out of a library/browse response. Modern YouTube renders
/// them as `lockupViewModel` with `LOCKUP_CONTENT_TYPE_PLAYLIST`.
pub(crate) fn parse_playlists(v: &Value) -> Vec<Playlist> {
    let mut out = Vec::new();
    walk(v, &mut |node| {
        if let Some(lvm) = node.get("lockupViewModel") {
            if lvm["contentType"].as_str() == Some("LOCKUP_CONTENT_TYPE_PLAYLIST") {
                if let Some(id) = lvm["contentId"].as_str() {
                    let title = lvm["metadata"]["lockupMetadataViewModel"]["title"]["content"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    let thumbnail = first_video_thumb(&lvm["contentImage"]);
                    out.push(Playlist { id: id.to_string(), title, thumbnail });
                }
            }
        }
    });
    out
}

/// Parse `browse VL…` playlist contents into title + videos.
pub(crate) fn parse_playlist_contents(v: &Value) -> PlaylistContents {
    let mut items = Vec::new();
    let mut title = None;
    walk(v, &mut |node| {
        // Legacy renderer (kept for older responses).
        if let Some(pvr) = node.get("playlistVideoRenderer") {
            if let Some(it) = video_item(pvr) {
                items.push(it);
            }
        }
        // Current ViewModel format: playlist items are `lockupViewModel`. Reuse
        // `lockup_video` (only VIDEO lockups become items; nested playlist/channel lockups
        // are skipped).
        if let Some(lk) = node.get("lockupViewModel") {
            if let Some(it) = lockup_video(lk) {
                items.push(it);
            }
        }
        if title.is_none() {
            if let Some(h) = node.get("playlistHeaderRenderer") {
                title = text(&h["title"]);
            }
        }
    });
    PlaylistContents { title, items }
}
