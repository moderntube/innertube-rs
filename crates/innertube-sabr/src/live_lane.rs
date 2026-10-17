//! The live SABR lane: one [`SabrEngine`] in raw-segment mode and the per-round
//! merge that hands its complete segments and server directives to the run loop.

use std::collections::BTreeMap;

use crate::engine::{Discontinuity, Ingest, SabrEngine, SelectedFormat};

/// The merged output for one round.
#[derive(Debug, Default)]
pub struct MergeOut {
    /// `SABR_REDIRECT`: switch the POST URL when present.
    pub redirect: Option<String>,
    /// `STREAM_PROTECTION_STATUS` this round.
    pub protection_status: Option<i32>,
    /// `RELOAD_PLAYER_RESPONSE` token, if any.
    pub reload_token: Option<String>,
    /// The video itag delivered this round, if any.
    pub served_video_itag: Option<i32>,
    /// `SABR_ERROR` (part 44) this round.
    pub sabr_error: Option<crate::engine::SabrErrorInfo>,
    /// The complete segments of this round, in arrival order.
    pub segments: Vec<crate::MediaSegment>,
    /// `SABR_SEEK` (part 45) target in ms: the server repositioned the session.
    pub sabr_seek_ms: Option<i64>,
    /// Sequence-continuity anomalies (diagnostic).
    pub discontinuities: Vec<Discontinuity>,
}

/// The live lane's engine plus the output dedup it ingests through.
pub struct LiveLane {
    pub engine: SabrEngine,
    /// Output dedup high-water mark (itag -> highest emitted seq), kept across rounds.
    emit_seq: BTreeMap<i32, i64>,
    viewport: (i32, i32),
}

impl LiveLane {
    pub fn new(
        ustreamer_config: Vec<u8>,
        po_token: Vec<u8>,
        selected: Vec<SelectedFormat>,
        all_formats: &[SelectedFormat],
        viewport: (i32, i32),
    ) -> Self {
        let mut engine = SabrEngine::new(ustreamer_config, po_token, selected, all_formats);
        engine.set_viewport(viewport.0, viewport.1);
        // The in-page player, as opposed to an inline preview.
        engine.set_inline(false);
        engine.set_ad_report(None);
        Self { engine, emit_seq: BTreeMap::new(), viewport }
    }

    /// Output the server's live segments whole and unmodified. The consumer orders them
    /// by their own timestamps, as a `SourceBuffer` does.
    pub fn set_raw(&mut self, on: bool) {
        self.engine.set_collect_segments(on);
    }

    /// Field 24's `unknown_field_5`: the ladder's lowest video rung (278 in the
    /// reference capture).
    pub fn set_lane_ref_itag(&mut self, itag: i32) {
        self.engine.set_ad_report(Some(itag));
    }

    /// Join the live DVR at `ms` instead of the edge.
    pub fn set_live_join_ms(&mut self, ms: Option<i64>) {
        self.engine.set_live_join_ms(ms);
    }

    /// Adopt the rotated `ustreamer_config` after a `RELOAD_PLAYER_RESPONSE`
    /// (see [`SabrEngine::set_ustreamer_config`]).
    pub fn set_ustreamer_config(&mut self, config: Vec<u8>) {
        self.engine.set_ustreamer_config(config);
    }

    /// Set the live-edge chunk count (see [`SabrEngine::set_live_chunk_readahead`]).
    pub fn set_live_chunk_readahead(&mut self, chunks: u32) {
        self.engine.set_live_chunk_readahead(chunks);
    }

    /// A manual quality pick modelled on the reference's (REQ-018): empty video ranges (the
    /// reference also empties audio), the new rung from the playhead.
    pub fn set_video_quality(&mut self, itag: i32, height: i32) {
        self.engine.set_video_quality(itag, height);
    }

    /// Clear the manual pick. Auto resumes: full ladder plus measured bandwidth, the
    /// server picks.
    pub fn unpin_video_quality(&mut self) {
        self.engine.unpin_video_quality();
    }

    /// Whether a manual pick is armed.
    pub fn is_video_pinned(&self) -> bool {
        self.engine.is_video_pinned()
    }

    /// Re-anchor a live session to the live edge: clears the engine's progress and the
    /// output dedup, so the next request re-joins via the live-edge sentinel with empty
    /// `buffered_ranges`.
    pub fn reseek_live(&mut self) {
        self.engine.seek();
        self.engine.set_live_join_ms(None);
        self.emit_seq.clear();
    }

    /// Pin `field72.max_resolution` to the session ceiling without inflating the
    /// viewport to match.
    pub fn set_res_ceiling(&mut self, height: i32) {
        self.engine.set_res_ceiling(height);
    }

    pub fn viewport(&self) -> (i32, i32) {
        self.viewport
    }

    /// Ingest one response and produce the round's output.
    pub fn round_merge(&mut self, ump: impl crate::AsUmp) -> MergeOut {
        let e = self.engine.ingest_with_shared_dedup(ump, &mut self.emit_seq);
        Self::merge(&self.engine, e)
    }

    fn merge(engine: &SabrEngine, mut e: Ingest) -> MergeOut {
        let mut out = MergeOut { segments: std::mem::take(&mut e.segments), ..Default::default() };
        for itag in e.media.keys() {
            if engine.is_video(*itag) {
                out.served_video_itag.get_or_insert(*itag);
            }
        }
        out.redirect = e.redirect;
        out.protection_status = e.protection_status;
        out.reload_token = e.reload_token;
        out.sabr_error = e.sabr_error;
        out.sabr_seek_ms = e.sabr_seek_ms;
        out.discontinuities = e.discontinuities;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::video_streaming::{LiveMetadata, MediaHeader};
    use prost::Message;

    fn formats() -> Vec<SelectedFormat> {
        vec![
            SelectedFormat { itag: 140, last_modified: 1, is_video: false, xtags: None },
            SelectedFormat { itag: 399, last_modified: 2, is_video: true, xtags: None },
            SelectedFormat { itag: 396, last_modified: 3, is_video: true, xtags: None },
        ]
    }

    fn part(part_type: u32, payload: &[u8]) -> Vec<u8> {
        let mut u = Vec::new();
        crate::write_part(&mut u, part_type, payload);
        u
    }
    /// A self-init fMP4 audio segment: `ftyp` (16 B), `moov` (8), `moof` (8), `mdat` (12).
    fn init_segment() -> Vec<u8> {
        let mut v = vec![0, 0, 0, 0x10, b'f', b't', b'y', b'p', b'm', b'p', b'4', b'2', 0, 0, 0, 0];
        v.extend_from_slice(&[0, 0, 0, 8, b'm', b'o', b'o', b'v']);
        v.extend_from_slice(&[0, 0, 0, 8, b'm', b'o', b'o', b'f']);
        v.extend_from_slice(&[0, 0, 0, 0x0C, b'm', b'd', b'a', b't', 0xAA, 0xBB, 0xCC, 0xDD]);
        v
    }
    /// A WebM init (EBML header, `webm` DocType, unknown-size Segment) plus a Cluster, the
    /// shape of a live VP9 video segment.
    fn webm_init_segment() -> Vec<u8> {
        let mut v = vec![0x1A, 0x45, 0xDF, 0xA3, 0x84, 0x42, 0x82, 0x84];
        v.extend_from_slice(b"webm");
        v.extend_from_slice(&[0x18, 0x53, 0x80, 0x67, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        // A Cluster whose first block is a VP9 keyframe: ID, size, Timecode(0), SimpleBlock.
        v.extend_from_slice(&[
            0x1F, 0x43, 0xB6, 0x75, // Cluster ID
            0x8D, // Cluster size = 13
            0xE7, 0x81, 0x00, // Timecode = 0
            0xA3, 0x88, 0x81, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, // SimpleBlock, flags 0x80 = keyframe
        ]);
        v
    }
    fn ump_segment(header_id: u32, itag: i32, seq: i32, start_ms: i64, body: &[u8]) -> Vec<u8> {
        let mh = MediaHeader {
            header_id: Some(header_id),
            itag: Some(itag),
            sequence_number: Some(seq),
            start_ms: Some(start_ms),
            ..Default::default()
        };
        let mut out = part(20, &mh.encode_to_vec());
        let mut media = Vec::new();
        crate::write_varint(&mut media, header_id);
        media.extend_from_slice(body);
        out.extend(part(21, &media));
        out
    }
    fn live_metadata_ump(head_time_ms: i64) -> Vec<u8> {
        let lm = LiveMetadata { head_time_ms: Some(head_time_ms), ..Default::default() };
        part(31, &lm.encode_to_vec())
    }

    fn end_part(header_id: u32) -> Vec<u8> {
        let mut p = Vec::new();
        crate::write_varint(&mut p, header_id);
        part(22, &p)
    }

    #[test]
    fn a_raw_live_round_yields_each_segment_whole_with_its_init() {
        let sel = formats();
        let (a, v) = (init_segment(), webm_init_segment());
        let round = |seq: i32, start_ms: i64| {
            let mut body = live_metadata_ump(100_000);
            body.extend(ump_segment(1, 399, seq, start_ms, &v));
            body.extend(end_part(1));
            body.extend(ump_segment(2, 140, seq, start_ms, &a));
            body.extend(end_part(2));
            body
        };
        let mut d = LiveLane::new(vec![0], vec![0], sel.clone(), &sel, (1920, 1080));
        d.set_raw(true);
        let first = d.round_merge(&round(10, 90_000));
        let second = d.round_merge(&round(11, 95_000));
        let got: Vec<(i32, Option<i64>, Vec<u8>)> = first.segments.iter().chain(&second.segments).map(|s| (s.itag, s.seq, s.bytes.clone())).collect();
        assert_eq!(
            got,
            vec![(399, Some(10), v.clone()), (140, Some(10), a.clone()), (399, Some(11), v), (140, Some(11), a)]
        );
        assert_eq!(second.served_video_itag, Some(399));
    }

}
