//! The VOD session bootstrap shared by every VOD reader: player response, streaming url,
//! PO token, and the hooks that re-mint the token and reload the session.

use std::sync::Arc;

use crate::client::{decode_base64_lenient, PlayerOptions, Session};
use innertube_js::{extract_signature_timestamp, NSigSolver};
use innertube_pot::PoTokenProvider;

use crate::attest::{Reattest, ReattestBudget, ReattestOutcome};
use crate::bootstrap::{Bootstrap, StartupTimer};
use crate::StreamError;

/// Re-mints the GVS PO token on an attestation escalation, under one budget shared by every
/// fetch path that clones it (`crate::attest` says why the budget exists).
#[derive(Clone)]
pub(crate) struct Reattester {
    pot: Arc<dyn PoTokenProvider>,
    binding: String,
    challenge: Option<innertube_pot::PageAttestation>,
    budget: ReattestBudget,
}

impl Reattester {
    pub(crate) async fn run(&self, status: Option<i32>) -> ReattestOutcome {
        let hard = match self.budget.on_status(status) {
            Reattest::Soft => false,
            Reattest::Hard => true,
            Reattest::Nothing => {
                return if self.budget.exhausted() && self.budget.warn_once() {
                    ReattestOutcome::Exhausted
                } else {
                    ReattestOutcome::Nothing
                }
            }
        };
        // A plain re-mint reuses the provider's cached attestation. Once the server has
        // rejected tokens from it, only invalidating that cache helps.
        if hard {
            self.pot.invalidate_attestation().await;
        }
        match self
            .pot
            .get_po_token(&self.binding, self.challenge.as_ref())
            .await
            .ok()
            .and_then(|t| decode_base64_lenient(&t))
        {
            Some(token) => ReattestOutcome::Reminted { token },
            None => ReattestOutcome::Nothing,
        }
    }
}

/// Honours a `RELOAD_PLAYER_RESPONSE` (UMP part 46): re-runs `player()` with the reload
/// token, re-solves `n`, and yields the new `(POST url, ustreamer_config)`.
///
/// The server withholds media until the reload is honoured (SABR-037). A new PO token does
/// not satisfy it, and the rotated config must replace the old one.
pub(crate) type ReloadFn = Arc<
    dyn Fn(
            String,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<(String, Vec<u8>)>> + Send>>
        + Send
        + Sync,
>;

/// How far ahead of the playhead a track is fetched, in ms of media time. Each round requests
/// both tracks, so the bound is time per track (SABR-020, `base.js` 9fc68080 `policy.AC`).
pub(crate) const PREFETCH_AHEAD_MS: i64 = 120_000;

/// The buffered-ahead (ms) at which a track stops being requested: [`PREFETCH_AHEAD_MS`],
/// lowered to the server's readahead target when it sent one (SABR-019).
pub(crate) fn request_gate_ms(server_target_ms: i32) -> i64 {
    if server_target_ms > 0 {
        PREFETCH_AHEAD_MS.min(server_target_ms as i64)
    } else {
        PREFETCH_AHEAD_MS
    }
}

/// What every VOD session opens with: the player response, the streaming url, and the
/// PO token together with the hooks that re-mint it and reload the session.
pub(crate) struct VodBootstrap {
    pub(crate) player: crate::client::PlayerResponse,
    pub(crate) url: String,
    pub(crate) po_token: Vec<u8>,
    pub(crate) reattest: Reattester,
    pub(crate) reload: ReloadFn,
    pub(crate) watch_details: Option<crate::client::VideoDetails>,
}

/// Mint the PO token and build the session hooks for a bootstrap whose player response
/// is not live.
pub(crate) async fn bootstrap_vod(
    session: &Session,
    pot: Arc<dyn PoTokenProvider>,
    video_id: &str,
    bootstrap: Bootstrap,
    timer: &mut StartupTimer,
) -> Result<VodBootstrap, StreamError> {
        let Bootstrap { player, url, base_js, base_js_url: _, visitor_data, bg_challenge, watch_details } =
            bootstrap;

        // The GVS token is bound to the video id (POT-008).
        let gvs_binding = video_id.to_string();
        let po_token = decode_base64_lenient(&pot.get_po_token(&gvs_binding, bg_challenge.as_ref()).await?)
            .ok_or(StreamError::BadPoToken)?;
        timer.lap("PO token mint");

        // Re-mints use the binding and page challenge the session opened with (POT-003).
        let reattest = Reattester {
            pot: pot.clone(),
            binding: gvs_binding.clone(),
            challenge: bg_challenge.clone(),
            budget: ReattestBudget::new(),
        };
        let reload: ReloadFn = {
            let session = session.clone();
            let video_id = video_id.to_string();
            let visitor_data = visitor_data.clone();
            let base_js = base_js.clone();
            Arc::new(move |token: String| {
                let (session, video_id, visitor_data, base_js) =
                    (session.clone(), video_id.clone(), visitor_data.clone(), base_js.clone());
                Box::pin(async move {
                    let sts = base_js.as_deref().and_then(extract_signature_timestamp);
                    let player = session
                        .player(
                            &video_id,
                            &PlayerOptions {
                                visitor_data: Some(&visitor_data),
                                signature_timestamp: sts,
                                reload_token: Some(&token),
                                ..Default::default()
                            },
                        )
                        .await
                        .ok()?;
                    // A reloaded url needs a new n-sig solve (PLAYER-006).
                    let url = match &base_js {
                        Some(js) => crate::bootstrap::solve_url_n(
                            &player.server_abr_streaming_url,
                            js,
                            &NSigSolver::new(),
                        )
                        .await
                        .ok()?,
                        None => player.server_abr_streaming_url.clone(),
                    };
                    Some((url, player.ustreamer_config.clone()))
                })
            })
        };

        Ok(VodBootstrap { player, url, po_token, reattest, reload, watch_details })
}

/// Ceiling on an announced backoff, in ms. The largest announced value is 12000 (SABR-002).
const NO_MEDIA_MAX_WAIT_MS: u64 = 15_000;

/// Wait used when the server announces no backoff at all.
pub(crate) const NO_MEDIA_FLOOR_MS: u64 = 700;

/// The hold the server asks for in `NEXT_REQUEST_POLICY.backoff_time_ms` (UMP part 35,
/// field 4), in ms, or `None` when it asks for nothing.
///
/// Callers turn it into a deadline. The reference player stores `now + backoff` and waits
/// until that instant (SABR-003). A `SABR_REDIRECT` response carries no policy, so
/// `request_policy()` still returns the previous one, and sleeping it again would serve the
/// hold twice.
///
/// Announced values are 2000, 4000, 5000, 8800 and 12000 (SABR-002).
pub(crate) fn announced_hold_ms(announced: Option<i32>) -> Option<u64> {
    // The field is signed. A negative value is treated as absent.
    announced
        .filter(|&ms| ms > 0)
        .map(|ms| (ms as u64).min(NO_MEDIA_MAX_WAIT_MS))
}
