use innertube_sabr::proto::video_streaming::MediaHeader;
use innertube_sabr::prost::Message;

/// A MEDIA_HEADER (part type 20) from a live SABR stream (notes/fixtures/README.md):
/// header_id 0, itag 140.
const MEDIA_HEADER: &[u8] = include_bytes!("../../../notes/fixtures/sabr-media-header.bin");

#[test]
fn media_header_decodes_header_id_and_itag() {
    let h = MediaHeader::decode(MEDIA_HEADER).unwrap();

    assert_eq!(h.header_id, Some(0));
    assert_eq!(h.itag, Some(140)); // AAC audio
}
