//! Search results, feed items and the video renderers they share.

use super::*;

/// A single search (or related-videos) result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchItem {
    Video {
        id: String,
        title: String,
        channel: Option<String>,
        channel_id: Option<String>,
        /// Uploader avatar (for list/feed rows that show it).
        channel_avatar: Option<String>,
        /// e.g. "1.2M views" / "37M views" (as YouTube formats it).
        views: Option<String>,
        /// e.g. "2 days ago" / "Streamed 3 weeks ago".
        published: Option<String>,
        /// e.g. "12:34" (None for live).
        duration: Option<String>,
        /// Currently live right now (show a LIVE badge instead of a duration).
        live: bool,
        /// Channel-membership-gated ("Members only"). These fail to play for non-members,
        /// so consumers can badge or filter them (IT-019).
        members_only: bool,
        /// The animated-WebP preview the reference client shows on a watch-sidebar hover
        /// (`i.ytimg.com/an_webp/<id>/mqdefault_6s.webp`). Only the modern lockup carries
        /// it. The url is signed (`du`/`sqp`/`rs`) and cannot be rebuilt from the video id,
        /// so it is passed through verbatim.
        moving_thumbnail: Option<String>,
        /// `playerParams` the feed's hover preview of this item plays with.
        preview_player_params: Option<String>,
    },
    /// A Shorts card; `reel` is the endpoint that opens it in the Shorts player.
    Short {
        title: String,
        /// e.g. "1.2M views".
        views: Option<String>,
        /// The portrait card image.
        thumbnail: Option<String>,
        reel: crate::client::ReelEntry,
    },
    /// A feed's Shorts shelf (`richShelfRenderer`), in its feed position; `items` are `Short`s.
    ShortsShelf { title: String, items: Vec<SearchItem> },
    Channel {
        id: String,
        title: String,
        /// Channel avatar URL (widest thumbnail), if present.
        avatar: Option<String>,
        /// Formatted subscriber count, e.g. `"4.51M subscribers"`.
        subscribers: Option<String>,
    },
    Playlist {
        id: String,
        title: String,
        /// First-video thumbnail (mqdefault), when the card carries one.
        thumbnail: Option<String>,
    },
}

/// One sort option on a channel tab (Latest / Popular / Oldest), read from the response's
/// chip bar. Its [`token`](SortChip::token) is fed to a `browse` continuation
/// (`Session::channel_tab_continuation`) to load the sorted view (IT-026).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SortChip {
    pub label: String,
    pub token: String,
    pub selected: bool,
}

/// A search filter dialog option. `params` is the `search` blob it sends, combined with the
/// filters in force; on a selected option it clears that option (IT-029).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchFilter {
    pub label: String,
    pub params: Option<String>,
    pub selected: bool,
    /// The option does not combine with the filters in force.
    pub disabled: bool,
}

/// A titled group of the search filter dialog (Type, Duration, Upload date, ...).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchFilterGroup {
    pub title: String,
    pub filters: Vec<SearchFilter>,
}

/// A search results chip; `token` is a `search` continuation that reloads the page (IT-030).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchChip {
    pub label: String,
    pub token: Option<String>,
    pub selected: bool,
}

/// A page of results and the next-page token, with a channel tab's or a search page's header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchResults {
    pub items: Vec<SearchItem>,
    pub continuation: Option<String>,
    pub sort_chips: Vec<SortChip>,
    pub filters: Vec<SearchFilterGroup>,
    pub chips: Vec<SearchChip>,
}

fn filter_groups(dialog: &Value) -> Vec<SearchFilterGroup> {
    let groups = dialog["groups"].as_array().map(Vec::as_slice).unwrap_or_default();
    groups
        .iter()
        .map(|g| &g["searchFilterGroupRenderer"])
        .map(|g| SearchFilterGroup {
            title: text(&g["title"]).unwrap_or_default(),
            filters: g["filters"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .map(|f| &f["searchFilterRenderer"])
                .filter_map(|f| {
                    Some(SearchFilter {
                        label: text(&f["label"])?,
                        params: f["navigationEndpoint"]["searchEndpoint"]["params"].as_str().map(str::to_string),
                        selected: f["status"].as_str() == Some("FILTER_STATUS_SELECTED"),
                        disabled: f["status"].as_str() == Some("FILTER_STATUS_DISABLED"),
                    })
                })
                .collect(),
        })
        .filter(|g| !g.filters.is_empty())
        .collect()
}

/// The `UC…` channel id behind a byline's run navigation endpoint (so a search result
/// can deep-link to its uploader's channel page). Tries the common byline fields.
fn byline_channel_id(vr: &Value) -> Option<String> {
    for field in ["ownerText", "longBylineText", "shortBylineText"] {
        let runs = vr[field]["runs"].as_array();
        if let Some(runs) = runs {
            for r in runs {
                if let Some(id) = r["navigationEndpoint"]["browseEndpoint"]["browseId"].as_str() {
                    return Some(id.to_string());
                }
            }
        }
    }
    None
}

/// Largest image url in a `{thumbnails:[…]}` (videoRenderer) or `{sources:[…]}` (view
/// models) list; the last entry is the biggest.
pub(super) fn last_thumb(v: &Value) -> Option<String> {
    let arr = v["thumbnails"].as_array().or_else(|| v["sources"].as_array())?;
    arr.last()?["url"].as_str().map(str::to_string)
}

/// Is this videoRenderer a currently-live stream? Live videos carry a LIVE time-status
/// overlay (or a LIVE_NOW badge) instead of a duration.
fn video_renderer_is_live(vr: &Value) -> bool {
    if let Some(overlays) = vr["thumbnailOverlays"].as_array() {
        for o in overlays {
            let style = o["thumbnailOverlayTimeStatusRenderer"]["style"].as_str();
            if style == Some("LIVE") || style == Some("DEFAULT_LIVE") {
                return true;
            }
        }
    }
    if let Some(badges) = vr["badges"].as_array() {
        for b in badges {
            if b["metadataBadgeRenderer"]["style"].as_str() == Some("BADGE_STYLE_TYPE_LIVE_NOW") {
                return true;
            }
        }
    }
    false
}

/// Is this `videoRenderer` channel-membership-gated? Marked by a
/// `BADGE_STYLE_TYPE_MEMBERS_ONLY` metadata badge in the same `badges` array that carries
/// the LIVE badge (IT-019).
fn video_renderer_members_only(vr: &Value) -> bool {
    vr["badges"].as_array().is_some_and(|badges| {
        badges
            .iter()
            .any(|b| b["metadataBadgeRenderer"]["style"].as_str() == Some("BADGE_STYLE_TYPE_MEMBERS_ONLY"))
    })
}

/// `playerParams` of the inline preview the feed plays on hover: a renderer's
/// `inlinePlaybackEndpoint`, or a lockup's or short's `inlinePlayerData.onVisible`.
fn preview_player_params(v: &Value) -> Option<String> {
    [
        &v["inlinePlaybackEndpoint"]["watchEndpoint"],
        &v["itemPlayback"]["inlinePlayerData"]["onVisible"]["innertubeCommand"]["watchEndpoint"],
        &v["inlinePlayerData"]["onVisible"]["innertubeCommand"]["watchEndpoint"],
    ]
    .into_iter()
    .find_map(|e| e["playerParams"].as_str())
    .map(str::to_string)
}

/// A `videoRenderer`/`compactVideoRenderer`/`playlistVideoRenderer` → `SearchItem::Video`.
pub(super) fn video_item(vr: &Value) -> Option<SearchItem> {
    let id = vr["videoId"].as_str()?.to_string();
    let live = video_renderer_is_live(vr);
    Some(SearchItem::Video {
        id,
        title: text(&vr["title"]).unwrap_or_default(),
        channel: text(&vr["ownerText"])
            .or_else(|| text(&vr["longBylineText"]))
            .or_else(|| text(&vr["shortBylineText"])),
        channel_id: byline_channel_id(vr),
        channel_avatar: last_thumb(
            &vr["channelThumbnailSupportedRenderers"]["channelThumbnailWithLinkRenderer"]["thumbnail"],
        )
        .or_else(|| last_thumb(&vr["channelThumbnail"])),
        views: text(&vr["viewCountText"]).or_else(|| text(&vr["shortViewCountText"])),
        published: text(&vr["publishedTimeText"]),
        duration: if live { None } else { text(&vr["lengthText"]) },
        live,
        members_only: video_renderer_members_only(vr),
        // Only the modern lockup carries one (see the field docs).
        moving_thumbnail: None,
        preview_player_params: preview_player_params(vr),
    })
}

/// A modern `lockupViewModel` of a video → `SearchItem::Video` (used on channel
/// pages, feeds, and `next` related). Returns `None` for non-video lockups.
/// The first collaborator's channel `browseId` from a collab lockup's avatar-stack
/// "Collaborators" dialog (`avatarStackViewModel` → showDialog → listItems). Used to give
/// a collab card a click-through target (the primary/first-listed channel).
fn first_collaborator_id(lmv: &Value) -> Option<String> {
    let items = lmv["image"]["avatarStackViewModel"]["rendererContext"]["commandContext"]
        ["onTap"]["innertubeCommand"]["showDialogCommand"]["panelLoadingStrategy"]
        ["inlineContent"]["dialogViewModel"]["customContent"]["listViewModel"]["listItems"]
        .as_array()?;
    items.iter().find_map(|it| {
        it["listItemViewModel"]["rendererContext"]["commandContext"]["onTap"]
            ["innertubeCommand"]["browseEndpoint"]["browseId"]
            .as_str()
            .map(String::from)
    })
}

pub(super) fn lockup_video(lvm: &Value) -> Option<SearchItem> {
    if lvm["contentType"].as_str()? != "LOCKUP_CONTENT_TYPE_VIDEO" {
        return None;
    }
    let id = lvm["contentId"].as_str()?.to_string();
    let lmv = &lvm["metadata"]["lockupMetadataViewModel"];
    let title = lmv["title"]["content"].as_str().unwrap_or_default().to_string();

    // The metadata rows render as "<channel> · <views> · <published>", but order and
    // presence vary (a channel's own page may omit the channel). Classify each part by
    // content: the part with a channel browseId is the uploader; "view(s)" is views;
    // "ago"/"Streamed" is published.
    let mut channel = None;
    let mut channel_id = None;
    let mut views = None;
    let mut published = None;
    let mut members_only = false;
    // First metadata part that is not a linked channel, views or published: the visible
    // byline text of a collaboration video ("A and B", "X and N more"), which has no channel
    // commandRun. Used as the channel fallback only when the avatar confirms a collab.
    let mut byline_fallback: Option<String> = None;
    // A single-channel avatar links the channel; the byline row may then be plain text.
    let avatar_channel_id = lmv["image"]["decoratedAvatarViewModel"]["rendererContext"]["commandContext"]
        ["onTap"]["innertubeCommand"]["browseEndpoint"]["browseId"]
        .as_str();
    if let Some(rows) = lmv["metadata"]["contentMetadataViewModel"]["metadataRows"].as_array() {
        for (row_index, row) in rows.iter().enumerate() {
            // Membership gate: the view-model badgeStyle is "BADGE_MEMBERS_ONLY" (no
            // _STYLE_TYPE_ infix, unlike the classic videoRenderer badge; IT-019).
            if let Some(badges) = row["badges"].as_array() {
                if badges.iter().any(|b| {
                    b["badgeViewModel"]["badgeStyle"].as_str() == Some("BADGE_MEMBERS_ONLY")
                }) {
                    members_only = true;
                }
            }
            let Some(parts) = row["metadataParts"].as_array() else { continue };
            for part in parts {
                let t = &part["text"];
                let Some(content) = t["content"].as_str() else { continue };
                let bid = t["commandRuns"]
                    .as_array()
                    .and_then(|cr| cr.iter().find_map(|r| {
                        r["onTap"]["innertubeCommand"]["browseEndpoint"]["browseId"].as_str()
                    }));
                if let Some(bid) = bid {
                    if bid.starts_with("UC") && channel.is_none() {
                        channel = Some(content.to_string());
                        channel_id = Some(bid.to_string());
                        continue;
                    }
                }
                let lc = content.to_ascii_lowercase();
                let label = part["accessibilityLabel"].as_str().unwrap_or_default().to_ascii_lowercase();
                if views.is_none() && lc.contains("view") {
                    views = Some(content.to_string());
                } else if views.is_none() && label.contains("view") {
                    views = Some(format!("{content} views"));
                } else if row_index == 0 && channel.is_none() && avatar_channel_id.is_some() {
                    channel = Some(content.to_string());
                    channel_id = avatar_channel_id.map(str::to_string);
                } else if published.is_none() && (lc.contains("ago") || lc.contains("stream")) {
                    published = Some(content.to_string());
                } else if byline_fallback.is_none() {
                    byline_fallback = Some(content.to_string());
                }
            }
        }
    }

    // Collaboration video: the byline is plain text (no channel commandRun) and the channel
    // links live in an avatar-stack "Collaborators" dialog. When the linked-channel scan
    // above found nothing but the avatar is a stack, show the byline text and link the
    // primary (first-listed) collaborator. Gating on the stack keeps single-channel cards
    // (whose channel may be legitimately omitted, e.g. a channel's own page) untouched.
    if channel.is_none() && lmv["image"]["avatarStackViewModel"].is_object() {
        channel = byline_fallback;
        channel_id = first_collaborator_id(lmv);
    }

    let channel_avatar = last_thumb(
        &lmv["image"]["decoratedAvatarViewModel"]["avatar"]["avatarViewModel"]["image"],
    )
    .or_else(|| last_thumb(&lmv["image"]["avatarStackViewModel"]["avatars"][0]["avatarViewModel"]["image"]));

    // Duration (or live) is the thumbnail's bottom-overlay badge. The animated preview the
    // reference client shows on a sidebar hover is in the same array.
    let mut duration = None;
    let mut live = false;
    let mut moving_thumbnail = None;
    if let Some(overlays) = lvm["contentImage"]["thumbnailViewModel"]["overlays"].as_array() {
        for o in overlays {
            if moving_thumbnail.is_none() {
                // Widest source, matching `last_thumb`'s rule for still thumbnails. Taken
                // verbatim: the url is signed (`du`/`sqp`/`rs`) and a rebuilt one 404s.
                moving_thumbnail = o["animatedThumbnailOverlayViewModel"]["thumbnail"]
                    ["sources"]
                    .as_array()
                    .and_then(|srcs| {
                        srcs.iter()
                            .max_by_key(|s| s["width"].as_i64().unwrap_or(0))
                            .and_then(|s| s["url"].as_str())
                    })
                    .map(String::from);
            }
            if let Some(badges) = o["thumbnailBottomOverlayViewModel"]["badges"].as_array() {
                for b in badges {
                    if let Some(txt) = b["thumbnailBadgeViewModel"]["text"].as_str() {
                        if txt.eq_ignore_ascii_case("live") {
                            live = true;
                        } else if duration.is_none() {
                            duration = Some(txt.to_string());
                        }
                    }
                }
            }
        }
    }
    if published.as_deref().map(|p| p.to_ascii_lowercase().contains("watching")) == Some(true) {
        live = true;
    }

    Some(SearchItem::Video {
        id,
        title,
        channel,
        channel_id,
        channel_avatar,
        views,
        published,
        duration: if live { None } else { duration },
        live,
        members_only,
        moving_thumbnail,
        preview_player_params: preview_player_params(lvm),
    })
}

/// A `shortsLockupViewModel` (Shorts shelves, the channel Shorts tab, search) → `SearchItem::Short`.
pub(super) fn shorts_lockup(sl: &Value) -> Option<SearchItem> {
    let reel = crate::client::reel::parse_entry(&sl["onTap"]["innertubeCommand"]["reelWatchEndpoint"])?;
    Some(SearchItem::Short {
        title: sl["overlayMetadata"]["primaryText"]["content"].as_str().unwrap_or_default().to_string(),
        views: sl["overlayMetadata"]["secondaryText"]["content"].as_str().map(str::to_string),
        thumbnail: last_thumb(&sl["thumbnailViewModel"]["thumbnailViewModel"]["image"])
            .or_else(|| reel.thumbnail.clone()),
        reel,
    })
}

/// The `feedbackToken` from a "Not interested" menu item in a feed, if present.
/// Pass it to [`crate::Session::feedback`] to tune recommendations.
pub fn find_not_interested_token(feed: &Value) -> Option<String> {
    let mut token = None;
    walk(feed, &mut |node| {
        if token.is_none() && node["icon"]["iconType"].as_str() == Some("NOT_INTERESTED") {
            token = find_str(node, "feedbackToken");
        }
    });
    token
}

/// Parse a `search` response (or its continuation) into typed results + next token.
pub(crate) fn parse_search(v: &Value) -> SearchResults {
    let mut out = SearchResults::default();
    walk_pruned(v, &mut |node| {
        if let Some(dialog) = node.get("searchFilterOptionsDialogRenderer") {
            out.filters = filter_groups(dialog);
            return false;
        }
        if let Some(chip) = node.get("chipCloudChipRenderer") {
            if let Some(label) = text(&chip["text"]) {
                out.chips.push(SearchChip {
                    label,
                    token: chip["navigationEndpoint"]["continuationCommand"]["token"].as_str().map(str::to_string),
                    selected: chip["isSelected"].as_bool().unwrap_or(false),
                });
            }
            return false;
        }
        if let Some(shelf) = node.get("richShelfRenderer") {
            let mut items = Vec::new();
            walk(&shelf["contents"], &mut |n| {
                if let Some(sl) = n.get("shortsLockupViewModel") {
                    items.extend(shorts_lockup(sl));
                }
            });
            if !items.is_empty() {
                out.items.push(SearchItem::ShortsShelf { title: text(&shelf["title"]).unwrap_or_default(), items });
                return false;
            }
        }
        if let Some(vr) = node.get("videoRenderer") {
            if let Some(item) = video_item(vr) {
                out.items.push(item);
            }
        } else if let Some(sl) = node.get("shortsLockupViewModel") {
            out.items.extend(shorts_lockup(sl));
        } else if let Some(cr) = node.get("channelRenderer") {
            if let Some(id) = cr["channelId"].as_str() {
                // Avatar thumbnails are protocol-relative (`//yt3.ggpht.com/…`); make them
                // absolute so a plain HTTP fetch works. subscriberCountText is e.g.
                // "4.51M subscribers" (or a videoCountText fallback on some locales).
                let avatar = last_thumb(&cr["thumbnail"]).map(|u| {
                    if let Some(rest) = u.strip_prefix("//") {
                        format!("https://{rest}")
                    } else {
                        u
                    }
                });
                out.items.push(SearchItem::Channel {
                    id: id.to_string(),
                    title: text(&cr["title"]).unwrap_or_default(),
                    avatar,
                    subscribers: text(&cr["subscriberCountText"]),
                });
            }
        } else if let Some(pr) = node.get("playlistRenderer") {
            if let Some(id) = pr["playlistId"].as_str() {
                out.items.push(SearchItem::Playlist {
                    id: id.to_string(),
                    title: text(&pr["title"]).unwrap_or_default(),
                    thumbnail: first_video_thumb(pr),
                });
            }
        } else if let Some(lvm) = node.get("lockupViewModel") {
            // Feeds and search serve items as lockups instead of *Renderers; accept videos
            // and playlists so results are not dropped.
            if let Some(item) = lockup_video(lvm) {
                out.items.push(item);
            } else if lvm["contentType"].as_str() == Some("LOCKUP_CONTENT_TYPE_PLAYLIST") {
                if let Some(id) = lvm["contentId"].as_str() {
                    out.items.push(SearchItem::Playlist {
                        id: id.to_string(),
                        title: lvm["metadata"]["lockupMetadataViewModel"]["title"]["content"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        thumbnail: first_video_thumb(&lvm["contentImage"]),
                    });
                }
            }
        }
        if let Some(t) = continuation_token(node) {
            out.continuation = Some(t);
        }
        true
    });
    out
}
