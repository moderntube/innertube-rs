# Protocol fact register

This directory holds the protocol facts the code depends on: how YouTube's WEB player
talks to InnerTube and to googlevideo over SABR/UMP, and how the server answers. Code
comments cite a fact by its id (`// SABR-011`), and the evidence lives here.

## Files

| File | Prefix | Covers |
|---|---|---|
| [`ump.md`](ump.md) | `UMP` | UMP framing, part types, `MEDIA_HEADER`, segments and containers |
| [`request-fields.md`](request-fields.md) | `REQ` | the `VideoPlaybackAbrRequest` body, its URL and headers |
| [`sabr-rounds.md`](sabr-rounds.md) | `SABR` | round behaviour: policy, live join, seeks, redirects, ABR, the player's request gate |
| [`gates.md`](gates.md) | `GATE` | `STREAM_PROTECTION_STATUS`, the content-ads hold, rejected requests |
| [`attestation.md`](attestation.md) | `POT` | PO tokens and the BotGuard attestation they come from |
| [`player-js.md`](player-js.md) | `PLAYER` | `base.js`: n-sig, `signatureTimestamp`, the bandwidth estimator, playback nonces |
| [`innertube.md`](innertube.md) | `IT` | `player()`, `get_watch`, page bootstrap, Shorts and the JSON endpoints |

## A fact

```
### SABR-011 A client answers SABR_SEEK at the target plus 100 ms

What happens, with units and the conditions it was seen under.

Status: verified: capture 2026-05-29
Fixture: crates/innertube-sabr/tests/fixtures/sessions/live-join.replay
```

The heading is the fact in one line. The body says what the server or the reference
client does, in which units, and when it was observed. Field numbers go with their names:
`ClientAbrState` field 28 (`player_time_ms`).

## Ids

- An id is a prefix and a three-digit number. Numbers are assigned in order within a prefix.
- An id is never reused or renumbered. A fact that turns out wrong is rewritten with the
  corrected statement under the same id, or marked `withdrawn` with the reason, so a comment
  that cites it still lands on the current truth. Gaps in the numbering are expected.
- A new fact takes the next free number in its file.

## Status

| Status | Meaning |
|---|---|
| `verified: capture <date>` | Checked against a mitmproxy capture of the reference web client taken on that date. |
| `verified: base.js <build>` | Read in the player source of that build. |
| `unverified` | From an earlier write-up, a measurement of this client or an upstream source. No capture or player source has confirmed it. |

A fact can carry two sources, for example `verified: capture 2026-07-16, base.js 9fc68080`.

The captures are private: they hold session tokens, cookies and account data, so they are
not published. Where a scrubbed fixture of the same traffic is published, the fact links
it.

Capture periods used here:

| Label | Content |
|---|---|
| capture 2026-05-29 | Live and VOD browsing (WEB `2.20260528.01.00`), two live streams in one session, a live stream with manual quality changes and a DVR seek, and a signed-in session |
| capture 2026-05-30 | A signed-in walk of the whole site: home, watch pages, inline previews, Shorts, live |
| capture 2026-07-04 | Hover previews on the home feed |
| capture 2026-07-16 | VOD auto quality under a bandwidth throttle, pure auto (`sticky_resolution=0`), and seeks |
| capture 2026-08-18 | Several hover previews and watch opens on one session |

Player source: `base.js` build `9fc68080` (`player_es6`), served during May 2026.

## Published fixtures

- `crates/innertube-sabr/tests/fixtures/sessions/live-join.replay`: a live join on the main
  player, capture 2026-05-29.
- `crates/innertube-sabr/tests/fixtures/sessions/vod-open-seek.replay`: a VOD open with an
  ABR climb and a seek, capture 2026-07-16.
- `crates/innertube-sabr/tests/fixtures/sessions/vod-content-ads-hold.replay`: a VOD whose
  first round is the content-ads hold, capture 2026-05-29.
- `notes/fixtures/`: scrubbed `player()` responses (live and VOD), a 98-byte metadata-only UMP
  response, a `MEDIA_HEADER` payload, and request and response structure tables.

The replay fixtures keep request and response structure and replace media bytes and session
blobs with filler (`scripts/extract_replay_session.py`).
