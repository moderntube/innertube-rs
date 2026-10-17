use super::*;
use crate::proto::video_streaming::MediaHeader;
use prost::Message;

fn varint(buf: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            buf.push(b);
            break;
        }
        buf.push(b | 0x80);
    }
}
fn part(t: u64, payload: &[u8]) -> Vec<u8> {
    let mut u = Vec::new();
    varint(&mut u, t);
    varint(&mut u, payload.len() as u64);
    u.extend_from_slice(payload);
    u
}
fn hdr(header_id: u32, itag: i32, seq: i32, start_ms: i64) -> Vec<u8> {
    let mh = MediaHeader {
        header_id: Some(header_id),
        itag: Some(itag),
        sequence_number: Some(seq),
        start_ms: Some(start_ms),
        ..Default::default()
    };
    part(20, &mh.encode_to_vec())
}
fn media(header_id: u32, body: &[u8]) -> Vec<u8> {
    let mut p = Vec::new();
    varint(&mut p, header_id as u64);
    p.extend_from_slice(body);
    part(21, &p)
}
fn end(header_id: u32) -> Vec<u8> {
    let mut p = Vec::new();
    varint(&mut p, header_id as u64);
    part(22, &p)
}
fn seg_bytes(tag: u8) -> Vec<u8> {
    let mut v = vec![0, 0, 0, 0x10, b'f', b't', b'y', b'p', b'm', b'p', b'4', b'2', 0, 0, 0, 0];
    v.extend_from_slice(&[0, 0, 0, 8, b'm', b'o', b'o', b'v']);
    v.extend_from_slice(&[0, 0, 0, 8, b'm', b'o', b'o', b'f']);
    v.extend_from_slice(&[0, 0, 0, 0x0C, b'm', b'd', b'a', b't', tag, tag, tag, tag]);
    v
}

/// Two tracks interleaved, each segment's media split across several part-21s, and one
/// track closing while the other is open.
fn interleaved_body() -> Vec<u8> {
    let a = seg_bytes(0xAA);
    let v = seg_bytes(0xBB);
    let mut b = Vec::new();
    b.extend(hdr(1, 399, 10, 98_000));
    b.extend(media(1, &v[..20]));
    b.extend(hdr(2, 140, 10, 98_000));
    b.extend(media(2, &a[..24]));
    b.extend(media(1, &v[20..]));
    b.extend(end(2));
    b.extend(media(2, &[]));
    b.extend(end(1));
    b
}

fn engine() -> SabrEngine {
    let sel = vec![
        SelectedFormat { itag: 140, last_modified: 1, is_video: false, xtags: None },
        SelectedFormat { itag: 399, last_modified: 2, is_video: true, xtags: None },
    ];
    let mut engine = SabrEngine::new(vec![0], vec![0], sel.clone(), &sel);
    engine.set_collect_segments(true);
    engine
}

type Segments = Vec<(i32, Option<i64>, Vec<u8>)>;

fn summary(ingest: Ingest, media: &mut BTreeMap<i32, Vec<u8>>, segments: &mut Segments) {
    for (itag, bytes) in ingest.media {
        media.entry(itag).or_default().extend_from_slice(&bytes);
    }
    segments.extend(ingest.segments.into_iter().map(|s| (s.itag, s.seq, s.bytes)));
}

/// A response body can be cut anywhere when it is read from the network in chunks.
/// Ingesting the chunks yields the same media and the same whole segments as ingesting
/// the body at once, including when the cut falls inside an open segment.
#[test]
fn splitting_an_interleaved_body_at_any_offset_matches_a_whole_ingest() {
    let body = interleaved_body();
    let (mut whole_media, mut whole_segments) = (BTreeMap::new(), Vec::new());
    summary(engine().ingest(&body), &mut whole_media, &mut whole_segments);
    assert!(!whole_media.is_empty() && whole_segments.len() == 2, "the body delivers media and two segments");

    for k in 0..=body.len() {
        let mut e = engine();
        e.begin_response();
        let (mut media, mut segments) = (BTreeMap::new(), Vec::new());
        for chunk in [&body[..k], &body[k..]] {
            summary(e.ingest_streaming(chunk), &mut media, &mut segments);
        }
        assert_eq!(media, whole_media, "media: split at byte {k}");
        assert_eq!(segments, whole_segments, "segments: split at byte {k}");
    }
}
