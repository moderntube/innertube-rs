//! Captured web-client sessions replayed through `SabrEngine`; see `support/replay.rs`.
//!
//! Each allowed path is a known difference from the reference client. Removing an entry
//! is how a parity fix is checked.

mod support;

use innertube_sabr::DiscontinuityKind;
use support::replay::{engine_after, replay, Replay, Session};

/// Differences on every session.
const ALL: &[(&str, &str)] = &[
    ("1.21", "sticky_resolution is sent only while a manual pick is active: a constant 0 caps the served rung in our sessions"),
    ("1.29", "time_since_last_seek: the engine sends no telemetry counters"),
    ("1.36", "elapsed_wall_time_ms: the engine sends no telemetry counters"),
    ("1.39", "time_since_last_action_ms: the engine sends no telemetry counters"),
    ("1.57", "field 57 counter: the engine sends no telemetry counters"),
    ("1.59", "av1_quality_threshold: the engine sends 2160, the reference sends 1080"),
    ("1.68", "network interruption duration: the reference reports one, the engine sends 0"),
    ("1.80", "field 80: the engine sends it on every request, the reference on the first only"),
    ("1.85", "field 85: the engine sends it; these captures predate it"),
];

/// Differences on VOD sessions.
const VOD: &[(&str, &str)] = &[
    ("2", "selected_format_ids: the reference sends them after the first request, the engine never does"),
    ("3.1.2", "buffered-range lmt: the engine keeps one per itag; with multi-track audio the reference acks the track served"),
    ("3.1.3", "buffered-range xtags: the engine keeps one per itag; with multi-track audio the reference acks the track served"),
];

/// Differences on live sessions.
const LIVE: &[(&str, &str)] = &[
    ("24", "ad cue report: this capture's player sends state 11 without field 5; the engine sends state 3 with field 5"),
    ("16.2", "live FormatId lmt: the reference sends 0, the engine omits it"),
    ("17.2", "live FormatId lmt: the reference sends 0, the engine omits it"),
    ("3.3", "live range duration: the engine estimates from start_ms gaps, the reference sums sample durations"),
];

const FIXTURES: &[&str] = &["live-join.replay", "vod-open-seek.replay", "vod-content-ads-hold.replay"];

fn replays_in_full(fixture: &str) -> Replay {
    let session = Session::load(fixture);
    let extra = if session.is_live() { LIVE } else { VOD };
    let allowed: Vec<_> = ALL.iter().chain(extra).copied().collect();
    match replay(&session, &allowed) {
        Ok(r) => {
            assert_eq!(r.ingests.len(), session.rounds.len());
            r
        }
        Err(d) => panic!("{fixture}: {d}"),
    }
}

/// Main player of a live join, rounds 0-12: edge sentinel, two seek-only rounds, then
/// steady state.
#[test]
fn a_live_join_follows_the_server_seek_and_rides_the_edge() {
    replays_in_full("live-join.replay");
}

/// A VOD open with ABR climbing, then a seek.
#[test]
fn a_vod_open_with_a_seek_matches_the_reference() {
    replays_in_full("vod-open-seek.replay");
}

/// Round 0 is `[35, 57, 67]` with no media (the content-ads hold, GATE-005); media follows
/// the announced 4000 ms backoff.
#[test]
fn a_content_ads_hold_echoes_the_context_and_resumes() {
    replays_in_full("vod-content-ads-hold.replay");
}

/// An fMP4 init starts with an `ftyp` box, a WebM init with the EBML magic.
fn starts_at_container_init(b: &[u8]) -> bool {
    b.get(4..8) == Some(b"ftyp") || b.starts_with(&[0x1a, 0x45, 0xdf, 0xa3])
}

/// A live join lands mid-segment and a VOD open starts from the init, so the first bytes
/// the engine emits for a track are always a container init.
#[test]
fn each_track_stream_starts_at_a_container_init() {
    for fixture in FIXTURES {
        let r = replays_in_full(fixture);
        let mut seen = std::collections::BTreeSet::new();
        for ingest in &r.ingests {
            for (itag, bytes) in &ingest.media {
                if seen.insert(*itag) {
                    assert!(starts_at_container_init(bytes), "{fixture}: itag {itag} starts {:02x?}", &bytes[..8.min(bytes.len())]);
                }
            }
        }
        assert!(!seen.is_empty(), "{fixture}: no media emitted");
    }
}

/// A segment the engine drops as a resend is reported as a backward discontinuity. Every
/// other segment is emitted, and none is emitted twice unless a `SABR_SEEK` rewinds the
/// stream. The bytes emitted for a track never exceed the bytes of its emitted segments.
#[test]
fn no_segment_is_emitted_twice_except_across_a_seek() {
    for fixture in FIXTURES {
        let r = replays_in_full(fixture);
        let mut emitted = std::collections::BTreeSet::new();
        let mut segment_bytes = std::collections::BTreeMap::<i32, usize>::new();
        let mut media_bytes = std::collections::BTreeMap::<i32, usize>::new();
        for ingest in &r.ingests {
            if ingest.sabr_seek_ms.is_some() {
                emitted.clear();
            }
            for seg in &ingest.segments {
                let Some(seq) = seg.seq else {
                    *segment_bytes.entry(seg.itag).or_default() += seg.bytes.len();
                    continue;
                };
                let dropped = ingest.discontinuities.iter().any(|d| {
                    d.itag == seg.itag && d.seq == seq && matches!(d.kind, DiscontinuityKind::Backward)
                });
                if dropped {
                    continue;
                }
                assert!(emitted.insert((seg.itag, seq)), "{fixture}: itag {} segment {seq} emitted twice", seg.itag);
                *segment_bytes.entry(seg.itag).or_default() += seg.bytes.len();
            }
            for (itag, bytes) in &ingest.media {
                *media_bytes.entry(*itag).or_default() += bytes.len();
            }
        }
        for (itag, n) in &media_bytes {
            assert!(*n <= segment_bytes[itag], "{fixture}: itag {itag} emitted {n} B from {} B of segments", segment_bytes[itag]);
        }
    }
}

/// Every segment byte of a VOD response reaches the cache layer: the engine emits VOD
/// media verbatim, with no lead-in trimming and no dedup.
#[test]
fn a_vod_session_emits_every_segment_byte() {
    for fixture in FIXTURES.iter().filter(|f| f.starts_with("vod")) {
        let r = replays_in_full(fixture);
        let mut media = std::collections::BTreeMap::<i32, usize>::new();
        let mut segments = std::collections::BTreeMap::<i32, usize>::new();
        for ingest in &r.ingests {
            for (itag, bytes) in &ingest.media {
                *media.entry(*itag).or_default() += bytes.len();
            }
            for seg in &ingest.segments {
                *segments.entry(seg.itag).or_default() += seg.bytes.len();
            }
        }
        assert!(!media.is_empty(), "{fixture}: no media emitted");
        assert_eq!(media, segments, "{fixture}");
    }
}

/// A round's media blob is cached from its earliest segment's `start_ms`. The headerless
/// init region does not count, so a blob is never anchored at a made-up 0.
#[test]
fn a_media_blob_is_anchored_at_its_earliest_segment_start() {
    for fixture in FIXTURES {
        let r = replays_in_full(fixture);
        for (n, ingest) in r.ingests.iter().enumerate() {
            let mut earliest = std::collections::BTreeMap::<i32, i64>::new();
            for seg in &ingest.segments {
                if let Some(start) = seg.start_ms {
                    earliest.entry(seg.itag).and_modify(|m| *m = (*m).min(start)).or_insert(start);
                }
            }
            assert_eq!(ingest.media_start_ms, earliest, "{fixture}: round {n}");
        }
    }
}

/// Protection status, live head and server seek are surfaced from the parts the response
/// carries, which is how the app re-mints a token, tracks the edge and follows a seek.
#[test]
fn response_status_parts_are_surfaced() {
    use innertube_sabr::prost::Message;
    use innertube_sabr::proto::video_streaming::{LiveMetadata, SabrSeek, StreamProtectionStatus};
    for fixture in FIXTURES {
        let session = Session::load(fixture);
        let r = replays_in_full(fixture);
        for (n, (ingest, round)) in r.ingests.iter().zip(&session.rounds).enumerate() {
            let parts = innertube_sabr::parse(&round.response[..]);
            let last = |t: u32| parts.iter().rev().find(|p| p.part_type == t);
            let status = last(58).and_then(|p| StreamProtectionStatus::decode(&p.payload[..]).unwrap().status);
            assert_eq!(ingest.protection_status, status, "{fixture}: round {n}");
            let live = last(31).map(|p| LiveMetadata::decode(&p.payload[..]).unwrap());
            assert_eq!(ingest.head_sequence_number, live.as_ref().and_then(|m| m.head_sequence_number), "{fixture}: round {n}");
            assert_eq!(ingest.wall_time_ms, live.as_ref().and_then(|m| m.wall_time_ms), "{fixture}: round {n}");
            let seek = last(45).map(|p| SabrSeek::decode(&p.payload[..]).unwrap());
            let seek_ms = seek.and_then(|s| Some(s.seek_media_time? * 1000 / i64::from(s.seek_media_timescale?)));
            assert_eq!(ingest.sabr_seek_ms, seek_ms, "{fixture}: round {n}");
        }
    }
}

/// Round 0 of the live join is the server's `SABR_SEEK` with no media (SABR-009).
fn after_the_join_seek() -> innertube_sabr::SabrEngine {
    use innertube_sabr::prost::Message;
    use innertube_sabr::proto::video_streaming::SabrSeek;
    let session = Session::load("live-join.replay");
    let parts = innertube_sabr::parse(&session.rounds[0].response[..]);
    let seek = SabrSeek::decode(&parts.iter().find(|p| p.part_type == 45).unwrap().payload[..]).unwrap();
    let target = seek.seek_media_time.unwrap() * 1000 / i64::from(seek.seek_media_timescale.unwrap());
    let engine = engine_after(&session, 1);
    assert_eq!(engine.next_player_time(None), target, "the join adopts the server's target");
    engine
}

/// A seek target the server then serves nothing for is dropped: asking for it again would
/// wedge the session on a dead position while the head moves away.
#[test]
fn a_join_target_that_brings_no_media_goes_back_to_the_edge() {
    let mut engine = after_the_join_seek();
    engine.ingest(&[][..]);
    assert_eq!(engine.next_player_time(None), innertube_sabr::LIVE_EDGE_SEEK_MS);
}

/// A reposition (stall recovery, jump to live) forgets the join target, which is stale by
/// however long the session has played.
#[test]
fn a_reposition_forgets_the_join_target() {
    let mut engine = after_the_join_seek();
    engine.seek();
    assert_eq!(engine.next_player_time(None), innertube_sabr::LIVE_EDGE_SEEK_MS);
}
