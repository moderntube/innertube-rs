use super::*;
use serde_json::json;

#[test]
fn parse_playlist_reads_lockup_viewmodel() {
    // Playlist items are `lockupViewModel`: contentId is the video id, title.content the title.
    let v = json!({ "contents": { "items": [
        { "lockupViewModel": {
            "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
            "contentId": "abc12345678",
            "metadata": { "lockupMetadataViewModel": { "title": { "content": "First Video" } } }
        } },
        { "lockupViewModel": { "contentType": "LOCKUP_CONTENT_TYPE_PLAYLIST", "contentId": "PLxyz" } },
    ] }, "header": { "playlistHeaderRenderer": { "title": { "simpleText": "My Playlist" } } } });
    let pc = parse_playlist_contents(&v);
    assert_eq!(pc.title.as_deref(), Some("My Playlist"));
    assert_eq!(pc.items.len(), 1, "only the VIDEO lockup becomes an item");
    match &pc.items[0] {
        SearchItem::Video { id, title, .. } => {
            assert_eq!(id, "abc12345678");
            assert_eq!(title, "First Video");
        }
        _ => panic!("expected a video item"),
    }
}

#[test]
fn find_sort_chip_token_reads_viewmodel_continuation() {
    // YT's Videos-tab sort bar is the ViewModel format: chipBarViewModel → chipViewModel
    // with accessibilityLabel + tapCommand.innertubeCommand.continuationCommand.token.
    let v = json!({ "header": { "chipBarViewModel": { "chips": [
        { "chipViewModel": { "accessibilityLabel": "Latest", "selected": true,
            "tapCommand": { "innertubeCommand": { "continuationCommand": { "token": "TOK_LATEST" } } } } },
        { "chipViewModel": { "accessibilityLabel": "Popular", "selected": false,
            "tapCommand": { "innertubeCommand": { "continuationCommand": { "token": "TOK_POPULAR" } } } } },
        { "chipViewModel": { "accessibilityLabel": "Oldest", "selected": false,
            "tapCommand": { "innertubeCommand": { "continuationCommand": { "token": "TOK_OLDEST" } } } } },
    ] } } });
    assert_eq!(find_sort_chip_token(&v, "Popular").as_deref(), Some("TOK_POPULAR"));
    assert_eq!(find_sort_chip_token(&v, "Oldest").as_deref(), Some("TOK_OLDEST"));
    // A label with no chip → None (caller falls back to the default order).
    assert_eq!(find_sort_chip_token(&v, "Trending"), None);
}

#[test]
fn parse_live_chat_replay_extracts_video_offset() {
    // Replay batches wrap items in replayChatItemAction with videoOffsetTimeMsec.
    let v = json!({ "continuationContents": { "liveChatContinuation": {
        "actions": [
            { "replayChatItemAction": {
                "videoOffsetTimeMsec": "600056",
                "actions": [{ "addChatItemAction": { "item": { "liveChatTextMessageRenderer": {
                    "id": "a", "authorName": { "simpleText": "@x" },
                    "message": { "runs": [{ "text": "klay" }] }
                }}}}]
            }},
            // A live action has no offset.
            { "addChatItemAction": { "item": { "liveChatTextMessageRenderer": {
                "id": "b", "authorName": { "simpleText": "@y" },
                "message": { "runs": [{ "text": "hi" }] }
            }}}}
        ],
        "continuations": [{ "liveChatReplayContinuationData": { "continuation": "NEXT" } }]
    }}});
    let lc = parse_live_chat(&v);
    assert_eq!(lc.messages.len(), 2);
    assert_eq!(lc.messages[0].video_offset_ms, Some(600056));
    assert_eq!(lc.messages[0].text(), "klay");
    assert_eq!(lc.messages[1].video_offset_ms, None);
    assert_eq!(lc.continuation.as_deref(), Some("NEXT"));
}

#[test]
fn chat_send_success_returns_sent_with_echoed_item() {
    // Shape of a send_message response:
    // actions[].addChatItemAction.item.liveChatTextMessageRenderer.
    let v = json!({
        "actions": [{
            "addChatItemAction": { "item": { "liveChatTextMessageRenderer": {
                "id": "abc",
                "authorName": { "simpleText": "@me" },
                "message": { "runs": [{ "text": "hello" }] }
            }}}
        }]
    });
    match parse_chat_send_response(&v) {
        ChatSendResult::Sent(Some(m)) => assert_eq!(m.text(), "hello"),
        other => panic!("expected Sent(Some), got {other:?}"),
    }
}

#[test]
fn chat_send_error_renderer_returns_rejected_with_reason() {
    // Slow-mode reject as captured live: a top-level
    // errorMessage.liveChatTextActionsErrorMessageRenderer with multi-run errorText, next
    // to a dimChatItemAction.
    let v = json!({
        "actions": [{ "dimChatItemAction": { "clientAssignedId": "x" } }],
        "errorMessage": { "liveChatTextActionsErrorMessageRenderer": {
            "errorText": { "runs": [
                { "bold": true, "text": "Slow mode" },
                { "text": " is on. Wait " },
                { "text": "6 seconds" },
                { "text": " before posting." }
            ]},
            "editMessageText": { "runs": [{ "text": "Edit and try again." }] }
        }}
    });
    assert_eq!(
        parse_chat_send_response(&v),
        ChatSendResult::Rejected("Slow mode is on. Wait 6 seconds before posting.".into())
    );
}

#[test]
fn chat_send_no_item_no_error_is_rejected_not_silent_success() {
    // An empty response must not read as a successful send.
    let v = json!({ "responseContext": {}, "actions": [] });
    assert!(matches!(parse_chat_send_response(&v), ChatSendResult::Rejected(_)));
}

#[test]
fn parse_resolved_url_extracts_browse_target() {
    let v = json!({
        "endpoint": {
            "commandMetadata": { "webCommandMetadata": {
                "url": "/@MrBeast",
                "webPageType": "WEB_PAGE_TYPE_CHANNEL",
                "apiUrl": "/youtubei/v1/browse"
            }},
            "browseEndpoint": { "browseId": "UCX6OQ3DkcsbYNE6H8uQQuVA", "params": "abc" }
        }
    });
    let r = parse_resolved_url(&v);
    assert_eq!(r.browse_id.as_deref(), Some("UCX6OQ3DkcsbYNE6H8uQQuVA"));
    assert_eq!(r.page_type.as_deref(), Some("WEB_PAGE_TYPE_CHANNEL"));
    assert_eq!(r.canonical_url.as_deref(), Some("/@MrBeast"));
}

#[test]
fn parse_search_extracts_video_channel_and_continuation() {
    let v = json!({ "contents": { "sectionListRenderer": { "contents": [
        { "itemSectionRenderer": { "contents": [
            { "videoRenderer": {
                "videoId": "dQw4w9WgXcQ",
                "title": { "runs": [{ "text": "Never Gonna Give You Up" }] },
                "ownerText": { "runs": [{
                    "text": "Rick Astley",
                    "navigationEndpoint": { "browseEndpoint": { "browseId": "UCRickAstley" } }
                }] }
            }},
            { "channelRenderer": { "channelId": "UCabc", "title": { "simpleText": "Rick Astley" } } }
        ]}},
        { "continuationItemRenderer": { "continuationEndpoint": {
            "continuationCommand": { "token": "NEXTPAGE_TOKEN" }
        }}}
    ]}}});
    let r = parse_search(&v);
    assert_eq!(r.items.len(), 2);
    assert!(matches!(&r.items[0], SearchItem::Video { id, title, channel, channel_id, .. }
        if id == "dQw4w9WgXcQ"
            && title == "Never Gonna Give You Up"
            && channel.as_deref() == Some("Rick Astley")
            && channel_id.as_deref() == Some("UCRickAstley")));
    assert!(matches!(&r.items[1], SearchItem::Channel { id, .. } if id == "UCabc"));
    assert_eq!(r.continuation.as_deref(), Some("NEXTPAGE_TOKEN"));
}

#[test]
fn lockup_video_extracts_channel_views_published_duration() {
    // Real lockupViewModel shape (channel page / feed): channel name + browseId in a
    // metadata part, views + published in others, duration in the thumbnail badge.
    let lvm = json!({
        "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
        "contentId": "abc123",
        "contentImage": { "thumbnailViewModel": { "overlays": [
            { "thumbnailBottomOverlayViewModel": { "badges": [
                { "thumbnailBadgeViewModel": { "text": "32:08" } }
            ]}}
        ]}},
        "metadata": { "lockupMetadataViewModel": {
            "title": { "content": "Big Video" },
            "image": { "decoratedAvatarViewModel": { "avatar": { "avatarViewModel": {
                "image": { "sources": [{ "url": "https://yt3/avatar.jpg" }] }
            }}}},
            "metadata": { "contentMetadataViewModel": { "metadataRows": [
                { "metadataParts": [
                    { "text": { "content": "MrBeast", "commandRuns": [
                        { "onTap": { "innertubeCommand": { "browseEndpoint": { "browseId": "UCMrBeast" } } } }
                    ]}}
                ]},
                { "metadataParts": [
                    { "text": { "content": "37M views" } },
                    { "text": { "content": "1 day ago" } }
                ]}
            ]}}
        }}
    });
    let SearchItem::Video { id, title, channel, channel_id, channel_avatar, views, published, duration, .. } =
        lockup_video(&lvm).unwrap()
    else {
        panic!("a video")
    };
    assert_eq!((id.as_str(), title.as_str()), ("abc123", "Big Video"));
    assert_eq!(channel.as_deref(), Some("MrBeast"));
    assert_eq!(channel_id.as_deref(), Some("UCMrBeast"));
    assert_eq!(channel_avatar.as_deref(), Some("https://yt3/avatar.jpg"));
    assert_eq!(views.as_deref(), Some("37M views"));
    assert_eq!(published.as_deref(), Some("1 day ago"));
    assert_eq!(duration.as_deref(), Some("32:08"));
}

#[test]
fn lockup_video_reads_the_an_webp_moving_thumbnail() {
    // The animated WebP url is signed (`du`/`sqp`/`rs`), so it is read from the response
    // (IT-027). It sits next to the duration badge in the same `overlays` array.
    let lvm = json!({
        "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
        "contentId": "B4EPW7JUMTM",
        "contentImage": { "thumbnailViewModel": { "overlays": [
            { "thumbnailBottomOverlayViewModel": { "badges": [
                { "thumbnailBadgeViewModel": { "text": "13:06" } }
            ]}},
            { "animatedThumbnailOverlayViewModel": { "thumbnail": { "sources": [
                { "url": "https://i.ytimg.com/an_webp/B4EPW7JUMTM/mqdefault_6s.webp?du=3000&sqp=CND7y9QG&rs=AOn4CLAfWe0wx-bhiISprVbkpGeeFWD_Dw",
                  "width": 320, "height": 180 }
            ]}}}
        ]}},
        "metadata": { "lockupMetadataViewModel": { "title": { "content": "Some Video" } } }
    });

    let SearchItem::Video { moving_thumbnail, duration, .. } =
        lockup_video(&lvm).expect("a video lockup")
    else {
        panic!("lockup_video must return a Video");
    };

    assert_eq!(
        moving_thumbnail.as_deref(),
        Some("https://i.ytimg.com/an_webp/B4EPW7JUMTM/mqdefault_6s.webp?du=3000&sqp=CND7y9QG&rs=AOn4CLAfWe0wx-bhiISprVbkpGeeFWD_Dw"),
        "the signed url survives verbatim"
    );
    assert_eq!(duration.as_deref(), Some("13:06"));
}

/// A watch-page sidebar lockup: unlinked channel name, channel id on the avatar, bare view count.
#[test]
fn lockup_video_reads_an_unlinked_channel_and_a_bare_view_count() {
    let lvm = json!({
        "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
        "contentId": "side1",
        "metadata": { "lockupMetadataViewModel": {
            "title": { "content": "Sidebar Video" },
            "image": { "decoratedAvatarViewModel": {
                "avatar": { "avatarViewModel": { "image": { "sources": [{ "url": "https://yt3/p.jpg" }] } } },
                "rendererContext": { "commandContext": { "onTap": { "innertubeCommand": {
                    "browseEndpoint": { "browseId": "UCpezle" }
                }}}}
            }},
            "metadata": { "contentMetadataViewModel": { "metadataRows": [
                { "metadataParts": [ { "text": { "content": "Pezle" } } ] },
                { "metadataParts": [
                    { "text": { "content": "5.7M" }, "accessibilityLabel": "5.7 million views" },
                    { "text": { "content": "3w ago" }, "accessibilityLabel": "3 weeks ago" }
                ]}
            ]}}
        }}
    });
    let SearchItem::Video { channel, channel_id, views, published, .. } = lockup_video(&lvm).unwrap() else {
        panic!("a video")
    };
    assert_eq!(channel.as_deref(), Some("Pezle"));
    assert_eq!(channel_id.as_deref(), Some("UCpezle"));
    assert_eq!(views.as_deref(), Some("5.7M views"));
    assert_eq!(published.as_deref(), Some("3w ago"));
}

#[test]
fn lockup_video_collab_byline_falls_back_to_text_and_primary_channel() {
    // Collaboration card (scrubbed from a FEsubscriptions response): the byline is plain
    // text and the channel links sit in the avatar-stack dialog. The first-listed
    // collaborator is the primary channel.
    let collaborator = |bid: &str| json!({
        "listItemViewModel": { "rendererContext": { "commandContext": { "onTap": {
            "innertubeCommand": { "browseEndpoint": { "browseId": bid } }
        }}}}
    });
    let avatar_stack = json!({
        "avatars": [
            { "avatarViewModel": { "image": { "sources": [{ "url": "https://yt3/a.jpg" }] } } },
            { "avatarViewModel": { "image": { "sources": [{ "url": "https://yt3/b.jpg" }] } } }
        ],
        "rendererContext": { "commandContext": { "onTap": { "innertubeCommand": {
            "showDialogCommand": { "panelLoadingStrategy": { "inlineContent": {
                "dialogViewModel": { "customContent": { "listViewModel": {
                    "listItems": [ collaborator("UCprimary"), collaborator("UCsecond") ]
                }}}
            }}}
        }}}}
    });
    let lvm = json!({
        "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
        "contentId": "collab1",
        "metadata": { "lockupMetadataViewModel": {
            "title": { "content": "Collab Video" },
            "image": { "avatarStackViewModel": avatar_stack },
            "metadata": { "contentMetadataViewModel": { "metadataRows": [
                { "metadataParts": [ { "text": { "content": "Alpha and 1 more" } } ] },
                { "metadataParts": [
                    { "text": { "content": "232K views" } },
                    { "text": { "content": "23 hours ago" } }
                ]}
            ]}}
        }}
    });
    let SearchItem::Video { channel, channel_id, channel_avatar, views, published, .. } =
        lockup_video(&lvm).unwrap()
    else {
        panic!("a video")
    };
    assert_eq!(channel.as_deref(), Some("Alpha and 1 more"));
    assert_eq!(channel_id.as_deref(), Some("UCprimary"));
    assert_eq!(channel_avatar.as_deref(), Some("https://yt3/a.jpg"));
    assert_eq!(views.as_deref(), Some("232K views"));
    assert_eq!(published.as_deref(), Some("23 hours ago"));
}

#[test]
fn video_items_carry_the_preview_player_params() {
    let vr = json!({
        "videoId": "v1",
        "title": { "runs": [{ "text": "t" }] },
        "navigationEndpoint": { "watchEndpoint": { "videoId": "v1", "playerParams": "click" } },
        "inlinePlaybackEndpoint": { "watchEndpoint": { "playerParams": "YAHIAQGi%3D" } }
    });
    assert!(matches!(video_item(&vr),
        Some(SearchItem::Video { preview_player_params: Some(p), .. }) if p == "YAHIAQGi%3D"));

    let lvm = json!({
        "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
        "contentId": "v2",
        "rendererContext": { "commandContext": { "onTap": { "innertubeCommand": {
            "watchEndpoint": { "videoId": "v2", "playerParams": "click" } } } } },
        "itemPlayback": { "inlinePlayerData": { "onVisible": { "innertubeCommand": {
            "watchEndpoint": { "playerParams": "YAHIAQG6" } } } } }
    });
    assert!(matches!(lockup_video(&lvm),
        Some(SearchItem::Video { preview_player_params: Some(p), .. }) if p == "YAHIAQG6"));

    let bare = json!({ "videoId": "v3", "title": { "runs": [{ "text": "t" }] } });
    assert!(matches!(video_item(&bare), Some(SearchItem::Video { preview_player_params: None, .. })));
}

#[test]
fn detects_members_only_video_renderer() {
    // Classic videoRenderer membership gate: BADGE_STYLE_TYPE_MEMBERS_ONLY.
    let vr = json!({
        "videoId": "memOnly1",
        "title": { "runs": [{ "text": "Members Video" }] },
        "badges": [{ "metadataBadgeRenderer": { "style": "BADGE_STYLE_TYPE_MEMBERS_ONLY" } }]
    });
    assert!(matches!(
        video_item(&vr),
        Some(SearchItem::Video { members_only: true, .. })
    ));
    // A normal video is not flagged.
    let vr2 = json!({ "videoId": "x", "title": { "runs": [{ "text": "t" }] } });
    assert!(matches!(
        video_item(&vr2),
        Some(SearchItem::Video { members_only: false, .. })
    ));
}

#[test]
fn detects_members_only_lockup() {
    // Modern lockupViewModel membership gate: badgeViewModel.badgeStyle BADGE_MEMBERS_ONLY
    // sitting in a metadataRow's badges.
    let lvm = json!({
        "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
        "contentId": "memLockup",
        "metadata": { "lockupMetadataViewModel": {
            "title": { "content": "Gated" },
            "metadata": { "contentMetadataViewModel": { "metadataRows": [
                { "badges": [{ "badgeViewModel": { "badgeStyle": "BADGE_MEMBERS_ONLY" } }] }
            ]}}
        }}
    });
    assert!(matches!(
        lockup_video(&lvm),
        Some(SearchItem::Video { members_only: true, .. })
    ));
}

#[test]
fn parse_search_handles_home_feed_rich_grid_shape() {
    // The home/subscriptions feeds wrap videos in richItemRenderer (richGrid), with
    // the next-page token in a richGrid continuationItemRenderer. walk() should find
    // both, so a feed isn't silently empty.
    let v = json!({ "contents": { "twoColumnBrowseResultsRenderer": { "tabs": [
        { "tabRenderer": { "content": { "richGridRenderer": { "contents": [
            { "richItemRenderer": { "content": { "videoRenderer": {
                "videoId": "homeVid1",
                "title": { "runs": [{ "text": "Home Feed Video" }] },
                "ownerText": { "runs": [{
                    "text": "Some Channel",
                    "navigationEndpoint": { "browseEndpoint": { "browseId": "UCfeed" } }
                }] }
            }}}},
            { "continuationItemRenderer": { "continuationEndpoint": {
                "continuationCommand": { "token": "FEEDPAGE2" }
            }}}
        ]}}}}
    ]}}});
    let r = parse_search(&v);
    assert!(matches!(&r.items[0], SearchItem::Video { id, channel_id, .. }
        if id == "homeVid1" && channel_id.as_deref() == Some("UCfeed")));
    assert_eq!(r.continuation.as_deref(), Some("FEEDPAGE2"));
}

#[test]
fn parse_search_groups_a_home_shorts_shelf() {
    let short = |id: &str| json!({ "richItemRenderer": { "content": { "shortsLockupViewModel": {
        "onTap": { "innertubeCommand": { "reelWatchEndpoint": { "videoId": id } } },
        "overlayMetadata": { "primaryText": { "content": format!("Short {id}") } }
    }}}});
    let video = |id: &str| json!({ "richItemRenderer": { "content": { "videoRenderer": {
        "videoId": id, "title": { "runs": [{ "text": id }] }
    }}}});
    let v = json!({ "richGridRenderer": { "contents": [
        video("v1"),
        { "richSectionRenderer": { "content": { "richShelfRenderer": {
            "title": { "runs": [{ "text": "Shorts" }] },
            "contents": [short("s1"), short("s2")]
        }}}},
        video("v2"),
    ]}});
    let r = parse_search(&v);
    assert_eq!(r.items.len(), 3);
    assert!(matches!(&r.items[0], SearchItem::Video { id, .. } if id == "v1"));
    let SearchItem::ShortsShelf { title, items } = &r.items[1] else { panic!("{:?}", r.items[1]) };
    assert_eq!(title, "Shorts");
    let ids: Vec<_> = items
        .iter()
        .map(|i| match i {
            SearchItem::Short { reel, .. } => reel.video_id.as_str(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(ids, ["s1", "s2"]);
    assert!(matches!(&r.items[2], SearchItem::Video { id, .. } if id == "v2"));
}

#[test]
fn parse_live_chat_messages_and_continuation() {
    let v = json!({ "continuationContents": { "liveChatContinuation": {
        "actions": [
            { "addChatItemAction": { "item": { "liveChatTextMessageRenderer": {
                "id": "MSG1",
                "authorName": { "simpleText": "Alice" },
                "authorExternalChannelId": "UCalice",
                "authorPhoto": { "thumbnails": [{ "url": "p32" }, { "url": "p64" }] },
                "authorBadges": [{ "liveChatAuthorBadgeRenderer": { "icon": { "iconType": "MODERATOR" } } }],
                "timestampUsec": "1700000000000000",
                "message": { "runs": [
                    { "text": "hi " },
                    { "emoji": { "isCustomEmoji": true, "shortcuts": [":wave:"],
                                 "image": { "thumbnails": [{ "url": "e24" }, { "url": "e48" }] } } }
                ] }
            }}}},
            { "addChatItemAction": { "item": { "liveChatPlaceholderItemRenderer": {} } } }
        ],
        "continuations": [{ "invalidationContinuationData": { "continuation": "CHAT_NEXT", "timeoutMs": 5000 } }]
    }}});
    let lc = parse_live_chat(&v);
    assert_eq!(lc.messages.len(), 1, "non-text items skipped");
    let m = &lc.messages[0];
    assert_eq!(m.author, "Alice");
    assert_eq!(m.author_photo_url.as_deref(), Some("p64"), "largest author photo");
    assert_eq!(m.badges, vec![ChatBadge::Moderator]);
    assert_eq!(m.timestamp_usec, 1_700_000_000_000_000);
    assert_eq!(
        m.runs,
        vec![
            ChatRun::Text("hi ".into()),
            ChatRun::Emoji { url: "e48".into(), is_custom: true, shortcut: ":wave:".into() },
        ]
    );
    assert_eq!(m.text(), "hi :wave:", "flattened body");
    assert_eq!(lc.continuation.as_deref(), Some("CHAT_NEXT"));
    assert_eq!(lc.poll_ms, Some(5000));
}

#[test]
fn find_live_chat_continuation_from_next() {
    let next = json!({ "contents": { "twoColumnWatchNextResults": { "conversationBar": {
        "liveChatRenderer": { "continuations": [{ "reloadContinuationData": { "continuation": "LC0" } }] }
    }}}});
    assert_eq!(find_live_chat_continuation(&next).as_deref(), Some("LC0"));
}

#[test]
fn parse_caption_tracks_and_timedtext() {
    let player = json!({ "captions": { "playerCaptionsTracklistRenderer": { "captionTracks": [
        { "languageCode": "en", "name": { "simpleText": "English" }, "kind": "asr", "baseUrl": "https://x/timedtext?v=1" }
    ]}}});
    let tracks = parse_caption_tracks(&player);
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].language_code, "en");
    assert!(tracks[0].auto);

    let tt = json!({ "events": [
        { "tStartMs": 0, "dDurationMs": 1500, "segs": [{ "utf8": "Hello " }, { "utf8": "world" }] },
        { "tStartMs": 1500, "segs": [{ "utf8": "\n" }] }
    ]});
    let cues = parse_timedtext_json3(&tt);
    assert_eq!(cues.len(), 1, "blank/whitespace cues are dropped");
    assert_eq!(cues[0].text, "Hello world");
    assert_eq!(cues[0].start_ms, 0);
}

#[test]
fn parse_channel_extracts_header_tabs_and_videos() {
    let v = json!({
        "metadata": { "channelMetadataRenderer": {
            "title": "Linus Tech Tips",
            "externalId": "UCXuqSBlHAE6Xw-yeJA0Tunw",
            "description": "Tech reviews"
        }},
        "header": { "pageHeaderRenderer": { "content": { "pageHeaderViewModel": { "metadata": {
            "contentMetadataViewModel": { "metadataRows": [
                { "metadataParts": [{ "text": { "content": "@LinusTechTips" } }] },
                { "metadataParts": [
                    { "text": { "content": "16.5M subscribers" } },
                    { "text": { "content": "7K videos" } }
                ]}
            ]}
        }}}}},
        "contents": { "twoColumnBrowseResultsRenderer": { "tabs": [
            { "tabRenderer": { "title": "Home" } },
            { "tabRenderer": { "title": "Videos", "content": {
                "richGridRenderer": { "contents": [
                    { "richItemRenderer": { "content": { "lockupViewModel": {
                        "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
                        "contentId": "abc123",
                        "metadata": { "lockupMetadataViewModel": { "title": { "content": "A Video" } } }
                    }}}}
                ]}
            }}}
        ]}}
    });
    let c = parse_channel(&v);
    assert_eq!(c.name.as_deref(), Some("Linus Tech Tips"));
    assert_eq!(c.id.as_deref(), Some("UCXuqSBlHAE6Xw-yeJA0Tunw"));
    assert_eq!(c.handle.as_deref(), Some("@LinusTechTips"));
    assert_eq!(c.subscribers.as_deref(), Some("16.5M subscribers"));
    assert_eq!(c.tabs, vec!["Home", "Videos"]);
    assert_eq!(c.videos.len(), 1);
    assert!(matches!(&c.videos[0], SearchItem::Video { id, .. } if id == "abc123"));
}

#[test]
fn parse_search_reads_the_filter_dialog_of_a_filtered_response() {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/search_filters_videos_selected.json")).unwrap();
    let r = parse_search(&v);
    let titles: Vec<&str> = r.filters.iter().map(|g| g.title.as_str()).collect();
    assert_eq!(titles, vec!["Type", "Duration", "Upload date", "Features", "Prioritize"]);
    let videos = &r.filters[0].filters[0];
    assert_eq!((videos.label.as_str(), videos.selected), ("Videos", true));
    assert_eq!(videos.params.as_deref(), Some("EgA%3D"), "a selected option clears itself");
    let today = &r.filters[2].filters[0];
    assert_eq!(today.params.as_deref(), Some("EgQIAhAB"), "Today combined with Videos");
    let relevance = &r.filters[4].filters[0];
    assert!(relevance.selected && !relevance.clickable, "the selected default has no endpoint");
}

#[test]
fn parse_search_keeps_a_selected_upload_date_clickable_to_remove_it() {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/search_filters_week_selected.json")).unwrap();
    let r = parse_search(&v);
    let week = &r.filters[2].filters[1];
    assert_eq!((week.label.as_str(), week.selected, week.clickable), ("This week", true, true));
    assert_eq!(week.params, None, "removing it is a search with no filter");
    assert_eq!(week.tooltip.as_deref(), Some("Remove This week filter"));
    let channels = &r.filters[0].filters[2];
    assert!(channels.disabled && !channels.clickable, "Channels does not combine with This week");
}

#[test]
fn parse_search_reads_the_chips_of_a_chip_reload() {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/search_chips_videos_selected.json")).unwrap();
    let r = parse_search(&v);
    let labels: Vec<&str> = r.chips.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["All", "Shorts", "Unwatched", "Videos", "Recently uploaded", "Live"]);
    assert!(r.chips[3].selected && !r.chips[0].selected, "Videos is the selected chip");
    assert_eq!(r.chips[0].token.as_deref(), Some("TOKEN_ALL"), "All returns to the unfiltered page");
    assert_eq!(r.continuation.as_deref(), Some("SCROLL_TOKEN"), "chip tokens are separate from the scroll continuation");
}

#[test]
fn parse_channel_videos_extracts_sort_chips_and_keeps_scroll_continuation() {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/channel_videos_chips.json")).unwrap();
    let r = parse_channel_videos(&v);
    let labels: Vec<&str> = r.sort_chips.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["Latest", "Popular", "Oldest"]);
    assert_eq!(r.sort_chips[0].token, "TOKEN_LATEST");
    assert!(r.sort_chips[0].selected && !r.sort_chips[1].selected, "Latest is the selected chip");
    // Chip tokens are separate from the grid scroll continuation.
    assert_eq!(
        r.continuation.as_deref(),
        Some("SCROLL_TOKEN"),
        "the scroll continuation"
    );
}

#[test]
fn parse_channel_extracts_subscribed_and_banner() {
    let v = json!({
        "header": { "pageHeaderRenderer": { "content": { "pageHeaderViewModel": {
            "banner": { "imageBannerViewModel": { "image": { "sources": [
                { "url": "https://yt3/narrow", "width": 1060 },
                { "url": "https://yt3/widest", "width": 2120 }
            ]}}}
        }}}},
        "contents": { "twoColumnBrowseResultsRenderer": { "tabs": [
            { "tabRenderer": { "content": { "sectionListRenderer": { "contents": [
                { "subscribeButtonRenderer": { "channelId": "UCx", "subscribed": true } }
            ]}}}}
        ]}}
    });
    let c = parse_channel(&v);
    assert!(c.subscribed, "reads subscribeButtonRenderer.subscribed");
    assert_eq!(c.banner_url.as_deref(), Some("https://yt3/widest"), "picks the widest banner source");
}

#[test]
fn parse_channel_reads_video_count_verified_and_header_link() {
    let v = json!({ "header": { "pageHeaderRenderer": { "content": { "pageHeaderViewModel": {
        "title": { "dynamicTextViewModel": { "text": {
            "content": "WhistlinDiesel 1000",
            "attachmentRuns": [{ "element": { "type": { "imageType": { "image": { "sources": [
                { "clientResource": { "imageName": "CHECK_CIRCLE_FILLED" } }
            ]}}}}}]
        }}},
        "metadata": { "contentMetadataViewModel": { "metadataRows": [
            { "metadataParts": [{ "text": { "content": "@whistlindiesel1000" } }] },
            { "metadataParts": [
                { "text": { "content": "1.67M subscribers" } },
                { "text": { "content": "64 videos" } }
            ]}
        ]}},
        "attribution": { "attributionViewModel": { "text": {
            "content": " Instagram",
            "commandRuns": [{ "onTap": { "innertubeCommand": { "urlEndpoint": {
                "url": "https://www.youtube.com/redirect?event=channel_header&redir_token=T&q=https%3A%2F%2Fwww.instagram.com%2Fwhistlindiesel"
            }}}}]
        }}}
    }}}}});
    let c = parse_channel(&v);
    assert_eq!(c.video_count.as_deref(), Some("64 videos"));
    assert!(c.verified);
    assert_eq!(
        c.link,
        Some(("Instagram".to_string(), "https://www.instagram.com/whistlindiesel".to_string()))
    );
}

#[test]
fn parse_channel_takes_the_avatar_from_channel_metadata_not_a_video_lockup() {
    // In a real browse response most `decoratedAvatarViewModel` entries are per-video
    // lockup avatars. Only `metadata.channelMetadataRenderer.avatar` is the channel's own.
    let v = json!({
        "metadata": { "channelMetadataRenderer": {
            "title": "MrBeast",
            "avatar": { "thumbnails": [
                { "url": "https://yt3/small", "width": 88 },
                { "url": "https://yt3/largest", "width": 900 }
            ]}
        }},
        "contents": { "twoColumnBrowseResultsRenderer": { "tabs": [
            { "tabRenderer": { "content": { "sectionListRenderer": { "contents": [
                { "lockupViewModel": { "metadata": { "lockupMetadataViewModel": {
                    "image": { "decoratedAvatarViewModel": { "avatar": {
                        "avatarViewModel": { "image": { "sources": [
                            { "url": "https://yt3/WRONG-lockup-avatar", "width": 68 }
                        ]}}
                    }}}
                }}}}
            ]}}}}
        ]}}
    });
    let c = parse_channel(&v);
    assert_eq!(
        c.avatar_url.as_deref(),
        Some("https://yt3/largest"),
        "the widest channelMetadataRenderer avatar"
    );
}

#[test]
fn parse_comments_extracts_replies_token() {
    // Verified structure: a commentEntityPayload (text/author) + a commentThreadRenderer
    // carrying the same commentId and a replies continuation. Join by commentId.
    let v = json!({
        "frameworkUpdates": { "entityBatchUpdate": { "mutations": [
            { "payload": { "commentEntityPayload": {
                "properties": { "commentId": "C1", "content": { "content": "hi" } },
                "author": { "displayName": "A" }, "toolbar": {}
            }}}
        ]}},
        "onResponseReceivedEndpoints": [ { "appendContinuationItemsAction": { "continuationItems": [
            { "commentThreadRenderer": {
                "commentViewModel": { "commentViewModel": { "commentId": "C1" } },
                "replies": { "commentRepliesRenderer": { "contents": [
                    { "continuationItemRenderer": { "continuationEndpoint": {
                        "continuationCommand": { "token": "REPLIES_C1" } } } }
                ]}}
            }}
        ]}}]
    });
    let c = parse_comments(&v);
    let first = c.items.iter().find(|x| x.id == "C1").expect("comment C1 parsed");
    assert_eq!(first.replies_token.as_deref(), Some("REPLIES_C1"), "replies token joined by commentId");
}

#[test]
fn parse_comments_extracts_like_token() {
    // "CAUaAkMx" = base64 of the protobuf {1: 5 (type=like), 3: "C1" (commentId)}.
    let v = json!({
        "frameworkUpdates": { "entityBatchUpdate": { "mutations": [
            { "payload": { "commentEntityPayload": {
                "properties": { "commentId": "C1", "content": { "content": "hi" } },
                "author": { "displayName": "A" }, "toolbar": {}
            }}}
        ]}},
        "actions": [ { "performCommentActionEndpoint": { "action": "CAUaAkMx" } } ]
    });
    let c = parse_comments(&v);
    let first = c.items.iter().find(|x| x.id == "C1").expect("comment C1 parsed");
    assert_eq!(first.like_token.as_deref(), Some("CAUaAkMx"), "type-5 like action joined by id");
}

#[test]
fn parse_comments_joins_the_unlike_action_from_the_toolbar() {
    let v = json!({ "frameworkUpdates": { "entityBatchUpdate": { "mutations": [
        { "payload": { "commentEntityPayload": {
            "properties": { "commentId": "C1", "content": { "content": "hi" } },
            "author": { "displayName": "@a" }
        }}},
        { "payload": { "engagementToolbarSurfaceEntityPayload": {
            "key": "k",
            "unlikeCommand": { "innertubeCommand": { "performCommentActionEndpoint": { "action": "CAYaAkMx" } } }
        }}}
    ]}}});
    assert_eq!(parse_comments(&v).items[0].unlike_token.as_deref(), Some("CAYaAkMx"));
}

#[test]
fn parse_playlists_reads_lockup_playlists_only() {
    let v = json!({ "contents": [
        { "lockupViewModel": {
            "contentType": "LOCKUP_CONTENT_TYPE_PLAYLIST",
            "contentId": "LL",
            "metadata": { "lockupMetadataViewModel": { "title": { "content": "Liked videos" } } }
        }},
        { "lockupViewModel": { "contentType": "LOCKUP_CONTENT_TYPE_VIDEO", "contentId": "vid1" } }
    ]});
    let pls = parse_playlists(&v);
    assert_eq!(pls.len(), 1, "videos must not be counted as playlists");
    assert_eq!(pls[0], Playlist { id: "LL".into(), title: "Liked videos".into(), thumbnail: None });
}

#[test]
fn parse_comments_reads_entity_payloads_and_next_token() {
    let v = json!({
        "frameworkUpdates": { "entityBatchUpdate": { "mutations": [
            { "payload": { "commentEntityPayload": {
                "properties": {
                    "commentId": "UgwABC",
                    "content": { "content": "great video!" },
                    "publishedTime": "2 years ago"
                },
                "author": { "displayName": "@someone", "channelId": "UCxyz", "isCreator": false, "isVerified": true },
                "toolbar": { "likeCountNotliked": "1.5K", "replyCount": "44" }
            }}}
        ]}},
        "onResponseReceivedEndpoints": [{ "appendContinuationItemsAction": { "continuationItems": [
            { "continuationItemRenderer": { "continuationEndpoint": {
                "continuationCommand": { "token": "COMMENTS_PAGE2" }
            }}}
        ]}}]
    });
    let c = parse_comments(&v);
    assert_eq!(c.items.len(), 1);
    let cm = &c.items[0];
    assert_eq!(cm.id, "UgwABC");
    assert_eq!(cm.text, "great video!");
    assert_eq!(cm.author, "@someone");
    assert_eq!(cm.like_count.as_deref(), Some("1.5K"));
    assert_eq!(cm.reply_count.as_deref(), Some("44"));
    assert!(cm.is_verified && !cm.is_creator);
    assert_eq!(c.continuation.as_deref(), Some("COMMENTS_PAGE2"));
    assert_eq!(cm.reply_level, 0);
}

#[test]
fn parse_comments_reads_the_reply_level_of_threaded_replies() {
    let reply = |id: &str, level: u64| json!({ "payload": { "commentEntityPayload": {
        "properties": { "commentId": id, "content": { "content": id }, "replyLevel": level },
        "author": { "displayName": "@a" }
    }}});
    let v = json!({ "frameworkUpdates": { "entityBatchUpdate": { "mutations": [
        reply("r1", 1), reply("r1a", 2), reply("r2", 1)
    ]}}});
    let levels: Vec<_> = parse_comments(&v).items.iter().map(|c| (c.id.clone(), c.reply_level)).collect();
    assert_eq!(levels, [("r1".into(), 1), ("r1a".into(), 2), ("r2".into(), 1)]);
}

#[test]
fn parse_comments_joins_reply_params_from_toolbar_surface_entity() {
    // An authenticated response carries createReplyParams in an
    // engagementToolbarSurfaceEntityPayload mutation whose `key` is base64 of a proto
    // embedding "<commentId>/<n>".
    use base64::Engine as _;
    let key = base64::engine::general_purpose::URL_SAFE
        .encode(b"\x12\x1dUgwABC/12 F(\x01");
    let v = json!({
        "frameworkUpdates": { "entityBatchUpdate": { "mutations": [
            { "payload": { "commentEntityPayload": {
                "properties": { "commentId": "UgwABC", "content": { "content": "hi" } },
                "author": { "displayName": "@a" },
                "toolbar": {}
            }}},
            { "payload": { "engagementToolbarSurfaceEntityPayload": {
                "key": key,
                "replyCommand": { "innertubeCommand": { "createCommentReplyDialogEndpoint": {
                    "dialog": { "commentReplyDialogRenderer": { "replyButton": { "buttonRenderer": {
                        "serviceEndpoint": { "createCommentReplyEndpoint": {
                            "createReplyParams": "REPLY_PARAMS_ABC"
                        }}
                    }}}}
                }}}
            }}}
        ]}}
    });
    let c = parse_comments(&v);
    assert_eq!(c.items.len(), 1);
    assert_eq!(c.items[0].reply_params.as_deref(), Some("REPLY_PARAMS_ABC"));
}

#[test]
fn continuation_token_follows_show_more_replies_button() {
    // "Show more replies" carries its token under button.buttonRenderer.command.
    let v = json!({ "onResponseReceivedEndpoints": [{ "appendContinuationItemsAction": {
        "continuationItems": [{ "continuationItemRenderer": { "button": { "buttonRenderer": {
            "command": { "continuationCommand": { "token": "MORE_REPLIES" } }
        }}}}]
    }}]});
    assert_eq!(parse_comments(&v).continuation.as_deref(), Some("MORE_REPLIES"));
}

/// The like count sits on the segmented like button when likeCountEntity is a placeholder.
#[test]
fn parse_next_reads_the_like_count_from_the_like_button() {
    let v = json!({ "segmentedLikeDislikeButtonViewModel": { "likeButtonViewModel": { "likeButtonViewModel": {
        "likeCountEntity": { "key": "unset_like_count_entity_key" },
        "toggleButtonViewModel": { "toggleButtonViewModel": { "defaultButtonViewModel": { "buttonViewModel": {
            "title": "36K",
            "accessibilityText": "like this video along with 36,490 other people"
        }}}}
    }}}});
    assert_eq!(parse_next(&v).like_count.as_deref(), Some("36,490"));
}

#[test]
fn parse_next_extracts_title_channel_and_comments_token() {
    let v = json!({ "contents": { "twoColumnWatchNextResults": {
        "results": { "results": { "contents": [
            { "videoPrimaryInfoRenderer": {
                "title": { "runs": [{ "text": "My Video" }] },
                "viewCount": { "videoViewCountRenderer": { "viewCount": { "simpleText": "1,234 views" } } }
            }},
            { "videoSecondaryInfoRenderer": {
                "owner": { "videoOwnerRenderer": { "title": { "runs": [{ "text": "Some Channel" }] } } }
            }},
            { "itemSectionRenderer": {
                "sectionIdentifier": "comment-item-section",
                "contents": [{ "continuationItemRenderer": { "continuationEndpoint": {
                    "continuationCommand": { "token": "COMMENTS_TOKEN" }
                }}}]
            }}
        ]}},
        "secondaryResults": { "secondaryResults": { "results": [
            { "compactVideoRenderer": {
                "videoId": "relatedXYZ",
                "title": { "simpleText": "Related Video" },
                "longBylineText": { "runs": [{ "text": "Other Channel" }] }
            }}
        ]}}
    }}});
    let d = parse_next(&v);
    assert_eq!(d.title.as_deref(), Some("My Video"));
    assert_eq!(d.channel.as_deref(), Some("Some Channel"));
    assert_eq!(d.view_count.as_deref(), Some("1,234 views"));
    assert_eq!(d.comments_continuation.as_deref(), Some("COMMENTS_TOKEN"));
    assert_eq!(d.related.len(), 1);
    assert!(matches!(&d.related[0], SearchItem::Video { id, .. } if id == "relatedXYZ"));
}

#[test]
fn video_details_parses_owner_and_description() {
    // Owner fields live under owner.videoOwnerRenderer; subscribeButton is its sibling.
    let secondary = json!({
        "owner": { "videoOwnerRenderer": {
            "title": { "runs": [{ "text": "Some Channel" }] },
            "navigationEndpoint": { "browseEndpoint": { "browseId": "UCabc" } },
            "thumbnail": { "thumbnails": [{ "url": "https://yt3/a.jpg" }] },
            "subscriberCountText": { "simpleText": "4.51M subscribers" }
        }},
        "subscribeButton": { "subscribeButtonRenderer": { "subscribed": true, "channelId": "UCabc" } },
        "attributedDescription": { "content": "hello desc" }
    });
    // The like count arrives through frameworkUpdates.entityBatchUpdate.
    let v = json!({ "contents": { "twoColumnWatchNextResults": {
        "results": { "results": { "contents": [
            { "videoPrimaryInfoRenderer": {
                "title": { "runs": [{ "text": "My Video" }] },
                "viewCount": { "videoViewCountRenderer": { "viewCount": { "simpleText": "1,234 views" } } },
                "relativeDateText": { "simpleText": "13 years ago" }
            }},
            { "videoSecondaryInfoRenderer": secondary },
            { "itemSectionRenderer": {
                "sectionIdentifier": "comment-item-section",
                "contents": [{ "continuationItemRenderer": { "continuationEndpoint": {
                    "continuationCommand": { "token": "COMMENTS_TOKEN" }
                }}}]
            }}
        ]}},
        "secondaryResults": { "secondaryResults": { "results": [] } }
    }},
    "frameworkUpdates": { "entityBatchUpdate": { "mutations": [
        { "entityKey": "some_key", "payload": { "likeCountEntity": {
            "key": "some_key",
            "expandedLikeCountIfLiked": { "content": "19,214,590" }
        }}}
    ]}}});
    let d = parse_next(&v);
    assert_eq!(d.channel_id.as_deref(), Some("UCabc"));
    assert_eq!(d.channel_avatar.as_deref(), Some("https://yt3/a.jpg"));
    assert_eq!(d.subscriber_count.as_deref(), Some("4.51M subscribers"));
    assert_eq!(d.published.as_deref(), Some("13 years ago"));
    assert_eq!(d.description.as_deref(), Some("hello desc"));
    assert_eq!(d.like_count.as_deref(), Some("19,214,590"));
    assert!(d.subscribed);
}

#[test]
fn watch_page_related_carries_the_an_webp_moving_thumbnail() {
    // Only the watch page carries the animated-preview urls. The sidebar sits inside an
    // `itemSectionRenderer`, as on a real page.
    let html = concat!(
        r#"<script>var ytInitialData = "#,
        r#"{"contents": {"twoColumnWatchNextResults": {"secondaryResults": {"secondaryResults": {"results": [{"itemSectionRenderer": {"contents": [{"lockupViewModel": {"contentType": "LOCKUP_CONTENT_TYPE_VIDEO", "contentId": "B4EPW7JUMTM", "contentImage": {"thumbnailViewModel": {"overlays": [{"animatedThumbnailOverlayViewModel": {"thumbnail": {"sources": [{"url": "https://i.ytimg.com/an_webp/B4EPW7JUMTM/mqdefault_6s.webp?du=3000&sqp=X&rs=Y", "width": 320}]}}}]}}, "metadata": {"lockupMetadataViewModel": {"title": {"content": "Sidebar Video"}}}}}]}}]}}}}}"#,
        "</script>",
    );

    let d = video_details_from_watch_html(html).expect("the page embeds ytInitialData");
    let SearchItem::Video { id, moving_thumbnail, .. } =
        d.related.first().expect("a related video")
    else {
        panic!("related[0] must be a video");
    };
    assert_eq!(id, "B4EPW7JUMTM");
    assert_eq!(
        moving_thumbnail.as_deref(),
        Some("https://i.ytimg.com/an_webp/B4EPW7JUMTM/mqdefault_6s.webp?du=3000&sqp=X&rs=Y"),
    );
}

