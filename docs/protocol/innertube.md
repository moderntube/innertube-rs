# InnerTube: player responses, page bootstrap and JSON endpoints

InnerTube is the JSON API at `https://www.youtube.com/youtubei/v1/<endpoint>`. This file
covers what a SABR session needs from it, and the response shapes the code parses.

## The client context

### IT-001 Every InnerTube call is a POST of `{context, ...}` from one WEB client context

Calls are `POST /youtubei/v1/<endpoint>?prettyPrint=false` with a JSON body
`{context: {client, user, request, ...}, <endpoint fields>}`. The WEB client sends
`clientName: "WEB"` (numeric id 1) and a `clientVersion` of the form `2.YYYYMMDD.NN.NN`
(`2.20260528.01.00` and `2.20260529.01.00` in May 2026) on every call. `context.client` also
carries `visitorData`, `hl`, `gl`, `userAgent`, `osName`, `platform`, screen and window sizes,
`originalUrl`, `mainAppWebInfo` and `configInfo`.

Status: verified: capture 2026-05-29, 2026-05-30

### IT-002 A sparse `player()` context gets an unplayable response

An anonymous `player()` request needs a bootstrapped `visitorData` and a browser-like client
context; with a minimal context the response's `playabilityStatus` is `UNPLAYABLE`.

Status: unverified

### IT-003 `visitorData` comes from a page's `ytcfg`

The home page embeds `ytcfg.set({...})` with `VISITOR_DATA` (also in
`INNERTUBE_CONTEXT.client.visitorData`). A browser keeps one `visitorData` for its whole
session and sends it in `context.client.visitorData` and in the `X-Goog-Visitor-Id` header
(388 of 388 InnerTube calls in the signed-in walk).

Status: verified: capture 2026-05-30

### IT-004 A signed-in call is authenticated by `SAPISIDHASH`

The signed-in WEB client sends its cookies and
`Authorization: SAPISIDHASH <ts>_<sha1(ts + " " + SAPISID + " " + origin)>`, recomputed per
request, plus `X-Goog-AuthUser` and `X-Origin`. There is no bearer token on WEB.

Status: unverified

## Getting a player response

### IT-005 A cold watch page embeds the player response

A watch page fetched directly embeds `ytInitialPlayerResponse = {...}` (and the watch-next data
as `ytInitialData`); the player plays from it without a `player()` call.

Status: verified: capture 2026-05-29

### IT-006 Navigating to a video inside the site uses `get_watch`

Clicking a video on a loaded page sends no `/youtubei/v1/player` call. The page POSTs
`/youtubei/v1/get_watch` with `{context, playerRequest, watchNextRequest}` and gets back a
JSON array of two sub-responses: `[{playerResponse, ...}, {watchNextResponse, ...}]`. The
player request inside carries `contentPlaybackContext.vis` 0.

Status: verified: capture 2026-05-29, 2026-05-30, 2026-08-18

### IT-007 Inline previews and Shorts call `/youtubei/v1/player`

Hover and autoplay previews POST `/youtubei/v1/player` with `contentPlaybackContext.vis` 5 and
`autoplay` true. Shorts POST it too, with `/shorts/<id>` as `originalUrl` and
`contentPlaybackContext.currentUrl`. Every `player()` and `get_watch` request carries a PO token
(POT-002) and the player script's `signatureTimestamp` (PLAYER-003).

Status: verified: capture 2026-05-29, 2026-05-30, 2026-08-18

### IT-008 Opening the home page first is what gets watch playback held

See GATE-009: a visitor that loaded the home page is held on watch playback, one that went
straight to a watch page is not.

Status: unverified

## The player response

### IT-009 Live and VOD player responses offer different streaming data

A live response's `streamingData` has `expiresInSeconds`, `adaptiveFormats`, `hlsManifestUrl`
and `serverAbrStreamingUrl`. A VOD response has `expiresInSeconds`, `formats`,
`adaptiveFormats` and `serverAbrStreamingUrl`, and no HLS manifest. A blocked video has empty
streaming data and a `playabilityStatus` saying why.

Status: verified: capture 2026-05-29
Fixture: `notes/fixtures/player-response-live.json`, `notes/fixtures/player-response-vod.json`

### IT-010 `adaptiveFormats` entries carry the values the SABR request needs

Each adaptive format carries `itag`, `mimeType`, `lastModified` (the `lmt` of REQ-005),
`contentLength`, `initRange` and `indexRange` (VOD byte spans of the container init and the
`Cues` or `sidx` index), `targetDurationSec` (live segment length, SABR-018), and on
multi-audio-track videos `xtags` and `audioTrack {id, audioIsDefault}` (SABR-045).

Status: verified: capture 2026-05-29
Fixture: `notes/fixtures/player-response-vod.json`

### IT-011 The ustreamer config is in `playerConfig.mediaCommonConfig`

`playerConfig.mediaCommonConfig.mediaUstreamerRequestConfig.videoPlaybackUstreamerConfig` is a
base64 string; decoded, it is request field 5 (REQ-002), sent unchanged on every SABR request
of the session. The same `mediaCommonConfig` holds `dynamicReadaheadConfig` (SABR-021).

Status: verified: capture 2026-05-29

### IT-012 The ustreamer config and PO tokens use mixed base64 alphabets

The ustreamer config arrives in standard or URL-safe base64, with or without padding; the PO
token is base64url. A decoder has to accept both alphabets.

Status: unverified

### IT-013 Live player responses carry `videoDetails.liveChunkReadahead` and `latencyClass`

`videoDetails` of a live response holds `isLive`, `isLiveContent`, `isLowLatencyLiveStream`,
`latencyClass` (SABR-024) and `liveChunkReadahead` (SABR-022). `isLiveContent` stays true on a
finished broadcast's replay; `isLiveNow` tells whether it is live now.

Status: verified: capture 2026-05-29
Fixture: `notes/fixtures/player-response-live.json`

### IT-014 Live playback sends `player/heartbeat` every 15 s

The player response's `heartbeatParams.intervalMilliseconds` is 15000. The player POSTs
`/youtubei/v1/player/heartbeat` with `videoId`, `cpn`, `sequenceNumber`,
`heartbeatServerData` (the opaque value from the player response, then from the previous
heartbeat's answer), `heartbeatRequestParams` and `playbackState`.

Status: verified: capture 2026-05-29, 2026-05-30

## Shorts

### IT-015 The Shorts feed starts with a seedless `reel_item_watch`

Opening the Shorts tab POSTs `/youtubei/v1/reel/reel_item_watch` with `params: "CA8%3D"` (URL-encoded inside the JSON),
`inputType: "REEL_WATCH_INPUT_TYPE_SEEDLESS"` and `disablePlayerResponse: true`. The answer's
`replacementEndpoint.reelWatchEndpoint` is the first Short (`videoId`, `playerParams`,
`params`) and `sequenceContinuation` pages the feed.

Status: verified: capture 2026-05-30

### IT-016 `reel_watch_sequence` lists the next Shorts; the first entry carries its player response

`POST /youtubei/v1/reel/reel_watch_sequence` with `sequenceParams` and `playbackContext`
returns `entries[]`, each a `command.reelWatchEndpoint` (`videoId`, `playerParams`, `params`,
`ustreamerConfig`, a 1080x1920 thumbnail) and a continuation token. Only the first entry
carries `unserializedPrefetchData` with a complete player response, so the player skips
`player()` for it. Each shown Short also gets a `reel_item_watch` call with
`playerRequest {videoId, params}` for its overlay.

Status: unverified

## Reports and feeds

### IT-017 Watch reports are form POSTs to `/api/stats/playback` and `/watchtime`

The player reports a playback with `POST /api/stats/playback` and progress with
`/api/stats/watchtime`, form body `session_token=<ytcfg XSRF_TOKEN>`, keyed by `cpn`, with
`el` (`detailpage`, `shortspage`), `vis`, `state`, `muted` and `cmt` (media time in seconds)
among the query parameters. Shorts report about every 10 s; the first Shorts report comes
4.6 to 8.2 s into playback.

Unverified: the watch page sends `watchtime` at 7, 17, 27, 41 and 81 s after the `playback`
report and every 40 s from there. Each report carries the media spans played since the previous
one (`st`/`et`, comma-joined after a seek). Leaving sends a last `state=paused&final=1` report.

Status: verified: capture 2026-05-29, 2026-05-30

### IT-018 Comment actions are protobuf params naming the action and the comment

A `performCommentActionEndpoint.action` value is URL-encoded base64 of a protobuf whose field 1
is the action (4 remove like, 5 like, 6 dislike, 7 remove dislike) and field 3 the comment id.
A reply's `createReplyParams` arrives as an `engagementToolbarSurfaceEntityPayload` entity
whose key embeds the comment id.

Status: unverified

### IT-019 Members-only items carry a `MEMBERS_ONLY` badge

A `videoRenderer` is members-only when `badges[].metadataBadgeRenderer.style` is
`BADGE_STYLE_TYPE_MEMBERS_ONLY`; a `lockupViewModel` is when a metadata row's
`badges[].badgeViewModel.badgeStyle` is `BADGE_MEMBERS_ONLY`. Thumbnail-overlay badges never
carry it.

Status: unverified

### IT-020 An anonymous home feed is empty

`browse` of `FEwhat_to_watch` without sign-in returns the empty "start searching" shell; signed
in it returns the personalized feed as `lockupViewModel` cards with a continuation.
`FEtrending` answers 400.

Status: unverified

## Transport

### IT-021 IPv6 to googlevideo can black-hole the first POST

On a network whose IPv6 route to googlevideo drops packets, a client that tries IPv6 first with
no fallback hangs about 60 s on the first POST. IPv4 always reaches both `youtube.com` and
googlevideo.

Status: unverified

### IT-022 An anonymous session keeps the cookies YouTube sets

The reference browser's watch-page request carries a `VISITOR_INFO1_LIVE` cookie that YouTube
set on an earlier response. A client that drops `Set-Cookie` presents as a new browser on every
request. A watch page fetched without cookies carries no `animatedThumbnailOverlayViewModel`
entries; with the visitor cookie it carries signed `an_webp` preview URLs for the sidebar
videos (IT-027).

Status: unverified

### IT-023 Paging repeats the endpoint with a `continuation` token

A `continuationItemRenderer` holds `continuationEndpoint.continuationCommand.token`. The next
page is the same endpoint (`browse`, `next`, `search`) with `{context, continuation}` and no
`browseId`, `videoId` or `query`; its answer appends items through `onResponseReceivedActions`
or `onResponseReceivedEndpoints`.

Status: verified: capture 2026-05-30

### IT-024 Watch pages can be walled while the API stays reachable

For 25 to 40 minutes at a time, our client's `/watch` requests were redirected to a
`google.com/sorry` 429 page while InnerTube calls and the player script still answered.
`/iframe_api` still names the current player build during a wall.

Status: unverified

### IT-025 Live HLS is paced to one connection near `initcwndbps`

The live HLS manifest's segments are served over one connection paced to roughly the URL's
`initcwndbps` (about 2.95 Mbit/s), below the bitrate of 1080p60 rungs, so HLS cannot hold the
live edge at those rungs. SABR has no such pacing.

Status: unverified

### IT-026 A channel's Videos-tab sort is a continuation from its chip bar

The Videos tab renders its sort options as `chipBarViewModel` → `chipViewModel`, each with an
`accessibilityLabel` (Latest, Popular, Oldest) and
`tapCommand.innertubeCommand.continuationCommand.token`. Choosing a sort is a `browse`
continuation with that token; Latest is the tab's default order.

Status: unverified

### IT-027 The watch sidebar's hover preview is a signed animated WebP

The sidebar hover preview is a single GET of a signed
`i.ytimg.com/an_webp/<id>/mqdefault_6s.webp` URL: a silent loop of about 6 s, 130 to 150 KB.
The URL's `du`, `sqp` and `rs` parameters come from the watch-page response; a URL built from
the video id alone answers 404.

Status: unverified

### IT-028 A player() POST session starts with empty rounds

A session provisioned by a `player()` POST starts with SABR rounds that carry no media. The
player response embedded in the server-rendered watch page yields media on the first request.

Status: unverified
