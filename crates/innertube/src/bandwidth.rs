//! The VOD cold-start seed: the last measured link rate, persisted by the live path
//! (`innertube_sabr::bandwidth_persist`, our `yt-player-bandwidth`), with a cold default.

/// Conservative cold default when nothing is persisted: 1 MB/s (8 Mbps), the web
/// client's fallback. Enough for ~1080p, not 4K, until the link is measured.
pub const DEFAULT_BYTES_PER_SEC: i64 = 1_048_576;

/// The last persisted estimate (bytes/sec), or [`DEFAULT_BYTES_PER_SEC`].
pub fn load() -> i64 {
    innertube_sabr::bandwidth_persist::load().unwrap_or(DEFAULT_BYTES_PER_SEC)
}
