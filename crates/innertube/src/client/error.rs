//! The InnerTube client error type.

use super::*;

/// Errors from network requests.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Carries no URL: a googlevideo URL holds the client IP and signed parameters.
    #[error("http error: {0}")]
    Http(#[source] reqwest::Error),
    #[error(transparent)]
    Player(#[from] PlayerError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("auth error: {0}")]
    Auth(#[from] AuthError),
    /// An API-level problem (missing expected field, action not available, …).
    #[error("api error: {0}")]
    Api(String),
    /// A structured InnerTube top-level error (`error.code` / `error.status`).
    #[error("innertube error {code} ({status}): {message}")]
    InnerTube { code: i64, status: String, message: String },
    /// A write/account action was attempted on an anonymous session.
    #[error("not logged in: this action requires authentication")]
    NotLoggedIn,
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        let url = e.url().map(|u| super::dump::scrub_url(u.as_str()));
        let e = e.without_url();
        if let Some(url) = url {
            tracing::debug!("http error at {url}: {e}");
        }
        Self::Http(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Port 1 on loopback refuses the connection, so this stays offline.
    #[tokio::test]
    async fn a_transport_error_prints_no_url() {
        let e = reqwest::Client::new().get("http://127.0.0.1:1/videoplayback?ip=203.0.113.7&sig=SECRET").send().await;
        let e = Error::from(e.expect_err("loopback port 1 refuses"));
        let text = format!("{e} {e:?}");
        assert!(!text.contains("203.0.113.7") && !text.contains("SECRET"), "{text}");
    }
}
