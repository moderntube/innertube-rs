//! Pure request-body builders for the InnerTube action/read endpoints. Kept
//! separate from [`crate::Session`] so the body shapes are unit-testable without a
//! network. Shapes verified against captures of the web client
//! (like/subscribe/set_setting/browse).

use serde_json::{json, Value};

use crate::client::ClientConfig;

/// The shared `context.client` block every WEB-client RPC carries. Minimal but
/// sufficient (the heavy `configInfo`/`adSignalsInfo`/`clickTracking` blocks the
/// browser adds are telemetry, not required for the call to succeed).
pub(crate) fn context(c: &ClientConfig) -> Value {
    json!({
        "client": {
            "clientName": c.client_name,
            "clientVersion": c.client_version,
            "hl": c.hl,
            "gl": c.gl,
            "originalUrl": "https://www.youtube.com/",
            "platform": "DESKTOP",
        }
    })
}

/// `like/like`, `like/dislike` and `like/removelike` share `{context, target:{videoId}}`.
pub(crate) fn like_body(c: &ClientConfig, video_id: &str) -> Value {
    json!({ "context": context(c), "target": { "videoId": video_id } })
}

/// `subscription/subscribe` and `subscription/unsubscribe`: `{context, channelIds:[id]}`.
pub(crate) fn subscription_body(c: &ClientConfig, channel_id: &str) -> Value {
    json!({ "context": context(c), "channelIds": [channel_id] })
}

/// `browse` (channels, feeds, the subscriptions list, …): `{context, browseId}`.
pub(crate) fn browse_body(c: &ClientConfig, browse_id: &str) -> Value {
    json!({ "context": context(c), "browseId": browse_id })
}

/// `browse` with `params`: selects a channel tab (Videos/Shorts/Live), a sort, etc.
pub(crate) fn browse_params_body(c: &ClientConfig, browse_id: &str, params: &str) -> Value {
    json!({ "context": context(c), "browseId": browse_id, "params": params })
}

/// `player/heartbeat`: the ~15 s liveness keepalive (IT-014). Echoes the server's
/// `heartbeat_server_data` with an incrementing sequence number.
pub(crate) fn heartbeat_body(c: &ClientConfig, video_id: &str, seq: u64, server_data: &str) -> Value {
    json!({
        "context": context(c),
        "videoId": video_id,
        "sequenceNumber": seq,
        "heartbeatRequestParams": { "heartbeatChecks": ["HEARTBEAT_CHECK_TYPE_LIVE_STREAM_STATUS"] },
        "heartbeatServerData": server_data,
    })
}

/// `search`: `{context, query}` plus optional `params` for sort/type filters.
pub(crate) fn search_body(c: &ClientConfig, query: &str, params: Option<&str>) -> Value {
    let mut b = json!({ "context": context(c), "query": query });
    if let Some(p) = params {
        b["params"] = json!(p);
    }
    b
}

/// `next`: watch-page metadata for a video, `{context, videoId}`.
pub(crate) fn next_body(c: &ClientConfig, video_id: &str) -> Value {
    json!({ "context": context(c), "videoId": video_id })
}

/// `navigation/resolve_url`: `{context, url}` (unauthenticated).
pub(crate) fn resolve_url_body(c: &ClientConfig, url: &str) -> Value {
    json!({ "context": context(c), "url": url })
}

/// Generic continuation request, `{context, continuation}`, posted to the
/// endpoint that issued the token (browse/search/next).
pub(crate) fn continuation_body(c: &ClientConfig, token: &str) -> Value {
    json!({ "context": context(c), "continuation": token })
}

/// `live_chat/get_live_chat_replay`: a continuation plus the player clock, so the
/// server returns the chat batch around `player_offset_ms` (a string, like the web
/// client). Used for synced chat replay of a VOD that was live.
pub(crate) fn replay_continuation_body(c: &ClientConfig, token: &str, player_offset_ms: i64) -> Value {
    json!({
        "context": context(c),
        "continuation": token,
        "currentPlayerState": { "playerOffsetMs": player_offset_ms.max(0).to_string() }
    })
}

/// `playlist/create`: `{context, title, videoIds?, privacyStatus}`.
pub(crate) fn playlist_create_body(
    c: &ClientConfig,
    title: &str,
    video_ids: &[&str],
    privacy: &str,
) -> Value {
    let mut b = json!({ "context": context(c), "title": title, "privacyStatus": privacy });
    if !video_ids.is_empty() {
        b["videoIds"] = json!(video_ids);
    }
    b
}

/// `playlist/delete`: `{context, playlistId}`.
pub(crate) fn playlist_delete_body(c: &ClientConfig, playlist_id: &str) -> Value {
    json!({ "context": context(c), "playlistId": playlist_id })
}

/// `browse/edit_playlist` with the given `actions` array (add/remove/move).
pub(crate) fn edit_playlist_body(c: &ClientConfig, playlist_id: &str, actions: Value) -> Value {
    json!({ "context": context(c), "playlistId": playlist_id, "actions": actions })
}

/// `feedback`: `{context, feedbackTokens}` (e.g. "Not interested").
pub(crate) fn feedback_body(c: &ClientConfig, tokens: &[&str]) -> Value {
    json!({ "context": context(c), "feedbackTokens": tokens })
}

/// `comment/perform_comment_action`: `{context, actions:[action]}` (like/dislike/delete).
pub(crate) fn perform_comment_action_body(c: &ClientConfig, action: &str) -> Value {
    json!({ "context": context(c), "actions": [action] })
}

/// `comment/create_comment_reply`: `{context, createReplyParams, commentText}`.
pub(crate) fn create_comment_reply_body(c: &ClientConfig, reply_params: &str, text: &str) -> Value {
    json!({ "context": context(c), "createReplyParams": reply_params, "commentText": text })
}

/// `live_chat/send_message`: `{context, params, clientMessageId, richMessage}`.
pub(crate) fn send_live_chat_body(
    c: &ClientConfig,
    params: &str,
    text: &str,
    client_message_id: &str,
) -> Value {
    json!({
        "context": context(c),
        "params": params,
        "clientMessageId": client_message_id,
        "richMessage": { "textSegments": [{ "text": text }] },
    })
}

/// `comment/create_comment`: `{context, createCommentParams, commentText}`. The
/// params are a server-vended blob from the (authenticated) comment composer.
pub(crate) fn create_comment_body(c: &ClientConfig, params: &str, text: &str) -> Value {
    json!({ "context": context(c), "createCommentParams": params, "commentText": text })
}
