//! Shorts ("reels"): entering the feed, the per-Short overlay call, and the sequence of
//! next Shorts (IT-015, IT-016).

use serde_json::{json, Value};

use crate::client::{actions, ClientConfig};

/// One Short as a `reelWatchEndpoint` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReelEntry {
    pub video_id: String,
    /// Sent as the `/player` request's `params`.
    pub player_params: Option<String>,
    /// Sent as `reel_item_watch`'s `params`.
    pub params: Option<String>,
    /// The 1080x1920 first frame.
    pub thumbnail: Option<String>,
    /// The complete player response JSON the sequence prefetched for this Short.
    pub prefetched: Option<String>,
    /// A sequence token carried by the endpoint itself (a Shorts card may have one).
    pub sequence_params: Option<String>,
    /// An ad slot in the sequence: the endpoint carries `adClientParams` and no overlay.
    pub is_ad: bool,
}

/// What the Shorts overlay shows for the Short on screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReelOverlay {
    pub title: Option<String>,
    pub channel: Option<String>,
    /// The comments panel's first-page token, for [`crate::Session::comments_page`].
    pub comments_token: Option<String>,
    /// The comments panel's count, as shown (e.g. "25", "1.2K").
    pub comment_count: Option<String>,
    /// The like button's count, as shown, and whether the account has liked the Short.
    pub likes: Option<String>,
    pub liked: bool,
}

/// The overlay half of a `reel_item_watch` response.
pub(crate) fn parse_overlay(v: &Value) -> ReelOverlay {
    let items = &v["overlay"]["reelPlayerOverlayRenderer"]["playerOverlay"]["reelPlayerOverlayViewModel"]["metapanel"]
        ["reelMetapanelViewModel"]["metadataItems"];
    let mut out = ReelOverlay::default();
    for it in items.as_array().into_iter().flatten() {
        if let Some(t) = it["shortsVideoTitleViewModel"]["text"]["content"].as_str() {
            out.title = Some(t.to_string());
        }
        if let Some(c) = it["reelChannelBarViewModel"]["channelName"]["content"].as_str() {
            out.channel = Some(c.to_string());
        }
    }
    let buttons = &v["overlay"]["reelPlayerOverlayRenderer"]["playerOverlay"]["reelPlayerOverlayViewModel"]["actionBar"]
        ["reelActionBarViewModel"]["buttonViewModels"];
    if let Some(like) = buttons.as_array().into_iter().flatten().find_map(|b| b.get("likeButtonViewModel")) {
        let toggle = &like["toggleButtonViewModel"]["toggleButtonViewModel"];
        out.liked = toggle["isToggled"].as_bool().unwrap_or(false);
        let shown = if out.liked { "toggledButtonViewModel" } else { "defaultButtonViewModel" };
        out.likes = toggle[shown]["buttonViewModel"]["title"].as_str().map(str::to_string);
    }
    let panels = v["engagementPanels"].as_array().into_iter().flatten().map(|p| &p["engagementPanelSectionListRenderer"]);
    // Anonymous responses name the panel by `panelIdentifier`, signed-in ones by `targetId`.
    let comments_panel = |p: &&Value| {
        [&p["panelIdentifier"], &p["targetId"]].iter().any(|id| **id == "engagement-panel-comments-section")
    };
    if let Some(p) = panels.into_iter().find(comments_panel) {
        out.comments_token = p["content"]["sectionListRenderer"]["contents"][0]["itemSectionRenderer"]["contents"][0]
            ["continuationItemRenderer"]["continuationEndpoint"]["continuationCommand"]["token"]
            .as_str()
            .map(str::to_string);
        out.comment_count = p["header"]["engagementPanelTitleHeaderRenderer"]["contextualInfo"]["runs"][0]["text"]
            .as_str()
            .map(str::to_string);
    }
    out
}

/// The first Short of a seedless entry, and the token for the sequence after it.
#[derive(Debug, Clone)]
pub struct ReelStart {
    pub first: ReelEntry,
    pub sequence: Option<String>,
}

/// A page of the Shorts sequence.
#[derive(Debug, Clone, Default)]
pub struct ReelSequence {
    pub entries: Vec<ReelEntry>,
    pub continuation: Option<String>,
}

/// A `reelWatchEndpoint` object, or `None` without a `videoId`.
pub(crate) fn parse_entry(ep: &Value) -> Option<ReelEntry> {
    let video_id = ep["videoId"].as_str()?.to_string();
    let s = |k: &str| ep[k].as_str().map(str::to_string);
    let prefetched = ep["unserializedPrefetchData"]["playerResponse"]
        .as_object()
        .filter(|o| o.get("streamingData").is_some())
        .map(|o| Value::Object(o.clone()).to_string());
    Some(ReelEntry {
        video_id,
        player_params: s("playerParams"),
        params: s("params"),
        thumbnail: ep["thumbnail"]["thumbnails"][0]["url"].as_str().map(str::to_string),
        prefetched,
        sequence_params: s("sequenceParams"),
        is_ad: ep.get("adClientParams").is_some(),
    })
}

pub(crate) fn parse_seedless(v: &Value) -> Option<ReelStart> {
    Some(ReelStart {
        first: parse_entry(&v["replacementEndpoint"]["reelWatchEndpoint"])?,
        sequence: v["sequenceContinuation"].as_str().map(str::to_string),
    })
}

pub(crate) fn parse_sequence(v: &Value) -> ReelSequence {
    let entries = v["entries"]
        .as_array()
        .map(|a| a.iter().filter_map(|e| parse_entry(&e["command"]["reelWatchEndpoint"])).collect())
        .unwrap_or_default();
    let continuation =
        v["continuationEndpoint"]["continuationCommand"]["token"].as_str().map(str::to_string);
    ReelSequence { entries, continuation }
}

pub(crate) fn seedless_body(c: &ClientConfig) -> Value {
    json!({
        "context": actions::context(c),
        "params": "CA8%3D",
        "inputType": "REEL_WATCH_INPUT_TYPE_SEEDLESS",
        "disablePlayerResponse": true,
    })
}

pub(crate) fn item_watch_body(c: &ClientConfig, e: &ReelEntry) -> Value {
    let mut player_request = json!({ "videoId": e.video_id });
    if let Some(p) = &e.player_params {
        player_request["params"] = json!(p);
    }
    let mut body = json!({
        "context": actions::context(c),
        "playerRequest": player_request,
        "disablePlayerResponse": true,
    });
    if let Some(p) = &e.params {
        body["params"] = json!(p);
    }
    body
}

/// `current` is the Short on screen; the sequence is asked for from its page.
pub(crate) fn sequence_body(c: &ClientConfig, token: &str, current: &str, sts: Option<i64>) -> Value {
    let mut cpc = json!({
        "currentUrl": format!("/shorts/{current}"),
        "vis": 0,
        "splay": false,
        "autoCaptionsDefaultOn": false,
        "autonavState": "STATE_NONE",
        "html5Preference": "HTML5_PREF_WANTS",
        "lactMilliseconds": "-1",
    });
    if let Some(sts) = sts {
        cpc["signatureTimestamp"] = json!(sts);
    }
    json!({
        "context": actions::context(c),
        "sequenceParams": token,
        "playbackContext": { "contentPlaybackContext": cpc },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(id: &str) -> Value {
        json!({
            "videoId": id,
            "playerParams": "8AEB",
            "params": "CA8a",
            "thumbnail": { "thumbnails": [{ "url": format!("https://i.ytimg.com/vi/{id}/frame0.jpg") }] },
        })
    }

    // IT-015
    #[test]
    fn seedless_response_gives_the_first_short_and_the_sequence_token() {
        let v = json!({ "replacementEndpoint": { "reelWatchEndpoint": endpoint("q6") }, "sequenceContinuation": "GnEK" });
        let start = parse_seedless(&v).unwrap();
        assert_eq!(start.first.video_id, "q6");
        assert_eq!(start.first.player_params.as_deref(), Some("8AEB"));
        assert_eq!(start.first.params.as_deref(), Some("CA8a"));
        assert_eq!(start.sequence.as_deref(), Some("GnEK"));
    }

    // IT-016
    #[test]
    fn sequence_lists_entries_and_the_next_page() {
        let v = json!({
            "entries": [
                { "command": { "reelWatchEndpoint": endpoint("a") } },
                { "command": { "reelWatchEndpoint": endpoint("b") } },
                { "command": {} },
            ],
            "continuationEndpoint": { "continuationCommand": { "token": "next" } },
        });
        let seq = parse_sequence(&v);
        assert_eq!(seq.entries.iter().map(|e| e.video_id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(seq.continuation.as_deref(), Some("next"));
        assert!(seq.entries[0].prefetched.is_none());
        assert_eq!(seq.entries[1].thumbnail.as_deref(), Some("https://i.ytimg.com/vi/b/frame0.jpg"));
    }

    // IT-016: `reel_item_watch` carries `playerRequest {videoId, params}`.
    #[test]
    fn item_watch_body_wraps_the_player_request() {
        let e = parse_entry(&endpoint("q6")).unwrap();
        let b = item_watch_body(&ClientConfig::web(), &e);
        assert_eq!(b["playerRequest"], json!({ "videoId": "q6", "params": "8AEB" }));
        assert_eq!(b["params"], "CA8a");
        assert_eq!(b["disablePlayerResponse"], true);
    }

    // IT-016, IT-007
    #[test]
    fn sequence_body_names_the_short_on_screen() {
        let b = sequence_body(&ClientConfig::web(), "tok", "q6", Some(20594));
        let cpc = &b["playbackContext"]["contentPlaybackContext"];
        assert_eq!(b["sequenceParams"], "tok");
        assert_eq!(cpc["currentUrl"], "/shorts/q6");
        assert_eq!(cpc["signatureTimestamp"], 20594);
    }

    // IT-015
    #[test]
    fn seedless_body_matches_the_capture() {
        let b = seedless_body(&ClientConfig::web());
        assert_eq!(b["params"], "CA8%3D");
        assert_eq!(b["inputType"], "REEL_WATCH_INPUT_TYPE_SEEDLESS");
        assert_eq!(b["disablePlayerResponse"], true);
    }
}
