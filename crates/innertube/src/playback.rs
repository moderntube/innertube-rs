//! [`Playback::open`]: one bootstrap, then a VOD or a live session according to the
//! player response.

use std::sync::Arc;

use innertube_js::NSigSolver;
use innertube_pot::PoTokenProvider;

use crate::bootstrap::{bootstrap_player, StartupTimer};
use crate::client::{Navigation, Session};
use crate::{Quality, SabrStream, SabrStreamOptions, SegmentVod, SegmentVodOptions, StreamError};

/// Options for [`Playback::open`].
#[derive(Debug, Clone, Default)]
pub struct PlaybackOptions {
    /// A stored pick, pinned from the first request.
    pub quality: Quality,
    /// Where a VOD starts, in ms. A live stream joins at the edge.
    pub vod_start_ms: i64,
    /// `audioTrack.id` of a dub, or `None` for the original track.
    pub audio_track: Option<String>,
    /// The page and endpoint this playback was opened from.
    pub navigation: Navigation,
    /// The consumer decodes AV1, so an av01 ladder may be chosen.
    pub av1: bool,
}

/// An open playback session of either kind.
#[allow(clippy::large_enum_variant)]
pub enum Playback {
    Vod(SegmentVod),
    Live(SabrStream),
}

impl Playback {
    /// Bootstrap `video_id` once and open the session its player response calls for.
    pub async fn open(
        session: &Session,
        solver: &NSigSolver,
        pot: Arc<dyn PoTokenProvider>,
        video_id: &str,
        opts: &PlaybackOptions,
    ) -> Result<Playback, StreamError> {
        let mut timer = StartupTimer::new();
        let bootstrap =
            bootstrap_player(session, solver, video_id, &opts.navigation, Some(&pot), &mut timer).await?;
        if bootstrap.player.is_live {
            let live = SabrStreamOptions {
                quality: opts.quality,
                audio_track: opts.audio_track.clone(),
                navigation: opts.navigation.clone(),
                av1: opts.av1,
                ..Default::default()
            };
            Ok(Playback::Live(SabrStream::from_bootstrap(session, pot, video_id, bootstrap, &live).await?))
        } else {
            let vod = SegmentVodOptions {
                quality: opts.quality,
                start_ms: opts.vod_start_ms,
                audio_track: opts.audio_track.clone(),
                navigation: opts.navigation.clone(),
                av1: opts.av1,
                ..Default::default()
            };
            Ok(Playback::Vod(SegmentVod::from_bootstrap(session, pot, video_id, bootstrap, &vod, timer).await?))
        }
    }
}
