//! Format selection for the streaming `open()` paths.
//!
//! A video ladder holds one codec family, because rungs of different codecs need a fresh
//! container (UMP-016). Audio prefers MP4 on live only (`prefer_mp4`); VOD takes the
//! highest-bitrate default track whatever its codec.

use crate::client::Format;

use crate::bootstrap::StreamError;
use crate::live::Quality;

/// Resolve a quality cap to a max height (`Best` = no cap).
fn target_height(quality: Quality) -> i32 {
    match quality {
        Quality::Best => i32::MAX,
        Quality::Height(h) => h,
    }
}

/// Pick one video codec family and return `(codec, ladder)`: its formats up to the height cap,
/// highest first (by height, then bitrate). The family is the one whose capped ladder reaches
/// highest, ties going av01 > vp9 > avc1. `av1` says whether the consumer decodes AV1; without
/// it av01 is taken only when nothing else plays.
pub(crate) fn select_video_ladder(
    formats: &[Format],
    quality: Quality,
    av1: bool,
) -> Result<(&'static str, Vec<&Format>), StreamError> {
    let target_h = target_height(quality);
    let top = |codec: &str| {
        formats
            .iter()
            .filter(|f| f.is_video() && f.mime_type.contains(codec) && f.height.unwrap_or(0) <= target_h)
            .map(|f| f.height.unwrap_or(0))
            .max()
    };
    let families: &[&'static str] = if av1 { &["av01", "vp9", "avc1"] } else { &["vp9", "avc1"] };
    let codec = families
        .iter()
        .enumerate()
        .filter_map(|(rank, c)| top(c).map(|h| (h, std::cmp::Reverse(rank), *c)))
        .max()
        .map(|(_, _, c)| c)
        .or_else(|| top("av01").map(|_| "av01"))
        .unwrap_or("avc1");
    let mut ladder: Vec<&Format> = formats
        .iter()
        .filter(|f| f.is_video() && f.mime_type.contains(codec) && f.height.unwrap_or(0) <= target_h)
        .collect();
    ladder.sort_by_key(|f| std::cmp::Reverse((f.height.unwrap_or(0), f.bitrate.unwrap_or(0))));
    if ladder.is_empty() {
        return Err(StreamError::NoVideoFormats);
    }
    Ok((codec, ladder))
}

/// The default (original-language) audio track, highest bitrate. Dubbed videos list many
/// itag-140 formats (SABR-045); `audio_is_default` picks the original. `prefer_mp4` favours
/// AAC-LC/mp4 (live) over Opus/WebM.
pub(crate) fn select_default_audio(
    formats: &[Format],
    prefer_mp4: bool,
) -> Result<&Format, StreamError> {
    formats
        .iter()
        .filter(|f| f.is_audio())
        .max_by_key(|f| {
            (
                f.audio_is_default,
                prefer_mp4 && f.mime_type.contains("mp4"),
                f.bitrate.unwrap_or(0),
            )
        })
        .ok_or(StreamError::NoAudioFormats)
}

/// The audio format of the dub with `audioTrack.id` equal to `track_id`. Falls back to the
/// default track when `track_id` is `None` or not advertised. Within a track the tiebreak
/// matches [`select_default_audio`]: mp4 when `prefer_mp4`, then highest bitrate.
pub(crate) fn select_audio_track<'a>(
    formats: &'a [Format],
    prefer_mp4: bool,
    track_id: Option<&str>,
) -> Result<&'a Format, StreamError> {
    if let Some(id) = track_id {
        if let Some(f) = formats
            .iter()
            .filter(|f| f.is_audio() && f.audio_track_id.as_deref() == Some(id))
            .max_by_key(|f| {
                (
                    prefer_mp4 && f.mime_type.contains("mp4"),
                    f.bitrate.unwrap_or(0),
                )
            })
        {
            return Ok(f);
        }
        // Track not advertised: use the default track.
    }
    select_default_audio(formats, prefer_mp4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vid(itag: i32, codec: &str, height: i32, bitrate: i64) -> Format {
        Format {
            itag,
            mime_type: format!("video/{codec}"),
            last_modified: 0,
            width: Some(height * 16 / 9),
            height: Some(height),
            bitrate: Some(bitrate),
            content_length: Some(1000),
            xtags: None,
            audio_is_default: false,
            audio_track_id: None,
            audio_track_name: None,
            init_range: None,
            index_range: None,
        }
    }
    fn aud(itag: i32, mime: &str, bitrate: i64, is_default: bool) -> Format {
        Format {
            itag,
            mime_type: mime.to_string(),
            last_modified: 0,
            width: None,
            height: None,
            bitrate: Some(bitrate),
            content_length: Some(1000),
            xtags: None,
            audio_is_default: is_default,
            audio_track_id: None,
            audio_track_name: None,
            init_range: None,
            index_range: None,
        }
    }
    /// A dub track: one itag-140 format tagged with an `audioTrack.id` + name.
    fn dub(track_id: &str, name: &str, bitrate: i64, is_default: bool) -> Format {
        Format {
            itag: 140,
            mime_type: "audio/mp4".to_string(),
            last_modified: 0,
            width: None,
            height: None,
            bitrate: Some(bitrate),
            content_length: Some(1000),
            xtags: Some(format!("xtags-{track_id}")),
            audio_is_default: is_default,
            audio_track_id: Some(track_id.to_string()),
            audio_track_name: Some(name.to_string()),
            init_range: None,
            index_range: None,
        }
    }

    // A realistic mixed-codec set: vp9 + av01 + avc1 at several heights.
    fn mixed_video() -> Vec<Format> {
        vec![
            vid(248, "webm; codecs=\"vp9\"", 1080, 3_000_000),
            vid(247, "webm; codecs=\"vp9\"", 720, 1_500_000),
            vid(244, "webm; codecs=\"vp9\"", 480, 750_000),
            vid(399, "mp4; codecs=\"av01\"", 1080, 2_800_000),
            vid(136, "mp4; codecs=\"avc1\"", 720, 1_600_000),
        ]
    }

    /// A ladder mixes no codecs (UMP-016), descends in height, and stays under the cap.
    #[test]
    fn a_ladder_is_one_codec_family_descending_within_the_cap() {
        let formats = mixed_video();
        for (quality, cap) in [(Quality::Best, i32::MAX), (Quality::Height(720), 720)] {
            for av1 in [false, true] {
                let (codec, ladder) = select_video_ladder(&formats, quality, av1).unwrap();
                assert!(ladder.iter().all(|f| f.mime_type.contains(codec)));
                assert!(ladder.iter().all(|f| f.height.unwrap() <= cap));
                assert!(ladder.windows(2).all(|w| w[0].height >= w[1].height));
            }
        }
    }

    #[test]
    fn audio_track_selects_requested_dub() {
        // A dubbed video: three itag-140 tracks, one default (original English).
        let formats = vec![
            dub("en-US.4", "English (original)", 130_000, true),
            dub("de-DE.10", "German (Germany)", 132_000, false),
            dub("es-ES.6", "Spanish (Spain)", 131_000, false),
        ];
        // Requesting German picks the German track's format (xtags carried through).
        let a = select_audio_track(&formats, true, Some("de-DE.10")).unwrap();
        assert_eq!(a.audio_track_id.as_deref(), Some("de-DE.10"));
        assert_eq!(a.xtags.as_deref(), Some("xtags-de-DE.10"));
        // No selection → the default/original track.
        let a = select_audio_track(&formats, true, None).unwrap();
        assert_eq!(a.audio_track_id.as_deref(), Some("en-US.4"));
        assert!(a.audio_is_default);
    }

    #[test]
    fn audio_track_default_beats_higher_bitrate_dub() {
        let formats = vec![
            aud(140, "audio/mp4", 130_000, true),   // original/default
            aud(140, "audio/mp4", 256_000, false),  // a dub at higher bitrate
        ];
        let a = select_default_audio(&formats, true).unwrap();
        assert!(a.audio_is_default);
    }

}
