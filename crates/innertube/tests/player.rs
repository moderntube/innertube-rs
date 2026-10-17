use innertube::{player_request_body, ClientConfig, PlayerOptions, PlayerResponse};

#[test]
fn from_watch_html_extracts_ssr_player_response() {
    // A brace inside a string value.
    let html = r#"<script>var ytInitialPlayerResponse = {"videoDetails":{"title":"a } b","isLive":false},"streamingData":{"serverAbrStreamingUrl":"https://r.googlevideo.com/videoplayback?n=abc"}};</script>"#;
    let pr = PlayerResponse::from_watch_html(html).expect("streamable SSR response");
    assert_eq!(pr.server_abr_streaming_url, "https://r.googlevideo.com/videoplayback?n=abc");
    assert!(pr.playability.is_ok());
}

#[test]
fn from_watch_html_none_when_absent_or_unstreamable() {
    assert!(PlayerResponse::from_watch_html("<html>no player here</html>").is_none());
    // A login wall carries no streamingData.
    let gated = r#"ytInitialPlayerResponse = {"playabilityStatus":{"status":"LOGIN_REQUIRED"}}"#;
    assert!(PlayerResponse::from_watch_html(gated).is_none());
}

#[test]
fn web_player_request_carries_this_videos_watch_url() {
    // The WEB client's /youtubei/v1/player request names the video in both urls:
    //   originalUrl              "https://www.youtube.com/watch?v=M2w7tZsi8a4&pp=…"
    //   mainAppWebInfo.graftUrl  "/watch?v=M2w7tZsi8a4&pp=…"
    //   pwaInstallabilityStatus  "PWA_INSTALLABILITY_STATUS_CAN_BE_INSTALLED"
    // The `pp=` suffix is navigation context that a direct watch load lacks.
    let body = player_request_body("M2w7tZsi8a4", &ClientConfig::web(), &PlayerOptions::default());
    let client = &body["context"]["client"];

    assert_eq!(body["videoId"], "M2w7tZsi8a4");
    assert_eq!(client["clientName"], "WEB");
    assert_eq!(client["originalUrl"], "https://www.youtube.com/watch?v=M2w7tZsi8a4");
    assert_eq!(client["mainAppWebInfo"]["graftUrl"], "/watch?v=M2w7tZsi8a4");
    assert_eq!(
        client["mainAppWebInfo"]["pwaInstallabilityStatus"],
        "PWA_INSTALLABILITY_STATUS_CAN_BE_INSTALLED"
    );
}

#[test]
fn get_watch_request_carries_the_same_watch_url() {
    let body =
        innertube::get_watch_request_body("M2w7tZsi8a4", &ClientConfig::web(), &PlayerOptions::default());
    let client = &body["context"]["client"];

    assert_eq!(client["originalUrl"], "https://www.youtube.com/watch?v=M2w7tZsi8a4");
    assert_eq!(client["mainAppWebInfo"]["graftUrl"], "/watch?v=M2w7tZsi8a4");
}
