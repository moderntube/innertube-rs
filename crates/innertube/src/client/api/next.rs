//! The `next` response: watch-page metadata and the sidebar.

use super::*;

/// Watch-page metadata from `next`: the headline fields plus the token that loads the
/// comments (a separate continuation).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VideoDetails {
    pub title: Option<String>,
    pub channel: Option<String>,
    pub view_count: Option<String>,
    /// Token to pass to `continuation("next", …)` to load the comment thread.
    pub comments_continuation: Option<String>,
    pub related: Vec<SearchItem>,
    /// The uploader's `UC…` channel id (`owner.videoOwnerRenderer.navigationEndpoint.browseEndpoint.browseId`).
    pub channel_id: Option<String>,
    /// Largest uploader avatar thumbnail URL.
    pub channel_avatar: Option<String>,
    /// Formatted subscriber count (e.g. `"4.51M subscribers"`).
    pub subscriber_count: Option<String>,
    /// Relative publish date (e.g. `"13 years ago"`).
    pub published: Option<String>,
    /// Full video description text.
    pub description: Option<String>,
    /// Formatted like count (e.g. `"19,214,590"`).
    pub like_count: Option<String>,
    /// Whether the signed-in user is subscribed to the uploader (always false when
    /// anonymous).
    pub subscribed: bool,
    /// Every channel of a collab video, in YouTube's order; empty for a single-channel
    /// video. `channel` then holds the combined title ("A and B") and `channel_id` the first.
    pub collaborators: Vec<Collaborator>,
}

/// One channel from a collab video's "Collaborators" dialog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Collaborator {
    pub channel_id: String,
    pub name: String,
    /// Handle and subscriber count (e.g. `"@handle • 13.9K subscribers"`).
    pub subtitle: Option<String>,
    pub avatar: Option<String>,
}

/// The channels in a "Collaborators" `showDialogCommand`'s list.
fn collaborators(dialog: &Value) -> Vec<Collaborator> {
    let items = &dialog["showDialogCommand"]["panelLoadingStrategy"]["inlineContent"]["dialogViewModel"]
        ["customContent"]["listViewModel"]["listItems"];
    let Some(items) = items.as_array() else { return Vec::new() };
    items
        .iter()
        .filter_map(|it| {
            let item = &it["listItemViewModel"];
            let channel_id = item["title"]["commandRuns"].as_array()?.iter().find_map(|r| {
                r["onTap"]["innertubeCommand"]["browseEndpoint"]["browseId"].as_str()
            })?;
            Some(Collaborator {
                channel_id: channel_id.to_string(),
                name: item["title"]["content"].as_str()?.to_string(),
                // The subtitle wraps its parts in bidi isolation marks.
                subtitle: item["subtitle"]["content"]
                    .as_str()
                    .map(|s| s.chars().filter(|c| !matches!(c, '\u{200e}' | '\u{2068}' | '\u{2069}')).collect()),
                avatar: last_thumb(&item["leadingAccessory"]["avatarViewModel"]["image"]),
            })
        })
        .collect()
}

/// Parse the watch-next data YouTube renders into a watch page as `ytInitialData`. It has
/// the same shape as the `next` POST response and is the source of the browser's sidebar.
///
/// Prefer it over the POST: only the page carries the sidebar's
/// `animatedThumbnailOverlayViewModel` entries, the signed `an_webp` hover previews. No
/// `next` or `get_watch` response has them. It also saves a POST when the caller already
/// holds the page.
///
/// `None` when the page embeds no `ytInitialData`, so the caller can fall back to the POST.
pub fn video_details_from_watch_html(html: &str) -> Option<VideoDetails> {
    let json = crate::client::extract_json_object_after(html, "ytInitialData =")?;
    let v: Value = serde_json::from_str(&json).ok()?;
    Some(parse_next(&v))
}

/// The first digit run in `s`, thousands separators kept: "along with 36,490 other people" → "36,490".
fn first_grouped_number(s: &str) -> Option<String> {
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let n: String = s[start..].chars().take_while(|c| c.is_ascii_digit() || *c == ',').collect();
    Some(n.trim_end_matches(',').to_string())
}

pub(crate) fn parse_next(v: &Value) -> VideoDetails {
    let mut out = VideoDetails::default();
    walk(v, &mut |node| {
        if let Some(p) = node.get("videoPrimaryInfoRenderer") {
            out.title = out.title.take().or_else(|| text(&p["title"]));
            out.view_count = out.view_count.take().or_else(|| {
                text(&p["viewCount"]["videoViewCountRenderer"]["viewCount"])
            });
            out.published = out.published.take().or_else(|| text(&p["relativeDateText"]));
        }
        if let Some(s) = node.get("videoSecondaryInfoRenderer") {
            let owner = &s["owner"]["videoOwnerRenderer"];
            out.channel = out.channel.take().or_else(|| text(&owner["title"]));
            out.channel_id = out.channel_id.take().or_else(|| {
                owner["navigationEndpoint"]["browseEndpoint"]["browseId"]
                    .as_str()
                    .map(str::to_string)
            });
            out.channel_avatar = out.channel_avatar.take().or_else(|| last_thumb(&owner["thumbnail"]));
            out.subscriber_count =
                out.subscriber_count.take().or_else(|| text(&owner["subscriberCountText"]));
            // A collab owner has no title runs: `attributedTitle` reads "A and B" and its
            // endpoint opens the dialog listing each channel.
            if out.collaborators.is_empty() {
                out.collaborators = collaborators(&owner["navigationEndpoint"]);
            }
            if let Some(first) = out.collaborators.first() {
                out.channel = out.channel.take().or_else(|| owner["attributedTitle"]["content"].as_str().map(String::from));
                out.channel_id = out.channel_id.take().or_else(|| Some(first.channel_id.clone()));
                out.channel_avatar = out.channel_avatar.take().or_else(|| first.avatar.clone());
            }
            out.description = out.description.take().or_else(|| {
                s["attributedDescription"]["content"].as_str().map(str::to_string)
            });
            if let Some(sub) = s["subscribeButton"]["subscribeButtonRenderer"]["subscribed"].as_bool() {
                out.subscribed = out.subscribed || sub;
            }
        }
        // The like count is indirected via an entity key (frameworkUpdates.entityBatchUpdate,
        // as with commentEntityPayload) and is not inlined under videoActions.
        if let Some(c) = node["likeCountEntity"]["expandedLikeCountIfLiked"]["content"].as_str() {
            out.like_count = out.like_count.take().or(Some(c.to_string()));
        }
        if out.like_count.is_none() {
            let button = &node["segmentedLikeDislikeButtonViewModel"]["likeButtonViewModel"]["likeButtonViewModel"]
                ["toggleButtonViewModel"]["toggleButtonViewModel"]["defaultButtonViewModel"]["buttonViewModel"];
            out.like_count = button["accessibilityText"].as_str().and_then(first_grouped_number);
        }
        if let Some(cvr) = node.get("compactVideoRenderer") {
            if let Some(item) = video_item(cvr) {
                out.related.push(item);
            }
        }
        // Modern related videos arrive as lockupViewModel.
        if let Some(lvm) = node.get("lockupViewModel") {
            if let Some(item) = lockup_video(lvm) {
                out.related.push(item);
            }
        }
        // The comments live behind their own continuation (section id "comment…").
        if let Some(isr) = node.get("itemSectionRenderer") {
            let id = isr["sectionIdentifier"].as_str().unwrap_or("");
            if id.contains("comment") {
                if let Some(arr) = isr["contents"].as_array() {
                    for c in arr {
                        if let Some(t) = continuation_token(c) {
                            out.comments_continuation = Some(t);
                        }
                    }
                }
            }
        }
    });
    out
}
