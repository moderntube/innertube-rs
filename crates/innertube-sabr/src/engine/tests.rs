use crate::container::find_init_start;

#[test]
fn rejects_ebml_magic_without_doctype() {
    // The EBML magic can occur by chance inside media data.
    let buf = [0x00, 0x1A, 0x45, 0xDF, 0xA3, 0x11, 0x22, 0x33, 0x44];
    assert_eq!(find_init_start(&buf), None);
}

#[test]
fn rejects_ftyp_not_followed_by_moov() {
    // `ftyp` with a plausible size but an `mdat` box after it.
    let buf = [0, 0, 0, 0x10, b'f', b't', b'y', b'p', b'm', b'p', b'4', b'2', 1, 2, 3, 4, 0, 0, 0, 8, b'm', b'd', b'a', b't'];
    assert_eq!(find_init_start(&buf), None);
}

#[test]
fn rejects_ftyp_with_implausible_size_prefix() {
    let buf = [0xFF, 0xFF, 0xFF, 0xFF, b'f', b't', b'y', b'p'];
    assert_eq!(find_init_start(&buf), None);
}
