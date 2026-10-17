//! The warm-up caches the `base.js` URL the watch page embeds. Both `base.js` variants
//! exist under one build id, so warming the other one is a silent no-op.
//!
//!   cargo test -p innertube --test prewarm -- --ignored

use innertube::{ClientConfig, NSigSolver, Session};

#[tokio::test]
#[ignore = "network"]
async fn prewarm_warms_the_variant_the_watch_page_asks_for() {
    let session = Session::new(ClientConfig::web()).expect("session");
    let solver = NSigSolver::new();

    let warmed = innertube::prewarm_session(&session, &solver).await.expect("prewarm");

    // The bootstrap's own extractor names the url a cold start fetches.
    let watch = session
        .get_text("https://www.youtube.com/watch?v=dQw4w9WgXcQ")
        .await
        .expect("watch page");
    let want = innertube_js::extract_player_js_url(&watch).expect("player js url");

    assert_eq!(warmed, want, "prewarm cached a base.js URL the bootstrap will never request");
}
