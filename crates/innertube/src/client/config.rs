//! The WEB client identity: `ClientConfig` and the version constants it sends.

/// The WEB client identity attached to every request. Field values come from a
/// real WEB-client capture; `client_version` rotates with the player build.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub client_name: String,
    pub client_version: String,
    /// `x-youtube-client-name` header and StreamerContext client id (1 = WEB).
    pub client_name_id: i32,
    pub hl: String,
    pub gl: String,
    pub user_agent: String,
    pub os_name: String,
    pub os_version: String,
}

/// The WEB client version. SABR streaming requires the WEB client identity (REQ-024).
pub const WEB_CLIENT_VERSION: &str = "2.20260528.01.00";

/// Desktop Chrome UA matching the WEB client identity.
pub const WEB_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36";

/// Chrome major.build version, in sync with [`WEB_USER_AGENT`]; echoed in the WEB
/// player context's `browserVersion`. Single source so the two never drift.
pub const WEB_BROWSER_VERSION: &str = "148.0.0.0";

impl ClientConfig {
    pub fn web() -> Self {
        Self {
            client_name: "WEB".to_string(),
            client_version: WEB_CLIENT_VERSION.to_string(),
            client_name_id: 1,
            hl: "en".to_string(),
            gl: "US".to_string(),
            user_agent: WEB_USER_AGENT.to_string(),
            os_name: "X11".to_string(),
            os_version: String::new(),
        }
    }
}

/// Fallback signature timestamp (`sts`) for the player JS. It changes with the player
/// build; requests send the value read from the fetched player JS when one is available
/// (PLAYER-003).
pub const WEB_SIGNATURE_TIMESTAMP: i64 = 20594;
