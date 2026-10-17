use innertube_sabr::{parse, write_part};

/// A 98-byte metadata-only UMP response from a live SABR stream
/// (notes/fixtures/README.md). It holds five parts.
const METADATA_UMP: &[u8] = include_bytes!("../../../notes/fixtures/sabr-response-metadata.ump");

#[test]
fn parses_metadata_response_into_its_five_parts() {
    let parts = parse(METADATA_UMP);

    let types: Vec<u32> = parts.iter().map(|p| p.part_type).collect();
    let sizes: Vec<usize> = parts.iter().map(|p| p.payload.len()).collect();

    // STREAM_PROTECTION_STATUS, PLAYBACK_START_POLICY, REQUEST_IDENTIFIER,
    // LIVE_METADATA, NEXT_REQUEST_POLICY
    assert_eq!(types, vec![58, 47, 52, 31, 35]);
    assert_eq!(sizes, vec![2, 10, 9, 31, 36]);

    // STREAM_PROTECTION_STATUS payload, verbatim from the capture (hex 0801).
    assert_eq!(parts[0].payload, vec![0x08, 0x01]);
}

#[test]
fn unknown_part_types_pass_through_and_parsing_continues() {
    // type=100 (unassigned), size=3, payload AABBCC; then type=21 (MEDIA), size=2.
    let mut data = Vec::new();
    write_part(&mut data, 100, &[0xAA, 0xBB, 0xCC]);
    write_part(&mut data, 21, &[0x09, 0x09]);

    let parts = parse(&data[..]);

    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].part_type, 100);
    assert_eq!(parts[0].payload, vec![0xAA, 0xBB, 0xCC]);
    assert_eq!(parts[1].part_type, 21);
    assert_eq!(parts[1].payload, vec![0x09, 0x09]);
}

#[test]
fn a_body_cut_at_any_offset_parses_to_the_complete_leading_parts() {
    // Media spans several HTTP responses, so a body can end mid-part. Every part in
    // this capture has a one-byte type and a one-byte size.
    let full = parse(METADATA_UMP);
    let mut ends = Vec::new();
    let mut off = 0;
    for p in &full {
        off += 2 + p.payload.len();
        ends.push(off);
    }
    assert_eq!(off, METADATA_UMP.len());

    for cut in 0..=METADATA_UMP.len() {
        let complete = ends.iter().filter(|&&e| e <= cut).count();
        let parts = parse(&METADATA_UMP[..cut]);
        assert_eq!(parts.len(), complete, "cut at {cut}");
        for (got, want) in parts.iter().zip(&full) {
            assert_eq!((got.part_type, &got.payload), (want.part_type, &want.payload));
        }
    }
}
