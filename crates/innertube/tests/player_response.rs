use innertube::PlayerResponse;

// Field paths of the WEB player() response; the fixtures below are scrubbed captures.
const SAMPLE: &str = r#"{
  "streamingData": {
    "serverAbrStreamingUrl": "https://rr2.googlevideo.com/videoplayback?x=1",
    "adaptiveFormats": [
      {"itag": 140, "mimeType": "audio/mp4; codecs=\"mp4a.40.2\""},
      {"itag": 299, "mimeType": "video/mp4; codecs=\"avc1.64002a\""}
    ]
  },
  "playerConfig": {
    "mediaCommonConfig": {
      "mediaUstreamerRequestConfig": {
        "videoPlaybackUstreamerConfig": "AQID"
      }
    }
  }
}"#;

#[test]
fn parses_streaming_url_formats_and_ustreamer_config() {
    let pr = PlayerResponse::parse(SAMPLE).unwrap();

    assert_eq!(pr.server_abr_streaming_url, "https://rr2.googlevideo.com/videoplayback?x=1");
    assert_eq!(pr.ustreamer_config, vec![1, 2, 3]); // base64 "AQID"
    let itags: Vec<i32> = pr.formats.iter().map(|f| f.itag).collect();
    assert_eq!(itags, vec![140, 299]);
}

const LIVE_FIXTURE: &str = include_str!("../../../notes/fixtures/player-response-live.json");
const VOD_FIXTURE: &str = include_str!("../../../notes/fixtures/player-response-vod.json");

#[test]
fn live_fixture_is_live_no_duration() {
    let pr = PlayerResponse::parse(LIVE_FIXTURE).unwrap();
    assert!(pr.is_live, "live fixture videoDetails.isLive=true");
    assert_eq!(pr.duration_ms, None, "a live stream has no duration");
    assert!(pr.playability.is_ok());
}

#[test]
fn vod_fixture_not_live_has_duration() {
    let pr = PlayerResponse::parse(VOD_FIXTURE).unwrap();
    assert!(!pr.is_live, "vod fixture is not live");
    assert_eq!(pr.duration_ms, Some(15_000), "lengthSeconds=15 → 15000ms");
    assert!(pr.playability.is_ok());
}

#[test]
fn vod_formats_carry_init_and_index_range() {
    // indexRange.end bounds the seek-index prefix of a progressive cold start.
    let pr = PlayerResponse::parse(VOD_FIXTURE).unwrap();
    let f = pr
        .formats
        .iter()
        .find(|f| f.index_range.is_some())
        .expect("vod fixture has formats with an indexRange");
    let init = f.init_range.expect("a format with indexRange also has initRange");
    let index = f.index_range.unwrap();
    assert!(init.start <= init.end, "init range well-ordered");
    assert!(index.end > index.start, "index range non-empty");
    assert!(index.end >= init.end, "index follows init in the byte layout");
}

#[test]
fn login_required_without_streaming_data_is_unplayable() {
    let json = r#"{"playabilityStatus":{"status":"LOGIN_REQUIRED","reason":"Sign in"}}"#;
    let err = PlayerResponse::parse(json).unwrap_err();
    match err {
        innertube::PlayerError::Unplayable { playability } => {
            match playability {
                innertube::Playability::LoginRequired { reason } => {
                    assert_eq!(reason.as_deref(), Some("Sign in"));
                }
                other => panic!("expected LoginRequired, got {other:?}"),
            }
        }
        other => panic!("expected Unplayable, got {other:?}"),
    }
}

#[test]
fn parses_audio_track_xtags_from_dubbed_fixture() {
    // The VOD fixture is a dubbed video; xtags select the audio track.
    let pr = PlayerResponse::parse(VOD_FIXTURE).unwrap();
    let audio: Vec<_> = pr.formats.iter().filter(|f| f.is_audio()).collect();
    assert!(audio.iter().any(|f| f.xtags.is_some()), "dubbed audio formats carry xtags");
    assert!(audio.len() > 1, "dubbed video advertises multiple audio tracks");
}
