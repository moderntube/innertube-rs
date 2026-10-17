/// Compile the SABR `.proto` files we use into Rust types at build time.
/// Uses `protox` (pure Rust), so no `protoc` binary is required (musl-friendly).
///
/// Only an explicit set of top-level messages is compiled (imports resolve transitively).
/// The `onesie_*` and `ump_part_id` schemas are left out: they collide on package-scoped
/// enum names (proto2) and are not used.
fn main() {
    let roots = [
        "video_streaming/video_playback_abr_request.proto",
        "video_streaming/media_header.proto",
        "video_streaming/live_metadata.proto",
        "video_streaming/stream_protection_status.proto",
        "video_streaming/sabr_redirect.proto",
        "video_streaming/format_initialization_metadata.proto",
        "video_streaming/next_request_policy.proto",
        "video_streaming/reload_player_response.proto",
        "video_streaming/playback_start_policy.proto",
        // P0 protocol-robustness parts (decoded reactively by the engine):
        "video_streaming/sabr_context_update.proto",
        "video_streaming/sabr_context_sending_policy.proto",
        "video_streaming/sabr_error.proto",
        "video_streaming/sabr_seek.proto",
        "video_streaming/cuepoint_list.proto",
    ];

    println!("cargo:rerun-if-changed=proto");

    let descriptors = protox::compile(roots, ["proto"]).expect("protox failed to compile protos");
    prost_build::Config::new()
        // Emit `bytes::Bytes` instead of `Vec<u8>` for the large session-invariant request
        // fields: the engine holds them once and re-attaches them to every per-round
        // request, so a clone is a refcount bump instead of a deep copy of the multi-KB
        // ustreamer config, PO token and cookie.
        .bytes([
            ".video_streaming.VideoPlaybackAbrRequest.video_playback_ustreamer_config",
            ".video_streaming.StreamerContext.po_token",
            ".video_streaming.StreamerContext.playback_cookie",
        ])
        .compile_fds(descriptors)
        .expect("prost-build failed to generate types");
}
