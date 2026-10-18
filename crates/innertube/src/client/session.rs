//! The HTTP `Session` and its InnerTube endpoint methods.

use super::*;

const PLAYER_ENDPOINT: &str = "https://www.youtube.com/youtubei/v1/player?prettyPrint=false";
/// The reference client's SPA-navigation POST. It sends `{context, playerRequest,
/// watchNextRequest}` and gets a two-element array `[{playerResponse, ...},
/// {watchNextResponse, ...}]`. A click issues no `/player` call (IT-006).
const GET_WATCH_ENDPOINT: &str = "https://www.youtube.com/youtubei/v1/get_watch?prettyPrint=false";

/// A channel content tab, selected via channel-independent `browse` params.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelTab {
    /// Long-form videos (excludes Shorts and lives).
    Videos,
    /// Shorts.
    Shorts,
    /// Live streams, past (completed) and current.
    Live,
}

impl ChannelTab {
    /// The `browse` `params` blob that selects this tab (same for every channel).
    fn params(self) -> &'static str {
        match self {
            ChannelTab::Videos => "EgZ2aWRlb3PyBgQKAjoA",
            ChannelTab::Shorts => "EgZzaG9ydHPyBgUKA5oBAA==",
            ChannelTab::Live => "EgdzdHJlYW1z8gYECgJ6AA==",
        }
    }
}

/// Sort order for a channel's **Videos** tab, taken from the chip bar above the grid.
/// `Latest` is the tab default. `Popular` and `Oldest` read the matching chip's
/// continuation token from the channel response (IT-026).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelSort {
    /// Newest first (the default).
    Latest,
    /// Most-viewed first.
    Popular,
    /// Oldest first.
    Oldest,
}

impl ChannelSort {
    /// The English chip label (client is en_US) used to locate the chip in the response.
    fn chip_label(self) -> &'static str {
        match self {
            ChannelSort::Latest => "Latest",
            ChannelSort::Popular => "Popular",
            ChannelSort::Oldest => "Oldest",
        }
    }
}

/// An async InnerTube session over a shared `reqwest` client. Cheap to clone (the
/// `reqwest::Client` is reference-counted): clone it to give a streaming task its own handle.
#[derive(Clone)]
pub struct Session {
    http: reqwest::Client,
    client: ClientConfig,
    /// When present, the session is signed in: requests carry `Cookie` (via the
    /// client's cookie jar, which also absorbs `Set-Cookie` rotation) +
    /// `Authorization: SAPISIDHASH …` recomputed per request.
    credentials: Option<Credentials>,
    /// `visitorData` bootstrapped once per session and shared across clones; a browser keeps
    /// one for its whole session (IT-003).
    visitor_cache: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// The `ytcfg` experiment assignment of a page this session loaded; every InnerTube
    /// request of the web client carries it as `context.client.configInfo`.
    page_config: std::sync::Arc<std::sync::Mutex<Option<PageClientConfig>>>,
    direct_opens: std::sync::Arc<std::sync::atomic::AtomicBool>,
    watch_pages: std::sync::Arc<WatchPages>,
    id: u64,
}

/// Watch pages this session fetched, newest last, as `(video id, html, fetched at)`. A page
/// funds only the session that fetched it: its BotGuard challenge is bound to that page load.
#[derive(Debug, Default)]
pub struct WatchPages(std::sync::Mutex<Vec<(String, String, std::time::Instant)>>);

impl WatchPages {
    /// Keep `html` as `video_id`'s page, holding at most four.
    pub fn store(&self, video_id: &str, html: &str) {
        let mut pages = self.0.lock().unwrap();
        pages.retain(|(id, _, _)| id != video_id);
        if pages.len() >= 4 {
            pages.remove(0);
        }
        pages.push((video_id.to_string(), html.to_string(), std::time::Instant::now()));
    }

    /// `video_id`'s page if it is younger than `ttl_ms`; else, with `any_video`, the newest
    /// other page, flagged `false` because its SSR player response plays a different video.
    pub fn lookup(&self, video_id: &str, ttl_ms: u64, any_video: bool) -> Option<(String, bool)> {
        let mut pages = self.0.lock().unwrap();
        pages.retain(|(_, _, at)| at.elapsed().as_millis() as u64 <= ttl_ms);
        if let Some((_, html, _)) = pages.iter().find(|(id, _, _)| id == video_id) {
            return Some((html.clone(), true));
        }
        if !any_video {
            return None;
        }
        pages.last().map(|(_, html, _)| (html.clone(), false))
    }

    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

fn next_session_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl Session {
    /// Identifies this session and its clones: one visitor, one cookie jar.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Direct opens: each click plays its own watch page's SSR response, as a direct URL
    /// visit does. YouTube holds a click's `get_watch` response ~3.8 s for an ad slot; the
    /// SSR response starts at once. Shared by this session's clones.
    pub fn set_direct_opens(&self, on: bool) {
        self.direct_opens.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn direct_opens(&self) -> bool {
        self.direct_opens.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The watch pages this session fetched, for reuse by its next open.
    pub fn watch_pages(&self) -> &WatchPages {
        &self.watch_pages
    }

    /// Create an anonymous session for the given client identity.
    ///
    /// The jar is seeded with the EU consent cookie, so requests skip the consent wall, and
    /// keeps the cookies YouTube sets. A client that drops `Set-Cookie` presents as a new
    /// browser on every request (IT-022). Its watch pages also carry no signed `an_webp`
    /// preview urls in `animatedThumbnailOverlayViewModel`, and those urls cannot be rebuilt
    /// from a video id.
    pub fn new(client: ClientConfig) -> Result<Self, Error> {
        let jar = std::sync::Arc::new(reqwest::cookie::Jar::default());
        let url = "https://www.youtube.com".parse::<reqwest::Url>().expect("static url");
        jar.add_cookie_str("SOCS=CAI; Domain=.youtube.com; Path=/", &url);
        let http = Self::client_builder().cookie_provider(jar).build()?;
        Ok(Self { http, client, credentials: None, visitor_cache: Default::default(), page_config: Default::default(), direct_opens: Default::default(), watch_pages: Default::default(), id: next_session_id() })
    }

    /// Create a **signed-in** session. The cookies seed a jar attached to the client, so
    /// `Set-Cookie` rotation (the short-lived `__Secure-*PSIDTS` tokens) is absorbed and the
    /// session stays fresh. `Authorization` is added per request.
    pub fn authenticated(client: ClientConfig, credentials: Credentials) -> Result<Self, Error> {
        use std::sync::Arc;
        let jar = Arc::new(reqwest::cookie::Jar::default());
        let url: reqwest::Url = "https://www.youtube.com/".parse().expect("valid url");
        jar.add_cookie_str("SOCS=CAI; Domain=.youtube.com; Path=/", &url);
        for (name, value) in credentials.cookie_pairs() {
            jar.add_cookie_str(&format!("{name}={value}; Domain=.youtube.com; Path=/"), &url);
        }
        let http = Self::client_builder().cookie_provider(jar).build()?;
        Ok(Self { http, client, credentials: Some(credentials), visitor_cache: Default::default(), page_config: Default::default(), direct_opens: Default::default(), watch_pages: Default::default(), id: next_session_id() })
    }

    /// Shared reqwest client config (UA, forced IPv4, connect timeout).
    fn client_builder() -> reqwest::ClientBuilder {
        reqwest::Client::builder()
            .user_agent(WEB_USER_AGENT)
            // Force IPv4: reqwest tries IPv6 first with no fallback, and an IPv6 route to
            // googlevideo that drops packets hangs the first POST for about 60 s (IT-021).
            .local_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
            .connect_timeout(Duration::from_secs(10))
            // A pooled keep-alive socket whose path died never errors on its own.
            .read_timeout(Duration::from_secs(30))
    }

    /// Add the signed-in headers (`Authorization`/`X-Goog-AuthUser`/`X-Origin`) to a
    /// request when the session has credentials; a no-op for anonymous sessions.
    fn with_auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.credentials {
            None => rb,
            Some(creds) => {
                let epoch = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                rb.header("authorization", creds.authorization(epoch))
                    .header("x-goog-authuser", creds.auth_user())
                    .header("x-origin", "https://www.youtube.com")
            }
        }
    }

    /// True if this session is signed in.
    pub fn is_authenticated(&self) -> bool {
        self.credentials.is_some()
    }

    /// Return [`Error::NotLoggedIn`] before a round-trip when a write/account action
    /// is attempted anonymously (lets a GUI disable buttons / prompt login cheaply).
    fn require_auth(&self) -> Result<(), Error> {
        if self.is_authenticated() {
            Ok(())
        } else {
            Err(Error::NotLoggedIn)
        }
    }

    /// The client identity (name, version, …) this session uses.
    pub fn client(&self) -> &ClientConfig {
        &self.client
    }

    /// POST `body` to an InnerTube endpoint (`"like/like"`, `"browse"`, ...) with the client
    /// headers and, when signed in, auth. Returns the response JSON. A top-level `error`
    /// object becomes `Err`.
    ///
    /// Low-level and hidden from the curated surface: `pub` for the umbrella crate's typed
    /// methods. Prefer a typed method (`browse`, `search`, `player`, ...).
    #[doc(hidden)]
    pub async fn youtubei_post(
        &self,
        endpoint: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, Error> {
        let url = format!("https://www.youtube.com/youtubei/v1/{endpoint}?prettyPrint=false");
        let mut body = body;
        if let Some(cfg) = self.page_config.lock().unwrap().as_ref() {
            add_config_info(&mut body, cfg);
        }
        let rb = self
            .http
            .post(&url)
            .header("x-youtube-client-name", self.client.client_name_id.to_string())
            .header("x-youtube-client-version", &self.client.client_version)
            .header("origin", "https://www.youtube.com")
            .header("referer", "https://www.youtube.com/")
            .json(&body);
        let resp = self.with_auth(rb).send().await?;
        let status = resp.status().as_u16();
        let text = resp.error_for_status()?.text().await?;
        if dump::enabled() {
            let mut req = body.clone();
            dump::scrub_json(&mut req);
            dump::record(
                "control",
                serde_json::json!({
                    "endpoint": endpoint,
                    "url": dump::scrub_url(&url),
                    "status": status,
                    "req": req,
                    "resp_bytes": text.len(),
                }),
            );
        }
        let v: serde_json::Value = serde_json::from_str(&text)?;
        // An API-level failure is a 200 with a top-level `error` object.
        if let Some(err) = v.get("error") {
            return Err(Error::InnerTube {
                code: err["code"].as_i64().unwrap_or(0),
                status: err["status"].as_str().unwrap_or("").to_string(),
                message: err["message"].as_str().unwrap_or("unknown InnerTube error").to_string(),
            });
        }
        Ok(v)
    }

    /// Fetch the signed-in account menu, an authenticated read with no side effects.
    pub async fn account_menu(&self) -> Result<serde_json::Value, Error> {
        self.require_auth()?;
        let body = serde_json::json!({ "context": actions::context(&self.client) });
        self.youtubei_post("account/account_menu", body).await
    }

    /// The signed-in account's identity (display name / handle / avatar), parsed from
    /// the account menu. Requires auth.
    pub async fn account_info(&self) -> Result<AccountInfo, Error> {
        let v = self.account_menu().await?;
        Ok(crate::client::api::parse_account_info(&v))
    }

    /// `browse` any feed/channel/list by `browseId` (e.g. `"FEsubscriptions"`,
    /// `"FEchannels"`, a `UC…` channel id). Read-only.
    pub async fn browse(&self, browse_id: &str) -> Result<serde_json::Value, Error> {
        self.youtubei_post("browse", actions::browse_body(&self.client, browse_id)).await
    }

    /// POST `player/heartbeat`, the keepalive the WEB client sends every
    /// [`PlayerResponse::heartbeat_interval_ms`] (15 s) during a live session (IT-014). Echoes
    /// `heartbeat_server_data` (from the player response, then from the previous beat) with an
    /// incrementing `sequence_number`. Returns the fresh `heartbeatServerData` for the next
    /// beat, if the response carries one.
    ///
    /// Playback does not depend on it.
    pub async fn heartbeat(
        &self,
        video_id: &str,
        sequence_number: u64,
        heartbeat_server_data: &str,
    ) -> Result<Option<String>, Error> {
        let body = actions::heartbeat_body(&self.client, video_id, sequence_number, heartbeat_server_data);
        let v = self.youtubei_post("player/heartbeat", body).await?;
        // The response returns a fresh server-data blob for the next beat (path varies;
        // check the common locations).
        Ok(v.pointer("/playbackTracking/heartbeatParams/heartbeatServerData")
            .or_else(|| v.pointer("/heartbeatParams/heartbeatServerData"))
            .and_then(|s| s.as_str())
            .map(str::to_string))
    }

    /// The home feed (`FEwhat_to_watch`) as typed results and a continuation. Signed in, it
    /// is YouTube's personalized feed. Anonymous, it is the empty "start searching" shell
    /// (IT-020).
    pub async fn home(&self) -> Result<SearchResults, Error> {
        let v = self.browse("FEwhat_to_watch").await?;
        Ok(api::parse_search(&v))
    }

    /// The signed-in user's subscriptions feed (`FEsubscriptions`), newest first.
    /// Requires auth.
    pub async fn subscriptions(&self) -> Result<SearchResults, Error> {
        self.require_auth()?;
        let v = self.browse("FEsubscriptions").await?;
        Ok(api::parse_search(&v))
    }

    /// Next page of a feed ([`Session::home`]/[`Session::subscriptions`]) by the
    /// `continuation` token from the previous page.
    pub async fn feed_continuation(&self, token: &str) -> Result<SearchResults, Error> {
        let v = self.continuation("browse", token).await?;
        Ok(api::parse_search(&v))
    }

    /// Like a video (write; requires auth).
    pub async fn like(&self, video_id: &str) -> Result<serde_json::Value, Error> {
        self.require_auth()?;
        self.youtubei_post("like/like", actions::like_body(&self.client, video_id)).await
    }

    /// Dislike a video (write; requires auth).
    pub async fn dislike(&self, video_id: &str) -> Result<serde_json::Value, Error> {
        self.require_auth()?;
        self.youtubei_post("like/dislike", actions::like_body(&self.client, video_id)).await
    }

    /// Clear an existing like/dislike on a video (write; requires auth).
    pub async fn remove_like(&self, video_id: &str) -> Result<serde_json::Value, Error> {
        self.require_auth()?;
        self.youtubei_post("like/removelike", actions::like_body(&self.client, video_id)).await
    }

    /// Subscribe to a channel (write; requires auth).
    pub async fn subscribe(&self, channel_id: &str) -> Result<serde_json::Value, Error> {
        self.require_auth()?;
        self.youtubei_post(
            "subscription/subscribe",
            actions::subscription_body(&self.client, channel_id),
        )
        .await
    }

    /// Unsubscribe from a channel (write; requires auth).
    pub async fn unsubscribe(&self, channel_id: &str) -> Result<serde_json::Value, Error> {
        self.require_auth()?;
        self.youtubei_post(
            "subscription/unsubscribe",
            actions::subscription_body(&self.client, channel_id),
        )
        .await
    }

    /// Submit feedback tokens (e.g. a "Not interested" `feedbackToken` extracted from
    /// a feed via [`find_not_interested_token`]) to tune recommendations. Requires auth.
    pub async fn feedback(&self, tokens: &[&str]) -> Result<serde_json::Value, Error> {
        self.require_auth()?;
        self.youtubei_post("feedback", actions::feedback_body(&self.client, tokens)).await
    }

    /// Perform a comment action (like/dislike/delete) given its `action` param,
    /// extracted from a comments page via [`find_comment_actions`]. Requires auth.
    pub async fn perform_comment_action(&self, action: &str) -> Result<serde_json::Value, Error> {
        self.require_auth()?;
        self.youtubei_post(
            "comment/perform_comment_action",
            actions::perform_comment_action_body(&self.client, action),
        )
        .await
    }

    /// Reply to a comment given its `createReplyParams` (from [`find_reply_params`])
    /// and the reply text. Requires auth.
    pub async fn create_comment_reply(
        &self,
        reply_params: &str,
        text: &str,
    ) -> Result<serde_json::Value, Error> {
        self.require_auth()?;
        self.youtubei_post(
            "comment/create_comment_reply",
            actions::create_comment_reply_body(&self.client, reply_params, text),
        )
        .await
    }

    /// Search YouTube. `params` is a [`SearchFilter::params`] from an earlier page, or `None`
    /// for a default relevance search.
    pub async fn search(&self, query: &str, params: Option<&str>) -> Result<SearchResults, Error> {
        let v = self.youtubei_post("search", actions::search_body(&self.client, query, params)).await?;
        Ok(api::parse_search(&v))
    }

    /// Watch-page metadata for a video (`next`): title/channel/views, related
    /// videos, and the token that loads the comment thread.
    pub async fn next(&self, video_id: &str) -> Result<VideoDetails, Error> {
        let v = self.youtubei_post("next", actions::next_body(&self.client, video_id)).await?;
        Ok(api::parse_next(&v))
    }

    /// Enter the Shorts feed with no seed video, as the Shorts tab does.
    pub async fn reel_seedless(&self) -> Result<Option<ReelStart>, Error> {
        let v = self.youtubei_post("reel/reel_item_watch", reel::seedless_body(&self.client)).await?;
        Ok(reel::parse_seedless(&v))
    }

    /// The overlay for the Short being shown; the client sends this for every Short it shows.
    pub async fn reel_item_watch(&self, entry: &ReelEntry) -> Result<ReelOverlay, Error> {
        let v = self.youtubei_post("reel/reel_item_watch", reel::item_watch_body(&self.client, entry)).await?;
        if let Some(dir) = dump::raw_dir("reel") {
            let _ = std::fs::write(dir.join(format!("{}.json", entry.video_id)), v.to_string());
        }
        Ok(reel::parse_overlay(&v))
    }

    /// One page of a Short's comments. The Shorts page pages comments through `browse`;
    /// the same token sent to `next` answers with an ads panel.
    pub async fn reel_comments_page(&self, token: &str) -> Result<Comments, Error> {
        self.ensure_page_config().await;
        let v = self.continuation("browse", token).await?;
        Ok(api::parse_comments(&v))
    }

    /// The Shorts after `current`, from a sequence token.
    pub async fn reel_watch_sequence(
        &self,
        token: &str,
        current: &str,
        signature_timestamp: Option<i64>,
    ) -> Result<ReelSequence, Error> {
        let body = reel::sequence_body(&self.client, token, current, signature_timestamp);
        let v = self.youtubei_post("reel/reel_watch_sequence", body).await?;
        Ok(reel::parse_sequence(&v))
    }

    /// Load the first page of comments for a video. Chains `next` (for the comments
    /// token) → `continuation`. Page further with `comments_page(comments.continuation)`.
    /// Returns an empty page if comments are disabled.
    pub async fn comments(&self, video_id: &str) -> Result<Comments, Error> {
        let details = self.next(video_id).await?;
        match details.comments_continuation {
            None => Ok(Comments::default()),
            Some(token) => self.comments_page(&token).await,
        }
    }

    /// Fetch one comments page by token (from [`VideoDetails::comments_continuation`]
    /// or a prior [`Comments::continuation`]).
    pub async fn comments_page(&self, token: &str) -> Result<Comments, Error> {
        self.ensure_page_config().await;
        let v = self.continuation("next", token).await?;
        Ok(api::parse_comments(&v))
    }

    /// Read a playlist's contents by id (`browse VL<id>`). Anonymous works for
    /// public playlists; auth needed for your private ones.
    pub async fn playlist(&self, playlist_id: &str) -> Result<PlaylistContents, Error> {
        let browse_id =
            if playlist_id.starts_with("VL") { playlist_id.to_string() } else { format!("VL{playlist_id}") };
        let v = self.browse(&browse_id).await?;
        Ok(api::parse_playlist_contents(&v))
    }

    /// Your notification inbox (`notification/get_notification_menu`). Requires auth.
    pub async fn notifications(&self) -> Result<Vec<Notification>, Error> {
        self.require_auth()?;
        let body = serde_json::json!({
            "context": actions::context(&self.client),
            "notificationsMenuRequestType": "NOTIFICATIONS_MENU_REQUEST_TYPE_INBOX",
        });
        let v = self.youtubei_post("notification/get_notification_menu", body).await?;
        Ok(api::parse_notifications(&v))
    }

    /// List the playlists in your library (`browse FElibrary`). Requires auth.
    pub async fn list_playlists(&self) -> Result<Vec<Playlist>, Error> {
        self.require_auth()?;
        let v = self.browse("FElibrary").await?;
        Ok(api::parse_playlists(&v))
    }

    /// Add a video to Watch Later (the `WL` playlist). Requires auth.
    pub async fn watch_later_add(&self, video_id: &str) -> Result<serde_json::Value, Error> {
        self.playlist_add("WL", video_id).await
    }

    /// Remove a video from Watch Later. Requires auth.
    pub async fn watch_later_remove(&self, video_id: &str) -> Result<serde_json::Value, Error> {
        self.playlist_remove("WL", video_id).await
    }

    /// Create a playlist (write; requires auth). Returns the new `playlistId`.
    pub async fn create_playlist(
        &self,
        title: &str,
        video_ids: &[&str],
        privacy: &str,
    ) -> Result<String, Error> {
        self.require_auth()?;
        let v = self
            .youtubei_post(
                "playlist/create",
                actions::playlist_create_body(&self.client, title, video_ids, privacy),
            )
            .await?;
        api::find_str(&v, "playlistId")
            .ok_or_else(|| Error::Api("create_playlist: no playlistId in response".into()))
    }

    /// Delete a playlist (write; requires auth).
    pub async fn delete_playlist(&self, playlist_id: &str) -> Result<serde_json::Value, Error> {
        self.require_auth()?;
        self.youtubei_post("playlist/delete", actions::playlist_delete_body(&self.client, playlist_id))
            .await
    }

    /// Add a video to a playlist (write; requires auth).
    pub async fn playlist_add(
        &self,
        playlist_id: &str,
        video_id: &str,
    ) -> Result<serde_json::Value, Error> {
        self.require_auth()?;
        let actions = serde_json::json!([{ "addedVideoId": video_id, "action": "ACTION_ADD_VIDEO" }]);
        self.youtubei_post(
            "browse/edit_playlist",
            actions::edit_playlist_body(&self.client, playlist_id, actions),
        )
        .await
    }

    /// Remove a video from a playlist by video id (write; requires auth).
    pub async fn playlist_remove(
        &self,
        playlist_id: &str,
        video_id: &str,
    ) -> Result<serde_json::Value, Error> {
        self.require_auth()?;
        let actions = serde_json::json!([
            { "removedVideoId": video_id, "action": "ACTION_REMOVE_VIDEO_BY_VIDEO_ID" }
        ]);
        self.youtubei_post(
            "browse/edit_playlist",
            actions::edit_playlist_body(&self.client, playlist_id, actions),
        )
        .await
    }

    /// Post a top-level comment on a video (write; requires auth). Fetches the
    /// authenticated comment composer to obtain `createCommentParams`, then posts.
    /// Returns the raw `create_comment` response (contains the new comment).
    pub async fn comment(&self, video_id: &str, text: &str) -> Result<serde_json::Value, Error> {
        self.require_auth()?;
        // The composer + its createCommentParams only exist on the authed comments page.
        let token = self
            .next(video_id)
            .await?
            .comments_continuation
            .ok_or_else(|| Error::Api("comment: comments disabled".into()))?;
        let page = self.continuation("next", &token).await?;
        let params = api::find_str(&page, "createCommentParams").ok_or_else(|| {
            Error::Api("comment: no createCommentParams (not signed in?)".into())
        })?;
        self.youtubei_post("comment/create_comment", actions::create_comment_body(&self.client, &params, text))
            .await
    }

    /// Open live chat for a currently-live video: returns the first page of messages
    /// plus a `continuation`/`poll_ms` to keep polling with [`Session::live_chat_page`].
    /// Errors if the video isn't live or chat is disabled. Anonymous works (read-only).
    pub async fn live_chat(&self, video_id: &str) -> Result<LiveChat, Error> {
        let next = self.youtubei_post("next", actions::next_body(&self.client, video_id)).await?;
        let cont = api::find_live_chat_continuation(&next)
            .ok_or_else(|| Error::Api("live_chat: no chat continuation (not live or chat disabled)".into()))?;
        self.live_chat_page(&cont).await
    }

    /// Fetch one live-chat page by `continuation` (poll this on a loop, waiting
    /// [`LiveChat::poll_ms`] between calls).
    pub async fn live_chat_page(&self, continuation: &str) -> Result<LiveChat, Error> {
        let v = self
            .youtubei_post("live_chat/get_live_chat", actions::continuation_body(&self.client, continuation))
            .await?;
        Ok(api::parse_live_chat(&v))
    }

    /// Probe a VOD for chat replay. Returns [`ChatReplay::Available`] with the first batch
    /// (each [`ChatMessage::video_offset_ms`] says when it appears) and a `continuation` for
    /// [`Session::live_chat_replay_page`], [`ChatReplay::Unavailable`] with YouTube's notice
    /// when the video was live but has no replay, or [`ChatReplay::NotLive`] for an ordinary
    /// VOD. Anonymous works.
    pub async fn live_chat_replay(&self, video_id: &str) -> Result<ChatReplay, Error> {
        let next = self.youtubei_post("next", actions::next_body(&self.client, video_id)).await?;
        if let Some(cont) = api::find_live_chat_continuation(&next) {
            return Ok(ChatReplay::Available(self.live_chat_replay_page(&cont, 0).await?));
        }
        if let Some(msg) = api::find_replay_unavailable_message(&next) {
            return Ok(ChatReplay::Unavailable(Some(msg)));
        }
        Ok(ChatReplay::NotLive)
    }

    /// Fetch one replay-chat batch by `continuation`, synced to `player_offset_ms` (the
    /// current player position in ms). Poll as the player advances or on seek; reveal
    /// each returned message when the player clock reaches its `video_offset_ms`. The
    /// returned `continuation` carries forward to the next fetch.
    pub async fn live_chat_replay_page(
        &self,
        continuation: &str,
        player_offset_ms: i64,
    ) -> Result<LiveChat, Error> {
        let v = self
            .youtubei_post(
                "live_chat/get_live_chat_replay",
                actions::replay_continuation_body(&self.client, continuation, player_offset_ms),
            )
            .await?;
        Ok(api::parse_live_chat(&v))
    }

    /// Send a message to a live video's chat (write; requires auth). Resolves the chat, reads
    /// the send params from the authed action panel, and posts. Errors if the video is not
    /// live or chat is restricted (subscribers or members only). Returns
    /// [`ChatSendResult::Sent`] with the echoed item when present, or `Rejected(reason)` when
    /// the server refused the message (slow mode, timeout, duplicate, blocked). `Err` covers
    /// transport, not-live and restricted-chat failures.
    pub async fn live_chat_send(&self, video_id: &str, text: &str) -> Result<ChatSendResult, Error> {
        self.require_auth()?;
        let next = self.youtubei_post("next", actions::next_body(&self.client, video_id)).await?;
        let cont = api::find_live_chat_continuation(&next)
            .ok_or_else(|| Error::Api("live_chat_send: not live or chat disabled".into()))?;
        let chat = self
            .youtubei_post("live_chat/get_live_chat", actions::continuation_body(&self.client, &cont))
            .await?;
        let (params, prefix) = api::find_live_chat_send(&chat)
            .ok_or_else(|| Error::Api("live_chat_send: chat is restricted (subscribers/members-only)".into()))?;
        let client_message_id = format!("{prefix}0");
        let resp = self
            .youtubei_post(
                "live_chat/send_message",
                actions::send_live_chat_body(&self.client, &params, text, &client_message_id),
            )
            .await?;
        Ok(api::parse_chat_send_response(&resp))
    }

    /// List the caption tracks available for a video (from the `player` response).
    pub async fn caption_tracks(&self, video_id: &str) -> Result<Vec<CaptionTrack>, Error> {
        let raw = self.player_raw(video_id, &PlayerOptions::default()).await?;
        let v: serde_json::Value = serde_json::from_str(&raw)?;
        Ok(api::parse_caption_tracks(&v))
    }

    /// Fetch one caption track's cues the way the web player does: the track's `baseUrl`
    /// plus the client identity, `fmt=json3` and the content-bound PO token.
    pub async fn timedtext(&self, track: &CaptionTrack, po_token: Option<&[u8]>) -> Result<Vec<Caption>, Error> {
        use base64::Engine as _;
        let mut url = reqwest::Url::parse(&track.base_url).map_err(|e| Error::Api(format!("caption url: {e}")))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("c", &self.client.client_name)
                .append_pair("cver", &self.client.client_version)
                .append_pair("cplayer", "UNIPLAYER")
                .append_pair("cos", "X11")
                .append_pair("cbr", "Chrome")
                .append_pair("cbrver", WEB_BROWSER_VERSION)
                .append_pair("cplatform", "DESKTOP")
                .append_pair("fmt", "json3")
                .append_pair("xorb", "2")
                .append_pair("xobt", "3")
                .append_pair("xovt", "3");
            if let Some(pot) = po_token.filter(|p| !p.is_empty()) {
                q.append_pair("potc", "1")
                    .append_pair("pot", &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(pot));
            }
        }
        let body = self.get_text(url.as_str()).await?;
        let json: serde_json::Value = serde_json::from_str(&body)?;
        Ok(api::parse_timedtext_json3(&json))
    }

    /// Fetch a video's transcript as timed caption cues. Picks the track whose
    /// language code starts with `lang` (e.g. `"en"`), else the first track.
    /// Returns an empty vec if the video has no captions.
    pub async fn transcript(&self, video_id: &str, lang: Option<&str>) -> Result<Vec<Caption>, Error> {
        let tracks = self.caption_tracks(video_id).await?;
        let track = match lang {
            Some(l) => tracks.iter().find(|t| t.language_code.starts_with(l)).or_else(|| tracks.first()),
            None => tracks.first(),
        };
        let Some(track) = track else { return Ok(Vec::new()) };
        let sep = if track.base_url.contains('?') { '&' } else { '?' };
        let url = format!("{}{sep}fmt=json3", track.base_url);
        let body = self.get_text(&url).await?;
        let json: serde_json::Value = serde_json::from_str(&body)?;
        Ok(api::parse_timedtext_json3(&json))
    }

    /// Fetch a channel page by `UC...` id: header (name, handle, subscribers), tab list, and
    /// the videos on the landing (Home) tab. Resolve a handle first with
    /// [`Session::resolve_url`]. Anonymous works.
    ///
    /// `Channel::videos` is YouTube's curated Home tab, in no chronological order. For
    /// newest-first uploads use [`Session::channel_uploads`].
    pub async fn channel(&self, channel_id: &str) -> Result<Channel, Error> {
        let v = self.browse(channel_id).await?;
        Ok(api::parse_channel(&v))
    }

    /// A channel's uploads (the `UU...` playlist): long-form videos, Shorts and past live
    /// streams interleaved, newest first. For one content type use
    /// [`Session::channel_videos`] and its siblings. Anonymous works.
    pub async fn channel_uploads(&self, channel_id: &str) -> Result<PlaylistContents, Error> {
        let uploads = match channel_id.strip_prefix("UC") {
            Some(rest) => format!("UU{rest}"),
            None => format!("UU{channel_id}"),
        };
        self.playlist(&uploads).await
    }

    /// A channel's content for one [`ChannelTab`] (Videos / Shorts / Live), newest
    /// first, with a continuation token for the next page (feed it back to
    /// [`Session::channel_tab_continuation`]). Anonymous works.
    pub async fn channel_tab(&self, channel_id: &str, tab: ChannelTab) -> Result<SearchResults, Error> {
        let v = self
            .youtubei_post("browse", actions::browse_params_body(&self.client, channel_id, tab.params()))
            .await?;
        Ok(api::parse_channel_videos(&v))
    }

    /// A channel's tab content with an explicit sort (the chip bar). `Latest` is the tab
    /// default and takes one request. `Popular` and `Oldest` read the matching chip's token
    /// from the channel response and continue the browse with it (two requests). A tab with
    /// no chip bar (Shorts, Live, or a channel that hides it) returns its natural order.
    pub async fn channel_tab_sorted(
        &self,
        channel_id: &str,
        tab: ChannelTab,
        sort: ChannelSort,
    ) -> Result<SearchResults, Error> {
        let v = self
            .youtubei_post("browse", actions::browse_params_body(&self.client, channel_id, tab.params()))
            .await?;
        if sort == ChannelSort::Latest {
            return Ok(api::parse_channel_videos(&v));
        }
        match api::find_sort_chip_token(&v, sort.chip_label()) {
            // The sort chip carries a browse continuation token.
            Some(token) => {
                let v2 = self.continuation("browse", &token).await?;
                Ok(api::parse_channel_videos(&v2))
            }
            // No matching chip (tab has no sort bar) → the tab's default order.
            None => Ok(api::parse_channel_videos(&v)),
        }
    }

    /// Long-form videos only (excludes Shorts/lives), newest first.
    pub async fn channel_videos(&self, channel_id: &str) -> Result<SearchResults, Error> {
        self.channel_tab(channel_id, ChannelTab::Videos).await
    }

    /// A channel's Shorts, newest first.
    pub async fn channel_shorts(&self, channel_id: &str) -> Result<SearchResults, Error> {
        self.channel_tab(channel_id, ChannelTab::Shorts).await
    }

    /// A channel's live streams (past + current), newest first.
    pub async fn channel_live(&self, channel_id: &str) -> Result<SearchResults, Error> {
        self.channel_tab(channel_id, ChannelTab::Live).await
    }

    /// Next page of a [`Session::channel_tab`] result.
    pub async fn channel_tab_continuation(&self, token: &str) -> Result<SearchResults, Error> {
        let v = self.continuation("browse", token).await?;
        Ok(api::parse_channel_videos(&v))
    }

    /// Next page of a [`Session::search`] result. Pass the `continuation` token from
    /// the previous `SearchResults`; a `None` continuation in the reply means the end.
    pub async fn search_continuation(&self, token: &str) -> Result<SearchResults, Error> {
        let v = self.continuation("search", token).await?;
        Ok(api::parse_search(&v))
    }

    /// Resolve a vanity URL (`@handle`, `/user/...`, `/c/...`) to a navigation target.
    /// Works on an anonymous session.
    pub async fn resolve_url(&self, url: &str) -> Result<ResolvedUrl, Error> {
        let v = self
            .youtubei_post("navigation/resolve_url", actions::resolve_url_body(&self.client, url))
            .await?;
        Ok(api::parse_resolved_url(&v))
    }

    /// Fetch the next page of a paginated read. `endpoint` is the one that issued the
    /// `token` (`"browse"`, `"search"`, `"next"`). Returns the raw JSON; parse it with the
    /// parser used for the first page.
    pub async fn continuation(
        &self,
        endpoint: &str,
        token: &str,
    ) -> Result<serde_json::Value, Error> {
        self.youtubei_post(endpoint, actions::continuation_body(&self.client, token)).await
    }

    /// GET a URL and return its body text (uses the session's WEB UA + cookies).
    pub async fn get_text(&self, url: &str) -> Result<String, Error> {
        let resp = self.http.get(url).send().await?;
        let status = resp.status().as_u16();
        let text = resp.error_for_status()?.text().await?;
        if dump::enabled() {
            dump::record(
                "fetch",
                serde_json::json!({ "url": dump::scrub_url(url), "status": status, "resp_bytes": text.len() }),
            );
        }
        self.seed_page_config_from(&text);
        Ok(text)
    }

    /// POST a `/api/stats/{playback,watchtime}` report the way the web player sends one:
    /// form body `session_token=<ytcfg XSRF_TOKEN>`, cookies, and the page's visitor id.
    pub async fn post_stats(
        &self,
        url: &str,
        session_token: &str,
        visitor_id: Option<&str>,
        referer: &str,
    ) -> Result<u16, Error> {
        let mut rb = self
            .http
            .post(url)
            .header("x-youtube-client-name", self.client.client_name_id.to_string())
            .header("origin", "https://www.youtube.com")
            .header("referer", referer)
            .form(&[("session_token", session_token)]);
        if let Some(v) = visitor_id {
            rb = rb.header("x-goog-visitor-id", v);
        }
        let resp = rb.send().await?;
        let status = resp.status().as_u16();
        resp.error_for_status()?;
        Ok(status)
    }

    /// Search autocomplete suggestions for `query` via YouTube's public suggest endpoint.
    /// Returns the suggested completions (best-effort: empty on a blank query or any parse
    /// issue; only a transport error propagates). The `firefox` client yields clean JSON
    /// `["query", ["suggestion", ...], ...]` (no JSONP wrapper).
    pub async fn search_suggest(&self, query: &str) -> Result<Vec<String>, Error> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let text = self
            .http
            .get("https://suggestqueries.google.com/complete/search")
            .query(&[("client", "firefox"), ("ds", "yt"), ("q", query)])
            .send()
            .await?
            .text()
            .await?;
        let suggestions = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| {
                v.get(1).and_then(|a| a.as_array()).map(|a| {
                    a.iter().filter_map(|s| s.as_str().map(String::from)).collect::<Vec<_>>()
                })
            })
            .unwrap_or_default();
        Ok(suggestions)
    }

    /// Seed the cached `visitorData` from a page already fetched, so
    /// [`Self::bootstrap_visitor_data`] costs no homepage GET. That method reads
    /// `visitorData` from the homepage `ytcfg` once per session, since a browser keeps one
    /// for its whole session (IT-003).
    ///
    /// The last seed wins, so the cached value matches the page whose player response and
    /// challenge the session uses. `visitorData` follows the cookie jar, so pages agree.
    pub fn seed_visitor_data(&self, v: &str) {
        *self.visitor_cache.lock().unwrap() = Some(v.to_string());
    }

    /// Adopts `cfg` as the session's page config when it has none yet.
    pub fn seed_page_config(&self, cfg: PageClientConfig) {
        self.page_config.lock().unwrap().get_or_insert(cfg);
    }

    fn seed_page_config_from(&self, html: &str) {
        if let Some(cfg) = extract_client_config(html) {
            self.seed_page_config(cfg);
        }
    }

    /// Loads the homepage once when no page has given the session its `ytcfg` yet.
    async fn ensure_page_config(&self) {
        if self.page_config.lock().unwrap().is_some() {
            return;
        }
        if let Ok(html) = self.get_text("https://www.youtube.com/").await {
            if let Some(v) = find_json_string(&html, "\"visitorData\":\"") {
                self.visitor_cache.lock().unwrap().get_or_insert(v);
            }
        }
    }

    pub async fn bootstrap_visitor_data(&self) -> Result<Option<String>, Error> {
        if let Some(v) = self.visitor_cache.lock().unwrap().clone() {
            return Ok(Some(v));
        }
        let resp = self.http.get("https://www.youtube.com/").send().await?;
        let status = resp.status().as_u16();
        let html = resp.error_for_status()?.text().await?;
        if dump::enabled() {
            dump::record(
                "fetch",
                serde_json::json!({ "url": "https://www.youtube.com/", "status": status, "resp_bytes": html.len() }),
            );
        }
        self.seed_page_config_from(&html);
        let found = find_json_string(&html, "\"visitorData\":\"");
        if let Some(v) = &found {
            *self.visitor_cache.lock().unwrap() = Some(v.clone());
        }
        Ok(found)
    }

    /// Fetch the raw JSON body of the `player()` response for `video_id`.
    pub async fn player_raw(&self, video_id: &str, opts: &PlayerOptions<'_>) -> Result<String, Error> {
        let body = player_request_body(video_id, &self.client, opts);
        let resp = self
            .http
            .post(PLAYER_ENDPOINT)
            .header("x-youtube-client-name", self.client.client_name_id.to_string())
            .header("x-youtube-client-version", &self.client.client_version)
            .header("user-agent", &self.client.user_agent)
            .header("origin", "https://www.youtube.com")
            .json(&body)
            .send()
            .await?;
        let status = resp.status().as_u16();
        let text = resp.error_for_status()?.text().await?;
        if dump::enabled() {
            let mut req = body.clone();
            dump::scrub_json(&mut req);
            dump::record(
                "player",
                serde_json::json!({
                    "video_id": video_id,
                    "status": status,
                    "req": req,
                    "resp_bytes": text.len(),
                }),
            );
        }
        Ok(text)
    }

    /// Fetch and parse the `player()` response for `video_id`.
    pub async fn player(&self, video_id: &str, opts: &PlayerOptions<'_>) -> Result<PlayerResponse, Error> {
        Ok(PlayerResponse::parse(&self.player_raw(video_id, opts).await?)?)
    }

    /// The reference client's SPA-navigation POST: player + watch-next in one round trip.
    /// Returns both halves; the caller parses `watch_next` (e.g. with `api::parse_next`).
    pub async fn get_watch(
        &self,
        video_id: &str,
        opts: &PlayerOptions<'_>,
    ) -> Result<GetWatchResponse, Error> {
        let body = get_watch_request_body(video_id, &self.client, opts);
        let resp = self
            .http
            .post(GET_WATCH_ENDPOINT)
            .header("x-youtube-client-name", self.client.client_name_id.to_string())
            .header("x-youtube-client-version", &self.client.client_version)
            .header("user-agent", &self.client.user_agent)
            .header("origin", "https://www.youtube.com")
            .json(&body)
            .send()
            .await?;
        let status = resp.status().as_u16();
        let text = resp.error_for_status()?.text().await?;
        if dump::enabled() {
            let mut req = body.clone();
            dump::scrub_json(&mut req);
            dump::record(
                "get_watch",
                serde_json::json!({
                    "video_id": video_id,
                    "status": status,
                    "req": req,
                    "resp_bytes": text.len(),
                }),
            );
        }
        let player = PlayerResponse::parse(&extract_player_response(&text)?)?;
        let watch_next = extract_watch_next_response(&text);
        Ok(GetWatchResponse { player, watch_next })
    }

    /// POST a SABR media request body to `url`, returning the raw UMP response bytes.
    ///
    /// The `Origin`, `Referer` and `Sec-Fetch-*` headers are required: without them
    /// googlevideo answers 403 on some edge nodes (REQ-028, REQ-029).
    ///
    /// Low-level and hidden from the curated surface: `pub` for the SABR engine crate.
    /// Consumers use `SabrStream`.
    #[doc(hidden)]
    pub async fn post_sabr(&self, url: &str, body: Vec<u8>) -> Result<bytes::Bytes, Error> {
        let url = &with_request_number(url);
        let req_bytes = body.len();
        let raw = dump_sabr_request(url, &body);
        let resp = self
            .http
            .post(url)
            .header("origin", "https://www.youtube.com")
            .header("referer", "https://www.youtube.com/")
            .header("sec-fetch-site", "cross-site")
            .header("sec-fetch-mode", "cors")
            .header("sec-fetch-dest", "empty")
            .body(body)
            .send()
            .await?;
        let status = resp.status().as_u16();
        let bytes = resp.error_for_status()?.bytes().await?;
        if let Some((dir, n)) = &raw {
            let _ = std::fs::write(dir.join(format!("resp_{n:04}.bin")), &bytes);
        }
        if dump::enabled() {
            // HTTP-level only; the decoded per-round request/response summary is
            // dumped by the stream loop as `sabr_round` (no secrets there). The SABR
            // request body carries the PO token, so it is never dumped here.
            dump::record(
                "sabr",
                serde_json::json!({
                    "url": dump::scrub_url(url),
                    "req_bytes": req_bytes,
                    "status": status,
                    "resp_bytes": bytes.len(),
                }),
            );
        }
        Ok(bytes)
    }

    /// Like [`post_sabr`](Self::post_sabr), but returns the response body as an incremental
    /// [`SabrBody`] instead of buffering it whole. A reader that needs only the seek index
    /// can drop the body, which aborts the transfer, instead of downloading the server's
    /// whole warmup burst.
    ///
    /// Low-level and hidden from the curated surface.
    #[doc(hidden)]
    pub async fn post_sabr_stream(&self, url: &str, body: Vec<u8>) -> Result<SabrBody, Error> {
        let url = &with_request_number(url);
        let dump_tee = dump_sabr_request(url, &body);
        let resp = self
            .http
            .post(url)
            .header("origin", "https://www.youtube.com")
            .header("referer", "https://www.youtube.com/")
            .header("sec-fetch-site", "cross-site")
            .header("sec-fetch-mode", "cors")
            .header("sec-fetch-dest", "empty")
            .body(body)
            .send()
            .await?
            .error_for_status()?;
        Ok(SabrBody { resp, dump_tee, dump_buf: Vec::new() })
    }
}

/// An incremental SABR response body (see [`Session::post_sabr_stream`]). Pull chunks
/// with [`next_chunk`](Self::next_chunk); dropping it before the body is exhausted
/// aborts the underlying transfer.
pub struct SabrBody {
    resp: reqwest::Response,
    /// Raw response capture (`INNERTUBE_DUMP=raw:`): (dir, request index). Accumulates
    /// streamed chunks and writes `resp_NNNN.bin` on drop, even for an aborted stream.
    dump_tee: Option<(std::path::PathBuf, u32)>,
    dump_buf: Vec<u8>,
}

impl SabrBody {
    /// The next chunk of the body, or `None` at end of stream.
    pub async fn next_chunk(&mut self) -> Result<Option<bytes::Bytes>, Error> {
        let chunk = self.resp.chunk().await?;
        if self.dump_tee.is_some() {
            if let Some(c) = &chunk {
                self.dump_buf.extend_from_slice(c);
            }
        }
        Ok(chunk)
    }
}

impl Drop for SabrBody {
    fn drop(&mut self) {
        if let Some((dir, n)) = &self.dump_tee {
            let _ = std::fs::write(dir.join(format!("resp_{n:04}.bin")), &self.dump_buf);
        }
    }
}

/// Write a SABR request's body and url to `<dump>/raw/sabr/` when raw capture is on, and
/// return where its response belongs.
fn dump_sabr_request(url: &str, body: &[u8]) -> Option<(std::path::PathBuf, u32)> {
    let dir = dump::raw_dir("sabr")?;
    let n = dump::next_raw_index();
    let _ = std::fs::write(dir.join(format!("req_{n:04}.bin")), body);
    let _ = std::fs::write(dir.join(format!("url_{n:04}.txt")), url.as_bytes());
    Some((dir, n))
}
