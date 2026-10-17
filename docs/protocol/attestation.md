# PO tokens and attestation

The GVS PO token proves to googlevideo that a BotGuard attestation ran in a page. The
protection status it earns is in [`gates.md`](gates.md).

### POT-001 The GVS PO token travels in `StreamerContext` field 2

Each SABR request carries the token as raw bytes in `StreamerContext.po_token` (request field
19, sub-field 2): 85 bytes on most sessions, 83, 84 and 90 on others, and a 10-byte placeholder
on the first requests of some sessions (GATE-004). No SABR URL carries it (REQ-027). Minters
return it as base64url text, which the client decodes before sending.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18

### POT-002 `player()` and `get_watch` requests carry a PO token too

Every `/youtubei/v1/player` and `/youtubei/v1/get_watch` request in the captures carries
`serviceIntegrityDimensions.poToken` (108 of 108), including inline previews and Shorts.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-08-18

### POT-003 A token must be minted against the challenge and `ytcfg` of one page load

The BotGuard challenge a watch page embeds (`bgChallenge`) is bound to that page load's
`yt.config_`, in particular its `EVENT_ID`, which BotGuard reads from `window.yt.config_`
while it runs. A token minted from a fresh `/att/get` challenge, or from a challenge paired
with another page's config, is rejected: status 2 for a few rounds, then status 3 and no
media. Upstream reports of the same: LuanRT/BgUtils#44, Brainicism/bgutil-ytdlp-pot-provider
#242 and #188.

Status: unverified

### POT-004 One page funds the playbacks that follow it in the same session

The reference client never fetches a `/watch` or `/shorts/<id>` page for inline previews or
while swiping Shorts: in the signed-in walk capture the only HTML pages loaded are 4 home
pages, yet 66 `player()` calls and 8 `get_watch` calls each attest. On our sessions, a
challenge and `ytcfg` from any watch page loaded on the same `Session` mint tokens the server
accepts for other videos; POT-003 still requires the challenge and config to come from the same
page.

Status: verified: capture 2026-05-30

### POT-005 A page's attestation is still accepted 15 minutes later

Tokens minted against a saved watch page's challenge got status 1 at page ages of 30, 120,
300 and 900 s (32 rounds, no rejection). The corrupted-token control was not clean, so treat
900 s as a lower bound with weak evidence.

Status: unverified

### POT-006 The player mints a new content token when the server rejects one

`base.js` 9fc68080 publishes `spsumpreject` when a protection rejection arrives; the PO token
controller (`APT`) listens for it and mints a new content-bound token
(`html5_generate_content_po_token`) or a session token, unless a mint is already pending.

Status: verified: base.js 9fc68080

### POT-007 The reference player refreshes its token once per session

The reference client did not mint a new token on each status-2 round: it sat at status 2 for
several rounds with media flowing and never escalated. Our client minting a fresh token on
every status-2 round kept the server from accepting any of them. bgutil returns a different
token on every `/get_pot` call; against a bgutil whose cached integrity token was 20 hours old
every round was rejected, and invalidating the cached token recovered playback.

Status: unverified

### POT-008 The GVS token is bound to the video id

For the WEB client the GVS token's content binding is the video id, so each video needs its
own token. yt-dlp reports this experiment ("bind GVS PO Token to video ID").

Status: unverified

### POT-009 BotGuard runs through `att/get` and `att/log`

The page fetches a challenge with `POST /youtubei/v1/att/get` (`engagementType`, and on one
call `eacrToken` and `interpreterHash`) and submits results with `POST /youtubei/v1/att/log`
(`challenge`, `engagementType`, `ids`, `webResponse`). The signed-in walk capture has 2 and 17
of them.

Status: verified: capture 2026-05-30

### POT-010 One attestation mints tokens for many videos until it expires

A BotGuard integrity token from one attestation mints content tokens for any number of video
ids until its TTL runs out.

Status: unverified

### POT-011 Without a GVS token there is no SABR for live

Clients that need no GVS token (`android_vr`, `tv`) are served HLS only for live, and the WEB
client without a token falls back to HLS as well. SABR live requires the WEB client and a GVS
PO token.

Status: unverified
