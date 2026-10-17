//! PO-token provider abstraction.
//!
//! The default backend mints in-process through botguard-rs ([`NativeProvider`]). The other
//! talks to an external `bgutil` HTTP server (GPL-3.0, kept at arm's length over the
//! process boundary).

use serde::Deserialize;

mod native;
pub use native::NativeProvider;

/// The page-derived inputs a WebPO mint needs, both from the same watch-page fetch.
///
/// The initial attestation challenge is bound to that page load's `yt.config_`
/// (`EVENT_ID`). A token minted from a fresh `/att/get` challenge, or from a challenge
/// paired with another page's config, is rejected: googlevideo answers
/// `STREAM_PROTECTION_STATUS` 2, then 3 with no media (POT-003, GATE-003).
#[derive(Debug, Clone)]
pub struct PageAttestation {
    /// Raw JSON of the page's `bgChallenge` (`innertube::extract_bg_challenge`).
    pub challenge: String,
    /// Raw JSON of the page's `ytcfg` (`innertube::extract_yt_config`).
    pub yt_config: String,
}

/// Errors minting a PO token.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PotError {
    /// Carries no URL, the same as the InnerTube client's http error.
    #[error("po-token provider http error: {0}")]
    Http(#[source] reqwest::Error),
    /// The in-process minter failed: VM error, bad challenge, or no challenge to attest.
    #[error("native po-token minter: {0}")]
    Native(String),
    /// GenerateIT answered without an integrity token.
    #[error("po-token attestation refused: {0}")]
    Refused(String),
}

impl From<reqwest::Error> for PotError {
    fn from(e: reqwest::Error) -> Self {
        let at = e.url().map(|u| format!("{}{}", u.host_str().unwrap_or_default(), u.path()));
        let e = e.without_url();
        if let Some(at) = at {
            tracing::debug!("[pot] http error at {at}: {e}");
        }
        Self::Http(e)
    }
}

/// Which PO-token minter to use; parsed from a settings string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PotBackendKind {
    /// External bgutil HTTP server on `127.0.0.1:4416`.
    BgUtil,
    /// In-process BotGuard ([`NativeProvider`]).
    #[default]
    Native,
}

impl PotBackendKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "bgutil" => Some(Self::BgUtil),
            "native" => Some(Self::Native),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::BgUtil => "bgutil",
            Self::Native => "native",
        }
    }
}

/// The provider `kind` names. `user_agent` is the WEB client's; the native minter sends it
/// to Google.
pub fn provider(kind: PotBackendKind, user_agent: &str) -> std::sync::Arc<dyn PoTokenProvider> {
    match kind {
        PotBackendKind::BgUtil => std::sync::Arc::new(BgUtilProvider::local()),
        PotBackendKind::Native => std::sync::Arc::new(NativeProvider::new(user_agent)),
    }
}

/// A future returned by a [`PoTokenProvider`].
pub type PotFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Mints GVS PO tokens bound to a content binding (the video id, for web GVS). Sessions
/// hold one as `Arc<dyn PoTokenProvider>`.
pub trait PoTokenProvider: Send + Sync {
    /// A PO token (base64url string) for `content_binding`, minted against `attest`.
    ///
    /// `attest` comes from the page fetch this session uses: the challenge is bound to its
    /// `yt.config_`, and a fresh `/att/get` one is rejected (POT-003, LuanRT/BgUtils#44).
    fn get_po_token<'a>(
        &'a self,
        content_binding: &'a str,
        attest: Option<&'a PageAttestation>,
    ) -> PotFuture<'a, Result<String, PotError>>;

    /// Discard the provider's cached attestation so the next [`Self::get_po_token`]
    /// rebuilds it from scratch.
    ///
    /// A provider mints many PO tokens from one long-lived integrity token. Once the server
    /// rejects them (status 2, then 3, media withheld; GATE-003), every new token descends
    /// from the same dead attestation. Only the consumer sees the rejection, so the consumer
    /// calls this. A token minted against the wrong page challenge fails the same way and is
    /// the more common cause: pass a [`PageAttestation`] before suspecting a stale cache.
    fn invalidate_attestation(&self) -> PotFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// Talks to a running `bgutil-ytdlp-pot-provider` HTTP server.
#[derive(Clone)]
pub struct BgUtilProvider {
    base_url: String,
    http: reqwest::Client,
}

impl BgUtilProvider {
    pub fn new(base_url: impl Into<String>) -> Self {
        // A request timeout so a wedged-but-listening bgutil sidecar can't hang
        // `SabrStream::open` forever (this is the one lib network call with no
        // other deadline).
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("reqwest client builds with a timeout");
        Self { base_url: base_url.into(), http }
    }

    /// The conventional local server (`http://127.0.0.1:4416`).
    pub fn local() -> Self {
        Self::new("http://127.0.0.1:4416")
    }
}

#[derive(Deserialize)]
struct GetPotResponse {
    #[serde(rename = "poToken")]
    po_token: String,
}

/// Whether a mint is bound to a real page's attestation, and if not, why.
///
/// An unbound mint fails: bgutil falls back to `/att/get`, the server answers status 2,
/// then 3, and withholds all media for the rest of the session (POT-003). Re-minting
/// cannot recover it, because every re-mint takes the same fallback. The reason makes
/// that state diagnosable.
#[derive(Debug, PartialEq, Eq)]
pub enum Binding {
    /// The body carries this page load's `challenge` + `web_config`.
    Page,
    /// The body carries neither, and the mint will go through `/att/get`.
    Unbound(&'static str),
}

/// Build the `/get_pot` request body, and say whether it is page-bound.
pub fn mint_body(
    content_binding: &str,
    attest: Option<&PageAttestation>,
) -> (serde_json::Value, Binding) {
    let Some(a) = attest else {
        return (
            serde_json::json!({ "content_binding": content_binding }),
            Binding::Unbound("no page attestation was captured for this session"),
        );
    };
    let (Ok(c), Ok(w)) = (
        serde_json::from_str::<serde_json::Value>(&a.challenge),
        serde_json::from_str::<serde_json::Value>(&a.yt_config),
    ) else {
        // An unbound body here is the failure mode: the page was fetched, one of the two
        // blobs did not parse, and the session then fails attestation permanently.
        return (
            serde_json::json!({ "content_binding": content_binding }),
            Binding::Unbound("the page's bgChallenge or ytcfg did not parse as JSON"),
        );
    };
    (
        // `web_config` becomes `window.yt.config_` in the minter's global scope; BotGuard
        // reads EVENT_ID from there while it runs the challenge.
        serde_json::json!({
            "content_binding": content_binding,
            "challenge": c,
            "web_config": w,
        }),
        Binding::Page,
    )
}

impl BgUtilProvider {
    async fn mint(&self, content_binding: &str, attest: Option<&PageAttestation>) -> Result<String, PotError> {
        // bgutil honours a caller-supplied `challenge` and falls back to `/att/get` only when
        // none is given; an /att/get-derived token is what the server rejects.
        let (body, binding) = mint_body(content_binding, attest);
        if let Binding::Unbound(why) = binding {
            tracing::warn!(
                "[pot] minting WITHOUT a page challenge ({why}) — bgutil will fall back to \
                 /att/get, whose token the server rejects (prot=2, then 3 and no media)"
            );
        }
        let resp: GetPotResponse = self
            .http
            .post(format!("{}/get_pot", self.base_url))
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp.po_token)
    }
}

impl PoTokenProvider for BgUtilProvider {
    fn get_po_token<'a>(
        &'a self,
        content_binding: &'a str,
        attest: Option<&'a PageAttestation>,
    ) -> PotFuture<'a, Result<String, PotError>> {
        Box::pin(self.mint(content_binding, attest))
    }

    /// bgutil's `POST /invalidate_it` drops its cached integrity token (`invalidateIT`).
    /// Best-effort: on failure the next mint is what it would have been.
    fn invalidate_attestation(&self) -> PotFuture<'_, ()> {
        Box::pin(async move {
            let _ = self.http.post(format!("{}/invalidate_it", self.base_url)).send().await;
        })
    }
}

#[cfg(test)]
mod mint_body_tests {
    use super::*;

    fn attest(challenge: &str, yt_config: &str) -> PageAttestation {
        PageAttestation { challenge: challenge.to_string(), yt_config: yt_config.to_string() }
    }

    #[test]
    fn a_page_bound_mint_body_carries_the_challenge_and_config() {
        let a = attest(r#"{"bg":"c"}"#, r#"{"EVENT_ID":"e"}"#);
        let (body, binding) = mint_body("vid123", Some(&a));

        assert_eq!(binding, Binding::Page);
        assert_eq!(body["content_binding"], "vid123");
        assert_eq!(body["challenge"]["bg"], "c");
        assert_eq!(body["web_config"]["EVENT_ID"], "e");
    }

    #[test]
    fn an_unparseable_blob_is_reported_unbound() {
        // A blob that fails to parse drops the challenge, bgutil falls back to /att/get and the
        // server rejects the token (POT-003).
        for (challenge, cfg, label) in [
            ("not json at all", r#"{"EVENT_ID":"e"}"#, "bad challenge"),
            (r#"{"bg":"c"}"#, "<html>consent wall</html>", "bad ytcfg"),
        ] {
            let a = attest(challenge, cfg);
            let (body, binding) = mint_body("vid123", Some(&a));

            assert!(
                body.get("challenge").is_none() && body.get("web_config").is_none(),
                "{label}: the body is unbound"
            );
            assert!(
                matches!(binding, Binding::Unbound(_)),
                "{label}: an unbound body is reported unbound"
            );
        }
    }
}
