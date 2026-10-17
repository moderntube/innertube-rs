//! The ustreamer config and PO token arrive in standard and URL-safe base64, padded and
//! unpadded. `+/` and `-_` differ only in sextets 62 and 63.
use innertube::decode_base64_lenient;

#[test]
fn decodes_standard_and_url_safe_padded_and_unpadded() {
    // Standard alphabet, padded / unpadded.
    assert_eq!(decode_base64_lenient("SGVsbG8=").as_deref(), Some(&b"Hello"[..]));
    assert_eq!(decode_base64_lenient("SGVsbG8").as_deref(), Some(&b"Hello"[..]));

    // 0xFF 0xFF 0xFF is "////" (standard) and "____" (url-safe).
    assert_eq!(decode_base64_lenient("////").as_deref(), Some(&[0xFF, 0xFF, 0xFF][..]));
    assert_eq!(decode_base64_lenient("____").as_deref(), Some(&[0xFF, 0xFF, 0xFF][..]));

    // 0xFB 0xF0 0x00 is "+/AA" (standard) and "-_AA" (url-safe).
    assert_eq!(decode_base64_lenient("+/AA").as_deref(), Some(&[0xFB, 0xF0, 0x00][..]));
    assert_eq!(decode_base64_lenient("-_AA").as_deref(), Some(&[0xFB, 0xF0, 0x00][..]));
}
