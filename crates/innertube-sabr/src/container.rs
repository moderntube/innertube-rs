/// Per-track lead-in trimmer. Live segments each lead with a container init (MP4
/// `ftyp`+`moov` or a WebM EBML header, UMP-010), but a live join can start a track
/// mid-segment with a bare fragment (UMP-012). A non-seekable consumer such as a fifo then
/// cannot find `moov`. Per track, output is withheld until the first init boundary,
/// emitted from there, and passed through after.
#[derive(Debug, Default)]
pub(crate) struct LeadIn {
    started: bool,
    buf: Vec<u8>,
}

impl LeadIn {
    /// Feed newly received bytes; return the bytes that are safe to emit (empty until the
    /// stream's first container-init boundary is seen). Takes ownership so the steady-state
    /// path returns the buffer without a copy.
    pub(crate) fn feed(&mut self, bytes: Vec<u8>) -> Vec<u8> {
        if self.started {
            return bytes;
        }
        self.buf.extend_from_slice(&bytes);
        match find_init_start(&self.buf) {
            Some(k) => {
                self.started = true;
                let tail = self.buf.split_off(k);
                self.buf = Vec::new();
                tail
            }
            None => Vec::new(),
        }
    }
}

/// Offset of the first container-init boundary in `buf`: an MP4 `ftyp` box (with
/// a sane size prefix) or a WebM/Matroska EBML header (`1A 45 DF A3`), whichever
/// comes first. `None` if neither is present yet.
pub(crate) fn find_init_start(buf: &[u8]) -> Option<usize> {
    match (find_ftyp_box(buf), find_ebml_header(buf)) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Offset of a WebM/Matroska EBML header: the magic `1A 45 DF A3` followed
/// (within the header) by the `DocType` string `webm`/`matroska`. Requiring the
/// DocType rejects a coincidental 4-byte magic run inside MP4 `mdat`.
pub(crate) fn find_ebml_header(buf: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 4 <= buf.len() {
        if buf[i..i + 4] == [0x1A, 0x45, 0xDF, 0xA3] {
            let window = &buf[i..(i + 64).min(buf.len())];
            if window.windows(4).any(|w| w == b"webm") || window.windows(8).any(|w| w == b"matroska") {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Offset of the first MP4 init segment: a `ftyp` box (sane big-endian size)
/// immediately followed by a `moov` box. Requiring the `ftyp`→`moov` adjacency
/// rejects coincidental `ftyp` byte runs inside `mdat` (which a bare `ftyp`
/// scan would false-match when we join a segment mid-flight), and `ftyp`+`moov`
/// is exactly what an MP4 init segment / self-initializing live segment leads with.
pub(crate) fn find_ftyp_box(buf: &[u8]) -> Option<usize> {
    let mut i = 4;
    while i + 4 <= buf.len() {
        if &buf[i..i + 4] == b"ftyp" {
            let start = i - 4;
            let size = u32::from_be_bytes([buf[start], buf[start + 1], buf[start + 2], buf[start + 3]]) as usize;
            // The box after ftyp must be `moov`. If it has not arrived yet, this is not a match;
            // the check repeats once more bytes are buffered.
            let next = start + size;
            if (8..=4096).contains(&size) && next + 8 <= buf.len() && &buf[next + 4..next + 8] == b"moov" {
                return Some(start);
            }
        }
        i += 1;
    }
    None
}
