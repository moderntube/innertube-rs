//! The SABR streaming URL: the client playback nonce and the query parameters the player adds.

/// The 64-character alphabet the reference player draws a playback nonce from.
const CPN_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// A fresh 16-character client playback nonce, drawn the way the reference player draws
/// one (PLAYER-007): sixteen random bytes, each masked to six bits and indexed into
/// [`CPN_ALPHABET`]. There is one per playback session.
pub fn client_playback_nonce() -> String {
    use ring::rand::SecureRandom;
    let mut buf = [0u8; 16];
    if ring::rand::SystemRandom::new().fill(&mut buf).is_err() {
        // A failed draw must not yield a constant nonce: two sessions sharing one look to
        // the server like one session resumed.
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        for (i, b) in buf.iter_mut().enumerate() {
            *b = ((t >> (i * 4)) & 0xff) as u8;
        }
    }
    buf.iter().map(|b| CPN_ALPHABET[(b & 63) as usize] as char).collect()
}

/// Add the query parameters the reference client adds to `serverAbrStreamingUrl` before
/// posting: `alr=yes`, `cpn` and `cver` (the InnerTube client version). `rn` is added per
/// request by the POST helpers (REQ-026).
///
/// The additions leave the URL signature intact: `sparams` names the signed parameters and
/// none of `alr`/`cpn`/`cver`/`rn` is among them.
pub(crate) fn decorate_sabr_url(url: &str, client_version: &str, cpn: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}alr=yes&cpn={cpn}&cver={client_version}")
}

/// Replace (or add) the per-request `rn` counter on a SABR URL.
///
/// `rn` is one counter shared by every player on the page (REQ-026).
pub(super) fn with_request_number(url: &str) -> String {
    static RN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = RN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // The URL is built once per open and reused for every round, so a previous round's
    // `rn` must be dropped rather than accumulated.
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, q),
        None => (url, ""),
    };
    let kept: Vec<&str> =
        query.split('&').filter(|p| !p.is_empty() && !p.starts_with("rn=")).collect();
    if kept.is_empty() {
        format!("{base}?rn={n}")
    } else {
        format!("{base}?{}&rn={n}", kept.join("&"))
    }
}

#[cfg(test)]
mod sabr_url_tests {
    use super::*;

    // PLAYER-007: 16 characters from `A-Z a-z 0-9 - _`.
    #[test]
    fn a_playback_nonce_is_sixteen_alphabet_characters_and_differs_per_call() {
        let a = client_playback_nonce();
        assert_eq!(a.chars().count(), 16);
        assert!(a.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'), "{a}");
        // Two playback sessions sharing a nonce look to the server like one session resumed.
        assert_ne!(a, client_playback_nonce());
    }

    // REQ-026: the player adds `alr`, `cpn` and `cver` to the response's URL and removes nothing.
    #[test]
    fn the_streaming_url_keeps_its_query_and_gains_alr_cpn_cver() {
        let out = decorate_sabr_url("https://r1.googlevideo.com/videoplayback?id=X", "2.0", "N");
        assert!(out.starts_with("https://r1.googlevideo.com/videoplayback?id=X&"));
        for p in ["alr=yes", "cpn=N", "cver=2.0"] {
            assert!(out.contains(p), "missing {p} in {out}");
        }
    }

    // REQ-026: `rn` rises across requests. The URL is reused for every round, so a previous
    // `rn` is replaced.
    #[test]
    fn the_request_number_rises_and_is_never_repeated_in_the_url() {
        let first = with_request_number("https://x/videoplayback?id=X&alr=yes");
        let second = with_request_number(&first);
        let third = with_request_number(&second);
        for u in [&first, &second, &third] {
            assert_eq!(u.matches("rn=").count(), 1, "{u}");
            assert!(u.contains("id=X") && u.contains("alr=yes"), "{u}");
        }
        let n = |u: &str| u.rsplit("rn=").next().unwrap().parse::<u64>().unwrap();
        assert!(n(&third) > n(&second) && n(&second) > n(&first));
    }
}
