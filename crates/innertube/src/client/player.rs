//! The `player()` response: formats, audio tracks, playability, and its serde shapes.

use super::*;

/// A `[start, end]` byte span within a DASH representation, as advertised in the
/// player response (`initRange` / `indexRange`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

/// A streamable format advertised in the player response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Format {
    pub itag: i32,
    pub mime_type: String,
    /// `lastModified` (lmt), part of the SABR `FormatId` (REQ-005).
    pub last_modified: u64,
    /// Pixel dimensions (video only; `None` for audio).
    pub width: Option<i32>,
    pub height: Option<i32>,
    /// Average bitrate in bits/sec, when advertised.
    pub bitrate: Option<i64>,
    /// `contentLength`: total bytes of the progressive/DASH representation (VOD only; `None`
    /// for live). Sizes a seekable virtual file and bounds the last segment of a seek
    /// index.
    pub content_length: Option<u64>,
    /// `initRange`: byte span of the container init segment (header). VOD/DASH only.
    pub init_range: Option<ByteRange>,
    /// `indexRange`: byte span of the seek index (WebM `Cues` / MP4 `sidx`). Its `end` is
    /// the prefix length needed to parse the index (IT-010).
    pub index_range: Option<ByteRange>,
    /// `xtags`: the SABR track disambiguator. Selecting an audio track on a dubbed video
    /// needs it, since several formats share one itag; without it the server answers
    /// `sabr.no_audio_selected` (SABR-038, SABR-045).
    pub xtags: Option<String>,
    /// `audioTrack.audioIsDefault`: true for the video's default/original audio track.
    /// Absent (false) on single-track videos.
    pub audio_is_default: bool,
    /// `audioTrack.id` (e.g. `de-DE.10`): the stable per-language track id that selects a
    /// dub. `None` on single-track videos (no `audioTrack`).
    pub audio_track_id: Option<String>,
    /// `audioTrack.displayName` (e.g. `German (Germany)`): human-readable track name for an
    /// audio-language menu. `None` on single-track videos.
    pub audio_track_name: Option<String>,
}

impl Format {
    pub fn is_audio(&self) -> bool {
        self.mime_type.starts_with("audio/")
    }
    pub fn is_video(&self) -> bool {
        self.mime_type.starts_with("video/")
    }
    /// Whether this format's audio is an auto-dub (machine translation), read from the
    /// `xtags` marker `acont=dubbed-auto`. `xtags` is base64 of a small protobuf and the
    /// marker survives as a byte substring, so no protobuf decode is needed. Human and
    /// original tracks (`acont=dubbed` / `acont=original`) and single-track videos give
    /// false.
    fn xtags_is_auto_dubbed(&self) -> bool {
        self.xtags.as_deref().map(xtags_marks_auto_dub).unwrap_or(false)
    }
}

const DUBBED_AUTO: &[u8] = b"dubbed-auto";

/// True when a base64 `xtags` string carries `acont=dubbed-auto`. `acont=dubbed` and
/// `acont=original` give false (`dubbed-auto` is a longer, distinct string).
fn xtags_marks_auto_dub(xtags: &str) -> bool {
    decode_base64_lenient(xtags)
        .map(|b| b.windows(DUBBED_AUTO.len()).any(|w| w == DUBBED_AUTO))
        .unwrap_or(false)
}

/// One selectable audio (dub) track on a multi-language video. Surfaced by
/// [`PlayerResponse::audio_tracks`] so a consumer can populate a language menu and
/// pass the chosen [`AudioTrack::id`] back as the SABR audio-track selector.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioTrack {
    /// Stable id matching `audioTrack.id` (e.g. `de-DE.10`): pass this to select the track.
    pub id: String,
    /// Human-readable name (e.g. `German (Germany)`).
    pub display_name: String,
    /// The video's default/original track (what plays when nothing is selected).
    pub is_default: bool,
    /// Machine-translated auto-dub (vs. an original or human dub).
    pub is_auto_dubbed: bool,
}

/// The fields of a `player()` response the SABR engine needs.
#[derive(Debug, Clone)]
pub struct PlayerResponse {
    /// Where SABR media requests are POSTed.
    pub server_abr_streaming_url: String,
    /// Decoded ustreamer config bytes (request protobuf field 5).
    pub ustreamer_config: Vec<u8>,
    /// Adaptive (DASH/SABR) formats, audio and video.
    pub formats: Vec<Format>,
    /// `videoDetails.isLive`: true for a live broadcast, false for VOD. Selects the engine's
    /// playhead mode (live-edge sentinel vs. sequential from 0).
    pub is_live: bool,
    /// Total duration in ms from `videoDetails.lengthSeconds` (VOD only; `None` for
    /// live or when absent). Used for end-of-stream detection.
    pub duration_ms: Option<i64>,
    /// A live stream's segment length from the formats' `targetDurationSec`.
    pub live_segment_ms: Option<i64>,
    /// Typed `playabilityStatus` so a GUI can render the right call-to-action.
    pub playability: Playability,
    /// `playerConfig.livePlayerConfig.liveReadaheadSeconds`: a per-stream read-ahead depth
    /// hint in seconds, exposed as metadata. `base.js` never reads it (SABR-025).
    pub live_readahead_secs: Option<f64>,
    /// Seek-preview thumbnails (`storyboards.playerStoryboardSpecRenderer`). `None` for
    /// live or when absent. Used by a player to show a thumbnail on scrubber hover.
    pub storyboard: Option<Storyboard>,
    /// Live seek-preview thumbnails (`storyboards.playerLiveStoryboardSpecRenderer`).
    pub live_storyboard: Option<LiveStoryboard>,
    /// `videoDetails.liveChunkReadahead`: the chunk count the web client holds ahead at the
    /// live edge (default 3 when absent or NaN). It drives edge read-ahead (SABR-022);
    /// [`PlayerResponse::live_readahead_secs`] does not. Raw value; consumers apply the
    /// default. Often absent (normal-latency streams, VOD).
    pub live_chunk_readahead: Option<u32>,
    /// `videoDetails.latencyClass`: the declared live-latency tier, parsed from the
    /// `MDE_STREAM_OPTIMIZATIONS_RENDERER_LATENCY_*` string by exact match (`ULTRA_LOW` also
    /// ends in `LOW`). `None` for VOD, an unrecognized value, or absent (SABR-024).
    pub latency_class: Option<LatencyClass>,
    /// `videoDetails.isLowLatencyLiveStream`: defaults to `false` when absent.
    pub is_low_latency_live_stream: bool,
    /// `playerConfig.mediaCommonConfig.dynamicReadaheadConfig`: the read-ahead ramp
    /// `target(t) = min(min + growth·t, max)` ms (SABR-021). `None` when the stream
    /// advertises none.
    pub dynamic_readahead: Option<DynamicReadaheadConfig>,
    /// `heartbeatParams.heartbeatServerData`: the opaque base64 blob the WEB client echoes
    /// in each `player/heartbeat` POST (IT-014). The response returns a fresh one for the
    /// next beat. `None` when the response has no heartbeat params.
    pub heartbeat_server_data: Option<String>,
    /// `heartbeatParams.intervalMilliseconds`: how often the WEB client POSTs
    /// `player/heartbeat` (15000 in the captures). `None` when absent.
    pub heartbeat_interval_ms: Option<u64>,
    /// Caption tracks (`captions.playerCaptionsTracklistRenderer.captionTracks`).
    pub captions: Vec<CaptionTrack>,
}

/// `videoDetails.latencyClass` as the three known tiers. Parsed by exact string match on
/// `MDE_STREAM_OPTIMIZATIONS_RENDERER_LATENCY_{NORMAL,LOW,ULTRA_LOW}`; anything else,
/// including absent, is `None`. A suffix match on "LOW" would also catch `ULTRA_LOW`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatencyClass {
    Normal,
    Low,
    UltraLow,
}

impl LatencyClass {
    /// Exact-match a raw `latencyClass` string into a typed tier.
    fn from_raw(s: &str) -> Option<LatencyClass> {
        match s {
            "MDE_STREAM_OPTIMIZATIONS_RENDERER_LATENCY_NORMAL" => Some(LatencyClass::Normal),
            "MDE_STREAM_OPTIMIZATIONS_RENDERER_LATENCY_LOW" => Some(LatencyClass::Low),
            "MDE_STREAM_OPTIMIZATIONS_RENDERER_LATENCY_ULTRA_LOW" => Some(LatencyClass::UltraLow),
            _ => None,
        }
    }

    /// `true` for `Low` and `UltraLow`.
    pub fn is_low_latency(&self) -> bool {
        matches!(self, LatencyClass::Low | LatencyClass::UltraLow)
    }
}

/// `playerConfig.mediaCommonConfig.dynamicReadaheadConfig`, as sent. Any field can be
/// absent; `base.js` then falls back to 15 s, 1 s and 120 s (SABR-021). Consumers apply
/// the defaults; this type carries the raw, possibly partial config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DynamicReadaheadConfig {
    pub min_read_ahead_media_time_ms: Option<i64>,
    pub max_read_ahead_media_time_ms: Option<i64>,
    pub read_ahead_growth_rate_ms: Option<i64>,
}

/// Typed `playabilityStatus.status` so a GUI can render the right call-to-action
/// (sign-in prompt, age gate, "offline" notice) instead of a generic failure.
/// `reason` is the human string YouTube supplies (`playabilityStatus.reason`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Playability {
    Ok,
    LoginRequired { reason: Option<String> },
    Unplayable { reason: Option<String> },
    AgeVerificationRequired { reason: Option<String> },
    LiveStreamOffline { reason: Option<String> },
    Other { status: String, reason: Option<String> },
}

impl Playability {
    /// Maps a raw `playabilityStatus.status` and optional `reason` to a typed variant.
    pub fn from_status(status: &str, reason: Option<String>) -> Self {
        match status {
            "OK" => Playability::Ok,
            "LOGIN_REQUIRED" => Playability::LoginRequired { reason },
            "UNPLAYABLE" => Playability::Unplayable { reason },
            "AGE_VERIFICATION_REQUIRED" | "CONTENT_CHECK_REQUIRED" => {
                Playability::AgeVerificationRequired { reason }
            }
            "LIVE_STREAM_OFFLINE" => Playability::LiveStreamOffline { reason },
            other => Playability::Other { status: other.to_string(), reason },
        }
    }

    /// Returns `true` iff the video is fully playable (status `"OK"`).
    pub fn is_ok(&self) -> bool {
        matches!(self, Playability::Ok)
    }

    /// The human-readable reason YouTube gave, if any.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Playability::Ok => None,
            Playability::LoginRequired { reason }
            | Playability::Unplayable { reason }
            | Playability::AgeVerificationRequired { reason }
            | Playability::LiveStreamOffline { reason }
            | Playability::Other { reason, .. } => reason.as_deref(),
        }
    }
}

/// Errors parsing a `player()` response.
#[derive(Debug, thiserror::Error)]
pub enum PlayerError {
    #[error("invalid player response JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("player response is not streamable (missing serverAbrStreamingUrl or ustreamer config)")]
    NotStreamable,
    #[error("ustreamer config is not valid base64")]
    Base64,
    /// A `get_watch` body carried no `playerResponse` sub-response.
    #[error("get_watch response carries no playerResponse")]
    NoPlayerResponse,
    /// The video is not playable for a typed reason (login/age/offline/geo). Carries
    /// the typed `Playability` so the GUI can prompt the right action.
    #[error("video unplayable: {playability:?}")]
    Unplayable { playability: Playability },
}

impl PlayerResponse {
    /// The distinct selectable audio (dub) tracks, deduped by id, in first-seen order.
    /// Empty on single-track videos (no `audioTrack` metadata). Pass an
    /// [`AudioTrack::id`] to the SABR audio-track selector to play that language.
    pub fn audio_tracks(&self) -> Vec<AudioTrack> {
        let mut out: Vec<AudioTrack> = Vec::new();
        for f in self.formats.iter().filter(|f| f.is_audio()) {
            let (Some(id), Some(name)) = (&f.audio_track_id, &f.audio_track_name) else {
                continue;
            };
            if out.iter().any(|t| &t.id == id) {
                continue;
            }
            out.push(AudioTrack {
                id: id.clone(),
                display_name: name.clone(),
                is_default: f.audio_is_default,
                is_auto_dubbed: f.xtags_is_auto_dubbed(),
            });
        }
        out
    }

    /// Parse the player response YouTube renders into a watch page as
    /// `ytInitialPlayerResponse`. The web client plays from it on a fresh navigation
    /// (IT-005), and the SABR session behind it is ready, where a session provisioned by a
    /// `player()` POST cold-starts. Returns `None` when the page embeds no streamable
    /// response (age-gated, login-required, or a non-WEB page), so the caller can fall back
    /// to a `player()` POST.
    pub fn from_watch_html(html: &str) -> Option<PlayerResponse> {
        let json = extract_json_object_after(html, "ytInitialPlayerResponse =")?;
        PlayerResponse::parse(&json).ok()
    }

    /// Parse the JSON body of a `player()` response.
    ///
    /// With `INNERTUBE_DUMP=raw:<dir>` each response is first written to
    /// `<dir>/raw/player/<n>.json` in arrival order. SSR (via [`Self::from_watch_html`]) and
    /// POST (via `Session::player_raw`) both pass through here, so one hook captures both.
    ///
    /// The dumps contain session credentials: keep them out of the repository.
    pub fn parse(json: &str) -> Result<PlayerResponse, PlayerError> {
        dump_player_response(json);
        let raw: RawPlayer = serde_json::from_str(json)?;

        let playability = match &raw.playability_status {
            Some(ps) => Playability::from_status(
                ps.status.as_deref().unwrap_or("UNKNOWN"),
                ps.reason.clone(),
            ),
            None => Playability::Ok,
        };
        let is_live = raw.video_details.as_ref().map(|v| v.is_live).unwrap_or(false);
        let duration_ms = raw
            .video_details
            .as_ref()
            .and_then(|v| v.length_seconds.as_deref())
            .and_then(|s| s.parse::<i64>().ok())
            .filter(|&s| s > 0)
            .map(|s| s * 1000);

        let streaming = match raw.streaming_data {
            Some(s) => s,
            None => return Err(PlayerError::Unplayable { playability }),
        };
        let server_abr_streaming_url = match streaming.server_abr_streaming_url {
            Some(u) => u,
            None => return Err(PlayerError::Unplayable { playability }),
        };
        let live_readahead_secs = raw
            .player_config
            .as_ref()
            .and_then(|c| c.live_player_config.as_ref())
            .and_then(|c| c.live_readahead_seconds);
        let live_chunk_readahead =
            raw.video_details.as_ref().and_then(|v| v.live_chunk_readahead);
        let latency_class = raw
            .video_details
            .as_ref()
            .and_then(|v| v.latency_class.as_deref())
            .and_then(LatencyClass::from_raw);
        let is_low_latency_live_stream = raw
            .video_details
            .as_ref()
            .map(|v| v.is_low_latency_live_stream)
            .unwrap_or(false);
        let dynamic_readahead = raw
            .player_config
            .as_ref()
            .and_then(|c| c.media_common_config.as_ref())
            .and_then(|c| c.dynamic_readahead_config.as_ref())
            .map(|c| DynamicReadaheadConfig {
                min_read_ahead_media_time_ms: c.min_read_ahead_media_time_ms,
                max_read_ahead_media_time_ms: c.max_read_ahead_media_time_ms,
                read_ahead_growth_rate_ms: c.read_ahead_growth_rate_ms,
            });
        let storyboard = raw.storyboards.as_ref().and_then(|s| s.player_storyboard_spec_renderer.as_ref()).and_then(|r| {
            let spec = r.spec.as_deref()?;
            Storyboard::parse(spec, r.fine_scrubbing_recommended_level.or(r.recommended_level))
        });
        let live_storyboard = raw
            .storyboards
            .as_ref()
            .and_then(|s| s.player_live_storyboard_spec_renderer.as_ref())
            .and_then(|r| LiveStoryboard::parse(r.spec.as_deref()?));
        let (heartbeat_server_data, heartbeat_interval_ms) = raw
            .heartbeat_params
            .as_ref()
            .map(|h| {
                (
                    h.heartbeat_server_data.clone(),
                    h.interval_milliseconds.as_deref().and_then(|s| s.parse().ok()),
                )
            })
            .unwrap_or((None, None));
        let ustreamer_config = raw
            .player_config
            .and_then(|c| c.media_common_config)
            .and_then(|c| c.media_ustreamer_request_config)
            .and_then(|c| c.video_playback_ustreamer_config)
            .map(|b64| decode_base64_lenient(&b64).ok_or(PlayerError::Base64))
            .transpose()?
            .unwrap_or_default();
        let live_segment_ms = streaming
            .adaptive_formats
            .iter()
            .find_map(|f| f.target_duration_sec)
            .map(|s| (s * 1000.0).round() as i64);
        let formats = streaming
            .adaptive_formats
            .into_iter()
            .map(|f| Format {
                itag: f.itag,
                mime_type: f.mime_type,
                last_modified: f.last_modified.and_then(|s| s.parse().ok()).unwrap_or(0),
                width: f.width,
                height: f.height,
                bitrate: f.bitrate,
                content_length: f.content_length.and_then(|s| s.parse().ok()),
                xtags: f.xtags,
                audio_is_default: f.audio_track.as_ref().map(|t| t.audio_is_default).unwrap_or(false),
                audio_track_id: f.audio_track.as_ref().and_then(|t| t.id.clone()),
                audio_track_name: f.audio_track.as_ref().and_then(|t| t.display_name.clone()),
                init_range: f.init_range.and_then(|r| r.parse()),
                index_range: f.index_range.and_then(|r| r.parse()),
            })
            .collect();

        Ok(PlayerResponse {
            server_abr_streaming_url,
            ustreamer_config,
            formats,
            is_live,
            duration_ms,
            live_segment_ms,
            playability,
            live_readahead_secs,
            storyboard,
            live_storyboard,
            live_chunk_readahead,
            latency_class,
            is_low_latency_live_stream,
            dynamic_readahead,
            heartbeat_server_data,
            heartbeat_interval_ms,
            captions: raw
                .captions
                .map(|c| api::parse_caption_tracks(&serde_json::json!({ "captions": c })))
                .unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod player_response_tests {
    use super::*;

    const LIVE: &str = include_str!("../../../../notes/fixtures/player-response-live.json");
    const VOD: &str = include_str!("../../../../notes/fixtures/player-response-vod.json");

    // `acont=dubbed-auto` marks an AI dub; `dubbed` is a human dub.
    const XTAGS_AUTO: &str = "ChQKBWFjb250EgtkdWJiZWQtYXV0bwoNCgRsYW5nEgVkZS1ERQ";
    const XTAGS_HUMAN: &str = "Cg8KBWFjb250EgZkdWJiZWQKCgoEbGFuZxICYXI";
    const XTAGS_ORIGINAL: &str = "ChEKBWFjb250EghvcmlnaW5hbAoKCgRsYW5nEgJlbg";

    #[test]
    fn only_ai_dubs_are_marked_auto_dubbed() {
        assert!(xtags_marks_auto_dub(XTAGS_AUTO));
        assert!(!xtags_marks_auto_dub(XTAGS_HUMAN));
        assert!(!xtags_marks_auto_dub(XTAGS_ORIGINAL));
        assert!(!xtags_marks_auto_dub("not base64 %%%"));
    }

    #[test]
    fn a_vod_with_ai_dubs_lists_them_beside_the_original() {
        let tracks = PlayerResponse::parse(VOD).expect("parse").audio_tracks();
        assert!(tracks.iter().any(|t| t.is_auto_dubbed), "the fixture carries dubbed-auto tracks");
        let original = tracks.iter().find(|t| !t.is_auto_dubbed).expect("an original track");
        assert!(original.is_default);
        let mut ids: Vec<&str> = tracks.iter().map(|t| t.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), tracks.len(), "one entry per track id");
    }

    #[test]
    fn a_vod_lists_its_caption_tracks() {
        let pr = PlayerResponse::parse(VOD).expect("parse");
        let en = pr.captions.iter().find(|c| c.language_code == "en").expect("English track");
        assert!(en.auto, "kind=asr is auto-generated");
        assert!(en.base_url.starts_with("https://www.youtube.com/api/timedtext"));
    }

    #[test]
    fn a_low_latency_live_stream_reads_as_ultra_low_not_low() {
        let pr = PlayerResponse::parse(LIVE).expect("parse");
        assert_eq!(pr.latency_class, Some(LatencyClass::UltraLow));
        assert!(pr.is_low_latency_live_stream);
        assert!(pr.live_chunk_readahead.is_some());
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPlayer {
    streaming_data: Option<RawStreamingData>,
    player_config: Option<RawPlayerConfig>,
    video_details: Option<RawVideoDetails>,
    playability_status: Option<RawPlayabilityStatus>,
    storyboards: Option<RawStoryboards>,
    heartbeat_params: Option<RawHeartbeatParams>,
    captions: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawHeartbeatParams {
    heartbeat_server_data: Option<String>,
    interval_milliseconds: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawStoryboards {
    player_storyboard_spec_renderer: Option<RawStoryboardSpec>,
    player_live_storyboard_spec_renderer: Option<RawStoryboardSpec>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawStoryboardSpec {
    spec: Option<String>,
    recommended_level: Option<usize>,
    fine_scrubbing_recommended_level: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawVideoDetails {
    #[serde(default)]
    is_live: bool,
    length_seconds: Option<String>,
    live_chunk_readahead: Option<u32>,
    latency_class: Option<String>,
    #[serde(default)]
    is_low_latency_live_stream: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPlayabilityStatus {
    status: Option<String>,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawStreamingData {
    server_abr_streaming_url: Option<String>,
    #[serde(default)]
    adaptive_formats: Vec<RawFormat>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFormat {
    itag: i32,
    #[serde(default)]
    mime_type: String,
    last_modified: Option<String>,
    width: Option<i32>,
    height: Option<i32>,
    bitrate: Option<i64>,
    content_length: Option<String>,
    xtags: Option<String>,
    audio_track: Option<RawAudioTrack>,
    init_range: Option<RawByteRange>,
    index_range: Option<RawByteRange>,
    target_duration_sec: Option<f64>,
}

#[derive(Deserialize)]
struct RawByteRange {
    start: Option<String>,
    end: Option<String>,
}

impl RawByteRange {
    /// Parse the string-encoded `{start, end}` into a typed [`ByteRange`]; `None` if
    /// either bound is missing or non-numeric.
    fn parse(&self) -> Option<ByteRange> {
        Some(ByteRange {
            start: self.start.as_deref()?.parse().ok()?,
            end: self.end.as_deref()?.parse().ok()?,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAudioTrack {
    #[serde(default)]
    audio_is_default: bool,
    /// e.g. `de-DE.10`: stable per-language id.
    id: Option<String>,
    /// e.g. `German (Germany)`.
    display_name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPlayerConfig {
    media_common_config: Option<RawMediaCommonConfig>,
    live_player_config: Option<RawLivePlayerConfig>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLivePlayerConfig {
    live_readahead_seconds: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMediaCommonConfig {
    media_ustreamer_request_config: Option<RawUstreamerRequestConfig>,
    dynamic_readahead_config: Option<RawDynamicReadaheadConfig>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDynamicReadaheadConfig {
    min_read_ahead_media_time_ms: Option<i64>,
    max_read_ahead_media_time_ms: Option<i64>,
    read_ahead_growth_rate_ms: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawUstreamerRequestConfig {
    video_playback_ustreamer_config: Option<String>,
}

/// Write each raw player response to `<dump>/raw/player/<n>.json`, numbered in arrival
/// order. Off unless `INNERTUBE_DUMP=raw:<dir>`.
///
/// A field of the server-supplied ustreamer config (field 1.25) marks sessions that start
/// with a hold (GATE-006). The config is fixed when `player()` answers, so diffing the
/// responses of a fast and a slow open from one run shows what the server granted.
fn dump_player_response(json: &str) {
    let Some(dir) = crate::client::dump::raw_dir("player") else { return };
    let n = crate::client::dump::next_raw_index();
    {
        // A failed dump must never affect the parse it is observing.
        let _ = std::fs::write(dir.join(format!("{n:04}.json")), json);
        // A run can fetch two responses per video, with and without the field-25 marker
        // (GATE-008), and the session adopts one. The backtrace shows which call site
        // produced each, without changing `parse`'s signature at every call site. Needs
        // `RUST_BACKTRACE=1`.
        let bt = std::backtrace::Backtrace::force_capture();
        let _ = std::fs::write(dir.join(format!("{n:04}.trace.txt")), bt.to_string());
    }
}
