#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["mitmproxy"]
# ///
"""Cut one SABR session out of a mitmproxy capture as a scrubbed replay fixture.

The fixture is read by `crates/innertube-sabr/tests/support/replay.rs`. It is a UMP
stream of three record types:

  1 META      `key=value` lines: session (a descriptive label), live
  2 REQUEST   the reference client's `VideoPlaybackAbrRequest` body
  3 RESPONSE  the server's UMP body, MEDIA parts rewritten as MEDIA_ELIDED

MEDIA_ELIDED (part type 1021) holds the header id, then runs of
`[literal len][literal][zero-run len]`. Container structure (MP4 boxes other than the
`mdat` body, WebM up to the first Cluster's timecode) is kept; sample data becomes a
zero run, so the fixture holds no media content. Init segments are kept whole.

Opaque session blobs (ustreamer config, PO token, playback-cookie blob, SABR context
values, redirect URLs, cuepoint ids) are replaced by filler of the same length
everywhere they occur, so echoes still match.

Usage:
  extract_replay_session.py <capture.flow> <video-id prefix> <cpn prefix> <out> --label L [--rounds N] [--live]
"""
import argparse
import sys
from collections.abc import Iterator
from urllib.parse import parse_qs, urlparse

from mitmproxy.io import FlowReader

META, REQUEST, RESPONSE = 1, 2, 3
MEDIA_HEADER, MEDIA, MEDIA_END = 20, 21, 22
NEXT_REQUEST_POLICY, SABR_REDIRECT, SABR_CONTEXT_UPDATE, CUEPOINT_LIST = 35, 43, 57, 69
MEDIA_ELIDED = 1021


def read_uvar(b: bytes, i: int) -> tuple[int, int]:
    b0 = b[i]
    if b0 < 0x80:
        return b0, i + 1
    if b0 < 0xC0:
        return (b0 & 0x3F) | (b[i + 1] << 6), i + 2
    if b0 < 0xE0:
        return (b0 & 0x1F) | (b[i + 1] << 5) | (b[i + 2] << 13), i + 3
    if b0 < 0xF0:
        return (b0 & 0x0F) | (b[i + 1] << 4) | (b[i + 2] << 12) | (b[i + 3] << 20), i + 4
    return int.from_bytes(b[i + 1:i + 5], "little"), i + 5


def write_uvar(v: int) -> bytes:
    if v < 0x80:
        return bytes([v])
    if v < 0x4000:
        return bytes([0x80 | (v & 0x3F), v >> 6])
    if v < 0x200000:
        return bytes([0xC0 | (v & 0x1F), (v >> 5) & 0xFF, v >> 13])
    if v < 0x10000000:
        return bytes([0xE0 | (v & 0x0F), (v >> 4) & 0xFF, (v >> 12) & 0xFF, v >> 20])
    return bytes([0xF0]) + v.to_bytes(4, "little")


def ump_parts(b: bytes) -> list[tuple[int, bytes]]:
    out, i = [], 0
    while i < len(b):
        t, i = read_uvar(b, i)
        n, i = read_uvar(b, i)
        out.append((t, b[i:i + n]))
        i += n
    return out


def ump_part(t: int, payload: bytes) -> bytes:
    return write_uvar(t) + write_uvar(len(payload)) + payload


def read_pvar(b: bytes, i: int) -> tuple[int, int]:
    shift = val = 0
    while True:
        x = b[i]
        i += 1
        val |= (x & 0x7F) << shift
        if not x & 0x80:
            return val, i
        shift += 7


def pb_fields(b: bytes) -> Iterator[tuple[int, int, int | bytes]]:
    i = 0
    while i < len(b):
        key, i = read_pvar(b, i)
        f, w = key >> 3, key & 7
        if w == 0:
            v, i = read_pvar(b, i)
            yield f, w, v
        elif w == 2:
            n, i = read_pvar(b, i)
            yield f, w, b[i:i + n]
            i += n
        elif w == 5:
            yield f, w, b[i:i + 4]
            i += 4
        elif w == 1:
            yield f, w, b[i:i + 8]
            i += 8
        else:
            raise ValueError(f"wire type {w}")


def pb_get(b: bytes, *path: int) -> list[int | bytes]:
    """Every value at a field path, descending through length-delimited fields."""
    vals: list[int | bytes] = [b]
    for f in path:
        nxt: list[int | bytes] = []
        for v in vals:
            if isinstance(v, bytes):
                nxt += [x for ff, _, x in pb_fields(v) if ff == f]
        vals = nxt
    return vals


def container_keep(data: bytes) -> list[tuple[int, int]]:
    """Byte spans of `data` that hold container structure, not samples."""
    if data[:4] == b"\x1a\x45\xdf\xa3" or data[:4] == b"\x1f\x43\xb6\x75":
        at = data.find(b"\x1f\x43\xb6\x75")
        if at < 0:
            return [(0, len(data))]
        return [(0, min(len(data), at + 4 + 8 + 4))]
    spans, i = [], 0
    while i + 8 <= len(data):
        size = int.from_bytes(data[i:i + 4], "big")
        kind = data[i + 4:i + 8]
        hdr = 8
        if size == 1:
            size, hdr = int.from_bytes(data[i + 8:i + 16], "big"), 16
        if size < hdr or not kind.isalpha():
            break
        spans.append((i, i + (hdr if kind == b"mdat" else size)))
        i += size
    return spans


def elide(header_id: int, payload: bytes, keep: list[tuple[int, int]], base: int) -> bytes:
    """MEDIA_ELIDED for one MEDIA part covering `[base, base + len(payload))` of its segment."""
    runs: list[tuple[bytes, int]] = []
    lit, pos = bytearray(), 0
    for a, b in keep:
        a, b = max(a - base, pos), min(b - base, len(payload))
        if a >= b:
            continue
        if a > pos:
            runs.append((bytes(lit), a - pos))
            lit = bytearray()
        lit += payload[a:b]
        pos = b
    runs.append((bytes(lit), len(payload) - pos))
    out = bytearray(write_uvar(header_id))
    for lit_bytes, zeros in runs:
        out += write_uvar(len(lit_bytes)) + lit_bytes + write_uvar(zeros)
    return bytes(out)


def scrub_media(parts: list[tuple[int, bytes]]) -> list[tuple[int, bytes]]:
    is_init: dict[int, bool] = {}
    chunks: dict[int, list[int]] = {}
    for idx, (t, p) in enumerate(parts):
        if t == MEDIA_HEADER:
            hid = pb_get(p, 1)
            init = pb_get(p, 8)
            is_init[hid[0] if hid else 0] = bool(init and init[0])
        elif t == MEDIA:
            hid, _ = read_uvar(p, 0)
            chunks.setdefault(hid, []).append(idx)
    out = list(parts)
    for hid, idxs in chunks.items():
        bodies = []
        for idx in idxs:
            p = parts[idx][1]
            _, o = read_uvar(p, 0)
            bodies.append(p[o:])
        whole = b"".join(bodies)
        keep = [(0, len(whole))] if is_init.get(hid) else container_keep(whole)
        base = 0
        for idx, body in zip(idxs, bodies):
            out[idx] = (MEDIA_ELIDED, elide(hid, body, keep, base))
            base += len(body)
    return out


def secrets_of(req: bytes, resp_parts: list[tuple[int, bytes]]) -> list[bytes]:
    found: list[int | bytes] = []
    found += pb_get(req, 5)
    found += pb_get(req, 19, 2)
    for t, p in resp_parts:
        if t == NEXT_REQUEST_POLICY:
            found += pb_get(p, 7, 20)
        elif t == SABR_CONTEXT_UPDATE:
            found += pb_get(p, 3, 2)
            found += pb_get(p, 3, 1, 3, 1)
        elif t == SABR_REDIRECT:
            found += pb_get(p, 1)
        elif t == CUEPOINT_LIST:
            found += pb_get(p, 1, 1, 5) + pb_get(p, 1, 1, 6)
    return [s for s in found if isinstance(s, bytes) and len(s) >= 8]


def filler(i: int, secret: bytes) -> bytes:
    if secret.startswith(b"https://"):
        head = b"https://scrubbed.invalid/"
        return (head + b"x" * len(secret))[:len(secret)]
    return (b"SCRUB%03d" % i * (len(secret) // 8 + 1))[:len(secret)]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("flow")
    ap.add_argument("video")
    ap.add_argument("cpn")
    ap.add_argument("out")
    ap.add_argument("--rounds", type=int, default=0)
    ap.add_argument("--live", action="store_true")
    ap.add_argument("--label", required=True)
    a = ap.parse_args()

    rounds: list[tuple[float, bytes, bytes]] = []
    with open(a.flow, "rb") as fh:
        for f in FlowReader(fh).stream():
            r = getattr(f, "request", None)
            if r is None or r.method != "POST" or "videoplayback" not in r.path or f.response is None:
                continue
            q = parse_qs(urlparse(r.url).query)
            if q.get("id", [""])[0].startswith(a.video) and q.get("cpn", [""])[0].startswith(a.cpn):
                rounds.append((r.timestamp_start, r.raw_content or b"", f.response.raw_content or b""))
    rounds.sort(key=lambda x: x[0])
    if a.rounds:
        rounds = rounds[:a.rounds]
    if not rounds:
        sys.exit("no matching session")

    parsed = [(req, ump_parts(resp)) for _, req, resp in rounds]
    secrets: dict[bytes, bytes] = {}
    for req, parts in parsed:
        for s in secrets_of(req, parts):
            secrets.setdefault(s, filler(len(secrets), s))
    ordered = sorted(secrets.items(), key=lambda kv: -len(kv[0]))

    def scrub(b: bytes) -> bytes:
        for s, fill in ordered:
            b = b.replace(s, fill)
        return b

    meta = f"session={a.label}\nlive={int(a.live)}\n"
    out = bytearray(ump_part(META, meta.encode()))
    for req, parts in parsed:
        out += ump_part(REQUEST, scrub(req))
        scrubbed = [(t, scrub(p)) for t, p in parts]
        body = b"".join(ump_part(t, p) for t, p in scrub_media(scrubbed))
        out += ump_part(RESPONSE, body)
    with open(a.out, "wb") as fh:
        fh.write(out)
    print(f"{len(parsed)} rounds, {len(secrets)} blobs scrubbed, {len(out)} bytes -> {a.out}")


if __name__ == "__main__":
    main()
