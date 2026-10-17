# Gates: protection status and held rounds

What makes the server withhold media, and how it says so. PO-token minting is in
[`attestation.md`](attestation.md).

### GATE-001 `STREAM_PROTECTION_STATUS` reports the attestation state each round

Part 58 is `{1: status, 2: max_retries}`. Status 1 means the request is attested; it arrives
on every healthy round, encoded `08 01`. Status 2 arrives on the first rounds of a session
whose PO token is a 10-byte placeholder (GATE-004), while media still flows. Status 3, with
`max_retries`, is GATE-003. A round held for content ads carries no part 58 at all (GATE-005).

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18
Fixture: `notes/fixtures/sabr-response-metadata.ump`

### GATE-002 No round carries media without `STREAM_PROTECTION_STATUS`

Every response with a `MEDIA_HEADER` also carries part 58 (712 of 712: 709 with status 1, 3
with status 2). Rounds without part 58 carry no media.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18

### GATE-003 Status 3 withholds all media until the session attests again

A response with `{1: 3, 2: 10}` carries no media, and every later round of the session gets
the same until it presents a token the server accepts. Minting new tokens from the same
rejected attestation does not clear it. Seen only on our own sessions. A second session for
the same video, opened about 180 ms after the first, gets status 2, then 3 and no media.

Status: unverified

### GATE-004 The reference's first requests carry a 10-byte placeholder token

On a fresh playback the reference client sent a 10-byte `po_token` for its first one or two
requests and the 85-byte token after that. With the placeholder the server answered status 2
and still served media in five sessions; in a sixth it answered status 1. From the full token
on, every round was status 1.

Status: verified: capture 2026-07-16, 2026-08-18
Fixture: `crates/innertube-sabr/tests/fixtures/sessions/vod-open-seek.replay`

### GATE-005 The content-ads hold: round 0 is `[35, 57, 67]` with no media

On some opens the first response is 105 bytes: `SABR_CONTEXT_UPDATE` (type 5, scope 4 =
`CONTENT_ADS`), `SNACKBAR_MESSAGE` (`08 01`) and `NEXT_REQUEST_POLICY` with only a backoff
(4000, 8800 or 12000 ms) and an empty playback cookie. There is no `STREAM_PROTECTION_STATUS`
and no media. The reference waited out the backoff and sent one request 4.4 to 12.7 s later,
which got media. 9 of the 124 playback sessions in four captures opened this way.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-08-18
Fixture: `crates/innertube-sabr/tests/fixtures/sessions/vod-content-ads-hold.replay`

### GATE-006 Every held session's ustreamer config carries field 1.25

In the `videoPlaybackUstreamerConfig` (request field 5), sub-message 1 field 25 is present in
all 9 sessions whose round 0 was held. It is also present in 3 sessions that got media on
round 0; those rounds carried part 57 as well. The config comes from the player response, so
the hold is decided when `player()` answers.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-08-18

### GATE-007 The player ignores the backoff only while client-side ad insertion runs

When a policy arrives, `base.js` 9fc68080 logs `clsdai` with `ignbckoff` and keeps requesting
if `policy.FK` is set and its client-side DAI controller is active; otherwise it sets the
backoff deadline (SABR-003). A player without an ad controller (ads pruned by a blocker) waits
the hold out.

Status: verified: base.js 9fc68080

### GATE-008 Field 25 is set per `player()` request and needs `visitorData`

From our dumps: the same video got configs with and without field 25 seconds apart, so it is
assigned per `player()` request. Removing `visitorData` from the request removed it (2 of 2,
on two videos); removing the PO token or the `ytcfg` client config did not. Field 25 appears
only on responses whose `adPlacements` is non-empty. Stripping field 25 from the config makes
the server answer `sabr.malformed_config` with no media: the config is validated.

Status: unverified

### GATE-009 The hold follows a visitor that has loaded the homepage

A fresh visitor opening a watch page directly got media on round 0 (0 of 4 held); the same
kind of visitor after loading `https://www.youtube.com/` first was held 5 of 5 times
(3.8 to 15.8 s announced). Hover previews on a homepage-loaded visitor were not held; the
hold applies to watch playback.

Status: unverified

### GATE-010 An ad blocker does not avoid the hold; it only removes the ad

In a Chromium with uBlock Origin, the first watch round was the same 105-byte hold as without
it. Without the blocker the ad played on its own SABR stream (its own ustreamer config) and got
media 154 ms after the held round; with it, nothing played during the hold.

Status: unverified

### GATE-011 `isInlinePlaybackNoAd` does not lift the hold for our sessions

Adding `isInlinePlaybackNoAd` to `contentPlaybackContext` left the hold unchanged (about 3.8 s,
five runs).

Status: unverified

### GATE-012 Ad-blocker filter authors describe the hold as "fake buffering"

Filter lists identify parts 35, 57 and 67, match the 105-byte first response, and report a
backoff of about 80 % of the ad's duration that the server checks (uBlockOrigin/uAssets#27415
and #30157, and `iter.ca/post/yt-adblock/`).

Status: unverified

### GATE-013 Some videos do not enforce attestation

On some videos a corrupted PO token still gets status 1 and media, so a video
can look healthy whatever the token. A health check needs a video that enforces it.

Status: unverified

### GATE-014 The ANDROID client with a web token is cut off after about 35 s

ANDROID `player()` sessions using a web BotGuard token report status 2, then status 3 about
35 s in, and get no more media; new web tokens do not restore it. The WEB client with a
page-bound token stays at status 1 for the whole session.

Status: unverified
