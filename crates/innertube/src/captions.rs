//! [`Captions`]: a playing video's caption tracks, fetched with its session and PO token.

use crate::client::{Caption, CaptionTrack, Error, Session};

#[derive(Clone)]
pub struct Captions {
    session: Session,
    tracks: Vec<CaptionTrack>,
    po_token: Vec<u8>,
}

impl Captions {
    pub(crate) fn new(session: Session, tracks: Vec<CaptionTrack>, po_token: Vec<u8>) -> Self {
        Self { session, tracks, po_token }
    }

    pub fn tracks(&self) -> &[CaptionTrack] {
        &self.tracks
    }

    /// The cues of track `index`.
    pub async fn fetch(&self, index: usize) -> Result<Vec<Caption>, Error> {
        let Some(track) = self.tracks.get(index) else { return Ok(Vec::new()) };
        self.session.timedtext(track, Some(&self.po_token)).await
    }
}
