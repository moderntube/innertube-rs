# UMP framing, parts and segments

A SABR response body is UMP (`application/vnd.yt-ump`): a sequence of typed parts. Media
travels in `MEDIA` parts grouped into segments; everything else is protobuf control data.
Part payload schemas are vendored from LuanRT/googlevideo in
`crates/innertube-sabr/proto/`.

### UMP-001 A UMP body is a flat sequence of `[type][size][payload]` parts

Each part is a UMP varint part type, a UMP varint payload size in bytes, then that many
payload bytes, repeated to the end of the body. Every response body in the captures decodes
this way and ends on a part boundary (1,102 response bodies), so a part never spans two
responses.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-04, 2026-07-16, 2026-08-18
Fixture: `notes/fixtures/sabr-response-metadata.ump`

### UMP-002 UMP varints take their length from the high bits of the first byte

UMP varints are not protobuf varints.

| First byte | Bytes | Value |
|---|---|---|
| `0xxxxxxx` (< 0x80) | 1 | `b0` |
| `10xxxxxx` (< 0xC0) | 2 | `(b0 & 0x3F) \| b1 << 6` |
| `110xxxxx` (< 0xE0) | 3 | `(b0 & 0x1F) \| b1 << 5 \| b2 << 13` |
| `1110xxxx` (< 0xF0) | 4 | `(b0 & 0x0F) \| b1 << 4 \| b2 << 12 \| b3 << 20` |
| `1111xxxx` | 5 | `b1..b4` as a little-endian u32; the low bits of `b0` are ignored |

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-04, 2026-07-16, 2026-08-18
Fixture: `notes/fixtures/sabr-response-metadata.ump`

### UMP-003 SABR responses are `application/vnd.yt-ump`, chunked, with no Content-Length

The server sends `Content-Type: application/vnd.yt-ump` and `Transfer-Encoding: chunked`, so a
client can parse parts as bytes arrive. In a 7.7 MB first VOD response the audio init is
complete after 648 bytes and the video init after 35 KB; the first video media segment ends
at 1 MB.

Status: verified: capture 2026-05-30, 2026-07-16, 2026-08-18

### UMP-004 The part types in use

Names come from the vendored `ump_part_id.proto` (googlevideo). All of these appear in the
captures except 44, 46 and 59.

| Id | Name | Payload |
|---|---|---|
| 20 | `MEDIA_HEADER` | starts a segment: header id, itag, sequence number, times (UMP-007) |
| 21 | `MEDIA` | header id varint, then container bytes (UMP-005) |
| 22 | `MEDIA_END` | header id varint: the segment is complete |
| 31 | `LIVE_METADATA` | live head and seekable window (SABR-014) |
| 35 | `NEXT_REQUEST_POLICY` | readahead targets, backoff, playback cookie (SABR-001) |
| 42 | `FORMAT_INITIALIZATION_METADATA` | format metadata (UMP-013) |
| 43 | `SABR_REDIRECT` | field 1: the URL for later requests (SABR-036) |
| 44 | `SABR_ERROR` | field 1 type string, field 2 code (SABR-038) |
| 45 | `SABR_SEEK` | a server reposition (SABR-010) |
| 46 | `RELOAD_PLAYER_RESPONSE` | a reload token (SABR-037) |
| 47 | `PLAYBACK_START_POLICY` | start and resume readahead brackets (UMP-015) |
| 52 | `REQUEST_IDENTIFIER` | a per-response identifier |
| 53 | `REQUEST_CANCELLATION_POLICY` | cancellation hints |
| 57 | `SABR_CONTEXT_UPDATE` | a context the client echoes back (SABR-040) |
| 58 | `STREAM_PROTECTION_STATUS` | attestation status (GATE-001) |
| 59 | `SABR_CONTEXT_SENDING_POLICY` | which stored contexts to send |
| 67 | `SNACKBAR_MESSAGE` | a UI message; present in held rounds (GATE-005) |
| 69 | cue point list | ad cue points (UMP-014); newer than the vendored enum |

A client skips any other part type by its size. Part 69 postdates the vendored schema, so new
ids do appear.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16, 2026-08-18

### UMP-005 `MEDIA` and `MEDIA_END` payloads lead with the segment's header id

A `MEDIA` (21) payload is a UMP varint header id followed by container bytes; the bytes
append to the segment opened by the `MEDIA_HEADER` with that header id. A `MEDIA_END` (22)
payload starts with the same header id and closes that segment.

Status: verified: capture 2026-05-29, 2026-07-16
Fixture: `crates/innertube-sabr/tests/fixtures/sessions/live-join.replay`

### UMP-006 Responses interleave the segments of different tracks

One `MEDIA_HEADER` through its `MEDIA_END` is one segment. Within a response, `MEDIA`
parts of an audio segment and a video segment alternate, so when one segment ends another
is usually still open: in 313 of 371 live responses a `MEDIA_END` arrived while another
segment was open. A body can be split for incremental parsing only where no segment is open,
or the per-response header map has to persist across the pieces.

Status: verified: capture 2026-05-29
Fixture: `crates/innertube-sabr/tests/fixtures/sessions/live-join.replay`

### UMP-007 `MEDIA_HEADER` field numbers

| Field | Name | Live | VOD |
|---|---|---|---|
| 1 | `header_id` | yes | yes |
| 2 | `video_id` | yes | yes |
| 3 | `itag` | yes | yes |
| 4 | `lmt` | no | yes |
| 5 | `xtags` | no | multi-audio-track formats |
| 6 | `start_range` (byte offset in the format) | no | yes |
| 8 | `is_init_seg` | no | yes |
| 9 | `sequence_number` | yes | media segments only |
| 10 | `bitrate_bps` | yes | yes |
| 11 | `start_ms` (media time, ms) | yes | media segments only |
| 12 | `duration_ms` | no | media segments only |
| 13 | `format_id` | yes | yes |
| 14 | `content_length` | no | yes |
| 15 | `time_range` | yes | media segments only |
| 16 | `sequence_lmt` | yes | no |

Field 17 (not in the vendored schema) appears on a few VOD headers from July 2026.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-07-16
Fixture: `notes/fixtures/sabr-media-header.bin`

### UMP-008 Live `MEDIA_HEADER`s carry a sequence number and absolute start, no duration

Live headers carry `sequence_number` (9) and `start_ms` (11) in stream time, which is
absolute (milliseconds since the broadcast's time origin, in the millions). They carry no
`duration_ms` (12), so a segment's duration comes from the gap between consecutive
`start_ms` values of one track, or from the format's `targetDurationSec`.

Status: verified: capture 2026-05-29, 2026-05-30

### UMP-009 VOD init arrives under its own header with no sequence number or times

On VOD the container init (WebM header and `Cues`, or MP4 `moov` and `sidx`) arrives under a
`MEDIA_HEADER` with `is_init_seg = 1` and no `sequence_number`, `start_ms` or `duration_ms`
(143 of 143 such headers). Media headers carry all three, and VOD sequence numbers start at
1 at time 0.

Status: verified: capture 2026-05-30, 2026-07-16
Fixture: `crates/innertube-sabr/tests/fixtures/sessions/vod-open-seek.replay`

### UMP-010 Every live segment begins with its own container init

Each live segment's bytes start with a complete container init: an MP4 `ftyp` + `moov`
for AAC audio and AV1 video, a WebM EBML header for VP9 video. Each of 723 completed live
segments in the reference capture holds one init. A live track delivered as a byte
stream is therefore a run of self-initializing files, and every init after the first has
to be stripped for a single-stream demuxer.

Status: verified: capture 2026-05-29
Fixture: `crates/innertube-sabr/tests/fixtures/sessions/live-join.replay`

### UMP-011 Live WebM segments use an unknown-size `Segment` and absolute cluster timecodes

A live WebM segment's `Segment` element has unknown size (`18 53 80 67 01 FF FF FF FF FF FF
FF`), and its `Cluster` timecodes continue the stream's absolute timeline (each segment
2000 ms after the last on a 2 s stream). Appending only the clusters of later segments to the
first segment yields one valid WebM file without rewriting timestamps. About 70 % of live
clusters also use an unknown-size length (`01 FF FF FF FF FF FF FF`).

Status: unverified

### UMP-012 A track can start mid-segment after a live join

The first `MEDIA` bytes our client receives for a track after a live join can be a fragment
with no init, so the track is withheld until its first init boundary. In the reference
captures every delivered live segment starts at its init (UMP-010); the mid-segment case is
seen on our own sessions only.

Status: unverified

### UMP-013 `FORMAT_INITIALIZATION_METADATA` describes a format; the init bytes come in `MEDIA`

Part 42 carries format metadata: `video_id` (1), `format_id` (2) and `mime_type` (5) on every
instance, and on VOD also end time, end segment number, init and index byte ranges and the
duration. The init bytes themselves arrive inline in `MEDIA` (UMP-009, UMP-010), also after a
server-driven rung change, where part 42 for the new format arrives around the switch.

Status: verified: capture 2026-05-29, 2026-07-16

### UMP-014 Part 69 is a list of ad cue points

Part 69 holds field 1, a repeated message whose field 1 matches the vendored `Cuepoint`
shape: `type` 1, `event` (2 = `CONTINUE`), `total_duration_sec` 60.0, `playhead_time_sec`,
a 32-byte context and a 48-byte identifier, plus the cue's start sequence number. It sits
next to the cue's time (ticks and timescale). The live audio segments of the same stream
carry an `emsg` box with `Cuepoint-Type: TYPE_AD`. The request echoes the cue in field 24
(REQ-025).

Status: verified: capture 2026-05-29, 2026-05-30

### UMP-015 `PLAYBACK_START_POLICY` holds start and resume readahead brackets

Part 47 field 1 is a repeated start bracket and field 2 a repeated resume bracket; each
bracket is `{1: min_bandwidth_bytes_per_sec, 2: min_readahead_ms}`. The reference live
stream sent `{1: {1: 0, 2: 800}, 2: {2: 1500}}` on every round: start once 800 ms is
buffered at any bandwidth, resume after a rebuffer at 1500 ms. SABR-027 gives the selection
rule and the values other streams send.

Status: verified: capture 2026-05-29

### UMP-016 VP9 rungs of one VOD can be joined into one WebM at a segment boundary

Clusters of two VP9 rungs of the same video, cut at a segment boundary both share and appended
after the first rung's init, decode as one WebM in libav and mpv with no errors, across
resolution and frame-rate changes in both directions: the VP9 decoder takes its dimensions
from the frame headers. Rungs of different codecs need a fresh container.

Status: unverified
