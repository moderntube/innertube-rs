# AGENTS.md

Rules for anyone changing this repository, human or coding agent. `DESIGN.md` describes
the architecture. `docs/protocol/` holds the protocol facts the code relies on.

## North star: behave like YouTube's web client

Fetch and handle everything the way the WEB client does: request shapes and fields,
cold start, which quality rung, when to switch, behaviour at the edges. A mechanism that
exists only in this codebase is a defect to retire, even when it works.

The one sanctioned divergence is the display substrate. YouTube appends segments to an
MSE `SourceBuffer`. This library hands complete segments (or, for the CLI, a byte stream)
to a consumer that is not a browser. Code that exists for that reason says so.

Fix the protocol properly. A workaround that hides a problem (playing far behind the live
edge, capping quality, retrying until it works) does not count as a fix.

## Ground truth

- **The protocol register** in `docs/protocol/`: each fact has an id (`SABR-011`), a
  status and its evidence. Code comments cite facts by id. Add a fact there before you
  depend on it. When a capture or a newer player build contradicts a fact, update its text
  and status in the same change.
- **Captures of the reference client.** mitmproxy captures hold session tokens and
  account data, so they are not published. Keep your own under `dumps/` (gitignored).
  Published, scrubbed fixtures live in `notes/fixtures/` and
  `crates/innertube-sabr/tests/fixtures/`.
- **The player source** (`base.js`). Read it when a capture shows what happens but not
  why. Name the player build when you cite it, since minified names rotate.
- **Upstream work.** yt-dlp, LuanRT/googlevideo, LuanRT/BgUtils and public write-ups often
  have the answer already. Look there first.

## Method

1. **Find candidates in evidence, then test them.** Decode the capture and read the
   source before you change code. A measurement needs a control arm, alternated order and
   more than one video.
2. **Diff before theorising.** List the reference request sequence, gaps included, and
   compare it with ours.
3. **Check local state first.** A consumer can carry resume positions, a stored quality
   and a cached session across runs. Control for them before you blame the server.
4. **Control every variable.** Two videos opened in a fixed order cannot separate
   "second open" from "that video". Swap the order.
5. **A plausible story is a checkpoint.** Check the log ordering before you claim a
   cause.
6. **The unit of analysis is the session.** A session's configuration is fixed when
   `player()` answers, so many requests from one session count as one sample.
7. **Pick test videos from data.** Choose by a predicate read from a dumped response (for
   example `len(adPlacements) == 0`). A title says nothing reliable.
8. **Keep live traffic small.** Use a signed-out profile and a network path you can afford
   to have rate-limited. Space opens out; bursts from one address get throttled.
9. **Test the binary you built.** A harness that runs `target/release` needs a release
   build of your change.
10. **Never print secrets.** Dumps, cookies and PO tokens stay out of logs, issues and
    commits. Avoid commands that print a credential by default.

## Workspace

| Crate | Role |
|---|---|
| `innertube` | The crate an application links: `Session` and the InnerTube API, the playback bootstrap, `SegmentVod`, `SabrStream`, `Playback` |
| `innertube-sabr` | UMP codec, SABR protobufs (vendored, compiled by `protox`), `SabrEngine`, `LiveLane`, the bandwidth estimator |
| `innertube-pot` | `PoTokenProvider`: in-process BotGuard (`NativeProvider`, default) or an external bgutil HTTP server |
| `innertube-js` | n-sig solver: a native pass, with yt-dlp's `ejs` run in-process through `rquickjs` |
| `innertube-cli` | The `it` developer CLI and probes |

## Build and test

```sh
cargo build --workspace
cargo test --workspace                                # offline; network tests are #[ignore]
cargo clippy --workspace --all-targets -- -D warnings # must be clean
```

- Rust 1.95, edition 2021. TLS is rustls with ring; no OpenSSL.
- No `protoc`: `crates/innertube-sabr/build.rs` compiles the vendored protos with `protox`.
- `innertube-js` builds vendored QuickJS C, so a C compiler is needed. QuickJS is
  compiled at opt-level 3 in dev builds, or the n-sig solve is several times slower.
- Network tests run with `cargo test -- --ignored`, under rule 8 above.

## Debugging

`INNERTUBE_DUMP=<dir>` writes scrubbed request metadata as `.jsonl`.
`INNERTUBE_DUMP=raw:<dir>` adds raw SABR bodies and player responses under `<dir>/raw/`.
Raw dumps contain the PO token and session blobs: keep them outside the repository and
delete them when done.

Compare requests at the wire level. A schema decode drops unmodelled fields, which is
where an unexplained difference hides.

## Conventions

- **Comments** follow `docs/comment-style.md`. **Tests** follow `docs/test-style.md`.
- Test-driven for non-trivial logic. Engine behaviour is tested by replaying captured
  sessions (`crates/innertube-sabr/tests/replay.rs`). Internal engine functions get no
  unit tests of their own.
- Fixtures are scrubbed: no PO tokens, signatures, cookies, IP addresses or
  `visitorData`.
- Conventional commits, one kind of change per commit.
- Vendored code keeps its license: `crates/innertube-sabr/proto/` (googlevideo, MIT) and
  `crates/innertube-js/vendor/` (yt-dlp ejs, Unlicense). bgutil is GPL-3.0: talk to it
  over HTTP only.
