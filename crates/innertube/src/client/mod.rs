//! InnerTube client core: client contexts and request building.
//!
//! InnerTube is a JSON API (distinct from the protobuf SABR layer). See the
//! captured request shape in `notes/` for the WEB client `player()` request.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use serde::{Deserialize, Serialize};

mod actions;
mod api;
mod auth;
pub(crate) mod dump;
mod reel;
pub use reel::{ReelEntry, ReelOverlay, ReelSequence, ReelStart};
pub use api::{
    video_details_from_watch_html,
    find_comment_action_for, find_comment_actions,
    find_live_chat_continuation, find_live_chat_send, find_not_interested_token,
    find_reply_params, find_str, parse_comments,
    AccountInfo, Caption,
    CaptionTrack, Channel, ChatBadge, ChatKind, ChatMessage, ChatReplay, ChatRun, ChatSendResult, Comment, Comments, LiveChat,
    Notification, Playlist,
    PlaylistContents, ResolvedUrl, SearchItem, SearchResults, VideoDetails,
};
pub(crate) use api::parse_next;
pub use auth::{AuthError, Credentials};
mod config;
pub use config::*;
mod error;
pub use error::*;
mod page;
pub use page::*;
mod player;
pub use player::*;
mod player_request;
pub use player_request::*;
mod sabr_url;
pub use sabr_url::*;
mod session;
pub use session::*;
mod storyboard;
pub use storyboard::*;

/// Decode base64 that may be standard or URL-safe, padded or not (YouTube mixes these).
pub fn decode_base64_lenient(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    STANDARD
        .decode(s)
        .or_else(|_| URL_SAFE.decode(s))
        .or_else(|_| STANDARD_NO_PAD.decode(s))
        .or_else(|_| URL_SAFE_NO_PAD.decode(s))
        .ok()
}
