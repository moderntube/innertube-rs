use super::*;

/// Correlation state for one SABR response, kept across the calls that ingest it.
///
/// A chunk boundary can fall inside a segment: the `MEDIA_HEADER` arrives in one chunk and
/// the `MEDIA` payloads it owns in the next (UMP-006). Without this state those payloads
/// have no header and are dropped.
#[derive(Default)]
pub(crate) struct ResponseState {
    /// header_id -> (itag, sequence_number, start_ms). The VOD init header carries no
    /// sequence number or start_ms (UMP-009), so `start_ms` stays optional and keeps the
    /// init out of `media_start_ms`.
    headers: BTreeMap<u32, (i32, i64, Option<i64>)>,
    /// Last header_id whose media was appended per itag. A segment's media can span several
    /// MEDIA parts, so only a change of header_id marks a new segment.
    last_hid: BTreeMap<i32, u32>,
    /// Per-itag emit/drop decision for the current segment (header_id), so all of a
    /// segment's part-21 chunks follow the one dedup decision.
    seg_emit: BTreeMap<i32, bool>,
    /// header_id -> the segment being collected, until its MEDIA_END.
    seg_open: BTreeMap<u32, MediaSegment>,
    /// Trailing bytes of an incomplete UMP part, carried to the next chunk.
    carry: Vec<u8>,
}

/// `SABR_SEEK` target in ms (`seek_media_time` ticks of `1/seek_media_timescale` s);
/// `None` when a field is missing or the timescale is zero.
fn seek_target_ms(s: &SabrSeek) -> Option<i64> {
    match (s.seek_media_time, s.seek_media_timescale) {
        // i128 intermediate: `ticks * 1000` overflows i64 on a hostile seek_media_time.
        (Some(ticks), Some(scale)) if scale != 0 => Some((ticks as i128 * 1000 / scale as i128) as i64),
        _ => None,
    }
}

impl SabrEngine {
    /// Begins a new response for [`SabrEngine::ingest_streaming`]. Clears the per-response
    /// correlation state and the adopted `SABR_SEEK` target, which `ingest` would clear on
    /// every call and so wipe a seek that arrived in an earlier chunk of the same response.
    pub fn begin_response(&mut self) {
        self.resp_state = ResponseState::default();
        self.last_sabr_seek_ms = None;
    }

    /// Ingest one CHUNK of the response opened by [`SabrEngine::begin_response`],
    /// carrying correlation state across chunks so a segment split by a chunk boundary
    /// still resolves. Returns only what this chunk produced.
    pub fn ingest_streaming(&mut self, ump: impl AsUmp) -> Ingest {
        let mut emit = std::mem::take(&mut self.emit_seq);
        let out = self.ingest_streaming_with_shared_dedup(ump, &mut emit);
        self.emit_seq = emit;
        out
    }

    /// [`SabrEngine::ingest_streaming`] against a caller-owned `emit_seq`, for the
    /// `LiveLane` case where two engines share one dedup high-water map.
    pub(crate) fn ingest_streaming_with_shared_dedup(
        &mut self,
        ump: impl AsUmp,
        emit_seq: &mut BTreeMap<i32, i64>,
    ) -> Ingest {
        let incoming = ump.into_ump();
        // Splice any carried partial part onto the front, then ingest only whole parts
        // and carry whatever trails. Chunk boundaries land mid-part routinely.
        let buf = if self.resp_state.carry.is_empty() {
            incoming
        } else {
            let mut v = std::mem::take(&mut self.resp_state.carry);
            v.extend_from_slice(&incoming);
            crate::Bytes::from(v)
        };
        let n = crate::complete_parts_len(&buf);
        let tail = buf[n..].to_vec();
        // `carry` was drained above, so the state taken here has none; the new tail is
        // put back after ingesting, not before, or the take would discard it.
        let mut st = std::mem::take(&mut self.resp_state);
        let out = self.ingest_with_state(buf.slice(0..n), emit_seq, &mut st);
        st.carry = tail;
        self.resp_state = st;
        out
    }

    pub fn ingest(&mut self, ump: impl AsUmp) -> Ingest {
        // The adopted SABR_SEEK target is single-use: it steers the next request only
        // (SABR-009, SABR-011). A sticky target would re-join at a stale position after
        // stall recovery, and re-ask forever when the target yields no media. Falling back
        // to the sentinel earns a fresh seek.
        self.last_sabr_seek_ms = None;
        // Borrow the field out so the shared-dedup body can take `&mut` to it while
        // also holding `&mut self`; restore it after.
        let mut emit = std::mem::take(&mut self.emit_seq);
        let out = self.ingest_with_shared_dedup(ump, &mut emit);
        self.emit_seq = emit;
        out
    }

    /// Like [`SabrEngine::ingest`] but dedups emitted segments against a caller-owned
    /// `emit_seq` high-water mark. Two engines sharing one map dedup the segments they both
    /// deliver, keeping the merged output monotonic.
    ///
    /// `pub(crate)`: only `LiveLane` shares a map, and a wrong map would resplice media.
    pub(crate) fn ingest_with_shared_dedup(&mut self, ump: impl AsUmp, emit_seq: &mut BTreeMap<i32, i64>) -> Ingest {
        // One call is one response: a fresh state.
        let mut st = ResponseState::default();
        self.ingest_with_state(ump, emit_seq, &mut st)
    }

    /// The ingest body, parameterized over its per-response correlation state so one
    /// response can be ingested in several calls. `ingest_with_shared_dedup` passes a
    /// throwaway state; `ingest_streaming` passes the engine's own, cleared once per response,
    /// so media reaches the player before its `MEDIA_END`.
    pub(crate) fn ingest_with_state(
        &mut self,
        ump: impl AsUmp,
        emit_seq: &mut BTreeMap<i32, i64>,
        st: &mut ResponseState,
    ) -> Ingest {
        let parts = parse(ump);
        let ResponseState { headers, last_hid, seg_emit, seg_open, carry: _ } = st;
        // Raw media bytes received THIS CALL, per itag, before lead-in trimming.
        // Per-call, not per-response: it is this call's output.
        let mut raw: BTreeMap<i32, Vec<u8>> = BTreeMap::new();
        let mut out = Ingest::default();
        let mut saw_ad_cue = false;

        for part in &parts {
            match part.part_type {
                20 => {
                    if let Ok(h) = MediaHeader::decode(&part.payload[..]) {
                        if let (true, Some(itag)) = (self.collect_segments, h.itag) {
                            seg_open.insert(
                                h.header_id.unwrap_or(0),
                                MediaSegment {
                                    itag,
                                    seq: h.sequence_number.map(i64::from),
                                    start_ms: h.start_ms,
                                    duration_ms: h.duration_ms,
                                    init: h.is_init_seg.unwrap_or(false),
                                    bytes: Vec::new(),
                                },
                            );
                        }
                        if let Some(itag) = h.itag {
                            headers.insert(
                                h.header_id.unwrap_or(0),
                                (itag, h.sequence_number.unwrap_or(0) as i64, h.start_ms),
                            );
                        }
                    }
                }
                21 => {
                    if let Some((header_id, o)) = read_varint(&part.payload, 0) {
                        if let Some(seg) = seg_open.get_mut(&header_id) {
                            seg.bytes.extend_from_slice(&part.payload[o..]);
                        }
                        if let Some((itag, seq, start_ms_opt)) = headers.get(&header_id).copied() {
                            let start_ms = start_ms_opt.unwrap_or(0);
                            // New segment boundary: classify against the highest seq already
                            // emitted, then emit or drop it (dedup) and flag any anomaly. Later
                            // chunks of the segment reuse the decision.
                            // VOD cache mode emits everything: segments arrive in random-access
                            // order, and the cache layer dedups by segment index.
                            if last_hid.insert(itag, header_id) != Some(header_id) {
                                let emit = if self.vod_ranges.is_some() {
                                    true
                                } else {
                                    match emit_seq.get(&itag).copied() {
                                        None => true,
                                        Some(prev) if seq <= prev => {
                                            // Already emitted (resend, duplicate or overlap):
                                            // drop it.
                                            out.discontinuities.push(Discontinuity {
                                                itag,
                                                kind: DiscontinuityKind::Backward,
                                                prev_seq: prev,
                                                seq,
                                                start_ms,
                                            });
                                            false
                                        }
                                        Some(prev) => {
                                            if seq > prev + 1 {
                                                // Segments are missing and cannot be filled:
                                                // emit what arrived and flag the hole.
                                                out.discontinuities.push(Discontinuity {
                                                    itag,
                                                    kind: DiscontinuityKind::Gap,
                                                    prev_seq: prev,
                                                    seq,
                                                    start_ms,
                                                });
                                            }
                                            true
                                        }
                                    }
                                };
                                if emit {
                                    emit_seq.insert(itag, seq);
                                    // Time anchor for VOD caching: only headers that
                                    // carry start_ms count (not the init).
                                    if let Some(s) = start_ms_opt {
                                        out.media_start_ms
                                            .entry(itag)
                                            .and_modify(|lo| *lo = (*lo).min(s))
                                            .or_insert(s);
                                    }
                                }
                                seg_emit.insert(itag, emit);
                            }
                            if seg_emit.get(&itag).copied().unwrap_or(true) {
                                let n = part.payload.len() - o;
                                *self.bytes_by_itag.entry(itag).or_insert(0) += n as u64;
                                raw.entry(itag).or_default().extend_from_slice(&part.payload[o..]);
                            }
                        }
                    }
                }
                22 => {
                    // MEDIA_END: the segment under this header-id is complete.
                    if let Some((header_id, _)) = read_varint(&part.payload, 0) {
                        if let Some(seg) = seg_open.remove(&header_id) {
                            out.segments.push(seg);
                        }
                    }
                }
                43 => {
                    if let Some(Directive::SabrRedirect { url }) = Directive::from_part(part) {
                        out.redirect = Some(url);
                    }
                }
                31 => {
                    // LIVE_METADATA: the live head time and head sequence number (SABR-014).
                    if let Ok(m) = LiveMetadata::decode(&part.payload[..]) {
                        if let Some(h) = m.head_time_ms {
                            self.live_head_ms = Some(h);
                        }
                        // The server's playback boundaries. The request ceiling is
                        // max_seekable (SABR-017).
                        let scale = |ticks: Option<i64>, ts: Option<i32>| -> Option<i64> {
                            let t = ticks?;
                            let s = ts.unwrap_or(1000).max(1) as i64;
                            Some(t.saturating_mul(1000) / s)
                        };
                        self.max_seekable_ms =
                            scale(m.max_seekable_time_ticks, m.max_seekable_timescale)
                                .or(self.max_seekable_ms);
                        self.min_seekable_ms =
                            scale(m.min_seekable_time_ticks, m.min_seekable_timescale)
                                .or(self.min_seekable_ms);
                        if m.head_sequence_number.is_some() {
                            out.head_sequence_number = m.head_sequence_number;
                        }
                        if m.wall_time_ms.is_some() {
                            out.wall_time_ms = m.wall_time_ms;
                        }
                    }
                }
                35 => {
                    // NEXT_REQUEST_POLICY: pacing/buffering hints + the playback
                    // cookie we must echo back to keep our session alive.
                    if let Ok(p) = NextRequestPolicy::decode(&part.payload[..]) {
                        if let Some(cookie) = &p.playback_cookie {
                            self.playback_cookie = Some(cookie.encode_to_vec().into());
                        }
                        self.next_policy = Some(p);
                    }
                }
                47 => {
                    // PLAYBACK_START_POLICY: start and resume readahead brackets (UMP-015), read
                    // by `start_min_readahead_ms` and `resume_min_readahead_ms`.
                    if let Ok(p) = PlaybackStartPolicy::decode(&part.payload[..]) {
                        self.start_policy = Some(p);
                    }
                }
                58 => {
                    // STREAM_PROTECTION_STATUS (GATE-001). Status 3 withholds all media
                    // until the session attests again (GATE-003); the caller re-mints.
                    if let Ok(s) = StreamProtectionStatus::decode(&part.payload[..]) {
                        out.protection_status = s.status;
                    }
                }
                46 => {
                    // RELOAD_PLAYER_RESPONSE: keep the token for a fresh player() call
                    // (SABR-037).
                    if let Ok(ctx) = ReloadPlaybackContext::decode(&part.payload[..]) {
                        if let Some(token) = ctx.reload_playback_params.and_then(|p| p.token) {
                            out.reload_token = Some(token);
                        }
                    }
                }
                44 => {
                    // SABR_ERROR: surfaced for the caller to retry or abort (SABR-038).
                    if let Ok(e) = SabrError::decode(&part.payload[..]) {
                        out.sabr_error = Some(SabrErrorInfo { r#type: e.r#type, code: e.code });
                    }
                }
                45 => {
                    // SABR_SEEK: convert the target to ms, surface it, and reset dedup and
                    // lead-in so post-seek segments, which restart at an earlier sequence
                    // and lead with an init (SABR-012), are not dropped as backward resends.
                    if let Ok(s) = SabrSeek::decode(&part.payload[..]) {
                        if let Some(ms) = seek_target_ms(&s) {
                            out.sabr_seek_ms = Some(ms);
                            // Adopt it for the next request instead of re-probing the edge.
                            self.last_sabr_seek_ms = Some(ms);
                            // A segment consumer keeps what it holds across a seek, as a
                            // SourceBuffer does, so the ranges it reports stay.
                            if !self.keep_ranges_on_seek {
                                self.clear_progress();
                                emit_seq.clear();
                                last_hid.clear();
                                seg_emit.clear();
                            }
                        }
                    }
                }
                57 => {
                    // SABR_CONTEXT_UPDATE: store reactive ABR/attestation context the
                    // server wants echoed back. Honor write_policy (KEEP_EXISTING must
                    // not clobber) and send_by_default (start echoing immediately).
                    if let Ok(u) = SabrContextUpdate::decode(&part.payload[..]) {
                        if let (Some(ty), Some(val)) = (u.r#type, u.value.clone()) {
                            let keep = u.write_policy == Some(SabrContextWritePolicy::KeepExisting as i32);
                            if !(keep && self.sabr_contexts.contains_key(&ty)) {
                                self.sabr_contexts.insert(ty, val);
                            }
                            if u.send_by_default == Some(true) {
                                self.active_sabr_types.insert(ty);
                            }
                        }
                    }
                }
                59 => {
                    // SABR_CONTEXT_SENDING_POLICY: toggle which stored contexts are
                    // actively sent. start → send full value; stop → keep but list as
                    // unsent; discard → drop entirely.
                    if let Ok(p) = SabrContextSendingPolicy::decode(&part.payload[..]) {
                        for ty in p.start_policy {
                            self.active_sabr_types.insert(ty);
                        }
                        for ty in p.stop_policy {
                            self.active_sabr_types.remove(&ty);
                        }
                        for ty in p.discard_policy {
                            self.sabr_contexts.remove(&ty);
                            self.active_sabr_types.remove(&ty);
                        }
                    }
                }
                69 => {
                    let Ok(list) = CuepointList::decode(&part.payload[..]) else { continue };
                    let Some(c) = list
                        .ssap_infos
                        .iter()
                        .filter_map(|i| i.cuepoint.as_ref())
                        .find(|c| c.r#type == Some(1) && c.total_duration_sec.is_some())
                    else {
                        continue;
                    };
                    saw_ad_cue = true;
                    let total_ms = (c.total_duration_sec.unwrap_or(0.0) * 1000.0).round() as i64;
                    let playhead_ms = c.playhead_time_sec.map(|p| (p * 1000.0).round() as i64);
                    out.ad_break = Some(AdBreak {
                        start_sequence_number: c.start_sequence_number,
                        remaining_ms: (total_ms - playhead_ms.unwrap_or(0)).max(0),
                    });
                    if let Some(id) = c.identifier.clone() {
                        let start = self.last_request_pt.get().zip(playhead_ms).map(|(pt, p)| pt - p);
                        let prev = self.ad_cue.as_ref().filter(|a| a.id == id).and_then(|a| a.start_time_ms);
                        self.ad_cue = Some(AdCue {
                            id,
                            start_time_ms: prev.or(start),
                            duration_ms: total_ms,
                            stopped: c.event == Some(3),
                        });
                    }
                }
                _ => {}
            }
        }
        if !saw_ad_cue && self.ad_cue.as_ref().is_some_and(|c| c.stopped) {
            self.ad_cue = None;
        }

        // Per track, begin the emitted stream at the first container-init boundary
        // (live-edge joins land mid-segment).
        for (itag, bytes) in raw {
            let emit = if self.vod_ranges.is_some() {
                // VOD cache mode: emit verbatim. A continuation response sends no init,
                // so the lead-in gate would withhold it; the cache layer aligns containers.
                bytes
            } else {
                // Hand ownership to the lead-in so its steady path returns the bytes
                // without a copy.
                self.lead.entry(itag).or_default().feed(bytes)
            };
            if !emit.is_empty() {
                out.media.insert(itag, emit);
            }
        }

        for (itag, seq, start_ms_opt) in headers.values() {
            let start_ms = start_ms_opt.unwrap_or(0);
            match self.progress.get_mut(itag) {
                None => {
                    self.progress.insert(*itag, Progress::new(*seq, start_ms, self.live_segment_ms));
                }
                Some(p) if *seq > p.max_seq => {
                    // Learn segment duration only from adjacent segments. A jump (reload,
                    // stall recovery, server skip) gives a bogus gap that would inflate the
                    // edge margin and readahead target and starve the consumer.
                    if *seq == p.max_seq + 1 && start_ms > p.last_start_ms {
                        p.seg_dur_ms = (start_ms - p.last_start_ms).max(1);
                    }
                    p.max_seq = *seq;
                    p.last_start_ms = start_ms;
                }
                // A SABR_SEEK behind a kept range serves the segments before it; the
                // range grows back to them, or restarts there past a hole.
                Some(p) if self.keep_ranges_on_seek && *seq < p.min_seq => {
                    if *seq == p.min_seq - 1 {
                        p.min_seq = *seq;
                        p.first_start_ms = start_ms;
                    } else {
                        *p = Progress::new(*seq, start_ms, p.seg_dur_ms);
                    }
                }
                _ => {}
            }
        }
        // The DVR join override is consume-once: after media lands the frontier walk
        // owns player_time, and a later reposition (stall recovery) must go to the edge
        // sentinel. Rejoining the stale point would rewind the output.
        if !out.media.is_empty() {
            self.live_join_ms = None;
        }
        out
    }
}
