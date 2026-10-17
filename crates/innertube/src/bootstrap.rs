//! The SABR session bootstrap the live and VOD paths share: watch page, `base.js`,
//! `player()`, the n-descrambled streaming URL, and the caches behind them.

use std::sync::Arc;
use std::time::Instant;

use crate::client::{Error as CoreError, Navigation, PlayerOptions, PlayerResponse, Session};
use innertube_js::{extract_player_js_url, extract_signature_timestamp, JsError, NSigSolver};
use innertube_pot::{PoTokenProvider, PotError};

/// Errors from opening or running a playback session; [`StreamError::kind`] classifies them.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StreamError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error("n-sig: {0}")]
    Js(#[from] JsError),
    #[error("po token: {0}")]
    Pot(#[from] PotError),
    #[error("bootstrap returned no visitorData")]
    MissingVisitorData,
    #[error("watch page had no player jsUrl")]
    NoPlayerJsUrl,
    #[error("player response had no video formats")]
    NoVideoFormats,
    #[error("player response had no audio formats")]
    NoAudioFormats,
    #[error("PO token was not valid base64")]
    BadPoToken,
    #[error("seekable VOD player requires a non-live video")]
    NotVod,
    #[error("live stream player requires a live video")]
    NotLive,
    /// A running session gave up after `attempts` consecutive failed SABR requests.
    #[error("{attempts} requests failed in a row: {last}")]
    RequestsFailed { attempts: u32, last: CoreError },
}

/// The failure classes of a [`StreamError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamErrorKind {
    /// Transport failure (connect, timeout, reset, HTTP 5xx). A retry can succeed.
    Network,
    /// YouTube refused this client: unplayable, login or age gate, attestation or PO
    /// token, HTTP 401/403/429.
    Refused,
    /// The video is gone, offline, or has no stream this player can play.
    Unavailable,
    /// A response we cannot parse, or a failure inside our own code.
    Internal,
}

impl StreamError {
    pub fn kind(&self) -> StreamErrorKind {
        use StreamErrorKind::*;
        match self {
            Self::Core(e) | Self::RequestsFailed { last: e, .. } => core_kind(e),
            Self::Pot(e) => pot_kind(e),
            Self::BadPoToken => Refused,
            Self::NoVideoFormats | Self::NoAudioFormats | Self::NotVod | Self::NotLive => Unavailable,
            Self::Js(_) | Self::MissingVisitorData | Self::NoPlayerJsUrl => Internal,
        }
    }

    /// The HTTP status of a failed request, when the failure is one.
    pub fn http_status(&self) -> Option<u16> {
        match self {
            Self::Core(e) | Self::RequestsFailed { last: e, .. } => core_http_status(e),
            _ => None,
        }
    }

    /// The playability status YouTube refused the video with, when that is the failure.
    pub fn playability(&self) -> Option<&crate::client::Playability> {
        match self {
            Self::Core(CoreError::Player(crate::client::PlayerError::Unplayable { playability })) => {
                Some(playability)
            }
            _ => None,
        }
    }
}

fn core_http_status(e: &CoreError) -> Option<u16> {
    match e {
        CoreError::Http(h) => h.status().map(|s| s.as_u16()),
        CoreError::InnerTube { code, .. } => u16::try_from(*code).ok(),
        _ => None,
    }
}

fn pot_kind(e: &PotError) -> StreamErrorKind {
    use StreamErrorKind::*;
    match e {
        PotError::Http(h) if h.is_decode() => Internal,
        PotError::Http(h) => match h.status().map(|s| s.as_u16()) {
            Some(401 | 403 | 429) => Refused,
            _ => Network,
        },
        PotError::Refused(_) => Refused,
        _ => Internal,
    }
}

fn core_kind(e: &CoreError) -> StreamErrorKind {
    use crate::client::{Playability, PlayerError};
    use StreamErrorKind::*;
    match e {
        CoreError::Http(_) | CoreError::InnerTube { .. } => match core_http_status(e) {
            Some(401 | 403 | 429) => Refused,
            Some(404 | 410) => Unavailable,
            Some(500..=599) => Network,
            Some(_) => Internal,
            // A request that got no response is a transport failure.
            None if matches!(e, CoreError::Http(_)) => Network,
            None => Internal,
        },
        CoreError::Auth(_) | CoreError::NotLoggedIn => Refused,
        CoreError::Player(PlayerError::Unplayable { playability }) => match playability {
            // `ERROR` is the status of a removed or nonexistent video.
            Playability::LiveStreamOffline { .. } => Unavailable,
            Playability::Other { status, .. } if status == "ERROR" => Unavailable,
            _ => Refused,
        },
        CoreError::Player(PlayerError::NotStreamable) => Unavailable,
        CoreError::Player(_) | CoreError::Json(_) | CoreError::Api(_) => Internal,
    }
}

/// A coarse startup stopwatch: emits per-phase deltas + a running total as `debug!`
/// events.
pub struct StartupTimer {
    t0: Instant,
    last: Instant,
}

impl Default for StartupTimer {
    fn default() -> Self {
        Self::new()
    }
}

impl StartupTimer {
    pub fn new() -> Self {
        let now = Instant::now();
        Self { t0: now, last: now }
    }

    /// Mark the end of a phase, logging its duration since the previous lap and the
    /// cumulative startup time so far.
    pub fn lap(&mut self, label: &str) {
        let now = Instant::now();
        let step = (now - self.last).as_secs_f64() * 1000.0;
        let total = (now - self.t0).as_secs_f64() * 1000.0;
        tracing::debug!("[startup] {label:<24} +{step:>7.0}ms   (total {total:>7.0}ms)");
        self.last = now;
    }
}


pub(crate) struct Bootstrap {
    pub player: crate::client::PlayerResponse,
    pub url: String,
    pub base_js: Option<String>,
    /// Where `base_js` came from. The build id is in the path, so a re-bootstrap can tell
    /// whether a fresh watch page serves the same build (reuse `base_js`) or another one
    /// (refetch).
    pub base_js_url: Option<String>,
    pub visitor_data: String,
    /// The watch page's embedded BotGuard challenge, when the page carried one. The GVS PO
    /// token must be minted against it: a token from a fresh `/att/get` challenge is
    /// rejected (POT-003). It comes from the same fetch as `base_js` and the player response,
    /// because it is bound to that page load.
    pub bg_challenge: Option<innertube_pot::PageAttestation>,
    /// Metadata/sidebar from the `get_watch` POST's watch-next half, when that path
    /// was taken and the response carried one.
    pub watch_details: Option<crate::client::VideoDetails>,
}

/// The `signatureTimestamp` of the player build the session's funding page names. A Shorts
/// sequence request needs it, because the player responses it prefetches are signed for that
/// build (PLAYER-004). `base.js` comes through the process-wide cache, whose entries never go
/// stale because build URLs are content-addressed (PLAYER-001).
pub async fn session_signature_timestamp(session: &Session) -> Option<i64> {
    let page = fetch_preview_page(session).await.ok()?;
    let url = extract_player_js_url(&page)?;
    let js = match base_js_cached(&url) {
        Some(js) => js,
        None => {
            let js = session.get_text(&url).await.ok()?;
            base_js_store(&url, &js);
            js
        }
    };
    extract_signature_timestamp(&js)
}

fn base_js_cached(url: &str) -> Option<String> {
    BASE_JS_CACHE.lock().unwrap().iter().find(|(u, _)| u == url).map(|(_, js)| js.clone())
}
fn base_js_store(url: &str, js: &str) {
    let mut cache = BASE_JS_CACHE.lock().unwrap();
    if cache.len() >= 6 {
        cache.remove(0);
    }
    cache.push((url.to_string(), js.to_string()));
}
static BASE_JS_CACHE: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

/// Pays the video-independent part of a cold start before the user clicks.
///
/// The `base.js` fetch and the first n-sig solve depend on the player build, not the video.
/// The QuickJS-evaluated player stays resident, so later solves are the transform alone
/// (~2 ms). `visitorData` is session-scoped too. Warming all three while the user browses
/// removes most of the video-independent startup cost.
///
/// Best-effort and idempotent: each cache it fills is one the real bootstrap consults, so a
/// failure only means the click pays the cost. Returns the `base.js` URL it warmed.
pub async fn prewarm_session(session: &Session, solver: &NSigSolver) -> Option<String> {
    // /iframe_api names a player build without a video id, the same build the watch page
    // embeds. If the watch page names another build, this is a miss and the bootstrap
    // fetches as usual.
    let api_js = session.get_text("https://www.youtube.com/iframe_api").await.ok()?;
    let candidates = innertube_js::player_js_urls_from_iframe_api(&api_js)?;

    // Candidate order matters. `player_es6` and `player_ias` are different files under one
    // build id (PLAYER-002), so warming the wrong one caches a URL the bootstrap never
    // requests. A browser-like WEB session is served `player_es6`, which the helper lists
    // first. `tests/prewarm.rs` checks this against `extract_player_js_url`.
    let mut warmed = None;
    for url in candidates.iter() {
        if let Some(hit) = base_js_cached(url) {
            warmed = Some((url.clone(), hit));
            break;
        }
        if let Ok(fetched) = session.get_text(url).await {
            base_js_store(url, &fetched);
            warmed = Some((url.clone(), fetched));
            break;
        }
    }
    let (url, base_js) = warmed?;

    // Solving a throwaway challenge is what makes the evaluated player resident; the
    // transformed value is discarded. This is the expensive half.
    solver.solve_n(&base_js, &["warmup".to_string()]).await.ok()?;

    // Session-scoped, and seeding the cache keeps the fallback path free as well.
    let _ = session.bootstrap_visitor_data().await;
    Some(url)
}

/// How long a fetched watch page funds later opens (ms). Its challenge stays mintable for
/// at least 15 minutes (POT-005); the live loop clears the pages on `prot != 1`.
const WATCH_PAGE_CACHE_MS: u64 = 60_000;

/// A cached page for `video_id` (flag `true`), or else this session's newest page (`false`):
/// the web client funds every later video from the page it loaded, minus its SSR response.
fn watch_page_lookup(session: &Session, video_id: &str) -> Option<(String, bool)> {
    session.watch_pages().lookup(video_id, WATCH_PAGE_CACHE_MS, true)
}

/// Does this HTML belong to `video_id`'s own watch page? The SPA-reuse path hands back
/// another video's page, and the caller must then skip its SSR player response.
///
/// This compares the page's own id. A watch page embeds the id of every sidebar video, so a
/// substring test is true for each recommendation, and would play the funding video's
/// `ytInitialPlayerResponse` under the requested id.
fn watch_page_owned_by(video_id: &str, html: &str) -> bool {
    watch_page_video_id(html).is_some_and(|own| own == video_id)
}

/// The id of the video a watch page is *for*. Canonical link first (short, unambiguous,
/// present on every watch page); `videoDetails.videoId` as the fallback.
fn watch_page_video_id(html: &str) -> Option<&str> {
    const CANON: &str = r#"<link rel="canonical" href="https://www.youtube.com/watch?v="#;
    if let Some(at) = html.find(CANON) {
        let rest = &html[at + CANON.len()..];
        if let Some(end) = rest.find('"') {
            return Some(&rest[..end]);
        }
    }
    // `"videoDetails":{...,"videoId":"ID"`: the first videoId after videoDetails belongs
    // to the page's own player response.
    let at = html.find(r#""videoDetails""#)?;
    let rest = &html[at..];
    let key = r#""videoId":""#;
    let vid_at = rest.find(key)?;
    let rest = &rest[vid_at + key.len()..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn watch_page_store(session: &Session, video_id: &str, html: &str) {
    session.watch_pages().store(video_id, html);
}

/// Fetch the watch page, or reuse one cached on the session. Its BotGuard challenge is bound
/// to that page load's `EVENT_ID`: a token minted on a stale one gets `prot=2`, then `prot=3`.
async fn fetch_watch_page(session: &Session, video_id: &str) -> Result<String, crate::client::Error> {
    if let Some((html, same_video)) = watch_page_lookup(session, video_id) {
        if same_video {
            tracing::debug!("[watch-page] reusing a cached page for {video_id} ({} B)", html.len());
            return Ok(html);
        }
        // SPA click: another video's page funds this one (attestation, visitorData, base.js
        // URL); the caller plays the POST response.
        if !session.direct_opens() {
            tracing::debug!("[watch-page] SPA reuse: serving {video_id} from another video's cached page ({} B)", html.len());
            return Ok(html);
        }
    }
    let html = session.get_text(&format!("https://www.youtube.com/watch?v={video_id}")).await?;
    watch_page_store(session, video_id, &html);
    Ok(html)
}

/// Clicks POST `get_watch`; inline previews and Shorts POST `/player`, as the reference client does.
fn uses_get_watch(nav: &Navigation) -> bool {
    !nav.preview && !nav.short
}

/// The page that funds an inline preview: any fresh cached page, else the homepage. The
/// reference client previews from the homepage it already holds and never fetches `/watch`.
async fn fetch_preview_page(session: &Session) -> Result<String, crate::client::Error> {
    if let Some((html, _)) = watch_page_lookup(session, "") {
        return Ok(html);
    }
    let html = session.get_text("https://www.youtube.com/").await?;
    watch_page_store(session, "", &html);
    Ok(html)
}

/// Bootstrap a SABR session for `video_id`: visitorData, WEB `base.js` (for the
/// matching `signatureTimestamp` + n-sig), `player()`, and the n-descrambled POST
/// url.
pub(crate) async fn bootstrap_player(
    session: &Session,
    solver: &NSigSolver,
    video_id: &str,
    nav: &Navigation,
    session_pot: Option<&Arc<dyn PoTokenProvider>>,
    timer: &mut StartupTimer,
) -> Result<Bootstrap, StreamError> {
    // The n-sig solver fails on some `base.js` builds (PLAYER-011). A fresh watch-page fetch
    // may name a build it handles, so the whole bootstrap retries a bounded number of times.
    // `base.js` and the player response come from one fetch so the URL signature matches the
    // solved build.
    const MAX_NSIG_ATTEMPTS: u32 = 3;
    let mut attempt = 0;
    loop {
        attempt += 1;
        match bootstrap_attempt(
            session,
            solver,
            video_id,
            nav,
            session_pot,
            timer,
        )
        .await
        {
            Ok(b) => return Ok(b),
            Err(e) if should_retry_nsig(&e, attempt, MAX_NSIG_ATTEMPTS) => {
                timer.lap("n-sig retry (flaky build)");
                continue;
            }
            Err(e) => return Err(e),
        }
    }
}

/// One bootstrap attempt for [`bootstrap_player`]: fetch the WEB `base.js` and player
/// response and descramble the SABR url's `n`. A separate function so an n-sig solver
/// failure can retry the whole sequence with a fresh fetch.
async fn bootstrap_attempt(
    session: &Session,
    solver: &NSigSolver,
    video_id: &str,
    nav: &Navigation,
    session_pot: Option<&Arc<dyn PoTokenProvider>>,
    timer: &mut StartupTimer,
) -> Result<Bootstrap, StreamError> {
    // base.js comes first so player()'s signatureTimestamp matches the n-sig build
    // (PLAYER-004). The watch HTML is kept for its server-rendered player response.
    //
    // `ssr_allowed` goes false when the HTML held is not this video's (SPA reuse): its
    // embedded player response would play the wrong video.
    let mut ssr_allowed = true;
    let mut watch_html: Option<String> = None;
    let mut base_js_url: Option<String> = None;
    let base_js = {
        // A Short is funded like a preview: no `/shorts/<id>` page loads while swiping (POT-004).
        let page = if nav.preview || nav.short {
            fetch_preview_page(session).await
        } else {
            fetch_watch_page(session, video_id).await
        };
        match page {
            Ok(watch) => {
                if nav.preview || nav.short {
                    ssr_allowed = false;
                    timer.lap(if nav.short { "funding page (short)" } else { "funding page (preview)" });
                } else if !watch_page_owned_by(video_id, &watch) {
                    ssr_allowed = false;
                    timer.lap("watch page (SPA reuse)");
                } else {
                    timer.lap("watch page");
                }
                let js_url = extract_player_js_url(&watch).ok_or(StreamError::NoPlayerJsUrl)?;
                let js = match base_js_cached(&js_url) {
                    Some(js) => {
                        timer.lap("base.js (cached)");
                        js
                    }
                    None => {
                        let js = session.get_text(&js_url).await?;
                        base_js_store(&js_url, &js);
                        timer.lap("base.js fetch");
                        js
                    }
                };
                watch_html = Some(watch);
                base_js_url = Some(js_url);
                Some(js)
            }
            // A walled watch page (a google.com/sorry 429 on /watch while the API and player
            // JS still answer, IT-024) falls back to the player JS via /iframe_api and the
            // player() POST below.
            Err(e) => {
                tracing::warn!("watch page unavailable ({e}); falling back to iframe_api bootstrap");
                let api_js =
                    session.get_text("https://www.youtube.com/iframe_api").await.map_err(|_| e)?;
                let candidates = innertube_js::player_js_urls_from_iframe_api(&api_js)
                    .ok_or(StreamError::NoPlayerJsUrl)?;
                let mut js = None;
                for url in &candidates {
                    if let Some(cached) = base_js_cached(url) {
                        js = Some(cached);
                        base_js_url = Some(url.clone());
                        break;
                    }
                    if let Ok(fetched) = session.get_text(url).await {
                        base_js_store(url, &fetched);
                        js = Some(fetched);
                        base_js_url = Some(url.clone());
                        break;
                    }
                }
                timer.lap("base.js fetch (iframe_api fallback)");
                Some(js.ok_or(StreamError::NoPlayerJsUrl)?)
            }
        }
    };
    let sts = base_js.as_deref().and_then(extract_signature_timestamp);

    // `visitorData` comes out of the watch page we just fetched; only a session that could
    // not get one (the iframe_api wall path) pays for the separate homepage GET. Seeding the
    // Session cache keeps every later `bootstrap_visitor_data()` free.
    let visitor_data = match watch_html.as_deref().and_then(crate::client::extract_visitor_data) {
        Some(v) => {
            session.seed_visitor_data(&v);
            timer.lap("visitorData (from watch page)");
            v
        }
        None => {
            let v = session.bootstrap_visitor_data().await?.ok_or(StreamError::MissingVisitorData)?;
            timer.lap("visitorData (homepage fetch)");
            v
        }
    };
    let visitor_data = visitor_data.as_str();

    // Prefer the player response the watch page renders (`ytInitialPlayerResponse`): the
    // reference client plays from it (IT-005), and its SABR session starts with media at
    // once, where a `player()` POST session can start with empty rounds (IT-028). Fall back
    // to the POST when the page embeds none (age-gated or login-required). Hover previews
    // provision through the POST and get media on round 0.
    // The page's experiment assignment is session-scoped, so a page reused for another
    // video still carries the right one.
    let client_config = watch_html.as_deref().and_then(crate::client::extract_client_config);
    match &client_config {
        Some(c) => tracing::debug!(
            "[startup] ytcfg client config: appInstallData={} deviceExperimentId={} rolloutToken={}",
            c.app_install_data.as_ref().map_or(0, |s| s.len()),
            c.device_experiment_id.as_ref().map_or(0, |s| s.len()),
            c.rollout_token.as_ref().map_or(0, |s| s.len()),
        ),
        None => tracing::debug!("[startup] ytcfg client config: none"),
    }
    // The challenge is bound to this page load's EVENT_ID, so it must come from the same
    // fetch as base.js and the player response (POT-003). It is resolved before the
    // `player()` POST because that request is attested: its token must be minted against
    // this challenge, or the provisioned session withholds media for its first rounds. The
    // reference client attests its `get_watch` POST too (POT-002).
    let mut bg_challenge = watch_html.as_deref().and_then(|h| {
        Some(innertube_pot::PageAttestation {
            challenge: crate::client::extract_bg_challenge(h)?,
            yt_config: crate::client::extract_yt_config(h)?,
        })
    });
    // A session with no challenge fails: the mint takes bgutil's `/att/get` fallback, the
    // server rejects that token (POT-003), and every re-mint repeats it, so status goes 2 and
    // then 3 with no media. This happens after the iframe_api bootstrap (no watch page) or
    // when the page's `bgChallenge` or `ytcfg` could not be read. The binding is
    // session-scoped, so the homepage's challenge on the same Session is accepted (POT-004).
    if bg_challenge.is_none() {
        match session.get_text("https://www.youtube.com/").await {
            Ok(home) => {
                bg_challenge = crate::client::extract_bg_challenge(&home)
                    .zip(crate::client::extract_yt_config(&home))
                    .map(|(challenge, yt_config)| innertube_pot::PageAttestation {
                        challenge,
                        yt_config,
                    });
                match bg_challenge {
                    Some(_) => tracing::warn!(
                        "[attest] no challenge on this video's page — using the homepage's \
                         (the binding is session-scoped)"
                    ),
                    None => tracing::warn!(
                        "[attest] no challenge on this video's page OR the homepage — the \
                         mint will go through /att/get and the server will refuse media"
                    ),
                }
            }
            Err(e) => tracing::warn!(
                "[attest] no challenge on this video's page and the homepage is unreachable \
                 ({e}) — the mint will go through /att/get and the server will refuse media"
            ),
        }
    }
    let ssr = if !ssr_allowed {
        None
    } else {
        watch_html.as_deref().and_then(PlayerResponse::from_watch_html)
    };
    let ssr_used = ssr.is_some();
    let prefetched = nav.prefetched_player.as_deref().and_then(|j| PlayerResponse::parse(j).ok());
    let (player, watch_details) = match (ssr, prefetched) {
        (Some(ssr), _) => {
            timer.lap("player (SSR)");
            (ssr, None)
        }
        (None, Some(p)) => {
            timer.lap("player (prefetched by the Shorts sequence)");
            (p, None)
        }
        (None, None) => {
            // Minted only here: the SSR path never needs it. It uses the page attestation
            // resolved above; without it the mint takes the `/att/get` fallback (POT-003).
            let posted_pot = match session_pot {
                Some(p) => p.get_po_token(visitor_data, bg_challenge.as_ref()).await.ok(),
                None => None,
            };
            // On an SPA navigation the reference client POSTs get_watch (IT-006).
            let use_get_watch = uses_get_watch(nav);
            let opts = PlayerOptions {
                visitor_data: Some(visitor_data),
                signature_timestamp: sts,
                // The reference client attests this request (POT-002).
                po_token: posted_pot.as_deref(),
                client_config: client_config.as_ref(),
                player_params: nav.player_params.as_deref(),
                referer: nav.referer.as_deref(),
                preview: nav.preview,
                shorts: nav.short,
                ..Default::default()
            };
            if use_get_watch {
                let resp = session.get_watch(video_id, &opts).await?;
                timer.lap("get_watch POST");
                let details = resp.watch_next.as_ref().map(crate::client::parse_next);
                (resp.player, details)
            } else {
                let p = session.player(video_id, &opts).await?;
                timer.lap("player() POST");
                (p, None)
            }
        }
    };

    // Log the size of the provisioned ustreamer config. In the reference client's captures,
    // small configs reach the first media sooner than large ones.
    tracing::debug!(
        "[startup] ustreamer config: {} B (source: {})",
        player.ustreamer_config.len(),
        if ssr_used { "SSR" } else { "POST" },
    );
    // The raw config, so an SSR-provisioned session and a POST-provisioned one can be
    // diffed field by field for the same video.
    if let Some(dir) = crate::client::dump::raw_dir("ustreamer") {
        let path = dir.join(format!("{:04}.bin", crate::client::dump::next_raw_index()));
        match std::fs::write(&path, &player.ustreamer_config) {
            Ok(()) => tracing::info!(
                "[startup] wrote {} B of ustreamer config to {}",
                player.ustreamer_config.len(),
                path.display()
            ),
            Err(e) => tracing::warn!("[startup] could not write {}: {e}", path.display()),
        }
    }

    // Descramble the SABR url's `n`.
    let url = match &base_js {
        Some(js) => {
            let u = solve_url_n(&player.server_abr_streaming_url, js, solver).await?;
            timer.lap("n-sig solve");
            u
        }
        None => player.server_abr_streaming_url.clone(),
    };
    // The reference client adds alr, cpn and cver to the streaming url before posting (REQ-026).
    // `rn` is per request and added by the POST helpers. The nonce is drawn once per
    // bootstrap. Urls re-resolved after a RELOAD_PLAYER_RESPONSE or a re-bootstrap are not
    // decorated, so such a session continues without a cpn.
    let url = crate::client::decorate_sabr_url(
        &url,
        &session.client().client_version,
        &crate::client::client_playback_nonce(),
    );

    Ok(Bootstrap {
        player,
        url,
        base_js,
        base_js_url,
        visitor_data: visitor_data.to_string(),
        bg_challenge,
        watch_details,
    })
}

/// Descramble the SABR url's `n` via the in-process solver (WEB only). Done once;
/// SABR_REDIRECT urls are already valid and must not be re-solved.
pub(crate) async fn solve_url_n(
    url: &str,
    base_js: &str,
    solver: &NSigSolver,
) -> Result<String, JsError> {
    match query_param(url, "n") {
        Some(n) => {
            let solved = solver.solve_n(base_js, std::slice::from_ref(&n)).await?;
            Ok(url.replace(&format!("n={n}"), &format!("n={}", solved[0])))
        }
        None => Ok(url.to_string()),
    }
}

fn query_param(url: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=");
    url.split(['?', '&']).find(|kv| kv.starts_with(&pat)).map(|kv| kv[pat.len()..].to_string())
}

/// Whether to retry the bootstrap after a failure. Only an n-sig solver failure
/// ([`JsError::Solver`]) is retryable: it depends on the `base.js` build, and a fresh watch
/// page may name a build the solver handles (PLAYER-011). Network, playability and
/// JS-engine errors do not depend on the build. Bounded by `max`.
fn should_retry_nsig(err: &StreamError, attempt: u32, max: u32) -> bool {
    attempt < max && matches!(err, StreamError::Js(JsError::Solver(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The requested id appears on the page only as a recommendation.
    const PAGE: &str = concat!(
        r#"<link rel="canonical" href="https://www.youtube.com/watch?v=aqz-KE-bpKQ">"#,
        r#"<script>var x = {"videoDetails":{"videoId":"aqz-KE-bpKQ","title":"Big Buck Bunny"},"#,
        r#""secondaryResults":[{"videoId":"xOXolSQcEb4"},{"videoId":"B4EPW7JUMTM"}]};</script>"#,
    );

    #[test]
    fn a_watch_page_belongs_to_its_own_video() {
        assert!(watch_page_owned_by("aqz-KE-bpKQ", PAGE));
    }

    #[test]
    fn a_recommended_video_does_not_own_the_watch_page() {
        assert!(PAGE.contains("xOXolSQcEb4") && PAGE.contains("B4EPW7JUMTM"));
        assert!(!watch_page_owned_by("xOXolSQcEb4", PAGE));
        assert!(!watch_page_owned_by("B4EPW7JUMTM", PAGE));
    }

}
