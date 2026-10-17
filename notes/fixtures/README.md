# Protocol fixtures

Captured from a real YouTube **web client** session (mitmproxy), decoded offline, and
**scrubbed**. Source: a live+VOD web-client capture (not in this repo). Protocol
facts: `../../docs/protocol/`.

## Scrubbing

All credentials removed: PO tokens, `signatureCipher`, URL signatures/params, cookies,
`visitorData`, `cpn`, and the client **IP** (incl. IPs embedded in HLS URL path segments).
A final IPv4 regex backstop runs over the JSON. URL values are reduced to
`https://<host>/<redacted-path+query>`. Verified: 0 IPs, 0 token-like strings remain.
CDN edge hostnames keep their shape with the node id masked (`rr2---sn-xxxxxxxxxx-xxxxx`),
and the caption URL's `ei` and `signature` are placeholders.

## Files

| File | What | Use |
|---|---|---|
| `player-response-live.json` | scrubbed `player()` for a **live** stream (`8bx5-SueICY`) — has both `hlsManifestUrl` and `serverAbrStreamingUrl` | model the live streamingData shape |
| `player-response-vod.json` | scrubbed `player()` for a **VOD** (`ibLthGyU8Es`) — `formats` + `adaptiveFormats` + SABR, no HLS | model VOD shape / format parsing |
| `sabr-response-metadata.ump` | a tiny **98 B** real UMP response, all metadata parts (no media) | **golden input for the UMP parser** |
| `sabr-media-header.bin` | a real **51 B** `MEDIA_HEADER` payload (header_id 0, itag 140); protocol metadata only, no media bytes | golden input for the `MEDIA_HEADER` decoder |
| `sabr-response-media-parts.json` | decoded part table of an 8 MB media-bearing UMP response (framing only; media bytes NOT stored) | expected part sequence / reassembly logic |
| `sabr-request-protobuf-structure.json` | top-level field map of a `VideoPlaybackAbrRequest` (lengths only, values redacted) | model the request body layout |

## Regenerating

Decoder scripts are not committed (one-off). The flow is concatenated mitmproxy
tnetstrings (format v21); `;` = unicode string, dict keys are unicode. UMP varint scheme
and protobuf field map are documented in `../../docs/protocol/ump.md`. No `pot=` in the
URL — the PO token is protobuf field 5 of the request body.
