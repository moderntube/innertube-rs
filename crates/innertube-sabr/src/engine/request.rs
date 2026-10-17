use super::*;

/// `SsapPlaybackInfo.state` the main web player sends for a cue it plays no ad for.
const AD_STATE_SERVER_ERROR: i32 = 3;

impl SabrEngine {
    fn format_id(&self, itag: i32) -> FormatId {
        FormatId {
            itag: Some(itag),
            last_modified: self.lmt.get(&itag).copied(),
            xtags: self.xtags.get(&itag).cloned(),
        }
    }

    /// Encodes the next `VideoPlaybackAbrRequest` for the caller's playhead `player_time_ms`.
    pub fn request_bytes(&self, player_time_ms: i64) -> Vec<u8> {
        self.last_request_pt.set(Some(player_time_ms).filter(|&p| p != LIVE_EDGE_SEEK_MS));
        // Format ids differ by stream type. Live ids are itag-only (REQ-005): a live `lmt`
        // rotates, and a stale one earns RELOAD_PLAYER_RESPONSE. VOD ids carry the real
        // `lmt`; without it the server answers with policy and no media (SABR-039). The
        // engine does not send `selected_format_ids` (2).
        let is_live = self.is_live;
        let preferred = |f: &SelectedFormat| FormatId {
            itag: Some(f.itag),
            last_modified: if is_live { None } else { Some(f.last_modified) },
            // xtags select the track on dubbed videos; without them the server answers
            // `sabr.no_audio_selected` (SABR-038).
            xtags: f.xtags.clone(),
        };
        // A manual pick collapses the ladder to its itag (REQ-018). Auto keeps the full
        // ladder so the server drives the rung (REQ-017).
        let preferred_video_format_ids = match &self.manual_pick {
            Some(p) => vec![FormatId {
                itag: Some(p.itag),
                last_modified: if is_live { None } else { self.lmt.get(&p.itag).copied() },
                xtags: self.xtags.get(&p.itag).cloned(),
            }],
            None => self.selected.iter().filter(|f| f.is_video).map(preferred).collect(),
        };
        let preferred_audio_format_ids =
            self.selected.iter().filter(|f| !f.is_video).map(preferred).collect();
        // Acknowledge what we hold so the server sends only the missing delta. VOD cache
        // mode reports the cache-derived ranges instead of `progress`, which counts segments
        // on header arrival and can claim evicted ones; over-reporting makes the server
        // withhold media we still need.
        let buffered_ranges = match &self.vod_ranges {
            Some(ranges) => ranges
                .iter()
                .map(|r| BufferedRange {
                    format_id: self.format_id(r.itag),
                    start_time_ms: r.start_time_ms,
                    duration_ms: r.duration_ms,
                    start_segment_index: r.start_segment_index,
                    end_segment_index: r.end_segment_index,
                    ..Default::default()
                })
                .collect(),
            None => self
                .progress
                .iter()
                .map(|(itag, p)| {
                    // A live range runs from the first held segment to the frontier and grows
                    // by about one segment per round, as the reference's does (REQ-009).
                    let (start_seq, start_ms) = (p.min_seq, p.first_start_ms);
                    BufferedRange {
                        format_id: self.format_id(*itag),
                        start_time_ms: start_ms,
                        // End at the received content (see `range_end_ms`).
                        duration_ms: self.range_end_ms(p) - start_ms,
                        start_segment_index: start_seq as i32,
                        end_segment_index: p.max_seq as i32,
                        ..Default::default()
                    }
                })
                .collect(),
        };

        let body = VideoPlaybackAbrRequest {
            client_abr_state: Some(ClientAbrState {
                player_time_ms: Some(player_time_ms),
                // Quality is server-driven. The viewport and the capability and
                // authorization fields steer it: field72 resolution caps (REQ-012) and
                // playback_authorization (79).
                client_viewport_width: self.viewport.map(|(w, _)| w),
                client_viewport_height: self.viewport.map(|(_, h)| h),
                // sticky_resolution (21) and last_manual_selected_resolution (16) stay unset
                // unless a manual pick is active. The reference client sends 21 = 0 on every
                // auto request (REQ-020), but from our sessions a constant 21 = 0 caps the
                // served rung at 480p (REQ-021). While a pick is active, 13/14/16/21 are sent
                // as the manual-selection signal (REQ-018).
                last_manual_selected_resolution: self.manual_pick.as_ref().map(|p| p.height),
                sticky_resolution: self.manual_pick.as_ref().map(|p| p.height),
                last_manual_direction: self.manual_pick.as_ref().map(|p| p.direction),
                time_since_last_manual_format_selection_ms: self
                    .manual_pick
                    .as_ref()
                    .map(|p| p.picked_at.elapsed().as_millis() as i64),
                // 68 = 0 and 76 = false match the reference baseline. The 29/36/39/57
                // telemetry counters are not sent: their units and reset rules are
                // undecoded, and an invented value is less faithful than none (REQ-011).
                sabr_force_max_network_interruption_duration_ms: self.viewport.map(|_| 0),
                enable_voice_boost: self.viewport.map(|_| false),
                bandwidth_estimate: self.viewport.map(|_| self.bandwidth_estimate),
                // 35: only when the rate is not 1 (REQ-019).
                playback_rate: (self.playback_rate != 1.0).then_some(self.playback_rate),
                // 34: 0 for the main in-page player, 5 for an inline one (REQ-014).
                visibility: self.viewport.map(|_| i32::from(self.inline) * 5),
                drc_enabled: self.viewport.map(|_| true),
                prefer_vp9: self.viewport.map(|_| false),
                av1_quality_threshold: self.viewport.map(|_| 2160),
                field71: self.viewport.map(|_| 1),
                field80: self.viewport.map(|_| 1),
                // Field 85 is 1 on every request in the July and August captures (REQ-022).
                // It is not in the vendored schema.
                field85: self.viewport.map(|_| 1),
                // Resolution caps (REQ-012). max_resolution and max_resolution_alt carry the
                // ceiling; sd_resolution and hd_resolution stay 0, since advertising sd = 480
                // and hd = 720 parks the server on the 480p rung (REQ-013). The ceiling comes
                // from `res_ceiling`, else the viewport height.
                field72: self.viewport.map(|(_, h)| {
                    let cap = self.res_ceiling.unwrap_or(h);
                    MediaCapabilityCaps {
                        field1: Some(0),
                        max_resolution: Some(cap),
                        sd_resolution: Some(0),
                        hd_resolution: Some(0),
                        max_resolution_alt: Some(cap),
                        field6: Some(0),
                    }
                }),
                playback_authorization: self.viewport.map(|_| PlaybackAuthorization {
                    authorized_formats: vec![
                        AuthorizedFormat { track_type: Some(1), is_hdr: Some(false) },
                        AuthorizedFormat { track_type: Some(2), is_hdr: Some(false) },
                        AuthorizedFormat { track_type: Some(2), is_hdr: Some(true) },
                    ],
                    sabr_license_constraint: None,
                }),
                ..Default::default()
            }),
            preferred_video_format_ids,
            preferred_audio_format_ids,
            buffered_ranges,
            video_playback_ustreamer_config: Some(self.ustreamer_config.clone()),
            streamer_context: Some(StreamerContext {
                po_token: Some(self.po_token.clone()),
                // Echo the latest playback cookie so the server keeps our session
                // (committed formats/resolution) instead of resetting after a hiccup.
                playback_cookie: self.playback_cookie.clone(),
                client_info: self.client_info.as_ref().map(|c| ClientInfo {
                    // The reference client sends locale en_US and no os_version (REQ-024),
                    // so an empty os_version is omitted.
                    locale: Some("en_US".to_string()),
                    client_name: Some(c.client_name),
                    client_version: Some(c.client_version.clone()),
                    os_name: Some(c.os_name.clone()),
                    os_version: (!c.os_version.is_empty()).then(|| c.os_version.clone()),
                    ..Default::default()
                }),
                // Echo SABR contexts the server pushed (part 57): active types carry
                // their full value, the rest are acknowledged type-only as unsent.
                sabr_contexts: self
                    .sabr_contexts
                    .iter()
                    .filter(|(ty, _)| self.active_sabr_types.contains(ty))
                    .map(|(ty, val)| SabrContext { r#type: Some(*ty), value: Some(val.clone()) })
                    .collect(),
                unsent_sabr_contexts: self
                    .sabr_contexts
                    .keys()
                    .filter(|ty| !self.active_sabr_types.contains(ty))
                    .copied()
                    .collect(),
                ..Default::default()
            }),
            ssap_playback_infos: self
                .ad_report
                .zip(self.ad_cue.as_ref())
                .map(|(field5, cue)| SsapPlaybackInfo {
                    unknown_field_5: field5,
                    unknown_field_6: Some(String::new()),
                    cuepoint_id: Some(cue.id.clone()),
                    state: Some(AD_STATE_SERVER_ERROR),
                    start_time_ms: cue.start_time_ms,
                    duration_ms: Some(cue.duration_ms),
                    ..Default::default()
                })
                .into_iter()
                .collect(),
            ..Default::default()
        }
        .encode_to_vec();
        body
    }
}
