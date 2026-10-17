use innertube_js::extract_player_js_url;

#[test]
fn extracts_player_js_url_from_escaped_ytcfg() {
    let html = r#"...,"PLAYER_JS_URL":"\/s\/player\/9fc68080\/player_es6.vflset\/en_US\/base.js",..."#;
    assert_eq!(
        extract_player_js_url(html).as_deref(),
        Some("https://www.youtube.com/s/player/9fc68080/player_es6.vflset/en_US/base.js")
    );
}

#[test]
fn extracts_player_js_url_from_raw_src() {
    let html = r#"<script src="/s/player/abcd1234/player_ias.vflset/en_US/base.js"></script>"#;
    assert_eq!(
        extract_player_js_url(html).as_deref(),
        Some("https://www.youtube.com/s/player/abcd1234/player_ias.vflset/en_US/base.js")
    );
}

/// The native fast path agrees with the vendored ejs solver on every real `base.js`.
/// ejs is the oracle.
///
/// Skipped unless `NSIG_DIFF_PLAYERS` points at a directory of `base.js` files. Real players
/// are too large to keep in the repo.
#[tokio::test]
async fn native_agrees_with_ejs_on_real_players() {
    let Some(dir) = std::env::var_os("NSIG_DIFF_PLAYERS") else {
        eprintln!("NSIG_DIFF_PLAYERS unset, skipping differential");
        return;
    };
    let challenges: Vec<String> =
        ["MWAMDWLL0V3zA8t", "abcdefghijklmno", "0000000000000"].iter().map(|s| s.to_string()).collect();

    let mut checked = 0usize;
    for entry in std::fs::read_dir(&dir).expect("NSIG_DIFF_PLAYERS unreadable") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("js") {
            continue;
        }
        let js = std::fs::read_to_string(&path).expect("read player");
        let name = path.file_name().unwrap().to_string_lossy().to_string();

        // The default solver falls back to ejs, so only `native_only()` can disagree with it.
        let native = innertube_js::NSigSolver::native_only()
            .solve_n(&js, &challenges)
            .await
            .unwrap_or_else(|e| panic!("{name}: native path did not handle this player: {e}"));
        let ejs = innertube_js::NSigSolver::ejs_only()
            .solve_n(&js, &challenges)
            .await
            .unwrap_or_else(|e| panic!("{name}: ejs oracle failed: {e}"));

        assert_eq!(native, ejs, "{name}: native and ejs disagree");
        checked += 1;
    }
    assert!(checked > 0, "no .js players found under {dir:?}");
    eprintln!("differential OK across {checked} player builds");
}
