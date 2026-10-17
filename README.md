# innertube-rs

A Rust client for YouTube's private InnerTube API and its SABR/UMP streaming protocol.
It plays live and on-demand video the way YouTube's web player does and hands the media
to your player as complete segments, or as a byte stream.

The GTK4 app [moderntube](https://github.com/moderntube/moderntube) is built on it.

## Status

Usable and in daily use by moderntube, but young. The API changes without notice and the
crates are not on crates.io; depend on a git revision. YouTube changes its client often,
so expect breakage between releases of the web player.

## Features

- **InnerTube API:** search, watch-page metadata, home and subscription feeds, channels,
  playlists, comments, live chat, captions and transcripts, Shorts, notifications, and
  account actions (like, subscribe, comment, playlists) with imported cookies.
- **SABR streaming:** the server-driven protocol YouTube's web player uses in place of
  DASH and HLS. VOD with seeking, live with a live-edge join and DVR seeks, server-driven
  quality switching, dubbed audio tracks.
- **Attestation:** GVS PO tokens minted in-process by running BotGuard, or by an external
  bgutil server.
- **n-sig:** the streaming URL's `n` parameter solved in-process, with yt-dlp's `ejs`
  solver as the fallback.
- **Watch history:** the playback and watch-time reports that put a watch in an account's
  history.

## Build

You need Rust 1.95 or later and a C compiler (QuickJS is compiled from source). There is
no OpenSSL and no `protoc` dependency.

```sh
cargo build --release --workspace
```

- **Alpine and other musl systems:** `apk add build-base` (gcc and musl-dev).
- **glibc distributions:** install gcc or clang (for example `build-essential` on Debian
  and Ubuntu, `gcc` on Fedora).

## Usage

### Library

Depend on the `innertube` crate. It re-exports everything else:

```toml
innertube = { git = "https://github.com/moderntube/innertube-rs", rev = "<commit>" }
```

```rust
use innertube::{ClientConfig, NSigSolver, Playback, PlaybackOptions, Session};

let session = Session::new(ClientConfig::web())?;
let results = session.search("rust", None).await?;

let solver = NSigSolver::new();
let pot = innertube::pot_provider(Default::default(), innertube::WEB_USER_AGENT);
match Playback::open(&session, &solver, pot, "VIDEO_ID", &PlaybackOptions::default()).await? {
    Playback::Vod(vod) => { /* vod.track(true, 0).next_piece() ... */ }
    Playback::Live(live) => { /* live.segment_track(true) ... */ }
}
```

Build with `default-features = false` for the metadata API without streaming.
`DESIGN.md` explains the architecture.

### CLI

The `it` binary is a developer tool for checking the protocol against YouTube:

```sh
it smoke  <video_id>            # one SABR fetch that proves media flows
it play   <video_id>            # sustained loop, prints the realtime ratio
it stream <video_id> --play | mpv -
```

`it stream --play` muxes audio and video through `ffmpeg` into one Matroska stream on
stdout. Run `it --help` for every subcommand and flag.

## Testing

```sh
cargo test --workspace
```

The default suite is offline. Tests that contact YouTube are `#[ignore]`.

## Documentation

- [`DESIGN.md`](DESIGN.md): architecture.
- [`AGENTS.md`](AGENTS.md): rules for contributors and coding agents.
- [`docs/protocol/`](docs/protocol/README.md): the protocol facts the code relies on.

## License

MIT, see [`LICENSE`](LICENSE). Vendored code keeps its own license: the googlevideo
protobuf schemas (MIT) and yt-dlp's `ejs` solver (Unlicense).

## Disclaimer

This is an unofficial client. It is not affiliated with, endorsed by or supported by
YouTube or Google. YouTube is a trademark of Google LLC. Use it in line with the terms that
apply to you.
