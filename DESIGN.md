# Design

innertube-rs is a Rust client for YouTube's private InnerTube API and its SABR/UMP
streaming protocol. It aims to behave like YouTube's WEB client on the wire: the same
requests, the same fields, the same reactions to the server. This document describes how
the code is organised. Protocol facts live in the register under
[`docs/protocol/`](docs/protocol/README.md); this document cites them by id.

## Crates

```
innertube ──► innertube-sabr     (UMP, SABR protobufs, engine)
    │    ──► innertube-pot      (PO tokens)
    │    ──► innertube-js       (n-sig)
innertube-cli ──► innertube
```

| Crate | Role |
|---|---|
| `innertube` | The crate an application depends on. `Session` and the InnerTube API, the playback bootstrap, and the playback sessions `SegmentVod`, `SabrStream` and `Playback`. Streaming is the default-on `streaming` feature; without it the crate is the metadata API alone. |
| `innertube-sabr` | The protocol, with no networking: the UMP codec, the SABR protobufs, `SabrEngine`, `LiveLane`, the bandwidth estimator and the blocking `MediaReader`. |
| `innertube-pot` | The `PoTokenProvider` trait and two providers: `NativeProvider` (BotGuard in-process through `botguard-rs`, the default) and `BgUtilProvider` (an external bgutil HTTP server). |
| `innertube-js` | `NSigSolver`: descrambles the streaming URL's `n` parameter. A native pass parses `base.js` with `oxc`; yt-dlp's `ejs` solver, run in QuickJS through `rquickjs`, is the fallback. |
| `innertube-cli` | The `it` developer CLI and a few probes. |

`innertube-sabr`, `innertube-pot` and `innertube-js` do not depend on each other.
`innertube` composes them.

## Session

`Session` (`crates/innertube/src/client/session.rs`) is one browser-like WEB client: a
`reqwest` client with a cookie jar, the WEB `ClientConfig` (`client/config.rs`), and the
caches that a browser tab would hold (`visitorData`, the last watch page). Every InnerTube
call is a POST of the WEB context plus the endpoint's body (IT-001).

- `Session::new(ClientConfig::web())` is anonymous. `Session::authenticated` adds
  `Credentials`: a cookie header from which each request computes `SAPISIDHASH` (IT-004).
  The library never reads a browser profile; the consumer supplies the cookies.
- Connections are IPv4 only, because a black-holed IPv6 path stalls the first POST
  (IT-021).
- The metadata API is methods on `Session` in `client/api/`: search, `next`, browse
  (home, subscriptions, channels, playlists), comments, live chat, captions and
  transcripts, Shorts (`client/reel.rs`), notifications and account actions. Each returns
  a typed model; paging uses continuation tokens (IT-023).

## Playback bootstrap

`bootstrap.rs` opens a video the way a watch page does, and both VOD and live start here:

1. Fetch the watch page. It yields `base.js`'s URL, `visitorData` (IT-003), the
   server-rendered player response (IT-005) and the BotGuard challenge with its `ytcfg`.
   A navigation inside the app uses `get_watch` instead (IT-006). Shorts and previews use
   a `player()` POST (IT-007).
2. Fetch `base.js` before `player()`, so the request's `signatureTimestamp` matches the
   build that will solve `n` (PLAYER-003, PLAYER-004).
3. Mint the PO token against that same page's challenge and `ytcfg` (POT-003). The
   `player()` and `get_watch` requests carry it too (POT-002).
4. Descramble the SABR URL's `n` once per session (PLAYER-005, PLAYER-006) and add the
   query parameters the player adds (REQ-026).

The bootstrap retries with a fresh page when the n-sig solver fails on a build
(PLAYER-011). `prewarm_session` does the video-independent part (the `base.js` fetch and
the first solve) before the first click.

`Playback::open` runs the bootstrap once and returns a `SegmentVod` or a `SabrStream`,
depending on whether the player response is live.

## The SABR engine

`SabrEngine` (`crates/innertube-sabr/src/engine/`) holds one SABR session's client
state. It does no networking. The caller runs the loop:

```
loop {
    body = engine.request_bytes()      // VideoPlaybackAbrRequest
    resp = session.post_sabr(url, body)
    out  = engine.ingest(resp)         // UMP parts -> segments, directives, policy
}
```

- `request.rs` builds the request: `ClientAbrState`, the preferred formats, the buffered
  ranges built from the `MEDIA_HEADER`s held (REQ-008), and `StreamerContext` with the PO
  token, the playback cookie and the SABR contexts (REQ-024).
- `ingest.rs` parses UMP (UMP-001) and handles each part: media and its headers,
  `NEXT_REQUEST_POLICY` (SABR-001), `SABR_SEEK` (SABR-009), `SABR_REDIRECT` (SABR-036),
  `RELOAD_PLAYER_RESPONSE` (SABR-037), `STREAM_PROTECTION_STATUS` (GATE-001),
  `LIVE_METADATA` (SABR-014) and context updates (SABR-040).
- `gate.rs` decides when the next request may go: the server's backoff as a deadline
  (SABR-003) and the reference player's readahead targets (SABR-019 to SABR-023).
- `bandwidth.rs` ports the player's throughput estimator (PLAYER-008). The last
  session's estimate is stored in the cache directory, as the player stores it in
  `localStorage` (PLAYER-009).

The server picks the rung (SABR-031, SABR-049). The engine reports what it holds and
acknowledges whatever itags arrive.

`tests/replay.rs` feeds the engine captured reference sessions and compares each request
it builds with the one the web client sent. That is the main check on engine behaviour.

## Playback sessions

**VOD: `SegmentVod`** (`segment_vod.rs`). A task runs the engine loop and stores complete
segments by `(itag, sequence_number)`, kept across seeks and evicted far from the playhead
above a byte cap. The consumer reads each track through a `SegmentTrack` cursor that yields
`SegmentPiece`s in time order: one segment, led by its rung's init when the rung changed or
the cursor moved. A seek repositions
the cursor. The engine then reports the stored runs as buffered ranges, so the server
continues from the end of the run that holds the playhead (REQ-010, SABR-050).

**Live: `SabrStream`** (`live/`). `open` bootstraps, picks formats (`select.rs`) and starts
`run_loop`, which posts, merges the round through `LiveLane`, follows `SABR_SEEK`,
redirects and reloads, re-attests through the budget in `attest.rs`, and paces requests.
Complete segments go to `LiveSegments`, keyed by sequence number. A rung change is the same
sequence number under another itag (`live_segments.rs`). The consumer reads them through
`SabrStream::segment_track`. The live head and DVR window come from `LIVE_METADATA`. A live
session also runs the `player/heartbeat` task (IT-014).

**Byte stream.** `SabrStream::video` and `audio` return blocking `MediaReader`s over the
same live tracks, each led by its container init (UMP-010, UMP-012). The CLI pipes these
to ffmpeg. The reader is blocking so a non-async media thread can pull from it. Dropping
the last reader stops the session.

Around these: `captions.rs` (timedtext for the playing session), `client/storyboard.rs`
(seek thumbnails), `watch_report.rs` (the `/api/stats/playback` and `/watchtime` reports
that put a watch in history, IT-017) and `audio_window.rs` (a bounded audio-only fetch of a
live DVR range).

## The display substrate

The one sanctioned difference from the web client is what consumes the media. The web
player appends segments to an MSE `SourceBuffer`, which orders them by timestamp and
tolerates overlapping appends. This library hands complete, self-contained segments to the
consumer in time order. Ordering, overlap between rungs and container re-initialisation are
then the consumer's job. The moderntube app does it with GStreamer. Code that exists only
because the consumer is not MSE is marked as such.

## PO tokens

The GVS PO token proves a BotGuard attestation. It travels in `StreamerContext` field 2
(POT-001) and is bound to the video id (POT-008). `NativeProvider` runs BotGuard
in-process. One attestation, minted against the session's page challenge, serves many
videos until its TTL (POT-010). `BgUtilProvider` asks an external bgutil server, which is
GPL-3.0 and therefore stays behind HTTP.

`STREAM_PROTECTION_STATUS` reports the result each round. Status 3 withholds all media
(GATE-003). The player refreshes its token once per session (POT-007), so `attest.rs`
allows one soft refresh and, at status 3, one hard re-attest that discards the provider's
cached attestation.

## n-sig

`NSigSolver::solve_n(base_js, challenges)` transforms `n` values for a player build. Both
paths locate the transform structurally in `base.js` and run YouTube's own function in
QuickJS. The native pass parses `base.js` with `oxc`. When it declines or fails, the
vendored `ejs` solver parses it in JavaScript, which is much slower; its preprocessed
players are cached on disk under the user cache directory. The evaluated player stays
resident, so later solves for the same build are cheap. `SABR_REDIRECT` URLs arrive already descrambled (SABR-036).

## Errors and logging

`innertube::Error` covers InnerTube calls; `StreamError` covers opening and running a
playback session, and `StreamError::kind` sorts failures into network, refused,
unavailable and internal. The library logs through `tracing` and installs no subscriber.
`INNERTUBE_DUMP` is the only environment variable it reads: it writes scrubbed request
metadata, or raw bodies with the `raw:` prefix (`client/dump.rs`).

## Further reading

- [`docs/protocol/`](docs/protocol/README.md): the fact register.
- `notes/fixtures/`: scrubbed captured responses the tests read.
- [`docs/comment-style.md`](docs/comment-style.md) and
  [`docs/test-style.md`](docs/test-style.md).
