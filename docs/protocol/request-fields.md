# The SABR request

Media is requested by POSTing a `VideoPlaybackAbrRequest` protobuf to the session's
streaming URL. Field numbers follow the vendored `video_playback_abr_request.proto`,
`client_abr_state.proto` and `streamer_context.proto`.

### REQ-001 Media is fetched only by POSTing a protobuf body; there are no byte ranges

Every `videoplayback` request that carries media is a POST of a `VideoPlaybackAbrRequest`
to the `serverAbrStreamingUrl` (with `sabr=1` in its query). No request carries a `Range`
header or a `range=` parameter. Position is expressed in media time (REQ-004) and in the
reported buffered ranges (REQ-008).

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18

### REQ-002 The top-level request fields

| Field | Name | When |
|---|---|---|
| 1 | `client_abr_state` | every request |
| 2 | `selected_format_ids` (initialized formats) | VOD, once the client holds a format's init (REQ-007) |
| 3 | `buffered_ranges` | when the client holds media |
| 5 | `video_playback_ustreamer_config` | every request, the bytes from the player response |
| 16 | `preferred_audio_format_ids` | every request |
| 17 | `preferred_video_format_ids` | every request |
| 19 | `streamer_context` | every request |
| 24 | `ssap_playback_infos` | live, while an ad cue is active (REQ-025) |

Field 4 (a top-level `player_time_ms`) is never sent; the playhead travels in
`ClientAbrState` field 28.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18

### REQ-003 `ClientAbrState` field 40 is never sent

`enabled_track_types_bitfield` (40) is absent from every request in the captures; the client
selects tracks through the preferred format lists.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18

### REQ-004 `player_time_ms` is `ClientAbrState` field 28, the player's current media time

Field 28 carries the playhead in milliseconds of media time: absolute stream time on live,
time from the start of the file on VOD. On VOD it advances with playback inside the buffered
range. Special values: `2^53 - 1` asks for the live edge (SABR-009), and `0` on a live stream
asks for the start of the DVR window (SABR-044).

Status: verified: capture 2026-05-29, 2026-07-16

### REQ-005 Live format ids are itag-only; VOD format ids carry the real `lmt`

A `FormatId` is `{1: itag, 2: lmt, 3: xtags}`. In every live request the preferred format
ids carry the itag with no `lmt` (2,278 ids across the May captures). In every VOD request
they carry the format's `lastModified` from the player response as `lmt`. The reference
client writes an empty `xtags` string where a format has none.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18

### REQ-006 Every request names both tracks; live names one audio format

Every request carries at least one preferred video and one preferred audio format: there are
no single-track requests in the captures. Live requests name one audio format. VOD
requests of the main player usually name several (6 in the July captures, up to 46). In auto
quality the video list is the whole ladder, highest first (REQ-030).

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18

### REQ-007 Field 2 lists the formats whose init the client holds (VOD)

From the round after a VOD format's init arrives, field 2 lists `{itag, lmt}` of the audio
and video formats the player has initialized. The first request of a VOD session and every
live request in the captures leave it empty.

Status: verified: capture 2026-05-29, 2026-07-16

### REQ-008 Buffered ranges are built from the `MEDIA_HEADER`s the client holds

A `BufferedRange` is `{1: format_id, 2: start_time_ms, 3: duration_ms,
4: start_segment_index, 5: end_segment_index}`. For a contiguous run of held segments of one
format, the indices are the first and last `sequence_number`, the start is the first
segment's `start_ms`, and the duration is the sum of the segments' `duration_ms`. All 123
ranges in the July seek capture match this to the millisecond. A client reports one range per
contiguous run, so after a seek or with gaps a format has several ranges (REQ-010).

Status: verified: capture 2026-07-16, 2026-08-18
Fixture: `crates/innertube-sabr/tests/fixtures/sessions/vod-open-seek.replay`

### REQ-009 A live range grows by about one segment per round from the join

On live the first range for a format starts at the first held segment and its duration
grows with each segment (audio `998, 1996, 2995 ...` ms and video `1000, 2000, 3000 ...` ms on
a 1 s stream); live headers carry no `duration_ms`, so the client computes it.

Status: verified: capture 2026-05-29
Fixture: `crates/innertube-sabr/tests/fixtures/sessions/live-join.replay`

### REQ-010 A seek to an uncached time keeps the old ranges and adds one at the target

On a VOD seek past the buffer, the next request sets `player_time_ms` to the target and keeps
reporting every range it already holds. The server's response starts at the segment boundary
at or before the target (a seek to 360 s got a new range starting at 350.001 s, segments
36 to 38). A live DVR seek behaves the same way (prior ranges kept, `player_time_ms` =
target). A manual quality change is the one case that empties the ranges (REQ-018).

Status: verified: capture 2026-05-29, 2026-07-16
Fixture: `crates/innertube-sabr/tests/fixtures/sessions/vod-open-seek.replay`

### REQ-011 `ClientAbrState` fields the reference client sends

| Field | Name | Notes |
|---|---|---|
| 13 | `time_since_last_manual_format_selection_ms` | manual picks only (REQ-018) |
| 14 | `last_manual_direction` | sint32, manual picks only |
| 16 | `last_manual_selected_resolution` | manual picks only |
| 18, 19 | `client_viewport_width`, `_height` | player element size (REQ-015) |
| 21 | `sticky_resolution` | every request (REQ-020) |
| 23 | `bandwidth_estimate` | bytes/s (REQ-016) |
| 28 | `player_time_ms` | REQ-004 |
| 29 | `time_since_last_seek` | every request |
| 34 | `visibility` | REQ-014 |
| 35 | `playback_rate` | float, only when not 1 (REQ-019) |
| 36 | `elapsed_wall_time_ms` | every request |
| 39 | `time_since_last_action_ms` | every request |
| 46 | `drc_enabled` | every request |
| 57 | unnamed | every request, varies |
| 58 | `prefer_vp9` | every request |
| 59 | `av1_quality_threshold` | 2160 |
| 68 | `sabr_force_max_network_interruption_duration_ms` | every request |
| 71 | unnamed | 1 |
| 72 | capability caps | REQ-012 |
| 76 | `enable_voice_boost` | most requests |
| 79 | `playback_authorization` | every request |
| 80 | unnamed | some requests |
| 85 | unnamed | REQ-022 |

Fields 50, 51 and 54 appear on single requests in the July captures. Fields 20
(`client_bitrate_cap_bytes_per_sec`), 43 (`max_pacing_rate`) and 61 (`is_prefetch`) never
appear.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18

### REQ-012 Field 72 holds per-player quality caps that never change during a session

Field 72 is a six-field message. `base.js` 9fc68080 serializes it from
`{defaultPolicy, smooth, visibility, TV, performance, speed}` in that field order, each a
resolution cap from the player's quality constraints. The main player sends
`{1: 0, 2: ceiling, 3: 0, 4: 0, 5: ceiling, 6: 0}`, with a ceiling of 1080, 1440 or 2160 in the
captures. Muted inline players send `3: 480, 4: 720` with the same kind of ceiling. The value is
constant for the life of a player instance in every capture: a quality change never touches
it. REQ-013 records what the server does with `3: 480, 4: 720` on a main-player session.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18, base.js 9fc68080

### REQ-013 A main-player session advertising field 72 `3: 480, 4: 720` is served 480p

Our main-player requests with field 72 `{2: 1080, 3: 480, 4: 720, 5: 1080}` were served the
480p rung on live and VOD; the same requests with fields 3 and 4 set to 0 were served up to
1080p (checked with `ffprobe` on the delivered frames). The reference main player sends 0 and
0 (REQ-012), so 0 and 0 is both the reference shape and the working one.

Status: unverified

### REQ-014 `visibility` is 0 for the main player and 5 for an inline player

`ClientAbrState` field 34 is 0 on the main watch-page player and 5 on a muted, autoplaying
inline player (home-feed and watch-page previews); a few requests of both kinds carry 3, while
the player's stats reports show `vis=3`. `base.js` fills the field from the player's
`getVisibilityState()`.
The `/api/stats` reports of the same playback nonce agree: `vis=5`, `muted=1`, `autoplay=1`
for the inline player. The main player's `player()` request carries
`contentPlaybackContext.vis` 0 and an inline preview's carries 5.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-08-18

### REQ-015 The viewport is the player element's size

Fields 18 and 19 carry the player element's CSS size in pixels: 1333x750 for the watch-page
player in the captures, 640x360 or 360x640 for inline previews, 523x932 for Shorts. They can
change during a session (806x453 to 841x473 in the first seconds of one VOD), but they never
change to steer quality: a manual pick from 720p to 1080p to 240p kept 1333x750.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16

### REQ-016 `bandwidth_estimate` is the player's measured throughput in bytes per second

Field 23 is the player's current estimate of link throughput, in bytes/s. It follows the
link within a round: under a 120 KB/s throttle it dropped to 119,184 on the next request and
returned to 2.76 MB/s in the round after the throttle lifted. It is the only request field
that moves the server's automatic rung choice (SABR-031). PLAYER-008 describes how the
player computes it.

Status: verified: capture 2026-07-16

### REQ-017 Auto quality sends the full ladder and leaves the manual fields out

In automatic quality, `sticky_resolution` is 0 and fields 13, 14 and 16 are absent. The
May to September captures send the whole ladder in `preferred_video_format_ids` on every
request. The October 2026 player (0229cd64) sends no video preference on VOD: its request
builder leaves field 17 empty unless a pick is locked. Our engine sends none on VOD and the
full ladder on live, where no capture of the empty list exists yet. When the user limits quality to a range, `sticky_resolution`
carries the range's top (1080) and the ladder stays full; the server then adapts below that
cap.

Status: verified: capture 2026-07-16, 2026-10-17, base.js 0229cd64

### REQ-018 A manual quality pick collapses the ladder and empties the buffered ranges

The request after a pick from the quality menu: `preferred_video_format_ids` holds the one
picked itag; `sticky_resolution` (21) and `last_manual_selected_resolution` (16) carry the
picked height (1080, 240); `last_manual_direction` (14) is +1 for an upshift and -1 for a
downshift (zigzag encoded); `time_since_last_manual_format_selection_ms` (13) restarts near
0. `buffered_ranges` is empty for both tracks, `player_time_ms` stays continuous, and the
viewport and field 72 do not change. The fields stay set on later requests. The `base.js`
state builder sets 13, 14 and 16 only while a pick is stored in the `yt-player-quality`
local-storage entry. The server answers with SABR-012.

Status: verified: capture 2026-05-29, base.js 9fc68080

### REQ-019 `playback_rate` is sent only when the rate is not 1

`ClientAbrState` field 35 is a 32-bit float. `base.js` sets it only when
`getPlaybackRate() !== 1`; the captures carry `2.0` on the requests of a player at double
speed and nothing otherwise.

Status: verified: capture 2026-05-29, base.js 9fc68080

### REQ-020 The reference client sends `sticky_resolution = 0` on every auto request

Field 21 is present on every request, 0 in auto quality, including the first live-edge
request. REQ-021 records how the server treated the same field from our sessions.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18

### REQ-021 `sticky_resolution = 0` from our sessions capped the served rung at 480p

On a stream with a 1440p ladder, our requests with field 21 = 0 were served 480p from the
first round; without the field the same stream was served 720p60. This is why our engine
omits field 21 in auto quality, a known difference from the reference.

Status: unverified

### REQ-022 Field 85 is 1 on every request since July 2026

`ClientAbrState` field 85 is absent from all 461 requests of the May captures and present
with value 1 on every request of the July and August captures. It is not in the vendored
schema.

Status: verified: capture 2026-05-29, 2026-07-16, 2026-08-18

### REQ-023 The playback cookie from the last policy is echoed in `StreamerContext`

`NEXT_REQUEST_POLICY` field 7 holds an opaque playback cookie. The player's next request
carries the same bytes in `StreamerContext` field 3 (750 of 750 pairs byte-equal). A new
playback session starts with no cookie.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18
Fixture: `crates/innertube-sabr/tests/fixtures/sessions/live-join.replay`

### REQ-024 `StreamerContext` carries the client info, PO token, cookie and SABR contexts

| Field | Content |
|---|---|
| 1 | `client_info`: `{1: "en_US", 16: 1 (WEB), 17: client version, 18: "X11"}`; no `os_version` |
| 2 | the GVS PO token, raw bytes (POT-001) |
| 3 | the playback cookie (REQ-023) |
| 5 | SABR contexts the server asked to have echoed (SABR-040) |
| 6 | context types held but not sent |

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18

### REQ-025 Field 24 echoes the active ad cue point

On live, while the server has announced an ad cue (part 69, UMP-014), each request carries
field 24, `ssap_playback_infos`: `{5: 278, 7: the cue's 48-byte identifier, 8: a small
integer that differs between the main and inline player (11, 3), 10: the cue start in
stream ms, 11: 60000}`. 60000 is the cue's 60 s duration and 278 the ladder's lowest video
rung. Sessions with no cue send no field 24.

Status: verified: capture 2026-05-29, 2026-05-30

### REQ-026 The player adds `alr`, `cpn`, `cver` and `rn` to the streaming URL

The POST URL is the player response's `serverAbrStreamingUrl` with four query parameters
added and none removed (53 of 53 pairs): `alr=yes`, `cpn` (the playback nonce,
PLAYER-007), `cver` (the client version) and `rn`, a request counter that rises across the
requests of every player on the page. The `n` parameter is replaced by its descrambled value
(PLAYER-005). A `SABR_REDIRECT` URL already carries `alr`, `cpn` and `cver`; the player adds
only `rn` (SABR-036).

Status: verified: capture 2026-05-29, 2026-05-30

### REQ-027 The PO token is never in the URL

No SABR POST URL carries a `pot` parameter; the token travels in `StreamerContext` field 2.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18

### REQ-028 Every POST carries browser cross-origin headers

Each `videoplayback` POST carries `Origin: https://www.youtube.com`, `Referer`,
`Sec-Fetch-Site`, `Sec-Fetch-Mode`, `Sec-Fetch-Dest`, `Sec-Fetch-Storage-Access`, the
`sec-ch-ua*` client hints and `X-Client-Data`.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18

### REQ-029 Without `Origin`, `Referer` and `Sec-Fetch-*` googlevideo answers 403 at random

Our POSTs without these headers were refused with HTTP 403 on some edge nodes and accepted on
others. With the three present, no such 403 has been seen. The client hints and
`X-Client-Data` are not needed.

Status: unverified

### REQ-030 The preferred video list is ordered by height and may mix AV1 and VP9 on VOD

`preferred_video_format_ids` lists formats highest first. On live it holds one codec family
(AV1, VP9 or AVC) in every request. On VOD, inline previews send one family, while the main
player often interleaves AV1 and VP9 by height (`401, 315, 400, 308, 399, 303, ...` on every
request of the July sessions). In those sessions the server served AV1 rungs only.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18

### REQ-031 A return to Auto clears the manual fields and keeps the buffered ranges

The first request after Auto is picked from the quality menu drops fields 13, 14 and 16,
sets `sticky_resolution` back to 0 and sends no video preference (REQ-017). Unlike a pick
(REQ-018), `buffered_ranges` is not emptied: the picked rung's ranges are still reported.
`bandwidth_estimate` and `player_time_ms` continue. The server sends the auto rung from the
end of the buffered range (SABR-057). The pick that preceded it listed both 360p formats
(396, 243) in field 17 and emptied the ranges of both tracks.

Status: verified: capture 2026-10-17
