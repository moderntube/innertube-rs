//! The umbrella crate: the InnerTube API (auth, search, channels, comments, playlists, live
//! chat, via [`Session`]) and SABR streaming ([`SabrStream`] and its [`MediaReader`]s). The
//! GTK frontend and the CLI link only this crate.

// The page and SABR URL helpers in `client` serve the streaming modules.
#[cfg_attr(not(feature = "streaming"), allow(dead_code, unused_imports))]
mod client;

#[cfg(feature = "streaming")]
mod bootstrap;
#[cfg(feature = "streaming")]
pub use bootstrap::{prewarm_session, session_signature_timestamp, StreamError, StreamErrorKind};
#[cfg(feature = "streaming")]
mod live;
#[cfg(feature = "streaming")]
pub use live::{EndReason, Quality, SabrStream, SabrStreamOptions, StreamInfo, StreamStats};

#[cfg(feature = "streaming")]
mod vod_bootstrap;
#[cfg(feature = "streaming")]
mod segment_vod;
#[cfg(feature = "streaming")]
pub use segment_vod::{SegmentPiece, SegmentSource, SegmentTrack, SegmentVod, SegmentVodOptions};
#[cfg(feature = "streaming")]
mod playback;
#[cfg(feature = "streaming")]
pub use playback::{Playback, PlaybackOptions};
#[cfg(feature = "streaming")]
mod live_segments;
#[cfg(feature = "streaming")]
pub use live_segments::{LiveSegmentTrack, LiveSegments};

#[cfg(feature = "streaming")]
mod captions;
#[cfg(feature = "streaming")]
pub use captions::Captions;

#[cfg(feature = "streaming")]
mod audio_window;
#[cfg(feature = "streaming")]
pub use audio_window::{
    fetch_audio_range, fetch_audio_window, live_head_ms, open_audio_source, AudioSource, AudioWindow,
    AudioWindowProgress,
};

mod heartbeat;
pub use heartbeat::spawn_from_player as spawn_heartbeat;

mod watch_report;
pub use watch_report::WatchReport;

#[cfg(feature = "streaming")]
mod attest;
#[cfg(feature = "streaming")]
mod bandwidth;
#[cfg(feature = "streaming")]
mod select;

// The InnerTube API surface. Each item is listed, so a new `pub` item in `client`
// stays inside the crate until it is added here.
pub use client::{
    // Session + client identity + auth
    AuthError, ClientConfig, Credentials, Error, Session,
    // Player / streaming metadata
    AudioTrack, DynamicReadaheadConfig, Format, LatencyClass, Playability, PlayerError,
    LiveStoryboard, Navigation, PlayerOptions, PlayerResponse, Storyboard, StoryboardThumb,
    // Typed read models
    AccountInfo, Caption, CaptionTrack, Channel, ChannelSort, ChannelTab, ChatBadge, ChatKind, ChatMessage, ChatReplay, ChatRun,
    ChatSendResult, Comment, Comments, LiveChat,
    Notification, Playlist, PlaylistContents, ResolvedUrl, SearchFilter, SearchFilterGroup, SearchItem, SearchResults,
    VideoDetails,
    // Shorts
    ReelEntry, ReelOverlay, ReelSequence, ReelStart,
    // Sidebar from the watch page, the only response carrying the `an_webp` hover previews.
    video_details_from_watch_html,
    // Documented write-flow finders (extract tokens to pass into the write methods)
    find_comment_action_for, find_comment_actions,
    find_live_chat_continuation, find_live_chat_send, find_not_interested_token, find_reply_params,
    // Watch reporting (`/api/stats/*`)
    client_playback_nonce, extract_visitor_data,
    // Client identity constants
    WEB_BROWSER_VERSION, WEB_CLIENT_VERSION,
    WEB_SIGNATURE_TIMESTAMP, WEB_USER_AGENT,
};

// Wire helpers for the CLI probes and the integration tests.
#[doc(hidden)]
pub use client::{decode_base64_lenient, find_str, get_watch_request_body, parse_comments, player_request_body};

// Streaming building blocks the orchestration produces.
#[cfg(feature = "streaming")]
pub use innertube_sabr::{Discontinuity, DiscontinuityKind, MediaReader, TrackBuffer};

// Pluggable dependencies a consumer supplies to [`SabrStream::open`].
#[cfg(feature = "streaming")]
pub use innertube_js::NSigSolver;
#[cfg(feature = "streaming")]
pub use innertube_pot::{
    provider as pot_provider, BgUtilProvider, NativeProvider, PageAttestation, PoTokenProvider, PotBackendKind,
    PotError,
};
