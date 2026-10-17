//! Bounded audio-only DVR window fetch for the live catch-up summary.
//!
//! Pulls the audio track of a live stream over `[from_ms, to_ms]` (stream-absolute ms;
//! `to_ms = None` is the live head at fetch start) without a playback loop or pacing. The
//! result is the concatenated self-initializing fMP4 audio segments (UMP-010): a `.m4a`
//! byte stream that batch transcription APIs accept.

use std::sync::Arc;

use crate::client::{decode_base64_lenient, Navigation, Session};
use innertube_js::NSigSolver;
use innertube_pot::PoTokenProvider;
use innertube_sabr::{SabrEngine, SelectedFormat};

use crate::bootstrap::{bootstrap_player, StartupTimer, StreamError};

/// A fetched audio window.
pub struct AudioWindow {
    /// Concatenated self-initializing fMP4 audio segments (a playable `.m4a`).
    pub bytes: Vec<u8>,
    /// The resolved window start (stream-absolute ms): the first buffered position.
    pub from_ms: i64,
    /// The resolved window end (stream-absolute ms; the observed live head for
    /// an edge-bounded request).
    pub to_ms: i64,
    /// Seconds of audio content fetched (per the engine's buffered span).
    pub seconds: f64,
}

/// Per-round progress for a UI: `(buffered_end_ms, target_ms, bytes_so_far)`.
/// `Send` so a caller can drive the fetch from a work task (the GUI does).
pub type AudioWindowProgress<'a> = dyn FnMut(Option<i64>, Option<i64>, usize) + Send + 'a;

/// Fetch the audio-only window `[from_ms, to_ms]` of `video_id`'s live DVR.
/// `to_ms = None` resolves to the live head at fetch start, so the window ends at "now"
/// and does not chase the advancing edge. `from_ms = 0` is the DVR start
/// (`player_time_ms = 0` addresses the DVR window start, SABR-044).
///
/// Only the audio format is selected, so `buffered_ranges` and
/// `preferred_video_format_ids` carry no video and the server serves the audio track alone.
/// Each round requests at the buffered frontier, so the window streams forward at full
/// speed.
pub async fn fetch_audio_window(
    session: &Session,
    solver: &NSigSolver,
    pot: &Arc<dyn PoTokenProvider>,
    video_id: &str,
    from_ms: i64,
    to_ms: Option<i64>,
    progress: Option<&mut AudioWindowProgress<'_>>,
) -> Result<AudioWindow, StreamError> {
    let source = open_audio_source(session, solver, pot, video_id).await?;
    fetch_audio_range(&source, from_ms, to_ms, progress).await
}

/// One bootstrapped live stream's audio: any number of [`fetch_audio_range`] calls, each
/// with its own SABR stream, run off it concurrently without another page load.
#[derive(Clone)]
pub struct AudioSource {
    session: Session,
    url: String,
    ustreamer_config: Vec<u8>,
    po_token: Vec<u8>,
    audio: SelectedFormat,
    all_formats: Vec<SelectedFormat>,
}

impl AudioSource {
    /// The audio itag and the googlevideo host the SABR url points at, for logs.
    pub fn describe(&self) -> String {
        let host = self.url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or("?");
        format!("itag {} on {host}", self.audio.itag)
    }
}

pub async fn open_audio_source(
    session: &Session,
    solver: &NSigSolver,
    pot: &Arc<dyn PoTokenProvider>,
    video_id: &str,
) -> Result<AudioSource, StreamError> {
    // Minted against this page load's challenge, as the playback paths do: a minter with no
    // attestation yet has nothing else to attest with.
    let boot =
        bootstrap_player(session, solver, video_id, &Navigation::default(), Some(pot), &mut StartupTimer::new())
            .await?;
    let player = boot.player;

    // Smallest usable audio rung: transcription needs speech, not fidelity.
    let audio = player
        .formats
        .iter()
        .filter(|f| f.is_audio())
        .min_by_key(|f| f.bitrate.unwrap_or(i64::MAX))
        .ok_or(StreamError::NoAudioFormats)?;

    let po_token = decode_base64_lenient(&pot.get_po_token(video_id, boot.bg_challenge.as_ref()).await?)
        .ok_or(StreamError::BadPoToken)?;
    let all_formats: Vec<SelectedFormat> = player
        .formats
        .iter()
        .map(|f| SelectedFormat {
            itag: f.itag,
            last_modified: f.last_modified,
            is_video: f.is_video(),
            xtags: f.xtags.clone(),
        })
        .collect();
    Ok(AudioSource {
        session: session.clone(),
        url: boot.url,
        ustreamer_config: player.ustreamer_config.clone(),
        po_token,
        audio: SelectedFormat {
            itag: audio.itag,
            last_modified: audio.last_modified,
            is_video: false,
            xtags: audio.xtags.clone(),
        },
        all_formats,
    })
}

/// The live head (stream-absolute ms) as one SABR round at `near_ms` reports it.
pub async fn live_head_ms(source: &AudioSource, near_ms: i64) -> Result<Option<i64>, StreamError> {
    let mut engine = SabrEngine::new(
        source.ustreamer_config.clone(),
        source.po_token.clone(),
        vec![source.audio.clone()],
        &source.all_formats,
    );
    let resp = source.session.post_sabr(&source.url, engine.request_bytes(near_ms)).await?;
    engine.ingest(resp);
    Ok(engine.live_head_ms())
}

/// Fetch `[from_ms, to_ms]` of `source`'s audio on a SABR stream of its own.
pub async fn fetch_audio_range(
    source: &AudioSource,
    from_ms: i64,
    to_ms: Option<i64>,
    mut progress: Option<&mut AudioWindowProgress<'_>>,
) -> Result<AudioWindow, StreamError> {
    let session = &source.session;
    let audio = &source.audio;
    let mut engine = SabrEngine::new(
        source.ustreamer_config.clone(),
        source.po_token.clone(),
        vec![audio.clone()],
        &source.all_formats,
    );

    let mut url = source.url.clone();
    let mut bytes: Vec<u8> = Vec::new();
    let mut consecutive_errors = 0u32;
    let mut last_media = std::time::Instant::now();
    let mut target_ms = to_ms;
    // A forward jump past a gap re-anchors the engine; the window still starts at the first media.
    let mut first_anchor: Option<i64> = None;

    // Some live sources (rolling news, restarted encoders) leave holes in the DVR window:
    // the server answers metadata-only for any `player_time` inside one (SABR-043). When
    // the frontier stalls after media has arrived, probe `player_time` forward, doubling
    // the jump, until media resumes.
    const GAP_PROBE_AFTER: u32 = 2; // empty rounds post-media before probing
    const GAP_PROBE_START_MS: i64 = 5_000; // ~one segment
    const GAP_PROBE_MAX_MS: i64 = 10 * 60 * 1000; // give up past a 10-min hole
    let mut probe_skip_ms: i64 = 0;
    let mut stall_rounds = 0u32;

    loop {
        // Request at the buffered frontier, or `from_ms` before any media. Not clamped up to
        // `from_ms`: when the first segment straddles it, the buffered end can sit just
        // below `from_ms`, and asking ahead of the buffer earns a SABR_SEEK that clears
        // progress and sends the loop back to `from_ms` forever (SABR-051). Following the
        // frontier keeps `player_time` within the buffer.
        let base_pt = engine.buffered_end_ms().unwrap_or(from_ms);
        let req_pt = base_pt + probe_skip_ms;
        let t_post = std::time::Instant::now();
        let resp = match session.post_sabr(&url, engine.request_bytes(req_pt)).await {
            Ok(b) => {
                consecutive_errors = 0;
                b
            }
            Err(e) => {
                consecutive_errors += 1;
                if tracing::enabled!(tracing::Level::DEBUG) {
                    tracing::debug!("[aw-err] req_pt={req_pt} post={}ms error: {e}", t_post.elapsed().as_millis());
                }
                if consecutive_errors >= 5 {
                    return Err(StreamError::Core(e));
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue;
            }
        };
        let resp_len = resp.len();
        // Decode the parts of this round's response. For an empty response this shows
        // whether it was a SABR error, a redirect or metadata only.
        let dbg_parts: Option<Vec<(u32, usize)>> = if tracing::enabled!(tracing::Level::DEBUG) {
            Some(innertube_sabr::parse(resp.clone()).into_iter().map(|p| (p.part_type, p.payload.len())).collect())
        } else {
            None
        };
        let ingest = engine.ingest(resp);
        if let Some(r) = ingest.redirect {
            url = r; // SABR_REDIRECT urls arrive with `n` already transformed (SABR-036).
        }
        let mut got = 0usize;
        for (itag, b) in &ingest.media {
            // Server-driven: drop any non-selected track that arrives anyway
            // (progress/acks still advance via the engine's ingest).
            if *itag == audio.itag {
                bytes.extend_from_slice(b);
                got += b.len();
            }
        }
        if tracing::enabled!(tracing::Level::DEBUG) {
            tracing::debug!(
                "[aw-dbg] req_pt={req_pt} post={}ms resp={resp_len}B got={got} backoff={:?} buf_end={:?} head={:?} err={:?} prot={:?} parts={:?}",
                t_post.elapsed().as_millis(),
                engine.request_policy().backoff_time_ms,
                engine.buffered_end_ms(),
                engine.live_head_ms(),
                ingest.sabr_error,
                ingest.protection_status,
                dbg_parts.unwrap_or_default(),
            );
        }
        // An edge-bounded window resolves its end once the head is known.
        if target_ms.is_none() {
            target_ms = engine.live_head_ms();
        }
        let end = engine.buffered_end_ms();
        if let Some(cb) = progress.as_deref_mut() {
            cb(end, target_ms, bytes.len());
        }
        if let (Some(end), Some(t)) = (end, target_ms) {
            if end >= t {
                break;
            }
        }
        if got == 0 {
            // An ad cue says how much of the withheld break is left: request past it at once.
            if let Some(b) = ingest.ad_break.filter(|b| b.remaining_ms > 0) {
                let jump = req_pt - base_pt + b.remaining_ms;
                let capped = target_ms.map_or(jump, |t| (base_pt + jump).min(t) - base_pt);
                if tracing::enabled!(tracing::Level::DEBUG) {
                    tracing::debug!("[aw-dbg] ad break at req_pt={req_pt}: {} ms left, next req_pt={}", b.remaining_ms, base_pt + capped);
                }
                if capped <= probe_skip_ms {
                    break;
                }
                probe_skip_ms = capped;
                stall_rounds = 0;
                continue;
            }
            // Empty rounds after media, or under an announced backoff, mark a DVR
            // discontinuity: probe forward past it.
            let held = engine.request_policy().backoff_time_ms.is_some_and(|b| b > 0);
            if engine.buffered_end_ms().is_some() || held {
                stall_rounds += 1;
                if stall_rounds >= GAP_PROBE_AFTER {
                    stall_rounds = 0;
                    let grown =
                        if probe_skip_ms == 0 { GAP_PROBE_START_MS } else { probe_skip_ms * 2 };
                    // Clamp to the window end. The region after a hole runs to the head, so a
                    // probe past the hole lands on media.
                    let capped = match target_ms {
                        Some(t) => (base_pt + grown).min(t) - base_pt,
                        None => grown,
                    };
                    // No forward room left (probed to the window end already), or an
                    // implausibly long hole → stop with the partial content we have.
                    if capped <= probe_skip_ms || capped > GAP_PROBE_MAX_MS {
                        break;
                    }
                    probe_skip_ms = capped;
                    continue; // re-request immediately at the probed position
                }
            }
            // A held session announces its backoff. A minute without media ends the window
            // with a partial result.
            if last_media.elapsed() >= std::time::Duration::from_secs(60) {
                break;
            }
            let backoff = engine.request_policy().backoff_time_ms.unwrap_or(0).max(500);
            tokio::time::sleep(std::time::Duration::from_millis(backoff as u64)).await;
        } else {
            first_anchor = first_anchor.or(ingest.media_start_ms.get(&audio.itag).copied()).or(engine.anchor_ms());
            last_media = std::time::Instant::now();
            stall_rounds = 0;
            probe_skip_ms = 0; // media resumed: request at the frontier again
        }
    }

    if bytes.is_empty() {
        return Err(StreamError::NoAudioFormats);
    }
    // Live fMP4 fragments carry absolute decode times (a huge baseMediaDecodeTime).
    // Consumers read the container duration from them, so every `tfdt` is rebased to
    // start at 0.
    rebase_fmp4_decode_times(&mut bytes);
    let seconds = engine.buffered_ms() as f64 / 1000.0;
    // Report the window actually fetched. A `from_ms` before the DVR buffer is clamped by
    // the server to the DVR start, so the content starts later than `from_ms`. Reporting
    // the requested target would make a chunked caller re-request the same segment.
    // `anchor_ms` and `buffered_end_ms` are the first and last segment times served.
    let actual_from = first_anchor.or(engine.anchor_ms()).unwrap_or(from_ms);
    let actual_to = engine.buffered_end_ms().unwrap_or_else(|| target_ms.unwrap_or(from_ms));
    Ok(AudioWindow {
        bytes,
        from_ms: actual_from,
        to_ms: actual_to,
        seconds,
    })
}

/// Subtract the first fragment's `baseMediaDecodeTime` from every `tfdt` in the
/// stream (`moof`→`traf`→`tfdt`, version 0 = u32 / version 1 = u64), so the file's
/// media timeline starts at 0 while inter-fragment deltas are preserved. Truncated
/// or non-MP4 content is left untouched (best-effort walk, never panics).
fn rebase_fmp4_decode_times(bytes: &mut [u8]) {
    fn read_u32(b: &[u8], at: usize) -> Option<u32> {
        Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
    }
    // First pass: find the base (the first tfdt's value); second: subtract it.
    let mut base: Option<u64> = None;
    for pass in 0..2 {
        let mut i = 0usize;
        while let (Some(size), Some(kind)) = (read_u32(bytes, i), read_u32(bytes, i + 4)) {
            let size = size as usize;
            if size < 8 || i + size > bytes.len() {
                break; // size==1 (64-bit) / 0 (to-EOF) / truncated: stop, leave rest untouched
            }
            if kind == u32::from_be_bytes(*b"moof") {
                // moof > traf > tfdt
                let (mut j, moof_end) = (i + 8, i + size);
                while let (Some(tsize), Some(tkind)) = (read_u32(bytes, j), read_u32(bytes, j + 4)) {
                    let tsize = tsize as usize;
                    if tsize < 8 || j + tsize > moof_end {
                        break;
                    }
                    if tkind == u32::from_be_bytes(*b"traf") {
                        let (mut k, traf_end) = (j + 8, j + tsize);
                        while let (Some(fsize), Some(fkind)) =
                            (read_u32(bytes, k), read_u32(bytes, k + 4))
                        {
                            let fsize = fsize as usize;
                            if fsize < 8 || k + fsize > traf_end {
                                break;
                            }
                            if fkind == u32::from_be_bytes(*b"tfdt") && fsize >= 16 {
                                let version = bytes[k + 8];
                                if version == 1 && fsize >= 20 {
                                    let v = u64::from_be_bytes(
                                        bytes[k + 12..k + 20].try_into().unwrap(),
                                    );
                                    if pass == 0 {
                                        base.get_or_insert(v);
                                    } else {
                                        let nv = v.saturating_sub(base.unwrap_or(0));
                                        bytes[k + 12..k + 20].copy_from_slice(&nv.to_be_bytes());
                                    }
                                } else if version == 0 {
                                    let v = read_u32(bytes, k + 12).unwrap_or(0) as u64;
                                    if pass == 0 {
                                        base.get_or_insert(v);
                                    } else {
                                        let nv = (v.saturating_sub(base.unwrap_or(0))) as u32;
                                        bytes[k + 12..k + 16].copy_from_slice(&nv.to_be_bytes());
                                    }
                                }
                            }
                            k += fsize;
                        }
                    }
                    j += tsize;
                }
            }
            i += size;
        }
        if base.is_none() {
            return; // no fragments, nothing to rebase
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(payload);
        v
    }

    fn tfdt_v1(t: u64) -> Vec<u8> {
        let mut p = vec![1, 0, 0, 0]; // version 1, flags 0
        p.extend_from_slice(&t.to_be_bytes());
        boxed(b"tfdt", &p)
    }

    fn moof_with_tfdt(t: u64) -> Vec<u8> {
        boxed(b"moof", &boxed(b"traf", &tfdt_v1(t)))
    }

    #[test]
    fn absolute_decode_times_rebase_to_zero_keeping_the_deltas() {
        // Absolute live decode times 17_500_000_000 and 17_502_000_000 (48 kHz timescale)
        // become 0 and 2_000_000.
        let mut stream = boxed(b"ftyp", b"isom");
        stream.extend(boxed(b"moov", &[]));
        stream.extend(moof_with_tfdt(17_500_000_000));
        stream.extend(boxed(b"mdat", &[0xAA; 16]));
        stream.extend(moof_with_tfdt(17_502_000_000));
        stream.extend(boxed(b"mdat", &[0xBB; 16]));

        let mut expect = boxed(b"ftyp", b"isom");
        expect.extend(boxed(b"moov", &[]));
        expect.extend(moof_with_tfdt(0));
        expect.extend(boxed(b"mdat", &[0xAA; 16]));
        expect.extend(moof_with_tfdt(2_000_000));
        expect.extend(boxed(b"mdat", &[0xBB; 16]));

        rebase_fmp4_decode_times(&mut stream);
        assert_eq!(stream, expect, "only the tfdt values change");
    }

    #[test]
    fn non_mp4_and_truncated_input_pass_through_unchanged() {
        let mut webm = vec![0x1A, 0x45, 0xDF, 0xA3, 0x01, 0x02, 0x03];
        let orig = webm.clone();
        rebase_fmp4_decode_times(&mut webm);
        assert_eq!(webm, orig);

        let mut truncated = boxed(b"moof", &boxed(b"traf", &tfdt_v1(5)));
        truncated.truncate(10);
        let orig = truncated.clone();
        rebase_fmp4_decode_times(&mut truncated);
        assert_eq!(truncated, orig);
    }
}
