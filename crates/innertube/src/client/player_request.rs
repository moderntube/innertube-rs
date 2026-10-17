//! The `player()` and `get_watch` request bodies and the page config they carry.

use super::*;

/// Per-request options for `player()`. Anonymous web SABR needs a bootstrapped
/// `visitorData` and a browser-like context, or the response is `UNPLAYABLE`.
#[derive(Debug, Default, Clone)]
pub struct PlayerOptions<'a> {
    /// `visitorData` echoed into the client context (bootstrap it first).
    pub visitor_data: Option<&'a str>,
    /// `signatureTimestamp` of the player JS used for n-sig. Must match, or the
    /// streaming URL is signed for a different player and 403s. Falls back to
    /// [`WEB_SIGNATURE_TIMESTAMP`] when `None`.
    pub signature_timestamp: Option<i64>,
    /// Reload token from a `RELOAD_PLAYER_RESPONSE` (UMP part 46). When set, it's
    /// echoed into `playbackContext.reloadPlaybackContext.reloadPlaybackParams.token`
    /// so the server re-issues a fresh streaming session (the server demands this
    /// mid-session to rotate the session config).
    pub reload_token: Option<&'a str>,
    /// Session PO token (base64url) bound to `visitorData`, sent in the player
    /// request's `serviceIntegrityDimensions.poToken` as browser attestation. The
    /// web client always sends it; without it the server withholds media
    /// (`STREAM_PROTECTION_STATUS` never reaches the attested `status:1`).
    pub po_token: Option<&'a str>,
    /// Experiment-assignment block scraped from the page's `ytcfg` (see
    /// [`extract_client_config`]). The reference client sends it on every `player()`
    /// request. Session-scoped, so it survives SPA reuse.
    pub client_config: Option<&'a PageClientConfig>,
    /// `watchEndpoint.playerParams` of the endpoint that started this playback.
    pub player_params: Option<&'a str>,
    /// URL of the page this playback started from.
    pub referer: Option<&'a str>,
    /// A feed/search hover preview: sends `vis: 5`, `autoplay: true`, `autonav: true`.
    pub preview: bool,
    /// A Short: every url of the request is `/shorts/<id>`.
    pub shorts: bool,
}

/// Where a playback was opened from: the page it started on, and the `playerParams` of
/// the endpoint that started it (a feed's inline preview carries some; a click none).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Navigation {
    pub player_params: Option<String>,
    pub referer: Option<String>,
    /// A feed/search hover preview, not a click.
    pub preview: bool,
    /// A Short, opened from the Shorts feed.
    pub short: bool,
    /// The player response JSON the Shorts sequence already delivered for this video.
    pub prefetched_player: Option<String>,
}

/// The experiment/config assignment the WEB client carries in `context.client` on every
/// `/youtubei/v1/player` request, scraped from a watch page's `ytcfg`. It identifies the
/// A/B groups the session belongs to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageClientConfig {
    /// `configInfo.appInstallData`: the packed experiment-assignment blob.
    pub app_install_data: Option<String>,
    /// `configInfo.coldConfigData`.
    pub cold_config_data: Option<String>,
    /// `configInfo.coldHashData`.
    pub cold_hash_data: Option<String>,
    /// `configInfo.hotHashData`.
    pub hot_hash_data: Option<String>,
    /// `deviceExperimentId`.
    pub device_experiment_id: Option<String>,
    /// `rolloutToken`.
    pub rollout_token: Option<String>,
}

impl PageClientConfig {
    /// True when nothing was found; the caller should then send no `configInfo` rather
    /// than an empty object.
    pub fn is_empty(&self) -> bool {
        self.app_install_data.is_none()
            && self.cold_config_data.is_none()
            && self.cold_hash_data.is_none()
            && self.hot_hash_data.is_none()
            && self.device_experiment_id.is_none()
            && self.rollout_token.is_none()
    }
}

/// Sets `context.client.configInfo` from `cfg` unless the body already carries one.
pub(super) fn add_config_info(body: &mut serde_json::Value, cfg: &PageClientConfig) {
    let Some(client) = body.pointer_mut("/context/client").and_then(|c| c.as_object_mut()) else { return };
    if client.contains_key("configInfo") {
        return;
    }
    let mut info = serde_json::Map::new();
    for (k, v) in [
        ("appInstallData", &cfg.app_install_data),
        ("coldConfigData", &cfg.cold_config_data),
        ("coldHashData", &cfg.cold_hash_data),
        ("hotHashData", &cfg.hot_hash_data),
    ] {
        if let Some(v) = v {
            info.insert(k.to_string(), serde_json::Value::String(v.clone()));
        }
    }
    if !info.is_empty() {
        client.insert("configInfo".to_string(), serde_json::Value::Object(info));
    }
}

/// Pull [`PageClientConfig`] out of a watch page's `ytcfg`
/// (`INNERTUBE_CONTEXT.client`). Returns `None` when the page carries no usable
/// `ytcfg`, or when it has one but none of the three fields.
pub(crate) fn extract_client_config(html: &str) -> Option<PageClientConfig> {
    let cfg: serde_json::Value = serde_json::from_str(&extract_yt_config(html)?).ok()?;
    let client = cfg.get("INNERTUBE_CONTEXT")?.get("client")?;
    let str_at = |v: &serde_json::Value| v.as_str().map(str::to_owned);
    let info = |k: &str| client.get("configInfo").and_then(|c| c.get(k)).and_then(str_at);
    let out = PageClientConfig {
        app_install_data: info("appInstallData"),
        cold_config_data: info("coldConfigData"),
        cold_hash_data: info("coldHashData"),
        hot_hash_data: info("hotHashData"),
        device_experiment_id: client.get("deviceExperimentId").and_then(str_at),
        rollout_token: client.get("rolloutToken").and_then(str_at),
    };
    (!out.is_empty()).then_some(out)
}

pub fn player_request_body(
    video_id: &str,
    client: &ClientConfig,
    opts: &PlayerOptions,
) -> serde_json::Value {
    // `pp=` repeats `playerParams` in every url of the request.
    let pp = opts.player_params.map(|p| format!("&pp={p}")).unwrap_or_default();
    let graft_url = if opts.shorts {
        format!("/shorts/{video_id}")
    } else {
        format!("/watch?v={video_id}{pp}")
    };
    let original_url = format!("https://www.youtube.com{graft_url}");

    let request = PlayerRequest {
        context: Context {
            client: Client {
                client_name: &client.client_name,
                client_version: &client.client_version,
                hl: &client.hl,
                gl: &client.gl,
                visitor_data: opts.visitor_data,
                user_agent: &client.user_agent,
                browser_name: "Chrome",
                browser_version: WEB_BROWSER_VERSION,
                os_name: "X11",
                os_version: "",
                platform: "DESKTOP",
                client_form_factor: "UNKNOWN_FORM_FACTOR",
                device_make: "",
                device_model: "",
                user_interface_theme: "USER_INTERFACE_THEME_DARK",
                time_zone: "America/New_York",
                utc_offset_minutes: 0,
                original_url: &original_url,
                screen_pixel_density: 1,
                screen_density_float: 1.0,
                screen_width_points: 1920,
                screen_height_points: 1080,
                window_width_points: 1920,
                memory_total_kbytes: "8000000",
                client_screen: "WATCH",
                application_state: "ACTIVE",
                accept_header:
                    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7",
                main_app_web_info: MainAppWebInfo {
                    graft_url: &graft_url,
                    pwa_installability_status: "PWA_INSTALLABILITY_STATUS_CAN_BE_INSTALLED",
                    web_display_mode: "WEB_DISPLAY_MODE_BROWSER",
                    is_web_native_share_available: false,
                },
                config_info: opts.client_config.filter(|c| c.app_install_data.is_some()).map(|c| {
                    ConfigInfo {
                        app_install_data: c.app_install_data.as_deref(),
                        cold_config_data: c.cold_config_data.as_deref(),
                        cold_hash_data: c.cold_hash_data.as_deref(),
                        hot_hash_data: c.hot_hash_data.as_deref(),
                    }
                }),
                device_experiment_id: opts
                    .client_config
                    .and_then(|c| c.device_experiment_id.as_deref()),
                rollout_token: opts.client_config.and_then(|c| c.rollout_token.as_deref()),
            },
        },
        video_id,
        params: opts.player_params,
        content_check_ok: true,
        racy_check_ok: true,
        playback_context: PlaybackContext {
            content_playback_context: ContentPlaybackContext {
                current_url: &graft_url,
                vis: if opts.preview { 5 } else { 0 },
                splay: false,
                auto_captions_default_on: false,
                autonav_state: "STATE_NONE",
                autoplay: opts.preview.then_some(true),
                autonav: opts.preview.then_some(true),
                html5_preference: "HTML5_PREF_WANTS",
                signature_timestamp: opts.signature_timestamp.unwrap_or(WEB_SIGNATURE_TIMESTAMP),
                referer: opts.referer.unwrap_or(""),
                lact_milliseconds: "-1",
                watch_ambient_mode_context: WatchAmbientModeContext {
                    watch_ambient_mode_enabled: true,
                },
            },
            device_playback_capabilities: DevicePlaybackCapabilities {
                supports_vp9_encoding: true,
                support_xhr: true,
            },
            reload_playback_context: opts.reload_token.map(|token| ReloadPlaybackContext {
                reload_playback_params: ReloadPlaybackParams { token },
            }),
        },
        service_integrity_dimensions: opts
            .po_token
            .map(|po_token| ServiceIntegrityDimensions { po_token }),
    };
    serde_json::to_value(request).expect("player request serializes")
}

/// Build the JSON body for a `get_watch` request, the SPA-navigation shape of the
/// reference client (`{context, playerRequest, watchNextRequest}`, IT-006).
///
/// The `context` and the player half are what [`player_request_body`] builds; this lifts
/// the context up a level and adds the watch-next half beside it, so the two endpoints
/// send the same fields.
pub fn get_watch_request_body(
    video_id: &str,
    client: &ClientConfig,
    opts: &PlayerOptions,
) -> serde_json::Value {
    let mut player = player_request_body(video_id, client, opts);
    let context = player
        .as_object_mut()
        .and_then(|o| o.remove("context"))
        .unwrap_or(serde_json::Value::Null);
    serde_json::json!({
        "context": context,
        "playerRequest": player,
        // The reference client always sends this half; the endpoint is defined by the pair.
        "watchNextRequest": {
            "videoId": video_id,
            "racyCheckOk": true,
            "contentCheckOk": true,
            "autonavState": "STATE_NONE",
            "playbackContext": { "vis": 0, "lactMilliseconds": -1 },
            "captionsRequested": false,
        },
    })
}

/// A `get_watch` response's two halves.
#[derive(Debug, Clone)]
pub struct GetWatchResponse {
    pub player: PlayerResponse,
    /// The raw `watchNextResponse` sub-response, when the batch carried one.
    pub watch_next: Option<serde_json::Value>,
}

pub(super) fn extract_player_response(text: &str) -> Result<String, PlayerError> {
    let v: serde_json::Value = serde_json::from_str(text)?;
    let found = match &v {
        serde_json::Value::Array(items) => items.iter().find_map(|el| el.get("playerResponse")),
        // A single object (or a non-batched shape) is accepted directly.
        _ => v.get("playerResponse"),
    };
    match found {
        Some(p) => Ok(p.to_string()),
        None => Err(PlayerError::NoPlayerResponse),
    }
}

/// Pull the `watchNextResponse` sub-response out of a `get_watch` body, mirroring
/// [`extract_player_response`]'s array/object handling.
pub(super) fn extract_watch_next_response(text: &str) -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    match &v {
        serde_json::Value::Array(items) => {
            items.iter().find_map(|el| el.get("watchNextResponse")).cloned()
        }
        _ => v.get("watchNextResponse").cloned(),
    }
}

#[cfg(test)]
mod get_watch_split_tests {
    use super::*;

    const BATCH: &str = r#"[
        {"responseContext": {}, "playerResponse": {"videoDetails": {"videoId": "abc"}}},
        {"responseContext": {}, "watchNextResponse": {"contents": {"marker": "sidebar"}}}
    ]"#;

    // IT-006: get_watch answers with `[{playerResponse}, {watchNextResponse}]`.
    #[test]
    fn splits_player_and_watch_next_out_of_the_batch() {
        let player = extract_player_response(BATCH).expect("player response");
        assert!(player.contains("\"videoId\":\"abc\""));
        let next = extract_watch_next_response(BATCH).expect("watch-next response");
        assert_eq!(next["contents"]["marker"], "sidebar");
    }

}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlayerRequest<'a> {
    context: Context<'a>,
    video_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<&'a str>,
    content_check_ok: bool,
    racy_check_ok: bool,
    playback_context: PlaybackContext<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service_integrity_dimensions: Option<ServiceIntegrityDimensions<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceIntegrityDimensions<'a> {
    po_token: &'a str,
}

#[derive(Serialize)]
struct Context<'a> {
    client: Client<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Client<'a> {
    client_name: &'a str,
    client_version: &'a str,
    hl: &'a str,
    gl: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    visitor_data: Option<&'a str>,
    user_agent: &'a str,
    browser_name: &'a str,
    browser_version: &'a str,
    os_name: &'a str,
    os_version: &'a str,
    platform: &'a str,
    client_form_factor: &'a str,
    device_make: &'a str,
    device_model: &'a str,
    user_interface_theme: &'a str,
    time_zone: &'a str,
    utc_offset_minutes: i64,
    original_url: &'a str,
    // Device-capability signals the WEB client sends.
    screen_pixel_density: i64,
    screen_density_float: f64,
    screen_width_points: i64,
    screen_height_points: i64,
    window_width_points: i64,
    memory_total_kbytes: &'a str,
    client_screen: &'a str,
    /// `ACTIVE` when the tab is in the foreground. The reference client sends it on every
    /// navigation POST.
    application_state: &'a str,
    accept_header: &'a str,
    main_app_web_info: MainAppWebInfo<'a>,
    // Experiment assignment. Omitted when the page yielded none.
    #[serde(skip_serializing_if = "Option::is_none")]
    config_info: Option<ConfigInfo<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_experiment_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rollout_token: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigInfo<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    app_install_data: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cold_config_data: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cold_hash_data: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hot_hash_data: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MainAppWebInfo<'a> {
    graft_url: &'a str,
    pwa_installability_status: &'a str,
    web_display_mode: &'a str,
    is_web_native_share_available: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlaybackContext<'a> {
    content_playback_context: ContentPlaybackContext<'a>,
    device_playback_capabilities: DevicePlaybackCapabilities,
    #[serde(skip_serializing_if = "Option::is_none")]
    reload_playback_context: Option<ReloadPlaybackContext<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReloadPlaybackContext<'a> {
    reload_playback_params: ReloadPlaybackParams<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReloadPlaybackParams<'a> {
    token: &'a str,
}

/// The reference client's `contentPlaybackContext`, as sent on an SPA `get_watch` POST.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ContentPlaybackContext<'a> {
    current_url: &'a str,
    /// Player visibility, 0-100. The capture sends 0 on a fresh navigation.
    vis: u32,
    /// Whether playback was started by an explicit user gesture.
    splay: bool,
    auto_captions_default_on: bool,
    autonav_state: &'a str,
    /// Sent (`true`) only on a feed/search hover preview.
    #[serde(skip_serializing_if = "Option::is_none")]
    autoplay: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    autonav: Option<bool>,
    html5_preference: &'a str,
    signature_timestamp: i64,
    /// The page navigated from; empty on a cold open.
    #[serde(skip_serializing_if = "str::is_empty")]
    referer: &'a str,
    /// Milliseconds since the last user action; `-1` means "no action yet". Serialized as
    /// a string, as in the capture.
    lact_milliseconds: &'a str,
    watch_ambient_mode_context: WatchAmbientModeContext,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WatchAmbientModeContext {
    watch_ambient_mode_enabled: bool,
}

/// Sent beside `contentPlaybackContext` in the capture.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DevicePlaybackCapabilities {
    supports_vp9_encoding: bool,
    support_xhr: bool,
}

#[cfg(test)]
mod client_config_tests {
    use super::*;

    const PAGE: &str = r#"<script>ytcfg.set({"EVENT_ID":"e1","INNERTUBE_CONTEXT":{"client":{
        "hl":"en","configInfo":{"appInstallData":"CL6y6NAG"},
        "deviceExperimentId":"ChxOelkw","rolloutToken":"CMeWguPc"}}});</script>"#;

    #[test]
    fn scrapes_the_experiment_block_from_ytcfg() {
        let c = extract_client_config(PAGE).expect("client config");
        assert_eq!(c.app_install_data.as_deref(), Some("CL6y6NAG"));
        assert_eq!(c.device_experiment_id.as_deref(), Some("ChxOelkw"));
        assert_eq!(c.rollout_token.as_deref(), Some("CMeWguPc"));
    }

    // IT-001: `configInfo`, `deviceExperimentId` and `rolloutToken` ride in `context.client`.
    #[test]
    fn player_body_carries_the_experiment_block() {
        let cfg = extract_client_config(PAGE).expect("client config");
        let opts = PlayerOptions { client_config: Some(&cfg), ..Default::default() };
        let body = player_request_body("abc", &ClientConfig::web(), &opts);
        let client = &body["context"]["client"];
        assert_eq!(client["configInfo"]["appInstallData"], "CL6y6NAG");
        assert_eq!(client["deviceExperimentId"], "ChxOelkw");
        assert_eq!(client["rolloutToken"], "CMeWguPc");
    }

    /// A page with no experiment fields sends no `configInfo` object at all.
    #[test]
    fn player_body_omits_the_block_entirely_when_absent() {
        let body = player_request_body("abc", &ClientConfig::web(), &PlayerOptions::default());
        let client = &body["context"]["client"];
        assert!(client.get("configInfo").is_none(), "{client:?}");
        assert!(client.get("deviceExperimentId").is_none());
        assert!(client.get("rolloutToken").is_none());
    }

    // IT-007: a preview opened from a feed names the watch URL and the page it came from.
    #[test]
    fn player_body_carries_the_navigation() {
        let opts = PlayerOptions {
            player_params: Some("YAHIAQGiBhUB15olE_sT%3D"),
            referer: Some("https://www.youtube.com/"),
            ..Default::default()
        };
        let body = player_request_body("abc", &ClientConfig::web(), &opts);
        let client = &body["context"]["client"];
        let cpc = &body["playbackContext"]["contentPlaybackContext"];
        assert_eq!(body["params"], "YAHIAQGiBhUB15olE_sT%3D");
        assert_eq!(client["originalUrl"], "https://www.youtube.com/watch?v=abc&pp=YAHIAQGiBhUB15olE_sT%3D");
        assert_eq!(client["mainAppWebInfo"]["graftUrl"], "/watch?v=abc&pp=YAHIAQGiBhUB15olE_sT%3D");
        assert_eq!(cpc["currentUrl"], "/watch?v=abc&pp=YAHIAQGiBhUB15olE_sT%3D");
        assert_eq!(cpc["referer"], "https://www.youtube.com/");
    }

    // IT-007: a Short names `/shorts/<id>` as `originalUrl` and `currentUrl`.
    #[test]
    fn player_body_for_a_short_uses_shorts_urls() {
        let opts = PlayerOptions { player_params: Some("8AEByAMT"), shorts: true, ..Default::default() };
        let body = player_request_body("abc", &ClientConfig::web(), &opts);
        let client = &body["context"]["client"];
        let cpc = &body["playbackContext"]["contentPlaybackContext"];
        assert_eq!(body["params"], "8AEByAMT");
        assert_eq!(client["originalUrl"], "https://www.youtube.com/shorts/abc");
        assert_eq!(client["mainAppWebInfo"]["graftUrl"], "/shorts/abc");
        assert_eq!(cpc["currentUrl"], "/shorts/abc");
    }

    // IT-007: an inline preview sends `vis` 5 and `autoplay` true.
    #[test]
    fn player_body_preview_sends_vis_and_autoplay() {
        let opts = PlayerOptions { preview: true, ..Default::default() };
        let body = player_request_body("abc", &ClientConfig::web(), &opts);
        let cpc = &body["playbackContext"]["contentPlaybackContext"];
        assert_eq!(cpc["vis"], 5);
        assert_eq!(cpc["autoplay"], true);
    }

}
