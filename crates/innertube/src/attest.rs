//! Session-scoped PO-token re-attestation budget.
//!
//! `STREAM_PROTECTION_STATUS` (UMP part 58) reports 1 attested, 2 pending and 3 attestation
//! required with media withheld (GATE-001, GATE-003). The reference player refreshes its
//! token once per session (POT-006, POT-007). Re-minting on every status >= 2 round
//! presents a new attestation each round, because bgutil returns a different token per call
//! (POT-007), and the session escalates to status 3 with no media.
//!
//! The budget is one soft refresh per session plus at most one hard re-attest, which
//! invalidates the provider's cached integrity token (POT-007). The hard step is ours, not the
//! reference player's. It fires only at status 3, so it never runs during ordinary status-2
//! pending.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

/// What the fetch loop should do about a `STREAM_PROTECTION_STATUS` this round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reattest {
    /// Re-mint from the provider's existing attestation (YouTube's single refresh).
    Soft,
    /// Invalidate the provider's cached attestation first, then mint.
    Hard,
    /// Budget spent or nothing to do: keep the token already held.
    Nothing,
}

/// Shared across every fetch path of one playback session (prefetch, read-miss,
/// client-driven, warm, and both live lanes), because the budget is per *session*: the
/// server's view of our attestation does not care which of our loops sent the request.
#[derive(Debug, Clone, Default)]
pub(crate) struct ReattestBudget {
    soft_spent: Arc<AtomicU8>,
    hard_spent: Arc<AtomicU8>,
    warned: Arc<AtomicU8>,
}

impl ReattestBudget {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Decide what to do for a round that came back with `status`.
    ///
    /// `status` is `None` when the response carried no part 58 at all.
    pub(crate) fn on_status(&self, status: Option<i32>) -> Reattest {
        // The reference client sits at status 2 for several rounds with media flowing and
        // never escalates (POT-007).
        match status {
            Some(s) if s >= 2 => {
                if self.soft_spent.swap(1, Ordering::Relaxed) == 0 {
                    Reattest::Soft
                } else if s >= 3 && self.hard_spent.swap(1, Ordering::Relaxed) == 0 {
                    Reattest::Hard
                } else {
                    Reattest::Nothing
                }
            }
            _ => Reattest::Nothing,
        }
    }

    /// True once both steps are spent and the server still rejects: the provider is then
    /// the suspect.
    pub(crate) fn exhausted(&self) -> bool {
        self.soft_spent.load(Ordering::Relaxed) == 1 && self.hard_spent.load(Ordering::Relaxed) == 1
    }

    /// True exactly once per session, so the "provider is not producing accepted tokens"
    /// diagnostic is printed once instead of on every round of a wedged session.
    pub(crate) fn warn_once(&self) -> bool {
        self.warned.swap(1, Ordering::Relaxed) == 0
    }
}

/// What [`ReattestFn`](crate::vod_bootstrap::ReattestFn) did for this round.
#[derive(Debug)]
pub(crate) enum ReattestOutcome {
    /// A fresh token was minted.
    Reminted { token: Vec<u8> },
    /// The budget is spent and the server is still rejecting.
    Exhausted,
    /// Nothing to do this round.
    Nothing,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Status 2 spends one soft refresh per session and never reaches the hard step (POT-007).
    #[test]
    fn status_2_refreshes_the_token_once_per_session() {
        let b = ReattestBudget::new();
        assert_eq!(b.on_status(Some(2)), Reattest::Soft);
        for _ in 0..200 {
            assert_eq!(b.on_status(Some(2)), Reattest::Nothing);
        }
        assert!(!b.exhausted());
    }

}
