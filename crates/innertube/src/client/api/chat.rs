//! Live chat and chat replay: messages, badges, send results.

use super::*;

/// A segment of a chat message body: plain text, a link, or an emoji image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatRun {
    Text(String),
    /// A run with a `navigationEndpoint` (mention / link). `url` best-effort.
    Link { text: String, url: String },
    /// `emoji.image.thumbnails` URL; `is_custom` distinguishes channel emoji from
    /// unicode (YouTube serves both as images). `shortcut` e.g. `:wave:`.
    Emoji { url: String, is_custom: bool, shortcut: String },
}

/// An author badge next to the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatBadge {
    Moderator,
    Owner,
    Verified,
    /// Channel membership badge (per-channel image) + label (e.g. "Member (6 months)").
    Member { image_url: String, label: String },
}

/// The kind of chat item (plain message vs paid/membership), with the extra fields
/// those carry. Colors are `#AARRGGBB`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ChatKind {
    #[default]
    Text,
    SuperChat { amount: String, body_color: String, header_color: String },
    Sticker { amount: String, image_url: String },
    /// Membership join/milestone; `header` is the welcome/"member for N months" line.
    Membership { header: String },
}

/// A single live-chat message, rich enough to render like the web client.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChatMessage {
    /// Client dedup key (`id` on the renderer).
    pub id: String,
    pub author: String,
    pub author_channel_id: Option<String>,
    pub author_photo_url: Option<String>,
    pub badges: Vec<ChatBadge>,
    /// Message body as runs (text + emoji images). Empty for some superchats/memberships.
    pub runs: Vec<ChatRun>,
    /// Absolute send time in microseconds (`timestampUsec`), 0 if absent.
    pub timestamp_usec: i64,
    /// Replay only: when this message appears, in ms from the VOD start
    /// (`replayChatItemAction.videoOffsetTimeMsec`). `None` for live chat. The frontend
    /// reveals each replay message when the player clock reaches this offset.
    pub video_offset_ms: Option<i64>,
    pub kind: ChatKind,
}

impl ChatMessage {
    /// Flatten the body to a plain string (emoji → their `:shortcut:`). For logging,
    /// accessibility, and non-rich consumers.
    pub fn text(&self) -> String {
        self.runs
            .iter()
            .map(|r| match r {
                ChatRun::Text(t) => t.clone(),
                ChatRun::Link { text, .. } => text.clone(),
                ChatRun::Emoji { shortcut, .. } => shortcut.clone(),
            })
            .collect()
    }
}

/// Outcome of a [`crate::Session::live_chat_send`] post: the server accepts the message or
/// rejects it (slow mode, timeout, duplicate, blocked, ...). Callers surface a reject
/// instead of echoing the message optimistically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatSendResult {
    /// Accepted; carries the echoed message item when the response returned one.
    /// Boxed to keep the enum small.
    Sent(Option<Box<ChatMessage>>),
    /// Rejected; carries the human-readable reason from the response when available.
    Rejected(String),
}

/// Outcome of probing a VOD for chat replay ([`crate::Session::live_chat_replay`]).
#[derive(Debug, Clone)]
pub enum ChatReplay {
    /// Replay chat is available: the first batch + continuation.
    Available(LiveChat),
    /// The video was streamed live but chat replay isn't available (chat was off /
    /// not retained / members-only). Carries YouTube's own message, e.g.
    /// "Live chat replay is not available for this video."
    Unavailable(Option<String>),
    /// No live chat: an ordinary VOD that was never streamed live.
    NotLive,
}

/// YouTube's "replay unavailable" notice for a was-live VOD, from
/// `conversationBar.conversationBarRenderer.availabilityMessage` (present instead of a
/// `liveChatRenderer` when chat replay exists-but-isn't-available).
pub(crate) fn find_replay_unavailable_message(next: &Value) -> Option<String> {
    let m = &next["contents"]["twoColumnWatchNextResults"]["conversationBar"]
        ["conversationBarRenderer"]["availabilityMessage"]["messageRenderer"]["text"];
    renderer_text(m)
}

/// A page of live chat: messages + the token/poll-interval to fetch the next page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveChat {
    pub messages: Vec<ChatMessage>,
    /// Token for the next [`crate::Session::live_chat_page`] poll.
    pub continuation: Option<String>,
    /// Server-suggested delay (ms) before polling again.
    pub poll_ms: Option<i64>,
}

/// The initial live-chat continuation from a live video's `next` response
/// (`conversationBar.liveChatRenderer`).
pub fn find_live_chat_continuation(next: &Value) -> Option<String> {
    let lcr = &next["contents"]["twoColumnWatchNextResults"]["conversationBar"]["liveChatRenderer"];
    lcr["continuations"][0]["reloadContinuationData"]["continuation"]
        .as_str()
        .map(str::to_string)
        .or_else(|| find_str(lcr, "continuation"))
}

/// The `(params, clientIdPrefix)` needed to send a chat message. Present only in an
/// authenticated chat with open participation (absent for subscribers/members-only).
pub fn find_live_chat_send(chat: &Value) -> Option<(String, String)> {
    let mut found = None;
    walk(chat, &mut |node| {
        if found.is_none() {
            let ep = &node["sendLiveChatMessageEndpoint"];
            if let Some(params) = ep["params"].as_str() {
                found = Some((params.to_string(), ep["clientIdPrefix"].as_str().unwrap_or("").to_string()));
            }
        }
    });
    found
}

/// Largest (last) thumbnail URL from a `thumbnails` array.
fn pick_thumb(thumbs: &Value) -> Option<String> {
    thumbs.as_array()?.last()?["url"].as_str().map(str::to_string)
}

/// Decimal ARGB int → `#AARRGGBB` (YouTube's `colorFromDecimal`).
fn color_hex(n: i64) -> String {
    format!("#{:08X}", n as u32)
}

/// A message body's `runs` (text + emoji images) → typed [`ChatRun`]s. Falls back to
/// `simpleText`. Each run is either `{text, navigationEndpoint?}` or `{emoji}`.
fn chat_runs(message: &Value) -> Vec<ChatRun> {
    let mut out = Vec::new();
    if let Some(runs) = message["runs"].as_array() {
        for r in runs {
            let emoji = &r["emoji"];
            if emoji.is_object() {
                let url = pick_thumb(&emoji["image"]["thumbnails"]).unwrap_or_default();
                let is_custom = emoji["isCustomEmoji"].as_bool().unwrap_or(false);
                let shortcut = emoji["shortcuts"][0]
                    .as_str()
                    .or_else(|| emoji["emojiId"].as_str())
                    .unwrap_or_default()
                    .to_string();
                if !url.is_empty() {
                    out.push(ChatRun::Emoji { url, is_custom, shortcut });
                } else if !shortcut.is_empty() {
                    out.push(ChatRun::Text(shortcut));
                }
            } else if let Some(t) = r["text"].as_str() {
                let nav = &r["navigationEndpoint"];
                let url = nav["commandMetadata"]["webCommandMetadata"]["url"]
                    .as_str()
                    .or_else(|| nav["urlEndpoint"]["url"].as_str());
                match url {
                    Some(u) if nav.is_object() => {
                        out.push(ChatRun::Link { text: t.to_string(), url: u.to_string() })
                    }
                    _ => out.push(ChatRun::Text(t.to_string())),
                }
            }
        }
    } else if let Some(s) = message["simpleText"].as_str() {
        out.push(ChatRun::Text(s.to_string()));
    }
    out
}

/// `authorBadges[]` → typed badges (mod/owner/verified icon, or a member image badge).
fn chat_badges(renderer: &Value) -> Vec<ChatBadge> {
    let mut out = Vec::new();
    let Some(badges) = renderer["authorBadges"].as_array() else { return out };
    for b in badges {
        let br = &b["liveChatAuthorBadgeRenderer"];
        if let Some(icon) = br["icon"]["iconType"].as_str() {
            match icon {
                "MODERATOR" => out.push(ChatBadge::Moderator),
                "OWNER" => out.push(ChatBadge::Owner),
                "VERIFIED" => out.push(ChatBadge::Verified),
                _ => {}
            }
        } else if let Some(image_url) = pick_thumb(&br["customThumbnail"]["thumbnails"]) {
            let label = br["tooltip"]
                .as_str()
                .or_else(|| br["accessibility"]["accessibilityData"]["label"].as_str())
                .unwrap_or("Member")
                .to_string();
            out.push(ChatBadge::Member { image_url, label });
        }
    }
    out
}

/// One `addChatItemAction.item` → a [`ChatMessage`] (text / superchat / sticker /
/// membership). `None` for renderers we don't surface (e.g. viewer-engagement, polls).
fn parse_chat_item(item: &Value) -> Option<ChatMessage> {
    let (r, kind) = if item["liveChatTextMessageRenderer"].is_object() {
        (&item["liveChatTextMessageRenderer"], ChatKind::Text)
    } else if item["liveChatPaidMessageRenderer"].is_object() {
        let r = &item["liveChatPaidMessageRenderer"];
        let kind = ChatKind::SuperChat {
            amount: r["purchaseAmountText"]["simpleText"].as_str().unwrap_or_default().to_string(),
            body_color: r["bodyBackgroundColor"].as_i64().map(color_hex).unwrap_or_default(),
            header_color: r["headerBackgroundColor"].as_i64().map(color_hex).unwrap_or_default(),
        };
        (r, kind)
    } else if item["liveChatPaidStickerRenderer"].is_object() {
        let r = &item["liveChatPaidStickerRenderer"];
        let kind = ChatKind::Sticker {
            amount: r["purchaseAmountText"]["simpleText"].as_str().unwrap_or_default().to_string(),
            image_url: pick_thumb(&r["sticker"]["thumbnails"]).unwrap_or_default(),
        };
        (r, kind)
    } else if item["liveChatMembershipItemRenderer"].is_object() {
        let r = &item["liveChatMembershipItemRenderer"];
        let header: String = chat_runs(&r["headerSubtext"])
            .iter()
            .chain(chat_runs(&r["headerPrimaryText"]).iter())
            .map(|x| match x {
                ChatRun::Text(t) | ChatRun::Link { text: t, .. } => t.clone(),
                ChatRun::Emoji { shortcut, .. } => shortcut.clone(),
            })
            .collect();
        (r, ChatKind::Membership { header })
    } else {
        return None;
    };
    Some(ChatMessage {
        id: r["id"].as_str().unwrap_or_default().to_string(),
        author: r["authorName"]["simpleText"].as_str().unwrap_or_default().to_string(),
        author_channel_id: r["authorExternalChannelId"].as_str().map(str::to_string),
        author_photo_url: pick_thumb(&r["authorPhoto"]["thumbnails"]),
        badges: chat_badges(r),
        runs: chat_runs(&r["message"]),
        timestamp_usec: r["timestampUsec"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0),
        video_offset_ms: None, // set by parse_live_chat for replay actions
        kind,
    })
}

/// Parse a `live_chat/get_live_chat` (or `_replay`) response into messages + next token.
pub(crate) fn parse_live_chat(v: &Value) -> LiveChat {
    let lcc = &v["continuationContents"]["liveChatContinuation"];
    let mut messages = Vec::new();
    if let Some(actions) = lcc["actions"].as_array() {
        for a in actions {
            // Live actions are addChatItemAction; replay wraps them in replayChatItemAction
            // and carries videoOffsetTimeMsec (when the message appears in the VOD).
            let (item, offset) = if a["addChatItemAction"].is_object() {
                (&a["addChatItemAction"]["item"], None)
            } else {
                let r = &a["replayChatItemAction"];
                let off = r["videoOffsetTimeMsec"].as_str().and_then(|s| s.parse::<i64>().ok());
                (&r["actions"][0]["addChatItemAction"]["item"], off)
            };
            if let Some(mut msg) = parse_chat_item(item) {
                msg.video_offset_ms = offset;
                // Keep empty-body paid/membership items (they carry amount/header); drop
                // only empty plain text.
                if !matches!(msg.kind, ChatKind::Text) || !msg.runs.is_empty() {
                    messages.push(msg);
                }
            }
        }
    }
    // A replay response carries several continuations. Prefer playerSeekContinuationData:
    // re-posted with an updated `playerOffsetMs`, it fetches the chat window at any
    // position. Live responses have none, so the live types follow.
    let conts = lcc["continuations"].as_array();
    let pick = |key: &str| -> Option<String> {
        conts.and_then(|a| a.iter().find_map(|c| c[key]["continuation"].as_str())).map(str::to_string)
    };
    let token = pick("playerSeekContinuationData")
        .or_else(|| pick("invalidationContinuationData"))
        .or_else(|| pick("timedContinuationData"))
        .or_else(|| pick("liveChatReplayContinuationData"))
        .or_else(|| pick("reloadContinuationData"));
    let poll = conts.and_then(|a| {
        a.iter().find_map(|c| {
            c["invalidationContinuationData"]["timeoutMs"]
                .as_i64()
                .or_else(|| c["timedContinuationData"]["timeoutMs"].as_i64())
        })
    });
    LiveChat { messages, continuation: token, poll_ms: poll }
}

/// Parse the `live_chat/send_message` response into the posted message, for optimistic
/// local echo. The web client renders the message from this response; the poll may not
/// echo it promptly. Walks for the first `addChatItemAction.item`.
pub(crate) fn parse_chat_send_response(v: &Value) -> ChatSendResult {
    // A rejected send (slow mode, timeout, duplicate, blocked, chat closed) carries an
    // error renderer the web client toasts, with no addChatItemAction. Check it first so a
    // reject is not read as a successful echo.
    if let Some(reason) = find_chat_send_error(v) {
        return ChatSendResult::Rejected(reason);
    }
    let mut item = None;
    walk(v, &mut |node| {
        if item.is_none() {
            let it = &node["addChatItemAction"]["item"];
            if it.is_object() {
                item = parse_chat_item(it);
            }
        }
    });
    match item {
        Some(m) => ChatSendResult::Sent(Some(Box::new(m))),
        // No echoed item and no error renderer: treat as not sent. The reference client
        // always echoes the item on success.
        None => ChatSendResult::Rejected("Message wasn't sent".into()),
    }
}

/// Pull a human-readable reason from a `send_message` reject. Mirrors what the web
/// client surfaces: `liveChatErrorMessageRenderer.errorText` / `errorMessageRenderer`
/// (toasted via `yt-live-chat-add-message-to-toast`) or a top-level `errorMessage`.
fn find_chat_send_error(v: &Value) -> Option<String> {
    let mut found = None;
    walk(v, &mut |node| {
        if found.is_some() {
            return;
        }
        // The renderer a rejected send carries. `liveChatTextActionsErrorMessageRenderer`
        // is the real one observed live (slow mode / timeout); the others are defensive.
        for key in
            ["liveChatTextActionsErrorMessageRenderer", "liveChatErrorMessageRenderer", "errorMessageRenderer"]
        {
            let r = &node[key];
            if r.is_object() {
                if let Some(t) = renderer_text(&r["errorText"]).or_else(|| renderer_text(&r["text"])) {
                    found = Some(t);
                    return;
                }
            }
        }
        let em = &node["errorMessage"];
        if (em.is_object() || em.is_string()) && found.is_none() {
            found = renderer_text(em);
        }
    });
    found
}

/// Plain text from a `{simpleText}` / `{runs:[{text}]}` renderer, a bare string, or a
/// nested error renderer (`errorText` / `errorMessageRenderer` / `messageRenderer`).
fn renderer_text(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return (!s.is_empty()).then(|| s.to_string());
    }
    if let Some(s) = v["simpleText"].as_str() {
        return (!s.is_empty()).then(|| s.to_string());
    }
    if let Some(runs) = v["runs"].as_array() {
        let s: String = runs.iter().filter_map(|r| r["text"].as_str()).collect();
        if !s.is_empty() {
            return Some(s);
        }
    }
    for key in ["errorText", "errorMessageRenderer", "messageRenderer"] {
        if v[key].is_object() {
            if let Some(t) = renderer_text(&v[key]) {
                return Some(t);
            }
        }
    }
    None
}
