//! Channel pages, their tabs and sort chips, and `resolve_url`.

use super::*;

/// Result of `navigation/resolve_url`: a vanity URL resolved to a navigation target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedUrl {
    /// The `browseId` to load (e.g. a `UC…` channel id), if the URL resolved to a browse.
    pub browse_id: Option<String>,
    /// `webPageType`, e.g. `WEB_PAGE_TYPE_CHANNEL`.
    pub page_type: Option<String>,
    /// Canonical URL the target maps to.
    pub canonical_url: Option<String>,
}

/// Parse a `navigation/resolve_url` response into a [`ResolvedUrl`].
pub(crate) fn parse_resolved_url(v: &Value) -> ResolvedUrl {
    let ep = &v["endpoint"];
    let web = &ep["commandMetadata"]["webCommandMetadata"];
    ResolvedUrl {
        browse_id: ep["browseEndpoint"]["browseId"].as_str().map(str::to_string),
        page_type: web["webPageType"].as_str().map(str::to_string),
        canonical_url: web["url"].as_str().map(str::to_string),
    }
}

/// Finds the browse-continuation token of the channel sort chip labelled `title` (for
/// example Popular or Oldest). The Videos-tab sort bar is a `chipBarViewModel` of
/// `chipViewModel`s, each with an `accessibilityLabel` and a
/// `tapCommand.innertubeCommand.continuationCommand.token` (IT-026), so a sort is a browse
/// continuation. `None` when no chip matches. Labels are English because the client is en_US.
pub(crate) fn find_sort_chip_token(v: &Value, title: &str) -> Option<String> {
    fn walk(v: &Value, title: &str) -> Option<String> {
        if let Some(chip) = v.get("chipViewModel") {
            if chip["accessibilityLabel"].as_str() == Some(title) {
                if let Some(t) = chip["tapCommand"]["innertubeCommand"]["continuationCommand"]
                    ["token"]
                    .as_str()
                {
                    return Some(t.to_string());
                }
            }
        }
        match v {
            Value::Object(m) => m.values().find_map(|c| walk(c, title)),
            Value::Array(a) => a.iter().find_map(|c| walk(c, title)),
            _ => None,
        }
    }
    walk(v, title)
}

/// A channel page (`browse UC…`): header + tab list + the videos on the landing tab.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Channel {
    pub id: Option<String>,
    pub name: Option<String>,
    pub handle: Option<String>,
    pub subscribers: Option<String>,
    pub description: Option<String>,
    pub tabs: Vec<String>,
    pub videos: Vec<SearchItem>,
    /// Whether the signed-in user is subscribed (from `subscribeButtonRenderer.subscribed`;
    /// always false when anonymous).
    pub subscribed: bool,
    /// Channel banner image URL (widest `imageBannerViewModel` source), if the channel
    /// has one.
    pub banner_url: Option<String>,
    /// Channel avatar URL (widest `channelMetadataRenderer.avatar` thumbnail). It is not read
    /// from `decoratedAvatarViewModel`, which appears per video card as the uploader's avatar.
    pub avatar_url: Option<String>,
    /// e.g. "64 videos".
    pub video_count: Option<String>,
    /// The header title carries the verified check badge.
    pub verified: bool,
    /// The header's first link, as (label, target url).
    pub link: Option<(String, String)>,
}

/// Videos from a channel content-tab browse (Videos/Shorts/Live), in the tab's
/// order (Videos/Shorts = newest first), plus a continuation for the next page.
pub(crate) fn parse_channel_videos(v: &Value) -> SearchResults {
    let mut out = SearchResults::default();
    walk(v, &mut |node| {
        if let Some(lvm) = node.get("lockupViewModel") {
            if let Some(it) = lockup_video(lvm) {
                out.items.push(it);
            }
        }
        if let Some(sl) = node.get("shortsLockupViewModel") {
            out.items.extend(shorts_lockup(sl));
        }
        // Sort chips (Latest/Popular/Oldest). Each carries a browse continuation token.
        // `continuation_token` reads only `continuationItemRenderer`, so chip tokens are not
        // mistaken for the grid's scroll continuation.
        if let Some(chip) = node.get("chipViewModel") {
            // The label is in `accessibilityLabel`; `text` and `text.content` are fallbacks.
            let label = chip["accessibilityLabel"]
                .as_str()
                .or_else(|| chip["text"].as_str())
                .or_else(|| chip["text"]["content"].as_str())
                .unwrap_or_default()
                .to_string();
            let token = chip["tapCommand"]["innertubeCommand"]["continuationCommand"]["token"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if !label.is_empty() && !token.is_empty() {
                out.sort_chips.push(SortChip {
                    label,
                    token,
                    selected: chip["selected"].as_bool().unwrap_or(false),
                });
            }
        }
        if let Some(t) = continuation_token(node) {
            out.continuation = Some(t);
        }
    });
    out
}

/// Parse a channel `browse` response into a [`Channel`].
pub(crate) fn parse_channel(v: &Value) -> Channel {
    let mut c = Channel::default();
    let mut meta_texts: Vec<String> = Vec::new();
    walk(v, &mut |node| {
        if let Some(m) = node.get("channelMetadataRenderer") {
            c.name = c.name.take().or_else(|| m["title"].as_str().map(str::to_string));
            c.id = c.id.take().or_else(|| m["externalId"].as_str().map(str::to_string));
            c.description = c.description.take().or_else(|| m["description"].as_str().map(str::to_string));
            if c.avatar_url.is_none() {
                c.avatar_url = m["avatar"]["thumbnails"]
                    .as_array()
                    .and_then(|t| t.iter().max_by_key(|s| s["width"].as_i64().unwrap_or(0)))
                    .and_then(|s| s["url"].as_str())
                    .map(str::to_string);
            }
        }
        if let Some(t) = node.get("tabRenderer").and_then(|t| t["title"].as_str()) {
            c.tabs.push(t.to_string());
        }
        if let Some(lvm) = node.get("lockupViewModel") {
            if let Some(it) = lockup_video(lvm) {
                c.videos.push(it);
            }
        }
        // Subscribe state (authed): subscribeButtonRenderer.subscribed (bool). Repeated
        // across the page; first definite value wins.
        if let Some(sb) = node.get("subscribeButtonRenderer") {
            if let Some(s) = sb["subscribed"].as_bool() {
                c.subscribed = c.subscribed || s;
            }
        }
        // Banner: the widest imageBannerViewModel source url.
        if c.banner_url.is_none() {
            if let Some(sources) = node.pointer("/imageBannerViewModel/image/sources").and_then(Value::as_array) {
                c.banner_url = sources
                    .iter()
                    .max_by_key(|s| s["width"].as_i64().unwrap_or(0))
                    .and_then(|s| s["url"].as_str())
                    .map(str::to_string);
            }
        }
        if let Some(h) = node.get("pageHeaderViewModel") {
            let runs = &h["title"]["dynamicTextViewModel"]["text"]["attachmentRuns"];
            c.verified |= runs.to_string().contains("CHECK_CIRCLE");
            let text = &h["attribution"]["attributionViewModel"]["text"];
            if let (Some(label), Some(url)) = (
                text["content"].as_str(),
                text["commandRuns"][0]["onTap"]["innertubeCommand"]["urlEndpoint"]["url"].as_str(),
            ) {
                c.link = Some((label.trim().to_string(), redirect_target(url)));
            }
        }
        // pageHeaderViewModel metadata rows carry the @handle + "N subscribers".
        if let Some(parts) = node.get("metadataParts").and_then(Value::as_array) {
            for p in parts {
                if let Some(s) = p["text"]["content"].as_str() {
                    meta_texts.push(s.to_string());
                }
            }
        }
    });
    c.handle = meta_texts.iter().find(|s| s.starts_with('@')).cloned();
    c.subscribers = meta_texts.iter().find(|s| s.contains("subscriber")).cloned();
    c.video_count = meta_texts.iter().find(|s| s.ends_with(" videos") || s.ends_with(" video")).cloned();
    c
}

/// The destination of a `youtube.com/redirect?…&q=<url>` link; other urls pass through.
fn redirect_target(url: &str) -> String {
    url.split(['?', '&'])
        .find_map(|kv| kv.strip_prefix("q="))
        .map(percent_decode)
        .unwrap_or_else(|| url.to_string())
}
