//! Values scraped from watch-page HTML: visitorData, the BotGuard challenge, `ytcfg`.

/// Find the string value following `marker` (e.g. `"visitorData":"`) in raw text,
/// up to the next unescaped `"`. Used to scrape `ytcfg` from the homepage HTML.
pub(super) fn find_json_string(haystack: &str, marker: &str) -> Option<String> {
    let start = haystack.find(marker)? + marker.len();
    let rest = &haystack[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Extract the first balanced `{…}` object following `marker` in `text`, tracking
/// string literals + escapes so braces inside strings don't miscount. Used to pull
/// `ytInitialPlayerResponse = {…}` out of the watch-page HTML.
pub(crate) fn extract_json_object_after(text: &str, marker: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = text.find(marker)? + marker.len();
    while i < bytes.len() && bytes[i] != b'{' {
        i += 1;
    }
    let start = i;
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            match c {
                _ if esc => esc = false,
                b'\\' => esc = true,
                b'"' => in_str = false,
                _ => {}
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(text[start..=i].to_string());
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Extract `visitorData` from a page's embedded config.
///
/// The watch page carries it, so a session that fetches the watch page does not need the
/// separate homepage round trip `bootstrap_visitor_data` makes.
pub fn extract_visitor_data(html: &str) -> Option<String> {
    let v = find_json_string(html, "\"visitorData\":\"")?;
    // A truncated or placeholder value is worse than falling back to the homepage fetch.
    (v.len() > 16).then_some(v)
}

/// Extract the page-embedded BotGuard attestation challenge (`bgChallenge`) from a
/// YouTube watch page, as raw JSON text.
///
/// The challenge is bound to the page's `yt.config_` (`EVENT_ID`). A token minted from a
/// fresh `/att/get` challenge is rejected: googlevideo answers `STREAM_PROTECTION_STATUS`
/// 2, serves media for a short grace window, then answers 3 and withholds media for the
/// rest of the session (POT-003, GATE-003).
///
/// The challenge and the `ytcfg` of one page load go together; pages loaded on the same
/// `Session` are interchangeable (POT-004). This parses HTML already in hand instead of
/// issuing its own request.
///
/// The object sits inside a JS string literal, so `\xNN` escapes are decoded first. `\/`,
/// `\uNNNN` and `\\` are left for the JSON parser: a second unescaping pass corrupts the
/// base64 `program` field, which contains `/` and `+`.
pub(crate) fn extract_bg_challenge(html: &str) -> Option<String> {
    let anchor = html.find("bgChallenge")?;
    // `program` alone runs to ~36 KB; take a generous window so the object is not
    // truncated mid-string (an unterminated-string parse error). The bound is a raw byte
    // offset, so walk it forward to a char boundary: a multi-byte character straddling the
    // limit would panic.
    let mut end = (anchor + 400_000).min(html.len());
    while end < html.len() && !html.is_char_boundary(end) {
        end += 1;
    }
    let window = unescape_js_hex(&html[anchor..end]);
    let open = window.find('{')?;
    let close = match_braces(&window[open..])? + open;
    let json = &window[open..close];
    // Only hand back something that actually parses and has what bgutil destructures.
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    v.get("program")?;
    v.get("interpreterUrl")?
        .get("privateDoNotAccessOrElseTrustedResourceUrlWrappedValue")?;
    Some(json.to_string())
}

/// Decode `\xNN` escapes only; [`extract_bg_challenge`] explains why a single pass.
fn unescape_js_hex(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1] == b'x' {
            if let Ok(v) = u8::from_str_radix(&s[i + 2..i + 4], 16) {
                out.push(v as char);
                i += 4;
                continue;
            }
        }
        // Multi-byte UTF-8 must be copied whole.
        let ch = s[i..].chars().next().unwrap_or('\u{fffd}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Index just past the `}` closing the object that starts at `s[0] == '{'`, tracking
/// string literals and escapes so braces inside `program` don't miscount.
fn match_braces(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    for (i, &c) in b.iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

#[cfg(test)]
mod bg_challenge_tests {
    use super::*;

    /// Shaped exactly like the real watch page: the object lives inside a JS string
    /// literal (`\x22` for `"`, `\x7b`/`\x7d` for braces) and `program` is base64 with
    /// `/` and `+` in it, escaped as `\/`.
    fn page(program: &str) -> String {
        format!(
            "junk\\x22bgChallenge\\x22:\\x7b\\x22interpreterUrl\\x22:\\x7b\
             \\x22privateDoNotAccessOrElseTrustedResourceUrlWrappedValue\\x22:\
             \\x22\\/\\/www.google.com\\/js\\/th\\/abc.js\\x22\\x7d,\
             \\x22interpreterHash\\x22:\\x22abc\\x22,\\x22program\\x22:\\x22{program}\\x22,\
             \\x22globalName\\x22:\\x22trayride\\x22\\x7d,\\x22next\\x22:1"
        )
    }

    #[test]
    fn extracts_the_challenge_object() {
        let html = page("hrhA+ok\\/RqpWy1\\/1ec");
        let got = extract_bg_challenge(&html).expect("challenge");
        let v: serde_json::Value = serde_json::from_str(&got).unwrap();
        assert_eq!(v["globalName"], "trayride");
        assert_eq!(v["interpreterHash"], "abc");
        // The base64 program must survive byte-exact. A second unescaping pass would
        // eat the `\/` and corrupt it.
        assert_eq!(v["program"], "hrhA+ok/RqpWy1/1ec");
        assert_eq!(
            v["interpreterUrl"]["privateDoNotAccessOrElseTrustedResourceUrlWrappedValue"],
            "//www.google.com/js/th/abc.js"
        );
    }

    #[test]
    fn a_brace_inside_program_does_not_truncate_the_object() {
        let html = page("aaa}bbb{ccc");
        let v: serde_json::Value =
            serde_json::from_str(&extract_bg_challenge(&html).expect("challenge")).unwrap();
        assert_eq!(v["program"], "aaa}bbb{ccc");
    }

    #[test]
    fn a_quote_inside_program_does_not_truncate_the_object() {
        let html = page("aa\\\"bb");
        let v: serde_json::Value =
            serde_json::from_str(&extract_bg_challenge(&html).expect("challenge")).unwrap();
        assert_eq!(v["program"], "aa\"bb");
    }

    /// The 400 KB window bound is a raw byte offset. A multi-byte character straddling it
    /// must not panic; the extractor walks the bound to a char boundary.
    #[test]
    fn no_panic_when_the_window_splits_a_utf8_char() {
        // The 3-byte em dash occupies bytes 399_999..400_002 with anchor=0, so the window
        // bound lands inside it.
        let mut html = String::from("bgChallenge");
        html.push_str(&"x".repeat(399_988));
        html.push('—');
        html.push_str("\"program\": \"AAAA\"");
        let _ = extract_bg_challenge(&html); // must not panic; value irrelevant
    }

    /// Missing the fields bgutil destructures is as useless as no challenge at all.
    #[test]
    fn none_when_interpreter_url_is_absent() {
        let html = "\\x22bgChallenge\\x22:\\x7b\\x22program\\x22:\\x22abc\\x22\\x7d";
        assert!(extract_bg_challenge(html).is_none());
    }
}

/// Extract the watch page's `ytcfg` object (`yt.config_`) as raw JSON text.
///
/// Companion to [`extract_bg_challenge`]: BotGuard reads `yt.config_.EVENT_ID` from the
/// global scope while descrambling the challenge, so the minter needs the config of the
/// same page load or the token is rejected (POT-003). Picks the largest `ytcfg.set({…})`
/// payload; the page calls it several times and only the main one carries `EVENT_ID`.
pub(crate) fn extract_yt_config(html: &str) -> Option<String> {
    let mut best: Option<String> = None;
    let mut from = 0usize;
    while let Some(rel) = html[from..].find("ytcfg.set(") {
        let at = from + rel + "ytcfg.set(".len();
        from = at;
        let rest = &html[at..];
        let open = match rest.find('{') {
            Some(o) if o < 4 => o, // `ytcfg.set({…})`, not `ytcfg.set("KEY", …)`
            _ => continue,
        };
        let Some(close) = match_braces(&rest[open..]) else { continue };
        let json = &rest[open..open + close];
        if serde_json::from_str::<serde_json::Value>(json).is_ok()
            && best.as_ref().is_none_or(|b| b.len() < json.len())
        {
            best = Some(json.to_string());
        }
    }
    best.filter(|b| b.contains("EVENT_ID"))
}

#[cfg(test)]
mod yt_config_tests {
    use super::*;

    #[test]
    fn picks_the_largest_set_call_that_has_event_id() {
        let html = r#"<script>ytcfg.set({"SMALL":1});</script>
            <script>ytcfg.set("OTHER_KEY", {"not":"this"});</script>
            <script>ytcfg.set({"EVENT_ID":"abc123","INNERTUBE_CONTEXT":{"client":{"hl":"en"}}});</script>"#;
        let v: serde_json::Value =
            serde_json::from_str(&extract_yt_config(html).expect("config")).unwrap();
        assert_eq!(v["EVENT_ID"], "abc123");
    }

    #[test]
    fn none_without_an_event_id() {
        let html = r#"ytcfg.set({"SOMETHING":"else"});"#;
        assert!(extract_yt_config(html).is_none());
    }

    /// A brace inside a string value must not end the object early.
    #[test]
    fn braces_inside_strings_do_not_truncate() {
        let html = r#"ytcfg.set({"EVENT_ID":"a}b{c","X":1});"#;
        let v: serde_json::Value =
            serde_json::from_str(&extract_yt_config(html).expect("config")).unwrap();
        assert_eq!(v["EVENT_ID"], "a}b{c");
        assert_eq!(v["X"], 1);
    }
}

#[cfg(test)]
mod visitor_data_tests {
    use super::*;

    #[test]
    fn extracts_visitor_data_from_a_page() {
        let html = r#"junk{"visitorData":"CgtBQUFBQUFBQUFBQSiA8tbKBjIKCgJVUxIEGgAg"} more"#;
        assert_eq!(
            extract_visitor_data(html).as_deref(),
            Some("CgtBQUFBQUFBQUFBQSiA8tbKBjIKCgJVUxIEGgAg")
        );
    }

    /// A stub/truncated value must fall through to the homepage fetch rather than be sent
    /// as if it were a real session id.
    #[test]
    fn none_for_an_implausibly_short_value() {
        assert!(extract_visitor_data(r#"{"visitorData":"short"}"#).is_none());
    }
}
