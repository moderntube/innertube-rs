//! Web-client authentication: cookies and the `SAPISIDHASH` `Authorization` header.
//!
//! The library takes cookies from the consumer (a browser, a `cookies.txt`, a webview)
//! and only uses them; it never reads a browser. The WEB client authenticates every
//! signed-in InnerTube call with `Cookie` + `Authorization: SAPISIDHASH ...`, recomputed
//! per request from the long-lived `SAPISID` cookie (IT-004).

/// Origin the SAPISIDHASH is computed against for the WEB client.
const YT_ORIGIN: &str = "https://www.youtube.com";

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// No `SAPISID` cookie in the provided set (a logged-out jar, or a cookie export that
    /// dropped the Google auth cookies).
    #[error("no SAPISID cookie in the provided credentials (not logged in?)")]
    MissingSapisid,
}

/// Signed-in WEB credentials: the `Cookie` header to send plus the `SAPISID` used
/// to compute the per-request `Authorization: SAPISIDHASH …` header. Construct from
/// whatever cookie source the consumer has; the library never reads a browser.
#[derive(Clone)]
pub struct Credentials {
    cookie_header: String,
    sapisid: String,
    auth_user: String,
}

impl Credentials {
    /// Build from a raw `Cookie` header value (`"SID=…; SAPISID=…; …"`), e.g. the
    /// string a browser would send. The header is preserved verbatim.
    pub fn from_cookie_header(header: &str) -> Result<Self, AuthError> {
        let sapisid = parse_cookie_pairs(header)
            .find(|(n, _)| *n == "SAPISID")
            .map(|(_, v)| v.to_string())
            .ok_or(AuthError::MissingSapisid)?;
        Ok(Self { cookie_header: header.to_string(), sapisid, auth_user: "0".to_string() })
    }

    /// Build from `(name, value)` cookie pairs (e.g. read from a browser DB by the
    /// consumer). Pairs are joined into a standard `Cookie` header.
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Result<Self, AuthError> {
        let pairs: Vec<(String, String)> = pairs.into_iter().collect();
        let sapisid = pairs
            .iter()
            .find(|(n, _)| n == "SAPISID")
            .map(|(_, v)| v.clone())
            .ok_or(AuthError::MissingSapisid)?;
        let cookie_header = pairs
            .iter()
            .map(|(n, v)| format!("{n}={v}"))
            .collect::<Vec<_>>()
            .join("; ");
        Ok(Self { cookie_header, sapisid, auth_user: "0".to_string() })
    }

    /// The `Cookie` header value to send on authenticated requests.
    pub fn cookie_header(&self) -> &str {
        &self.cookie_header
    }

    /// The cookies as owned `(name, value)` pairs, for seeding a cookie jar.
    pub fn cookie_pairs(&self) -> Vec<(String, String)> {
        parse_cookie_pairs(&self.cookie_header)
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect()
    }

    /// `X-Goog-AuthUser` value (which signed-in account); `"0"` = primary.
    pub fn auth_user(&self) -> &str {
        &self.auth_user
    }

    /// The `Authorization` header value for a request at `epoch` (unix seconds):
    /// `SAPISIDHASH <epoch>_<sha1(epoch SAPISID origin)>`. Recompute per request.
    pub fn authorization(&self, epoch: i64) -> String {
        format!("SAPISIDHASH {}", sapisid_hash(&self.sapisid, YT_ORIGIN, epoch))
    }
}

/// `<epoch>_<sha1hex(epoch + " " + sapisid + " " + origin)>`, the SAPISIDHASH body.
pub(crate) fn sapisid_hash(sapisid: &str, origin: &str, epoch: i64) -> String {
    let digest = ring::digest::digest(
        &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
        format!("{epoch} {sapisid} {origin}").as_bytes(),
    );
    let mut hex = String::with_capacity(40);
    for b in digest.as_ref() {
        hex.push_str(&format!("{b:02x}"));
    }
    format!("{epoch}_{hex}")
}

/// Split a `Cookie` header (`"a=b; c=d"`) into `(name, value)` pairs.
fn parse_cookie_pairs(header: &str) -> impl Iterator<Item = (&str, &str)> {
    header.split(';').filter_map(|kv| {
        let (n, v) = kv.split_once('=')?;
        Some((n.trim(), v.trim()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Independent vector: sha1("1700000000 TEST_SAPISID_VALUE https://www.youtube.com")
    const VECTOR_HASH: &str = "1700000000_e282e657f4e0afff39f69ac49643a00a0cf76a53";

    #[test]
    fn sapisid_hash_matches_known_vector() {
        assert_eq!(
            sapisid_hash("TEST_SAPISID_VALUE", "https://www.youtube.com", 1_700_000_000),
            VECTOR_HASH
        );
    }

    #[test]
    fn credentials_from_cookie_header_extracts_sapisid_and_preserves_header() {
        let header = "SID=aaa; SAPISID=TEST_SAPISID_VALUE; HSID=bbb";
        let creds = Credentials::from_cookie_header(header).unwrap();
        assert_eq!(creds.cookie_header(), header);
        assert_eq!(
            creds.authorization(1_700_000_000),
            format!("SAPISIDHASH {VECTOR_HASH}")
        );
    }

    #[test]
    fn credentials_from_pairs_builds_cookie_header_and_authorizes() {
        let creds = Credentials::from_pairs([
            ("SID".to_string(), "aaa".to_string()),
            ("SAPISID".to_string(), "TEST_SAPISID_VALUE".to_string()),
        ])
        .unwrap();
        assert!(creds.cookie_header().contains("SAPISID=TEST_SAPISID_VALUE"));
        assert_eq!(
            creds.authorization(1_700_000_000),
            format!("SAPISIDHASH {VECTOR_HASH}")
        );
    }

    #[test]
    fn missing_sapisid_is_an_error() {
        assert!(Credentials::from_cookie_header("SID=aaa; HSID=bbb").is_err());
    }

}
