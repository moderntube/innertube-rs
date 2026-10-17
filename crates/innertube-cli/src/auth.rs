//! `it auth`: proves web auth end to end. It reads the signed-in cookies from a
//! LibreWolf/Firefox profile, hands them to the library, and fetches
//! `account/account_menu` to print the account name.
//!
//! Browser-cookie acquisition lives in this consumer. The library only receives a
//! `Credentials` built from generic cookie pairs.
//!
//! `it auth [path/to/cookies.sqlite]` auto-detects a LibreWolf profile when no path is
//! given. `INNERTUBE_COOKIE_FILE` names a file holding a `Cookie` header and takes
//! precedence.

use std::path::{Path, PathBuf};

use innertube::{ClientConfig, Credentials, Session};

/// The auth-relevant cookies to hand the library. Sending *all* youtube.com cookies
/// (150+, incl. expired/path-scoped) overflows GFE's cookie-header limit → HTTP 413,
/// so the consumer curates the set the SAPISIDHASH session actually needs.
const AUTH_COOKIES: &[&str] = &[
    "SID", "HSID", "SSID", "APISID", "SAPISID",
    "__Secure-1PSID", "__Secure-3PSID",
    "__Secure-1PAPISID", "__Secure-3PAPISID",
    "__Secure-1PSIDTS", "__Secure-3PSIDTS",
    "__Secure-1PSIDCC", "__Secure-3PSIDCC", "SIDCC",
    "LOGIN_INFO", "PREF", "VISITOR_INFO1_LIVE", "VISITOR_PRIVACY_METADATA",
    "__Secure-ROLLOUT_TOKEN", "YSC",
];

/// `it auth [cookies.sqlite]`: signs in with browser cookies and prints the account name.
pub async fn run(cookies_db: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let session = load_session(cookies_db)?;
    let name = find_account_name(&session.account_menu().await?)
        .ok_or("account_menu returned no account header — cookies logged out/expired?")?;
    println!("✓ authenticated as: {name}");
    Ok(())
}

/// Build an authenticated session from a LibreWolf profile (or an explicit path).
fn load_session(db_arg: Option<&str>) -> Result<Session, Box<dyn std::error::Error>> {
    if let Ok(path) = std::env::var("INNERTUBE_COOKIE_FILE") {
        let creds = Credentials::from_cookie_header(std::fs::read_to_string(&path)?.trim())?;
        return Ok(Session::authenticated(ClientConfig::web(), creds)?);
    }
    let db = match db_arg {
        Some(p) => PathBuf::from(p),
        None => find_librewolf_cookie_db()
            .ok_or("no LibreWolf profile with YouTube cookies found; pass a cookies.sqlite path")?,
    };
    eprintln!("reading cookies from {}", db.display());
    let pairs = read_youtube_cookies(&db)?;
    let creds = Credentials::from_pairs(pairs)?;
    eprintln!("loaded {} YouTube cookies (SAPISID present ✓)", creds.cookie_pairs().len());
    Ok(Session::authenticated(ClientConfig::web(), creds)?)
}

/// Read the first-party `www.youtube.com` cookies: host `.youtube.com`, `youtube.com` or
/// `www.youtube.com` with no partition (`originAttributes = ''`), as the browser sends to
/// youtubei. Partitioned cookies and other subdomains (music, studio) overflow the header
/// (HTTP 413). Deduped by name. The DB is copied to a temp file first so a running
/// browser's lock/WAL does not block the read.
fn read_youtube_cookies(db: &Path) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    let tmp = std::env::temp_dir().join(format!("itr_cookies_{}.sqlite", std::process::id()));
    std::fs::copy(db, &tmp)?;
    let result = (|| -> rusqlite::Result<Vec<(String, String)>> {
        let conn =
            rusqlite::Connection::open_with_flags(&tmp, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut stmt = conn.prepare(
            "SELECT name, value FROM moz_cookies \
             WHERE host IN ('.youtube.com', 'youtube.com', 'www.youtube.com') \
             AND originAttributes = '' ORDER BY host",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        // Keep only the auth-relevant cookies, deduped by name (a cookie may exist
        // for several hosts); last wins.
        let mut by_name = std::collections::BTreeMap::new();
        for row in rows {
            let (n, v) = row?;
            if AUTH_COOKIES.contains(&n.as_str()) {
                by_name.insert(n, v);
            }
        }
        Ok(by_name.into_iter().collect())
    })();
    let _ = std::fs::remove_file(&tmp);
    Ok(result?)
}

/// Find a LibreWolf `cookies.sqlite` that actually holds YouTube auth cookies.
fn find_librewolf_cookie_db() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let root = PathBuf::from(home).join(".config/librewolf/librewolf");
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(&root)
        .ok()?
        .flatten()
        .map(|e| e.path().join("cookies.sqlite"))
        .filter(|p| p.exists())
        .collect();
    // Prefer a profile whose cookies include SAPISID (the signed-in one).
    candidates.sort();
    candidates
        .into_iter()
        .find(|db| read_youtube_cookies(db).map(|c| c.iter().any(|(n, _)| n == "SAPISID")).unwrap_or(false))
}

/// Recursively locate the first `accountName` text in the account-menu response
/// (`activeAccountHeaderRenderer.accountName`). Returns the display name only and never
/// reads the email field.
fn find_account_name(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(an) = map.get("accountName") {
                if let Some(t) = simple_or_runs(an) {
                    return Some(t);
                }
            }
            map.values().find_map(find_account_name)
        }
        serde_json::Value::Array(arr) => arr.iter().find_map(find_account_name),
        _ => None,
    }
}

/// Extract text from a `{simpleText}` or `{runs:[{text}]}` renderer.
fn simple_or_runs(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.get("simpleText").and_then(|s| s.as_str()) {
        return Some(s.to_string());
    }
    let runs = v.get("runs")?.as_array()?;
    let joined: String = runs.iter().filter_map(|r| r.get("text").and_then(|t| t.as_str())).collect();
    (!joined.is_empty()).then_some(joined)
}
