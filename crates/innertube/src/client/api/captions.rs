//! Caption tracks and `timedtext` cues.

use super::*;

/// A caption track advertised in the player response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptionTrack {
    pub language_code: String,
    pub name: String,
    /// `true` for auto-generated (ASR) captions.
    pub auto: bool,
    pub base_url: String,
}

/// One timed caption cue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caption {
    pub start_ms: i64,
    pub dur_ms: i64,
    pub text: String,
}

/// The caption tracks from a `player` response (`captions.playerCaptionsTracklistRenderer`).
pub(crate) fn parse_caption_tracks(player: &Value) -> Vec<CaptionTrack> {
    let tracks = &player["captions"]["playerCaptionsTracklistRenderer"]["captionTracks"];
    tracks
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| {
                    Some(CaptionTrack {
                        language_code: t["languageCode"].as_str()?.to_string(),
                        name: text(&t["name"]).unwrap_or_default(),
                        auto: t["kind"].as_str() == Some("asr"),
                        base_url: t["baseUrl"].as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a timedtext `json3` body into caption cues.
pub(crate) fn parse_timedtext_json3(json: &Value) -> Vec<Caption> {
    json["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter_map(|e| {
                    let segs = e["segs"].as_array()?;
                    let text: String = segs.iter().filter_map(|s| s["utf8"].as_str()).collect();
                    if text.trim().is_empty() {
                        return None;
                    }
                    Some(Caption {
                        start_ms: e["tStartMs"].as_i64().unwrap_or(0),
                        dur_ms: e["dDurationMs"].as_i64().unwrap_or(0),
                        text,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}
