# SABR rounds

A round is one request and its response. This file covers what the server sends back and
how the reference player paces and positions its requests: the request policy, live joins,
seeks, redirects, server-driven quality, and the request gate in `base.js`.

## Request policy and pacing

### SABR-001 `NEXT_REQUEST_POLICY` carries readahead targets, a backoff and the playback cookie

Part 35 fields: 1 `target_audio_readahead_ms`, 2 `target_video_readahead_ms`, 3
`max_time_since_last_request_ms`, 4 `backoff_time_ms`, 7 the playback cookie, 8 `video_id`.
Field 3 is 60000 whenever present. The targets start near 15000 ms on a session's first
response and grow round by round, up to 120000 ms. `base.js` reads fields 1, 2, 4, 7 and 8.
Fields 5, 6 (minimum readahead) and 8 do not appear in the captures.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18, base.js 9fc68080
Fixture: `notes/fixtures/sabr-response-metadata.ump`

### SABR-002 The backoff values the server announces

`backoff_time_ms` is absent from rounds that carry media in steady state. Values seen: 2000
on policy-only rounds when the client already holds enough (VOD and Shorts), 5000 on the
answer to a live-edge request, and 4000, 8800 and 12000 on the content-ads hold (GATE-005).

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18

### SABR-003 The player treats the backoff as a deadline

`base.js` stores `now + backoffTimeMs` when a policy arrives and declines to send any request
until that instant. A response without a new policy leaves the deadline as it was. The one
exception is GATE-007.

Status: verified: base.js 9fc68080

### SABR-004 A request before the deadline earns the remainder of the same hold

During a content-ads hold, our request sent before the announced backoff ran out got a new
policy announcing the time still left (3892 ms announced, request 2000 ms later, then 1698 ms
announced). The hold is a deadline on the server too; requesting early does not shorten it.

Status: unverified

### SABR-005 "Nothing more for now" is a policy-only round with a 2000 ms backoff

When a VOD or Shorts player already holds what the server will send, the response is
110 to 215 bytes: `NEXT_REQUEST_POLICY` with `backoff_time_ms` 2000, `PLAYBACK_START_POLICY`,
`REQUEST_IDENTIFIER`, `REQUEST_CANCELLATION_POLICY` and `STREAM_PROTECTION_STATUS`, no media.
It is the normal idle answer; the content-ads hold (GATE-005) has a different shape.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18

### SABR-006 Each player has at most one request in flight

Requests of one playback nonce never overlap in time (0 overlaps in 465 consecutive pairs
across four captures); two concurrent requests always belong to two players. `base.js`
blocks a new request while one is in flight (unless a pipelining policy allows a second),
before the backoff deadline (SABR-003), and above a per-10-second request budget, past which
it raises `toomanyrequests`.

Status: verified: capture 2026-05-29, 2026-07-16, base.js 9fc68080

### SABR-007 The "edge and cushion lanes" in the captures are two different players

The pairs of concurrent live sessions on one video in the May captures belong to two player
instances, each with its own playback nonce (`cpn`) and playlist id. The high-resolution one
is the main watch-page player: `visibility` 0, viewport 1333x750, field 72 `3: 0, 4: 0`,
unmuted. The low-resolution one is a muted autoplay inline player: `visibility` 5, viewport
640x360, field 72 `3: 480, 4: 720`, and its stats reports say `muted=1`, `autoplay=1`,
`state=paused`. The web player does not run a second, low-resolution lane for one playback.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-08-18

### SABR-008 A paused player keeps buffering up to the readahead target

After its first seconds, the paused inline player's `player_time_ms` stays at one value while
its buffered range grows round after round, to tens of seconds. That is the request gate
(SABR-019) at work: the playhead does not move, so buffered-ahead keeps rising until it meets
the target.

Status: verified: capture 2026-05-29

## Live join and seeks

### SABR-009 A live join asks for `2^53 - 1` and the server answers with a `SABR_SEEK`

The first request of a live player sets `player_time_ms` to `9007199254740991`
(`Number.MAX_SAFE_INTEGER`, the player's `seekToLiveHead` to `Infinity`) and sends no
buffered ranges. The response carries a `SABR_SEEK` with `seek_source` 10
(`SABR_SEEK_TO_HEAD`) naming the edge position, plus `LIVE_METADATA` and a policy. On one
stream that response had no media (543 bytes, backoff 5000 ms); on another it already carried
the first segments of both tracks. The next request asks at the seek target (SABR-011).

Status: verified: capture 2026-05-29, base.js 9fc68080
Fixture: `crates/innertube-sabr/tests/fixtures/sessions/live-join.replay`

### SABR-010 `SABR_SEEK` gives the target as ticks and a timescale

Part 45 fields: 1 `seek_media_time` in ticks, 2 `seek_media_timescale` (ticks per second:
1000000000, 90000 and 44100 seen), 3 `seek_source` (10 `SABR_SEEK_TO_HEAD` on a live join, 11
`SABR_LIVE_DVR_USER_SEEK` after a DVR seek or a quality change). The target in milliseconds
is `ticks * 1000 / timescale`.

Status: verified: capture 2026-05-29

### SABR-011 A client answers `SABR_SEEK` at the target plus 100 ms

The request after a `SABR_SEEK` sets `player_time_ms` to the seek target plus 100 or 101 ms
(24 of 28 cases). In two cases the target was 0 and the next request asked for 0; in two the
inline player repeated its edge request first. The player seeks its media element to the
target plus 0.1 s and reports the new position. Later requests advance from there; the
target is not requested again.

Status: verified: capture 2026-05-29, 2026-05-30
Fixture: `crates/innertube-sabr/tests/fixtures/sessions/live-join.replay`

### SABR-012 The server answers a manual quality pick with a `SABR_SEEK`

After a manual pick (REQ-018, empty buffered ranges) the server answered with `SABR_SEEK`
source 11 to the start of the segment holding `player_time_ms`, and delivered both tracks
from that segment, the video in the new rung. Sequence numbers restart at that segment,
earlier than the last segment held.

Status: verified: capture 2026-05-29

### SABR-013 A live DVR seek keeps the held ranges and is answered with a `SABR_SEEK`

A seek back into the live DVR window sets `player_time_ms` to the target and keeps reporting
the ranges near the edge; the server answers with `SABR_SEEK` source 11 and media from the
target. After that the request reports both islands.

Status: verified: capture 2026-05-29

### SABR-014 `LIVE_METADATA` gives the head and the seekable window

Part 31 fields: 3 `head_sequence_number`, 4 `head_time_ms` (stream time), 5 `wall_time_ms`
(server wall clock, ms since the Unix epoch), 10 `headm` (1, on edge-join answers only),
12 and 13 `min_seekable_time_ticks` and its timescale, 14 and 15 `max_seekable_time_ticks`
and its timescale. Live responses carry it on every round.

Status: verified: capture 2026-05-29, 2026-05-30

### SABR-015 `max_seekable` trails the head by one or two segments

`head_time_ms - max_seekable` was 1000 ms on every round of six streams with 1 s segments and
4000 ms on a stream with 2 s segments.

Status: verified: capture 2026-05-29, 2026-05-30

### SABR-016 On a 5 s-segment normal-latency stream `max_seekable` trails the head by 10 s

Measured on our sessions: `head_time_ms - max_seekable` was 10000 ms, constant over 190
rounds.

Status: unverified

### SABR-017 On SABR live the request ceiling is the server's `max_seekable`

`base.js` `FX()` returns `videoData.A.rN` when `policy.wT` is set, and
`max(head - Kl, seekable start)` otherwise. `policy.wT` is set for SABR streams under the
`html5_sabr_parse_live_metadata_playback_boundaries` experiment, and `rN` is
`LIVE_METADATA` field 14 divided by field 15. So the furthest live position the player asks
for is `max_seekable`, taken from the server.

Status: verified: base.js 9fc68080

### SABR-018 Live segment duration is per stream and given by `targetDurationSec`

The live formats' `targetDurationSec` in the player response is the segment duration: 1 s,
2 s and 5 s in the captures. The gap between consecutive `MEDIA_HEADER.start_ms` values
matches it (about 1000 ms, with 998 to 1022 ms jitter, on 1 s streams).

Status: verified: capture 2026-05-29, 2026-05-30

## The player's request gate

### SABR-019 The player requests while either track's buffered-ahead is under its target

`base.js` `Yyp` decides each request. With no readahead target yet it requests. Otherwise
`B = min(min(kV(audio) * 1000, targetAudioReadaheadMs), min(kV(video) * 1000,
targetVideoReadaheadMs))`, and it requests while the audio or the video buffered-ahead is
below `B`, else logs `readaheadmet` and waits. The server's targets therefore cap the
buffer from above; they are never a minimum the player must reach. Buffered-ahead counts
contiguous ranges from the playhead and stops at the first gap (`by`).

Status: verified: base.js 9fc68080

### SABR-020 `kV` is a byte budget divided by the track's bitrate, capped at 120 s

`kV(track) = (audio ? 8 MiB : 20 MiB) / (byterate * yd)` seconds, capped at `AC`. The defaults
in the policy class `g.Gkp` are `Om = 20971520` (video bytes), `yg = 8388608` (audio bytes),
`yd = 1`, `AC = 120`. `kV` returns 3, 1 or 4 instead in three special loader states. A
high-bitrate track therefore buys fewer seconds of readahead than a low-bitrate one.

Status: verified: base.js 9fc68080

### SABR-021 `dynamicReadaheadConfig` sets the minimum, growth and maximum readahead

The player response's `playerConfig.mediaCommonConfig.dynamicReadaheadConfig` carries
`minReadAheadMediaTimeMs` 15000, `readAheadGrowthRateMs` 1000 and `maxReadAheadMediaTimeMs`
120000 in the captures. `base.js` maps them to `PY`, `f7` and `AC` (seconds), with defaults 15,
1 and 120 when absent.

Status: verified: capture 2026-05-29, 2026-05-30, base.js 9fc68080

### SABR-022 `liveChunkReadahead` is a segment count from `videoDetails`, default 3

`base.js` reads `videoDetails.liveChunkReadahead` (2 on the ultra-low-latency streams of the
May captures) and uses 3 when it is missing. `PXH` lowers it by 1 when the video's `bu` flag is
set and sets it to 1 when `Cj` holds: the latency class is LOW or ULTRALOW and the learned
segment duration is at least 5 s. `h4` clamps the result to [1, 8] segments.

Status: verified: capture 2026-05-29, base.js 9fc68080

### SABR-023 `Kl` is segment duration times chunk count

`Kl = vB * chunks`, where `vB` is the mean learned segment duration (rounded to 0.5 s) or 5 s
before one is learned. `Kl` is the edge margin of the non-SABR branch of `FX()` (SABR-017);
the request gate's buffer target is `B` (SABR-019), a separate quantity.

Status: verified: base.js 9fc68080

### SABR-024 The latency class comes from `videoDetails.latencyClass`

`videoDetails.latencyClass` is `MDE_STREAM_OPTIMIZATIONS_RENDERER_LATENCY_{UNKNOWN, NORMAL,
LOW, ULTRA_LOW}`, mapped in `base.js` to `NORMAL`, `LOW`, `ULTRALOW`. Every live stream the
main player watches in the May captures is `ULTRA_LOW` with 1 s segments; the one `NORMAL`
stream is an inline preview. Measurements of the reference's live distance therefore come from
ultra-low-latency streams.

Status: verified: capture 2026-05-29, base.js 9fc68080

### SABR-025 `liveReadaheadSeconds` is in the player response and `base.js` never reads it

`playerConfig.livePlayerConfig.liveReadaheadSeconds` appears in player responses (1.6 and
12). The string does not occur in `base.js` 9fc68080; the live gate uses
`liveChunkReadahead` (SABR-022).

Status: verified: capture 2026-05-29, base.js 9fc68080

### SABR-026 Live playback may start with 0.25 s buffered

`base.js` sets the minimum startup buffer for live to
`html5_min_startup_buffered_media_duration_for_live_secs`, default 0.25 s.
`PLAYBACK_START_POLICY` can raise it (SABR-027).

Status: verified: base.js 9fc68080

### SABR-027 The start and resume readahead come from `PLAYBACK_START_POLICY` brackets

Among the start brackets (UMP-015) whose `min_bandwidth_bytes_per_sec` is at most the
player's bandwidth estimate, the player waits for the smallest `min_readahead_ms` before
starting; the resume brackets apply the same way after a rebuffer. Streams send different
values: 800 ms start and 1500 ms resume on one, 1200 ms start or 3500 ms resume on others.

Status: unverified

### SABR-028 The drift seek back to the live edge runs only when `policy.C` is not 1

`base.js` `Wla` seeks to the live edge (`seektimeline_pegToLive`) when the playhead drifts
too far behind, but only when `WS()` holds, and `WS()` is `policy.C !== 1`. `policy.C`
defaults to 1. With the default the player applies no drift correction and stays where the
server placed it.

Status: verified: base.js 9fc68080

### SABR-029 On live the main player's `player_time_ms` sits 1 to 5 s behind its buffered end

On the ultra-low-latency reference streams, the main player's reported playhead trails the
end of its own buffered ranges by about 1 to 5 s in steady state, and it requests about once
per segment.

Status: verified: capture 2026-05-29

### SABR-030 The reference reports a buffered end past the head it knows

In a parallel capture of the reference and our client on one stream, the reference's
buffered range end was past the `head_time_ms` of the latest `LIVE_METADATA` on 78 to 80 % of
requests (audio median +999 ms, video median +2000 ms, up to +10 s). Clamping the reported end
to the head tells the server the client holds everything up to it.

Status: unverified

## Server-driven quality

### SABR-031 In auto quality the server picks the rung and switches it in band

With the full ladder offered (REQ-017), the served itag follows the client's
`bandwidth_estimate`. A change of rung arrives in band on a continuous sequence: a 1080p run
ended at segment 33 and 480p began at 34; 480p ended at 43 and 1080p began at 44; 720p ended at
29 and 144p began at 30. No seek, no reset of the sequence. A rung the player has not
initialized yet arrives with its init segment and a `FORMAT_INITIALIZATION_METADATA` part
first; a rung it initialized earlier in the session (listed in request field 2, REQ-007)
arrives as media only.

Status: verified: capture 2026-07-16

### SABR-032 The server downshifts slowly and upshifts at once

Under a throttle the reported `bandwidth_estimate` fell within one round, yet the server kept
serving the higher rung for 45 to 60 s, draining the buffer, before it downshifted. When the
throttle lifted, the estimate and the served rung both rose in the same round.

Status: verified: capture 2026-07-16

### SABR-033 The first rung of an auto VOD open follows the first advertised estimate

First requests that advertised about 100 KB/s (95,027 and 104,857 B/s) were served 360p and
480p and climbed from there; a first request that advertised 6.8 MB/s was served 720p from
segment 1.

Status: verified: capture 2026-07-16

### SABR-034 A VOD request that names one track still gets the other track

A VOD request from our client with an audio format and no video format came back with a
default video format (itag 398 for audio itag 251, a high AV1 rung on some videos) besides
the audio. The reference never sends a single-track request (REQ-006).

Status: unverified

### SABR-035 A Shorts preload is one round on its own player, continued on swipe

Once the active Short has media, the player preloads the next Short: a player response (or
the prefetched one from the sequence) and one SABR round on a new playback nonce, then it
stops. When the user swipes to it, the same nonce continues with buffered ranges for what the
preload fetched. Shorts players send viewport 523x932 and `visibility` 0. Round 0 of a Short
usually holds the whole video (2 to 8 MB); the next round is the policy-only round of SABR-005.

Status: verified: capture 2026-05-30

## Directives

### SABR-036 After `SABR_REDIRECT` the player posts to the new URL as given

The player's next request goes to the host in the redirect URL, with the same `n` value the
redirect carried and only `rn` added (6 of 6 redirects). Running n-sig on a redirect URL
again would change an already-descrambled value.

Status: verified: capture 2026-05-29, 2026-05-30

### SABR-037 `RELOAD_PLAYER_RESPONSE` asks for a new `player()` call with its token

Part 46 carries a reload token. The client calls `player()` again with the token in
`playbackContext.reloadPlaybackContext.reloadPlaybackParams.token` and adopts the new
streaming URL and ustreamer config; until it does, the server withholds media. A live request
that carries a stale `lmt` in its format ids earns a reload. No reload occurs in the reference
captures.

Status: unverified

### SABR-038 `SABR_ERROR` names the problem with a type string

Part 44 is `{1: type, 2: code}`. Types seen on our sessions: `sabr.no_audio_selected` (a
multi-audio-track video requested without `xtags`), `sabr.no_video_selected` (no acceptable
video format), `sabr.malformed_config` (an edited ustreamer config). No `SABR_ERROR` occurs
in the reference captures.

Status: unverified

### SABR-039 A VOD request without `lmt` gets no media

VOD requests from our client whose preferred format ids carried no `lmt` were answered with
policy, context and snackbar parts and no media, while `STREAM_PROTECTION_STATUS` stayed 1.

Status: unverified

### SABR-040 `SABR_CONTEXT_UPDATE` stores a context the client echoes in `StreamerContext`

Part 57 fields: 1 `type`, 2 `scope` (4 = `CONTENT_ADS`), 3 `value` (bytes), 4
`send_by_default`, 5 `write_policy` (2 = `KEEP_EXISTING`). Every instance in the captures is
`type 5, scope 4, send_by_default 1, write_policy 2`. The player's next request carries the
context in `StreamerContext` field 5 (4 of 4). Part 59, which does not occur in the captures,
starts, stops or discards a stored context.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-08-18

## Live edge behaviour on our sessions

### SABR-041 A request for a live segment that is not published yet can be held open

When our request asked for media past what the packager had published, the server held the
connection for about 7.5 s before answering. 45 of the 48 holds over 5 s in one run came from
a single broadcaster whose head stopped advancing during them; on other streams such holds were
under 0.2 % of rounds.

Status: unverified

### SABR-042 A live stream's edge can stop producing for every client

On some streams the head stops advancing for minutes (10 s of stream time over 132 s of wall
time in one case) while rounds keep answering with status 1 and no media. A second client
probing the same edge at the same moment gets no media either.

Status: unverified

### SABR-043 A DVR window can contain holes the server never fills

On some live sources (rolling news, restarted encoders), requests inside part of the DVR
window get metadata-only answers indefinitely while positions on either side serve media.

Status: unverified

## More player and server behaviour

### SABR-044 `player_time_ms = 0` on a live stream starts at the DVR window start

A live request with `player_time_ms` 0 and no ranges was answered with a `SABR_SEEK` to time
0 and segments with sequence number 0 at `start_ms` 0.

Status: verified: capture 2026-05-29

### SABR-045 On dubbed videos several formats share one itag and `xtags` tells them apart

Auto-dubbed videos list many `adaptiveFormats` with the same itag (13 itag-140 formats, one
per language, in the fixture). Each carries an `xtags` string (base64 of a small protobuf, such
as `acont=dubbed-auto, lang=de-DE`) and an `audioTrack` with `audioIsDefault` on the original.
A request must carry the chosen format's `xtags` in its `FormatId` (SABR-038).

Status: verified: capture 2026-05-29
Fixture: `notes/fixtures/player-response-vod.json`

### SABR-046 On VOD the player stops requesting once both tracks are buffered to the end

`base.js` declines to request (`vodbufferedtoend`) when the lesser of the audio and video
buffered ends is within 10 ms of the duration. Shorts have the same check
(`shortsbufferedtoend`).

Status: verified: base.js 9fc68080

### SABR-047 After an underrun the player asks from the end of what it holds

`base.js` `O0E`: when the playhead is outside every buffered range of the media element, the
request position moves back to the end of the nearest range behind it (logged `sundrn`); if
that range ends 20 s or more behind the playhead, the player raises
`bufferunderrunexceedslimit`. While seeking, the playhead is used as is.

Status: verified: base.js 9fc68080

### SABR-048 The player does not speed up to catch the live edge

Every `setPlaybackRate` call in `base.js` 9fc68080 serves the user's speed menu or carries the
rate over a reload; none is tied to the distance from the live head.

Status: unverified

### SABR-049 The server picks the served itag from the offered list

The server serves any format the request offers, often below the first preference: a main
player offering `[315, 308, 303, 302, ...]` was served 302 throughout with an estimate of
0.9 to 1.5 MB/s. The client learns the served itag from `MEDIA_HEADER.itag` and reports
buffered ranges under that itag.

Status: verified: capture 2026-05-29

### SABR-050 The buffered ranges decide what the server sends next

From our sessions: with no buffered ranges the server re-sends a full readahead window of both
tracks from `player_time_ms`, including media the client already holds. Ranges that claim media
the client does not hold make the server withhold it. With accurate ranges each track continues
from its own range end, and a continuation response carries no new container init.

Status: unverified

### SABR-051 A request ahead of the held media is answered with a `SABR_SEEK` back to it

When our request's `player_time_ms` sat past the end of the media we held for a track (a DVR
fetch whose first segment ended just before the requested start), the server answered with a
`SABR_SEEK` to the end of the held media, to fill the gap.

Status: unverified

### SABR-052 On live, a low bandwidth estimate did not move the server off a rung the link could not carry

Over 80 minutes on a 24/7 stream, our requests advertised about 192 KB/s (the measured link)
while the server kept serving 720p VP9, which needs roughly 250 to 400 KB/s. The rung dropped
only during an outage. The automatic downshift of SABR-032 was seen on VOD.

Status: unverified

### SABR-053 A VOD request preferring an MP4 format was served WebM

A VOD request from our client whose video preference was an MP4/AVC format was served a
WebM/VP9 format, with WebM `Cues` as its seek index. The server's choice of format can leave
the requested container.

Status: unverified

### SABR-054 Moving the viewport mid-session moves the served rung on our sessions

Lowering our live session's viewport (1920x1080 to 854x480) got a 480p rung on the next round.
Raising it mid-session while still reporting the old rung's buffered ranges left the session
with no media on every later round. The reference client never changes the viewport to change
quality (REQ-015).

Status: unverified

### SABR-055 Many concurrent streams off one bootstrap draw HTTP 401

Opening many SABR audio streams in parallel off one bootstrap gets HTTP 401: past about 12,
some streams fail; at 36 or more, the whole session is refused.

Status: unverified

### SABR-056 A second concurrent session from one client gets no media

While a first SABR session runs, a second one from the same client is answered with 11-byte
rounds that carry no media and about 3.4 s of backoff, until the first session stops.

Status: unverified
