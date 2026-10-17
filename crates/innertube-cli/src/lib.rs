//! Subcommand implementations behind the `it` CLI.
pub mod api;
pub mod audio_window;
pub mod auth;
pub mod cmds;
pub mod find_live;
pub mod shorts;
pub mod vod;

/// Installs a stderr `tracing` subscriber filtered by `RUST_LOG` (default `info`).
pub fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).init();
}

/// The PO-token provider named by `INNERTUBE_POT` (`native` or `bgutil`, default native).
pub fn pot_from_env() -> std::sync::Arc<dyn innertube::PoTokenProvider> {
    let kind = std::env::var("INNERTUBE_POT")
        .ok()
        .and_then(|s| innertube::PotBackendKind::parse(&s))
        .unwrap_or_default();
    innertube::pot_provider(kind, innertube::WEB_USER_AGENT)
}
