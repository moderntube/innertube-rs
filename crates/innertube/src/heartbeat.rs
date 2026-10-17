//! The live `player/heartbeat` keepalive (IT-014): a background task that POSTs every
//! `heartbeatParams.intervalMilliseconds` (15 s). Playback does not depend on it, so it runs
//! on its own task and ignores errors.

use std::sync::Arc;
use std::time::Duration;

use crate::client::{PlayerResponse, Session};

/// Default beat interval when the player response doesn't specify one.
const DEFAULT_INTERVAL_MS: u64 = 15_000;

/// Spawn the heartbeat loop for a live session. Each beat POSTs `player/heartbeat` with an
/// incrementing sequence number and echoes the server-data blob, replaced when the response
/// returns a fresh one. The interval comes from `heartbeatParams` (default 15 s). Returns
/// the task handle, to abort or drop on stop, or `None` when the player response carries no
/// heartbeat params.
pub fn spawn_from_player(
    session: Arc<Session>,
    video_id: &str,
    player: &PlayerResponse,
) -> Option<tokio::task::JoinHandle<()>> {
    let server_data = player.heartbeat_server_data.clone()?;
    let interval_ms = player.heartbeat_interval_ms.unwrap_or(DEFAULT_INTERVAL_MS).max(1000);
    let video_id = video_id.to_string();
    Some(tokio::spawn(async move {
        let mut server_data = server_data;
        let mut seq: u64 = 0;
        let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms));
        // Fire the first beat immediately (the WEB client sends one right after load).
        loop {
            ticker.tick().await;
            match session.heartbeat(&video_id, seq, &server_data).await {
                Ok(Some(fresh)) => server_data = fresh, // rotate to the fresh blob
                Ok(None) => {}                          // pure-analytics response; reuse
                Err(_) => {}                            // soft-fail: never disturb playback
            }
            seq += 1;
        }
    }))
}
