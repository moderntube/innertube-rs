//! Stateful SABR client: drives successive `videoplayback` POSTs, tracking
//! buffered ranges so the server advances, and reassembling media per format.
//!
//! The consumer loop is: [`SabrEngine::request_bytes`] -> POST -> [`SabrEngine::ingest`],
//! repeat. The engine owns the per-format segment progress that tells the server
//! what we already have.

use std::collections::{BTreeMap, BTreeSet};

use prost::Message;

use crate::proto::misc::{AuthorizedFormat, FormatId, PlaybackAuthorization};
use crate::proto::video_streaming::sabr_context_update::SabrContextWritePolicy;
use crate::proto::video_streaming::streamer_context::{ClientInfo, SabrContext};
use crate::proto::video_streaming::{
    BufferedRange, ClientAbrState, CuepointList, LiveMetadata, MediaCapabilityCaps,
    MediaHeader, NextRequestPolicy, PlaybackStartPolicy, ReloadPlaybackContext,
    SabrContextSendingPolicy, SabrContextUpdate, SabrError, SabrSeek, SsapPlaybackInfo,
    StreamProtectionStatus, StreamerContext, VideoPlaybackAbrRequest,
};
use crate::{parse, read_varint, AsUmp, Bytes, Directive};
use crate::container::LeadIn;
use ingest::ResponseState;

mod gate;
mod ingest;
mod request;
#[cfg(test)]
mod streaming_ingest_tests;
#[cfg(test)]
mod tests;

/// A format the engine requests, identified for the SABR `FormatId`.
///
/// `is_video` routes the itag into `preferred_video_format_ids` (17) or
/// `preferred_audio_format_ids` (16). The engine does not send `selected_format_ids` (2).
/// In the `all_formats` argument the struct supplies `lastModified` and the video or audio
/// kind of every advertised itag.
#[derive(Debug, Clone, Default)]
pub struct SelectedFormat {
    pub itag: i32,
    pub last_modified: u64,
    pub is_video: bool,
    /// SABR `xtags` track disambiguator (multi-audio-track videos). `None` for the
    /// single-track common case.
    pub xtags: Option<String>,
}

/// One track's cache-derived buffered range for the VOD random-access path
/// ([`SabrEngine::set_vod_buffered_ranges`]). Mirrors the wire `BufferedRange` without
/// leaking the prost type.
///
/// Fields are wire values: the segment indices are 1-based `MEDIA_HEADER.sequence_number`s,
/// and `start_time_ms` and `duration_ms` span the contiguous held run in media time
/// (REQ-008). The engine adds the `FormatId` (itag, lmt, xtags).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VodBufferedRange {
    pub itag: i32,
    pub start_time_ms: i64,
    pub duration_ms: i64,
    pub start_segment_index: i32,
    pub end_segment_index: i32,
}

/// One whole media segment as the server delivered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSegment {
    pub itag: i32,
    /// `None` on the init/index region.
    pub seq: Option<i64>,
    pub start_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub init: bool,
    pub bytes: Vec<u8>,
}

/// Live `MEDIA_HEADER`s carry a `sequence_number` and absolute `start_ms` but no
/// per-segment duration, so we track the segment-index range and the start-time
/// span, estimating segment duration from consecutive `start_ms` gaps.
#[derive(Debug, Clone)]
struct Progress {
    min_seq: i64,
    max_seq: i64,
    first_start_ms: i64,
    last_start_ms: i64,
    seg_dur_ms: i64,
}

impl Progress {
    fn new(seq: i64, start_ms: i64, seg_dur_ms: i64) -> Self {
        // Live headers carry no duration: a lone segment reports the seeded `seg_dur_ms`
        // until start_ms gaps teach it.
        Self { min_seq: seq, max_seq: seq, first_start_ms: start_ms, last_start_ms: start_ms, seg_dur_ms }
    }
    /// Buffered span in ms: from the first segment's start to the estimated end of the
    /// latest one (see [`SabrEngine::range_end_ms`]).
    fn duration_ms(&self) -> i64 {
        self.last_start_ms - self.first_start_ms + self.seg_dur_ms
    }
}

/// An active manual video-quality pick (REQ-018): the request collapses
/// `preferred_video_format_ids` to this itag, sets `last_manual_selected_resolution` (16)
/// and `sticky_resolution` (21) to `height`, `last_manual_direction` (14) to `direction`,
/// and `time_since_last_manual_format_selection_ms` (13) to the ms since `picked_at`. These
/// fields are sent only while a pick is active.
#[derive(Debug, Clone)]
struct ManualPick {
    itag: i32,
    height: i32,
    direction: i32,
    picked_at: std::time::Instant,
}

/// A break in per-itag segment-sequence continuity, observed in arrival order. Media is
/// concatenated without reordering, so a gap, duplicate or out-of-order segment reaches
/// the muxer. Diagnostic only; the engine takes no action on these.
#[derive(Debug, Clone)]
pub struct Discontinuity {
    pub itag: i32,
    pub kind: DiscontinuityKind,
    /// Last segment sequence emitted for this itag before the anomaly.
    pub prev_seq: i64,
    /// The anomalous segment's sequence.
    pub seq: i64,
    /// The anomalous segment's absolute stream time (`MediaHeader.start_ms`).
    pub start_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscontinuityKind {
    /// `seq` jumped forward by more than 1: segments are missing between them.
    Gap,
    /// `seq <= prev_seq`: a duplicate or out-of-order segment.
    Backward,
}

/// Live-edge seek sentinel: a `player_time_ms` of `2^53 - 1` asks the server for the live
/// edge (SABR-009). Sent on the first request, before any head is known. 0 asks for the
/// start of the DVR window (SABR-044).
pub const LIVE_EDGE_SEEK_MS: i64 = 9_007_199_254_740_991;

/// Per-chunk floor (ms) for the live readahead target until a real segment duration is
/// learned. Without it a lone cold-start segment gives a zero target and closes the gate.
const LIVE_CHUNK_FLOOR_MS: i64 = 1_000;

/// Live-edge chunk count when the player response has no `liveChunkReadahead` (SABR-022).
pub const DEFAULT_LIVE_CHUNK_READAHEAD: u32 = 3;

/// A server SABR error (part 44), hand-rolled so the public `Ingest`/`MergeOut`
/// don't leak the generated prost type (consistent with their primitive siblings).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SabrErrorInfo {
    pub r#type: Option<String>,
    pub code: Option<i32>,
}

/// Result of ingesting one UMP response.
#[derive(Debug, Default)]
pub struct Ingest {
    /// Newly received media bytes this round, per itag.
    pub media: BTreeMap<i32, Vec<u8>>,
    /// Segments whose MEDIA_END arrived in this call, whole and in arrival order, when
    /// [`SabrEngine::set_collect_segments`] is on. Every segment is reported, resends
    /// included: the consumer's store dedups by `(itag, seq)`.
    pub segments: Vec<MediaSegment>,
    /// Per itag, the minimum `MEDIA_HEADER.start_ms` among emitted segments whose header
    /// carries one: the time anchor of this round's media blob for VOD caching. The VOD
    /// init header has no `start_ms` (UMP-009), so a cold response anchors at its first
    /// real segment.
    pub media_start_ms: BTreeMap<i32, i64>,
    /// Per-itag sequence-continuity anomalies seen this round (diagnostic; empty
    /// when every received segment is exactly the previous emitted seq + 1).
    pub discontinuities: Vec<Discontinuity>,
    /// A `SABR_REDIRECT`: switch subsequent POSTs to this URL (n must be re-solved).
    pub redirect: Option<String>,
    /// A `RELOAD_PLAYER_RESPONSE` (part 46): the token to pass to a fresh
    /// `player()` call (`PlayerOptions.reload_token`). The server withholds media
    /// until the session is reloaded (it demands this to rotate the session config).
    pub reload_token: Option<String>,
    /// `STREAM_PROTECTION_STATUS.status` (part 58) for this round, if present. The
    /// server sends `1` (OK) every healthy round; once our PO token goes stale it
    /// sends `3` (ATTESTATION_REQUIRED) with no media and keeps doing so until we
    /// re-attest with a fresh token. The caller acts on `>= 2` by re-minting.
    pub protection_status: Option<i32>,
    /// A `SABR_ERROR` (part 44): type and code (SABR-038). yt-dlp and LuanRT treat it as
    /// ending the response read.
    pub sabr_error: Option<SabrErrorInfo>,
    /// A `SABR_SEEK` (part 45): the server repositioned the live session. Value is
    /// the seek target in **ms** (`seek_media_time` ticks ÷ `timescale` × 1000). The
    /// caller should jump its playhead (`player_time_ms`) to this; the engine has
    /// already reset its dedup/lead-in so the post-seek segments are accepted.
    pub sabr_seek_ms: Option<i64>,
    /// `LIVE_METADATA.head_sequence_number` (part 31, field 3) for this round, if present:
    /// the live edge as a segment index. The request gate does not use it.
    pub head_sequence_number: Option<i64>,
    /// `LIVE_METADATA.wall_time_ms` (part 31, field 5) for this round, if present: the server
    /// wall clock at the live head, in ms since the Unix epoch (SABR-014). The request gate
    /// does not use it.
    pub wall_time_ms: Option<i64>,
    /// An ad `CUEPOINT_LIST` (part 69) this round: the server withholds content media for
    /// the break's duration and reports where the requested position sits inside it.
    pub ad_break: Option<AdBreak>,
}

/// The ad cue a request reports in field 24.
#[derive(Debug, Clone)]
struct AdCue {
    id: String,
    start_time_ms: Option<i64>,
    duration_ms: i64,
    stopped: bool,
}

/// A live ad break announced by `CUEPOINT_LIST`, relative to the requested position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdBreak {
    /// The first content segment the break withholds.
    pub start_sequence_number: Option<i32>,
    /// `total_duration_sec - playhead_time_sec`: how far past the requested position the break ends.
    pub remaining_ms: i64,
}

pub struct SabrEngine {
    // Held once per session and re-attached to every request. `Bytes` makes that a
    // refcount bump instead of a copy of the multi-KB buffers.
    ustreamer_config: Bytes,
    po_token: Bytes,
    selected: Vec<SelectedFormat>,
    lmt: BTreeMap<i32, u64>,
    /// itag -> xtags for the selected formats: the track disambiguator echoed in
    /// acked `BufferedRange.format_id`. Multi-audio-track videos have many formats
    /// per itag; the chosen track's xtags win.
    xtags: BTreeMap<i32, String>,
    /// itag -> is_video, for every advertised format. The readahead gate needs to
    /// classify whatever itag the server actually sends (it may substitute formats).
    kind: BTreeMap<i32, bool>,
    progress: BTreeMap<i32, Progress>,
    lead: BTreeMap<i32, LeadIn>,
    /// Live DVR join override: join at this stream-absolute ms instead of the edge sentinel
    /// while nothing is buffered. See [`SabrEngine::set_live_join_ms`].
    live_join_ms: Option<i64>,
    /// The target of the most recent `SABR_SEEK`, used for the next request only.
    ///
    /// The server answers the live-edge sentinel with a `SABR_SEEK` and no media, and the
    /// request at that target returns media (SABR-009, SABR-011). Re-sending the sentinel
    /// earns another seek and a wasted round trip.
    last_sabr_seek_ms: Option<i64>,
    /// Client viewport (width, height) for `ClientAbrState` fields 18 and 19: the player
    /// element's size, not the screen's (REQ-015). `None` omits it.
    viewport: Option<(i32, i32)>,
    /// Resolution ceiling for `field72.max_resolution` (REQ-012), separate from `viewport`.
    /// `None` falls back to the viewport height.
    res_ceiling: Option<i32>,
    /// Whether this engine is an inline player (hover preview) rather than the main in-page
    /// player. Drives `ClientAbrState.visibility` (34): 5 inline, 0 main (REQ-014).
    inline: bool,
    /// Client identity echoed in `StreamerContext.client_info` (REQ-024). It should match
    /// the client that minted the SABR URL.
    client_info: Option<SabrClientInfo>,
    /// `ClientAbrState.bandwidth_estimate` (bytes/s): the signal the server's automatic rung
    /// choice follows (REQ-016). Defaults to 3 MB/s; callers override it with a measured or
    /// seeded rate.
    bandwidth_estimate: i64,
    /// `ClientAbrState.playback_rate` (35). Sent only when the rate is not 1 (REQ-019), so
    /// 1.0 omits the field.
    playback_rate: f32,
    /// Server-declared furthest and earliest playable stream time in ms, from
    /// `LIVE_METADATA` (SABR-014). The request ceiling is `max_seekable` (SABR-017). `None`
    /// until the server sends them.
    max_seekable_ms: Option<i64>,
    min_seekable_ms: Option<i64>,

    /// Latest live-edge stream time in ms (`LIVE_METADATA.head_time_ms`).
    live_head_ms: Option<i64>,
    /// Opaque playback cookie from the latest `NEXT_REQUEST_POLICY`, echoed in
    /// `StreamerContext.playback_cookie` (REQ-023).
    playback_cookie: Option<Bytes>,
    /// Set by [`SabrEngine::set_ad_report`]: report ad cues in request field 24, with this
    /// `unknown_field_5`. `None` sends no field 24.
    ad_report: Option<Option<i32>>,
    /// The ad cue from the latest CUEPOINT_LIST, reported while `ad_report` is set.
    ad_cue: Option<AdCue>,
    /// `player_time_ms` of the latest request: a cue's position is relative to it.
    last_request_pt: std::cell::Cell<Option<i64>>,
    /// Latest server-dictated request pacing/buffering hints (readahead targets,
    /// backoff). The caller paces the loop with [`SabrEngine::request_policy`].
    next_policy: Option<NextRequestPolicy>,
    /// Latest `PLAYBACK_START_POLICY` (part 47): bandwidth-tiered readahead
    /// brackets the real player consults before releasing playback. Surfaced via
    /// [`SabrEngine::start_min_readahead_ms`].
    start_policy: Option<PlaybackStartPolicy>,
    /// Correlation state for the response being ingested in chunks. The whole-body path
    /// passes a throwaway state instead.
    resp_state: ResponseState,
    /// Cumulative media bytes received per itag. Divided by the media span in `progress`, it
    /// gives the byterate the readahead budget uses. The unit of `MediaHeader.bitrate_bps`
    /// is ambiguous, so the byterate is measured instead of read from headers.
    bytes_by_itag: BTreeMap<i32, u64>,
    /// SABR context values (`SABR_CONTEXT_UPDATE`, part 57) keyed by `type`. The
    /// server pushes reactive ABR/attestation policy here; the real web player
    /// stores it and echoes it back in `StreamerContext.sabr_contexts`. `write_policy`
    /// is applied on update; `SABR_CONTEXT_SENDING_POLICY` (59) discards entries.
    sabr_contexts: BTreeMap<i32, Vec<u8>>,
    /// Which stored context `type`s are *actively sent* (full value in
    /// `sabr_contexts`); the rest are listed type-only in `unsent_sabr_contexts`.
    /// `send_by_default` + sending-policy start/stop toggle membership.
    active_sabr_types: BTreeSet<i32>,
    /// Highest segment sequence emitted per itag: the dedup high-water mark.
    /// Media for any seq `<= emit_seq` is dropped (already delivered: a backward
    /// resend, or an overlapping concurrent response once pipelined), so the
    /// emitted byte stream is monotonic and never resplices a duplicate. Distinct
    /// from `Progress.max_seq` (buffered-range acking) which advances on arrival.
    emit_seq: BTreeMap<i32, i64>,
    /// Live vs VOD. Live seeks to the live edge on cold start and (re)join and sends
    /// the caller's playhead as `player_time`; VOD plays sequentially from 0 (or a
    /// seek target) and advances from the buffered tail. Default true (live).
    is_live: bool,
    /// Report complete segments in [`Ingest::segments`].
    collect_segments: bool,
    /// Keep the reported ranges across a SABR_SEEK (a joined raw stream).
    keep_ranges_on_seek: bool,
    /// Duration a lone live segment's range reports before a second one arrives.
    live_segment_ms: i64,
    /// Live-edge chunk count (`videoDetails.liveChunkReadahead`, default 3; SABR-022). The
    /// gate holds about this many segments buffered ahead of the playhead
    /// (`live_target_readahead_ms`). Clamped to [1, 8].
    live_chunk_readahead: u32,
    /// VOD cache mode: cache-derived `buffered_ranges` from the random-access VOD layer
    /// ([`SabrEngine::set_vod_buffered_ranges`]). `Some`, even empty, makes `request_bytes`
    /// report exactly these ranges, since `progress` can over-report evicted segments and
    /// over-reporting makes the server withhold media. `ingest` then emits media verbatim:
    /// the cache layer aligns containers and dedups, and a continuation response sends no
    /// init for the lead-in gate to find.
    vod_ranges: Option<Vec<VodBufferedRange>>,
    /// Active manual video-quality pick, if any. See [`SabrEngine::set_video_quality`].
    manual_pick: Option<ManualPick>,
    /// Height of the previous manual pick, so a later pick's `last_manual_direction` is
    /// relative to it. `None` before any pick; the first pick counts as an upshift.
    last_manual_height: Option<i32>,
}

/// Server-dictated pacing/buffering hints from `NEXT_REQUEST_POLICY` (part 35).
#[derive(Debug, Clone, Default)]
pub struct RequestPolicy {
    /// How far ahead of the playhead to keep each track buffered (ms).
    pub target_audio_readahead_ms: Option<i32>,
    pub target_video_readahead_ms: Option<i32>,
    /// Wait this long before issuing the next request (ms).
    pub backoff_time_ms: Option<i32>,
    /// Don't let more than this elapse between requests or the session lapses (ms).
    pub max_time_since_last_request_ms: Option<i32>,
}

/// Identifies the INNERTUBE client to the SABR edge (`StreamerContext.client_info`).
#[derive(Debug, Clone)]
pub struct SabrClientInfo {
    pub client_name: i32,
    pub client_version: String,
    pub os_name: String,
    pub os_version: String,
}

impl SabrEngine {
    /// `selected` are the formats we request; `all_formats` supplies the
    /// `lastModified` for every advertised itag (the server may send any of them).
    pub fn new(
        ustreamer_config: Vec<u8>,
        po_token: Vec<u8>,
        selected: Vec<SelectedFormat>,
        all_formats: &[SelectedFormat],
    ) -> Self {
        let kind = all_formats.iter().map(|f| (f.itag, f.is_video)).collect();
        // lmt/xtags keyed by itag. A multi-audio-track video advertises many formats
        // sharing one itag (the dubs), so `all_formats` collides on itag; the
        // *selected* formats are authoritative for the tracks we actually request and
        // ack, so overlay them last.
        let mut lmt: BTreeMap<i32, u64> = all_formats.iter().map(|f| (f.itag, f.last_modified)).collect();
        let mut xtags: BTreeMap<i32, String> = BTreeMap::new();
        for f in &selected {
            lmt.insert(f.itag, f.last_modified);
            if let Some(x) = &f.xtags {
                xtags.insert(f.itag, x.clone());
            }
        }
        Self {
            ustreamer_config: ustreamer_config.into(),
            po_token: po_token.into(),
            selected,
            lmt,
            xtags,
            kind,
            progress: BTreeMap::new(),
            lead: BTreeMap::new(),
            live_join_ms: None,
            last_sabr_seek_ms: None,
            viewport: None,
            res_ceiling: None,
            inline: false,
            client_info: None,
            // Callers override this with a measured or seeded estimate.
            bandwidth_estimate: 3_000_000,
            playback_rate: 1.0,
            live_head_ms: None,
            max_seekable_ms: None,
            min_seekable_ms: None,
            playback_cookie: None,
            ad_report: None,
            ad_cue: None,
            last_request_pt: std::cell::Cell::new(None),
            next_policy: None,
            start_policy: None,
            resp_state: ResponseState::default(),
            bytes_by_itag: BTreeMap::new(),
            sabr_contexts: BTreeMap::new(),
            active_sabr_types: BTreeSet::new(),
            emit_seq: BTreeMap::new(),
            is_live: true,
            collect_segments: false,
            keep_ranges_on_seek: false,
            live_segment_ms: 0,
            live_chunk_readahead: DEFAULT_LIVE_CHUNK_READAHEAD,
            vod_ranges: None,
            manual_pick: None,
            last_manual_height: None,
        }
    }

    /// Sets the live-edge chunk count from the player response's `liveChunkReadahead`
    /// (SABR-022). Clamped to [1, 8].
    pub fn set_live_chunk_readahead(&mut self, chunks: u32) {
        self.live_chunk_readahead = chunks.clamp(1, 8);
    }

    /// Reports ad cues in field 24, as the main web player does (REQ-025). `field5` is the
    /// ladder's lowest video rung.
    pub fn set_ad_report(&mut self, field5: Option<i32>) {
        self.ad_report = Some(field5);
    }

    /// Advertise a client viewport to the server's ABR (drives served quality).
    /// Set it to the dimensions of the video format you want.
    pub fn set_viewport(&mut self, width: i32, height: i32) {
        self.viewport = Some((width, height));
    }

    /// Sets `field72.max_resolution` independently of the viewport (REQ-012).
    pub fn set_res_ceiling(&mut self, height: i32) {
        self.res_ceiling = Some(height);
    }

    /// Marks this engine as an inline player (hover preview), which sends `visibility` 5
    /// instead of the main player's 0.
    pub fn set_inline(&mut self, inline: bool) {
        self.inline = inline;
    }

    /// Overrides the advertised `bandwidth_estimate` (bytes/s). The VOD path sets a seeded
    /// or measured link rate so the server paces a sane first burst.
    pub fn set_bandwidth_estimate(&mut self, bytes_per_sec: i64) {
        self.bandwidth_estimate = bytes_per_sec;
    }

    /// Sets the playback rate. Rates other than 1.0 are advertised in
    /// `ClientAbrState.playback_rate` (35); 1.0 omits the field (REQ-019).
    pub fn set_playback_rate(&mut self, rate: f32) {
        self.playback_rate = rate;
    }

    /// Swaps the ustreamer config after a `RELOAD_PLAYER_RESPONSE`. The fresh `player()` call
    /// returns a config the server now requires; sending the stale one keeps earning reloads
    /// and no media (SABR-037). Progress, dedup and buffers are untouched.
    pub fn set_ustreamer_config(&mut self, config: Vec<u8>) {
        self.ustreamer_config = config.into();
    }

    /// Switch the engine between live (default) and VOD playhead behavior. VOD plays
    /// sequentially from 0 and advances from the buffered tail; live rides the edge.
    pub fn set_live(&mut self, live: bool) {
        self.is_live = live;
    }

    /// Report each complete segment (MEDIA_HEADER through MEDIA_END) in
    /// [`Ingest::segments`], for a consumer that stores segments by time.
    pub fn set_collect_segments(&mut self, on: bool) {
        self.collect_segments = on;
    }

    /// Duration a lone live segment's range reports before a second one arrives: the
    /// format's `targetDurationSec` (SABR-018).
    pub fn set_live_segment_ms(&mut self, ms: i64) {
        self.live_segment_ms = ms;
    }

    /// After a raw live stream has joined, a `SABR_SEEK` keeps the reported ranges, as a
    /// `SourceBuffer` keeps what it holds across a seek. The join's own seek clears them.
    pub fn set_keep_ranges_on_seek(&mut self, on: bool) {
        self.keep_ranges_on_seek = on;
    }

    /// Whether this engine is in live mode.
    pub fn is_live(&self) -> bool {
        self.is_live
    }

    /// Supplies the cache-derived per-track `buffered_ranges` for the next VOD
    /// random-access requests, switching the engine into VOD cache mode (see the
    /// `vod_ranges` field docs). The caller rebuilds and re-sets the ranges before
    /// every fetch from its segment cache, and they list only the segments it holds
    /// right now. Under-reporting is safe (the server re-sends). Over-reporting makes
    /// the server skip segments the caller still needs, which stalls playback. An
    /// empty vec means cache mode with nothing held (a cold region): the server serves
    /// a full window from `player_time`, as for the web client's first request.
    pub fn set_vod_buffered_ranges(&mut self, ranges: Vec<VodBufferedRange>) {
        self.vod_ranges = Some(ranges);
    }

    /// Clear accumulated per-track progress + lead-in (a reposition). Shared by the
    /// server-driven `SABR_SEEK` handler and the user-driven [`SabrEngine::seek`].
    fn clear_progress(&mut self) {
        self.progress.clear();
        // The byterate divides these bytes by `progress` durations: both cover one window.
        self.bytes_by_itag.clear();
        self.lead.clear();
    }

    /// Reposition: drop accumulated progress, lead-in, and dedup state so the next
    /// `request_bytes(T)` fetches fresh from T (empty `buffered_ranges`) and the
    /// post-seek bytes lead with each track's container init. The caller supplies the
    /// new `player_time` via `request_bytes`; the engine stores no target.
    pub fn seek(&mut self) {
        self.clear_progress();
        // A remembered SABR_SEEK belongs to the join it arrived on. After a reposition it is
        // stale by the session's age, so the next request probes the edge and lets the
        // server place the join, as a cold start does.
        self.last_sabr_seek_ms = None;
        self.emit_seq.clear();
    }

    /// Video-only twin of [`SabrEngine::clear_progress`]: drops progress and lead-in state
    /// for video itags. Our manual pick empties the video `buffered_ranges`; the reference
    /// empties both tracks (REQ-018).
    fn clear_video_progress(&mut self) {
        let video_itags: Vec<i32> =
            self.kind.iter().filter(|(_, is_video)| **is_video).map(|(itag, _)| *itag).collect();
        for itag in video_itags {
            self.progress.remove(&itag);
            self.lead.remove(&itag);
        }
    }

    /// Forces a video rung, as a quality-menu pick does (REQ-018). Collapses
    /// `preferred_video_format_ids` to `itag`, arms the manual-selection fields (13/14/16/21)
    /// on every request from here on, and clears only the video track's progress and lead-in
    /// state. The next request reports empty video `buffered_ranges`; audio ranges stay,
    /// where the reference empties them too.
    /// `player_time` is unchanged: no seek and no session teardown.
    ///
    /// `emit_seq` stays: the new itag has its own sequence space, and clearing the dedup
    /// mark could re-emit a delivered audio segment.
    pub fn set_video_quality(&mut self, itag: i32, height: i32) {
        self.pin_video_quality(itag, height);
        self.clear_video_progress();
    }

    /// Arm the manual-pick wire fields for `itag` and leave the video pipeline as it is.
    pub fn pin_video_quality(&mut self, itag: i32, height: i32) {
        let direction = if height >= self.last_manual_height.unwrap_or(height) { 1 } else { -1 };
        self.last_manual_height = Some(height);
        self.manual_pick =
            Some(ManualPick { itag, height, direction, picked_at: std::time::Instant::now() });
    }

    /// Clears a manual pick: the full ladder rides in `preferred_video_format_ids` again and
    /// fields 13/14/16/21 go absent (REQ-017). The server retakes the rung from
    /// `bandwidth_estimate`.
    pub fn unpin_video_quality(&mut self) {
        self.manual_pick = None;
    }

    /// Whether a manual pick is currently armed (i.e. the ladder is collapsed to one itag).
    pub fn is_video_pinned(&self) -> bool {
        self.manual_pick.is_some()
    }

    /// Identifies the client to the SABR edge. Use the client that minted the streaming URL.
    pub fn set_client_info(&mut self, info: SabrClientInfo) {
        self.client_info = Some(info);
    }

    /// Playable extent so far: the minimum buffered span across received tracks.
    pub fn buffered_ms(&self) -> i64 {
        self.progress.values().map(Progress::duration_ms).min().unwrap_or(0)
    }

    /// Buffered span (ms) per kind: `(audio, video)`, 0 for a kind with no data.
    ///
    /// Takes the max within a kind, as [`Self::kind_end_ms`] does: a kind's itags are
    /// sequential spans of one timeline. A min lets an abandoned rung, whose lone segment
    /// spans 0 ms, pin the figure to zero for the rest of the session.
    pub fn buffered_ms_by_kind(&self) -> (i64, i64) {
        let span = |is_video: bool| -> i64 {
            self.progress
                .iter()
                .filter(|(itag, _)| self.kind.get(itag).copied().unwrap_or(false) == is_video)
                .map(|(_, p)| p.duration_ms())
                .max()
                .unwrap_or(0)
        };
        (span(false), span(true))
    }

    /// The stream timestamp we anchored on (first segment's `start_ms`), once
    /// known. The caller advances the playhead from here at wall-clock rate.
    pub fn anchor_ms(&self) -> Option<i64> {
        self.progress.values().map(|p| p.first_start_ms).min()
    }

    /// End (stream-time ms) of the contiguous buffer across received tracks: the minimum
    /// over kinds of each kind's range end. `None` before any media. The playhead must not
    /// leap past it, or the server skips the segments in between and leaves a gap.
    pub fn buffered_end_ms(&self) -> Option<i64> {
        // Min across kinds, since the least-buffered kind limits playback. Max within a
        // kind (see `kind_end_ms`).
        [false, true].iter().filter_map(|k| self.kind_end_ms(*k)).min()
    }

    /// End (stream-time ms) of the delivered timeline for one kind.
    ///
    /// Ranges of different video itags are sequential spans of one timeline: the reference
    /// client reports both rungs of a server-driven switch at once, the incoming range
    /// starting where the outgoing one ends (REQ-008, SABR-031). A kind's frontier is
    /// therefore the max end across its itags. A min pins the reading at a retired rung's
    /// frozen end, and the readahead gate never stops fetching.
    fn kind_end_ms(&self, is_video: bool) -> Option<i64> {
        self.progress
            .iter()
            .filter(|(itag, _)| self.kind.get(itag).copied().unwrap_or(false) == is_video)
            .map(|(_, p)| self.range_end_ms(p))
            .max()
    }

    /// Joins the live DVR at `ms` (stream-absolute) instead of the edge sentinel, while no
    /// media is buffered. Once media lands, the frontier walk takes over.
    pub fn set_live_join_ms(&mut self, ms: Option<i64>) {
        self.live_join_ms = ms;
    }

    /// Latest live-edge stream time from `LIVE_METADATA`, if seen.
    pub fn live_head_ms(&self) -> Option<i64> {
        self.live_head_ms
    }

    /// Swaps in a freshly minted PO token, sent in `StreamerContext.po_token` from the next
    /// request on (POT-001).
    pub fn set_po_token(&mut self, po_token: Vec<u8>) {
        self.po_token = po_token.into();
    }

    /// Latest server-dictated request pacing/buffering hints, if any. Use these to
    /// pace the request loop (honor `backoff_time_ms`, keep each track buffered to
    /// its `target_*_readahead_ms`) instead of a fixed wall-clock cadence.
    pub fn request_policy(&self) -> RequestPolicy {
        match &self.next_policy {
            Some(p) => RequestPolicy {
                target_audio_readahead_ms: p.target_audio_readahead_ms,
                target_video_readahead_ms: p.target_video_readahead_ms,
                backoff_time_ms: p.backoff_time_ms,
                max_time_since_last_request_ms: p.max_time_since_last_request_ms,
            },
            None => RequestPolicy::default(),
        }
    }

    /// Ms that must be buffered before resuming after a rebuffer, from the resume brackets
    /// of `PLAYBACK_START_POLICY` (UMP-015, SABR-027). Among brackets whose
    /// `min_bandwidth_bytes_per_sec` is at most `bandwidth_estimate`, the smallest
    /// `min_readahead_ms` wins. `None` when the server sent no resume policy.
    ///
    /// [`SabrEngine::start_min_readahead_ms`] selects from the start brackets the same way
    /// and floors the result at 250 ms (SABR-026).
    pub fn resume_min_readahead_ms(&self) -> Option<i64> {
        let p = self.start_policy.as_ref()?;
        p.resume_min_readahead_policy
            .iter()
            .filter(|b| i64::from(b.min_bandwidth_bytes_per_sec.unwrap_or(0)) <= self.bandwidth_estimate)
            .filter_map(|b| b.min_readahead_ms)
            .min()
            .map(i64::from)
    }

    pub fn start_min_readahead_ms(&self) -> i64 {
        const FLOOR_MS: i64 = 250;
        let min = self.start_policy.as_ref().and_then(|p| {
            p.start_min_readahead_policy
                .iter()
                .filter(|b| {
                    i64::from(b.min_bandwidth_bytes_per_sec.unwrap_or(0)) <= self.bandwidth_estimate
                })
                .filter_map(|b| b.min_readahead_ms)
                .min()
        });
        FLOOR_MS.max(min.map(i64::from).unwrap_or(0))
    }

    /// The server's furthest playable stream time, if it has sent one. `next_player_time`
    /// clamps the request position to it (SABR-017). The frontier margin is a different
    /// quantity.
    pub fn max_seekable_ms(&self) -> Option<i64> {
        self.max_seekable_ms
    }

    /// The server's earliest playable stream time (base.js `P2()`), if sent.
    pub fn min_seekable_ms(&self) -> Option<i64> {
        self.min_seekable_ms
    }

    /// Newest segment `start_ms` held per kind: `(audio, video)`. Subtracted from the live
    /// head it gives how far behind the edge the newest content begins.
    pub fn newest_start_ms_by_kind(&self) -> (Option<i64>, Option<i64>) {
        let newest = |is_video: bool| -> Option<i64> {
            self.progress
                .iter()
                .filter(|(itag, _)| self.kind.get(itag).copied().unwrap_or(false) == is_video)
                .map(|(_, p)| p.last_start_ms)
                .max()
        };
        (newest(false), newest(true))
    }

    /// Whether an itag is a video format, per the advertised formats.
    pub fn is_video(&self, itag: i32) -> bool {
        self.kind.get(&itag).copied().unwrap_or(false)
    }

    /// A track's reported range end. It can sit past the last head seen, as the reference
    /// client's does (SABR-030).
    fn range_end_ms(&self, p: &Progress) -> i64 {
        p.first_start_ms + p.duration_ms()
    }
}
