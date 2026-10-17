//! Opt-in dumps for field debugging, gated by `INNERTUBE_DUMP`.
//!
//! `INNERTUBE_DUMP=<dir>`: every HTTP request the session makes appends one JSON line to
//! a per-category `.jsonl` file there (`control.jsonl`, `player.jsonl`, `sabr.jsonl`,
//! `sabr_round.jsonl`, `fetch.jsonl`). Records hold the request shape and response
//! metadata (status, sizes, per-round summaries), never response bodies.
//!
//! Secrets are scrubbed (PO token, cookies, auth, `visitorData`, signature/`n` URL
//! params, IP), so the dump can be shared as is.
//!
//! `INNERTUBE_DUMP=raw:<dir>`: the same, plus unscrubbed captures under `<dir>/raw/`
//! (SABR request and response bodies, player responses, ustreamer configs, Shorts
//! overlays, live segments). These carry the PO token and session blobs: keep them
//! local.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

struct Config {
    dir: PathBuf,
    raw: bool,
}

static CONFIG: OnceLock<Option<Config>> = OnceLock::new();

fn config() -> Option<&'static Config> {
    CONFIG
        .get_or_init(|| {
            let v = std::env::var("INNERTUBE_DUMP").ok().filter(|s| !s.is_empty())?;
            let (raw, d) = match v.strip_prefix("raw:") {
                Some(d) => (true, d.to_string()),
                None => (false, v),
            };
            let dir = PathBuf::from(d);
            std::fs::create_dir_all(&dir).ok()?;
            Some(Config { dir, raw })
        })
        .as_ref()
}

fn dir() -> Option<&'static PathBuf> {
    config().map(|c| &c.dir)
}

/// `<dir>/raw/<kind>`, created, when raw capture is on.
pub(crate) fn raw_dir(kind: &str) -> Option<PathBuf> {
    let c = config().filter(|c| c.raw)?;
    let d = c.dir.join("raw").join(kind);
    std::fs::create_dir_all(&d).ok()?;
    Some(d)
}

/// The next index for a numbered raw capture, shared by every kind so files from one run
/// sort in the order they were written.
pub(crate) fn next_raw_index() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

/// Whether dumping is on (the env var named a usable directory). Cheap to call.
pub(crate) fn enabled() -> bool {
    dir().is_some()
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Append one record to `<dir>/<category>.jsonl` (no-op when disabled). A `t_ms`
/// epoch-millis timestamp is added automatically.
pub(crate) fn record(category: &str, mut fields: Value) {
    let Some(dir) = dir() else { return };
    if let Value::Object(map) = &mut fields {
        map.insert("t_ms".into(), json!(now_ms()));
    }
    let Ok(line) = serde_json::to_string(&fields) else { return };
    if let Ok(mut f) =
        OpenOptions::new().create(true).append(true).open(dir.join(format!("{category}.jsonl")))
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Redact secret-bearing query params from a URL, keeping its shape for diffing.
pub(crate) fn scrub_url(url: &str) -> String {
    const SECRET: &[&str] = &["pot", "sig", "lsig", "signature", "ip", "n", "sparams"];
    let Some((base, query)) = url.split_once('?') else { return url.to_string() };
    let scrubbed: Vec<String> = query
        .split('&')
        .map(|kv| match kv.split_once('=') {
            Some((k, _)) if SECRET.contains(&k) => format!("{k}=<scrubbed>"),
            _ => kv.to_string(),
        })
        .collect();
    format!("{base}?{}", scrubbed.join("&"))
}

/// Recursively redact secret-bearing keys from a JSON value (for request bodies),
/// in place. Case-insensitive on the key.
pub(crate) fn scrub_json(v: &mut Value) {
    const SECRET: &[&str] = &[
        "visitorData",
        "poToken",
        "po_token",
        "serviceIntegrityDimensions",
        "authorization",
        "cookie",
        "sessionIndex",
        "datasyncId",
        "visitorId",
        "Bearer",
    ];
    match v {
        Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                if SECRET.iter().any(|s| s.eq_ignore_ascii_case(k)) {
                    *val = Value::String("<scrubbed>".into());
                } else {
                    scrub_json(val);
                }
            }
        }
        Value::Array(arr) => arr.iter_mut().for_each(scrub_json),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_url_redacts_only_secret_params() {
        let u = "https://r.googlevideo.com/videoplayback?id=abc&pot=SECRET&n=NSIG&mime=video/webm&ip=1.2.3.4";
        let s = scrub_url(u);
        assert!(s.contains("id=abc") && s.contains("mime=video/webm"), "keeps non-secrets");
        assert!(s.contains("pot=<scrubbed>") && s.contains("n=<scrubbed>") && s.contains("ip=<scrubbed>"));
        assert!(!s.contains("SECRET") && !s.contains("NSIG") && !s.contains("1.2.3.4"));
    }

    #[test]
    fn scrub_json_redacts_nested_secrets_case_insensitively() {
        let mut v = json!({
            "context": {"client": {"visitorData": "VD", "clientName": "WEB"}},
            "serviceIntegrityDimensions": {"poToken": "PT"},
            "videoId": "keepme"
        });
        scrub_json(&mut v);
        assert_eq!(v["context"]["client"]["visitorData"], "<scrubbed>");
        assert_eq!(v["context"]["client"]["clientName"], "WEB", "non-secret kept");
        assert_eq!(v["serviceIntegrityDimensions"], "<scrubbed>", "whole secret subtree redacted");
        assert_eq!(v["videoId"], "keepme");
    }
}
