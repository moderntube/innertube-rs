# The player JS (`base.js`)

What the client takes from YouTube's player script and how the script computes the values
the requests carry. Build `9fc68080` (`player_es6`, May 2026) is the reference unless a fact
says otherwise; minified names rotate between builds.

### PLAYER-001 The player script lives at `/s/player/<build>/…/base.js`

The watch page and `ytcfg` (`PLAYER_JS_URL`) name the player script as
`/s/player/<build>/player_es6.vflset/en_US/base.js`. The build id is content-addressed, so a
fetched script never changes under its URL. YouTube serves several builds at once (A/B).

Status: verified: capture 2026-05-30

### PLAYER-002 `player_es6` and `player_ias` are different scripts under one build id

Under one build id, `player_es6.vflset` and `player_ias.vflset` are different files (2.56 MB
and 2.88 MB). Which one a page names depends on the requester: a browser-like WEB session
gets `player_es6`, a bare `curl` gets `player_ias`. The `signatureTimestamp` and the n-sig
function must come from the script the page names.

Status: unverified

### PLAYER-003 `signatureTimestamp` is a constant in the player script

`base.js` 9fc68080 contains `signatureTimestamp:20594`, and every `player()` and `get_watch`
request in the May capture sends `playbackContext.contentPlaybackContext.signatureTimestamp`
20594.

Status: verified: capture 2026-05-29, base.js 9fc68080

### PLAYER-004 A `signatureTimestamp` from another build gets the streaming URL refused

When the `player()` request's `signatureTimestamp` comes from a different player build than
the one used to descramble `n`, the streaming URL is signed for the other build and POSTs to
it fail with HTTP 403.

Status: unverified

### PLAYER-005 The streaming URL's `n` must be descrambled before use

The `n` parameter of `serverAbrStreamingUrl` is transformed by a function in the player script
before the first POST: the `n` the player POSTs differs from the player response's value in 396
of 396 requests. A URL with the untransformed `n` is refused (throttled or HTTP 403).
`SABR_REDIRECT` URLs arrive already transformed (SABR-036).

Status: verified: capture 2026-05-30

### PLAYER-006 The streaming URL's `n` is fixed for a session, so one solve serves it

A session's streaming URL keeps one `n` value for all its requests, so the transform runs once
per URL. A new URL (after a `RELOAD_PLAYER_RESPONSE` or a new `player()` call) needs a new
solve.

Status: unverified

### PLAYER-007 The playback nonce `cpn` is 16 characters from a 64-character alphabet

`base.js` `Wox` draws 16 random bytes and maps each `& 63` into
`A-Z a-z 0-9 - _`. Every `cpn` in the captures is 16 characters from that set, and each player
instance (main player, inline preview, Shorts preload) has its own.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18, base.js 9fc68080

### PLAYER-008 The bandwidth estimate is the larger of a long and a short throughput window

`base.js` keeps a long ring `G = wz(17, 20, .5, .4)` (17 s at 20 slots per second, read at the
mean of the 10th, 50th and 90th percentiles) and a short ring `WY = wz(5, 1, .25)` (5 s, 25th
percentile). Each completed download adds `bytes / elapsed` weighted by its elapsed time. The
advertised value `Kb` is the larger of the long-window rate (corrected by a stall term) and the
short-window rate, never below 1 B/s (`Lg`). The short window lets the estimate recover within
seconds after a slow spell.

Status: verified: base.js 9fc68080

### PLAYER-009 The estimate starts from the stored byterate or `navigator.connection.downlink`

At start the long ring is seeded from the `yt-player-bandwidth` local-storage entry's
`byterate` (written for 30 days), else from `navigator.connection.downlink * 64 * 1024`
(downlink in Mbit/s). The cold first requests of the July captures advertised 95,027 and
104,857 B/s, which are downlinks of 1.45 and 1.6 Mbit/s through that formula.

Status: verified: capture 2026-07-16, base.js 9fc68080

### PLAYER-010 A manual quality pick is stored in `yt-player-quality`

The quality menu writes `{quality, previousQuality, creation}` to the `yt-player-quality`
local-storage entry, and the request builder reads it on every request (REQ-018), with an
optional expiry. Opening a video while a pick is stored sends the manual-pick fields from the
first request.

Status: verified: base.js 9fc68080

### PLAYER-011 The n-sig solver fails on some player builds

The yt-dlp `ejs` solver occasionally fails on one of the builds YouTube serves; a fresh watch
page fetch may name a build it handles.

Status: unverified

### PLAYER-012 The player reconfigures its `SourceBuffer` only on a codec or container change

`base.js` calls `SourceBuffer.changeType` when the next format's container type or codec
differs from the current one. A rung change within one codec (VP9 to VP9) appends the new
format's init and media to the same buffer.

Status: verified: base.js 9fc68080
