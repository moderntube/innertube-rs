#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["mitmproxy"]
# ///
"""Decode SABR rounds from a mitmproxy capture or from our own raw dump.

A source is either a `.flow` capture of the web client or the `raw/sabr` directory that
`INNERTUBE_DUMP=raw:<dir>` writes (`req_NNNN.bin`, `resp_NNNN.bin`, `url_NNNN.txt`).
Both go through the same schema-free decoder, so the two sides are comparable.

  sabr.py requests  <src> [--fields 20,23,43]   one line per POST: player time, read-ahead,
                                                 formats, buffered ranges, viewport
  sabr.py responses <src> [--raw 69,47] [--empty] [--live]
                                                 one line per round: media, segments, live
                                                 metadata, backoff, part types; --empty adds
                                                 the next request on the same cpn
  sabr.py ssap      <src>                        request field 24 and CUEPOINT_LIST parts
  sabr.py extract   <src> [<index> <out.bin>]    list POSTs, or write one request body for
                                                 diff_sabr_requests.py

Field numbers follow the vendored googlevideo protos in crates/innertube-sabr/proto/.
"""
import argparse
import glob
import os
import struct
import sys
from urllib.parse import parse_qs, urlparse

LIVE_EDGE_SENTINEL = 2**53 - 1

MEDIA_HEADER, MEDIA, LIVE_METADATA, NEXT_REQUEST_POLICY, CUEPOINT_LIST = 20, 21, 31, 35, 69


def pb_varint(b, i):
    shift = val = 0
    while True:
        byte = b[i]
        i += 1
        val |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return val, i
        shift += 7


def fields(b):
    """Yield (field_number, wire_type, value) for one protobuf message."""
    i, n = 0, len(b)
    while i < n:
        tag, i = pb_varint(b, i)
        fnum, wire = tag >> 3, tag & 7
        if wire == 0:
            val, i = pb_varint(b, i)
            yield fnum, wire, val
        elif wire == 1:
            yield fnum, wire, b[i:i + 8]
            i += 8
        elif wire == 2:
            ln, i = pb_varint(b, i)
            yield fnum, wire, b[i:i + ln]
            i += ln
        elif wire == 5:
            yield fnum, wire, b[i:i + 4]
            i += 4
        else:
            raise ValueError(f"bad wire type {wire} at {i}")


def ump_varint(b, i):
    b0 = b[i]
    if b0 < 0x80:
        return b0, i + 1
    if b0 < 0xC0:
        return (b0 & 0x3F) + 64 * b[i + 1], i + 2
    if b0 < 0xE0:
        return (b0 & 0x1F) + 32 * (b[i + 1] + 256 * b[i + 2]), i + 3
    if b0 < 0xF0:
        return (b0 & 0x0F) + 16 * (b[i + 1] + 256 * (b[i + 2] + 256 * b[i + 3])), i + 4
    return int.from_bytes(b[i + 1:i + 5], "little"), i + 5


def ump_parts(b):
    """[(part_type, payload)] of one UMP response body."""
    i, out = 0, []
    while i < len(b):
        t, i = ump_varint(b, i)
        n, i = ump_varint(b, i)
        out.append((t, b[i:i + n]))
        i += n
    return out


class Round:
    def __init__(self, index, t, url, req, resp):
        self.index, self.t, self.url, self.req, self.resp = index, t, url, req, resp
        q = parse_qs(urlparse(url).query)
        self.cpn = q.get("cpn", ["?"])[0]
        self.live = "yt_live_broadcast" in q.get("source", [""])[0] or "live" in q
        self.host = (urlparse(url).hostname or "?").split(".")[0]


def rounds(src):
    """Every SABR POST in `src`, in request order."""
    if os.path.isdir(src):
        for n, path in enumerate(sorted(glob.glob(os.path.join(src, "req_*.bin")))):
            num = os.path.basename(path)[4:8]
            url_path = os.path.join(src, f"url_{num}.txt")
            resp_path = os.path.join(src, f"resp_{num}.bin")
            url = open(url_path).read() if os.path.exists(url_path) else ""
            resp = open(resp_path, "rb").read() if os.path.exists(resp_path) else None
            yield Round(n, float(n), url, open(path, "rb").read(), resp)
        return
    from mitmproxy import io as mio

    out = []
    with open(src, "rb") as fh:
        for flow in mio.FlowReader(fh).stream():
            req = getattr(flow, "request", None)
            if req is None or req.method != "POST" or "videoplayback" not in req.path:
                continue
            body = req.raw_content or b""
            if not body:
                continue
            resp = flow.response.content if getattr(flow, "response", None) else None
            out.append((req.timestamp_start, req.pretty_url, body, resp))
    out.sort(key=lambda r: r[0])
    for n, (t, url, body, resp) in enumerate(out):
        yield Round(n, t, url, body, resp)


def format_itag(payload):
    for f, w, v in fields(payload):
        if f == 1 and w == 0:
            return v
    return None


def parse_request(body, extra=()):
    """The fields of one VideoPlaybackAbrRequest that describe the client's position.

    `extra` names further ClientAbrState varint fields to return under `abr`."""
    d = {"pt": None, "selected": [], "pref_v": [], "pref_a": [], "ranges": [], "vis": None,
         "vw": None, "vh": None, "abr": {}}
    for f, w, v in fields(body):
        if f == 1 and w == 2:
            for sf, sw, sv in fields(v):
                if sw != 0:
                    continue
                if sf == 28:
                    d["pt"] = sv
                elif sf == 34:
                    d["vis"] = sv
                elif sf == 18:
                    d["vw"] = sv
                elif sf == 19:
                    d["vh"] = sv
                if sf in extra:
                    d["abr"][sf] = sv
        elif f == 2 and w == 2:
            d["selected"].append(format_itag(v))
        elif f == 17 and w == 2:
            d["pref_v"].append(format_itag(v))
        elif f == 16 and w == 2:
            d["pref_a"].append(format_itag(v))
        elif f == 4 and w == 0 and d["pt"] is None:
            d["pt"] = v
        elif f == 3 and w == 2:
            r = {1: None, 2: None, 3: None, 4: None, 5: None}
            for sf, sw, sv in fields(v):
                if sf == 1 and sw == 2:
                    r[1] = format_itag(sv)
                elif sf in r and sw == 0:
                    r[sf] = sv
            d["ranges"].append((r[1], r[2], r[3], r[4], r[5]))
    return d


def fmt_pt(pt):
    if pt is None:
        return "-"
    return "EDGE" if pt == LIVE_EDGE_SENTINEL else str(pt)


def int_list(s):
    return [int(x) for x in s.split(",") if x]


def cmd_requests(args):
    extra = int_list(args.fields)
    rows = list(rounds(args.src))
    if not rows:
        print("no videoplayback POSTs")
        return
    t0 = rows[0].t
    readaheads = []
    cols = "".join(f" {'f' + str(f):>10}" for f in extra)
    print(f"{'#':>4} {'t(s)':>7} {'host':>14} {'cpn':>6} {'player_t':>14} {'ahead_s':>7} {'sel':>10} "
          f"{'prefV':>8} {'prefA':>6} {'vis':>3} {'viewport':>10}{cols}  buffered itag(dur,segs)")
    for r in rows:
        try:
            d = parse_request(r.req, extra)
        except (ValueError, IndexError) as e:
            print(f"{r.index:4} parse error: {e}", file=sys.stderr)
            continue
        ends = [s + du for _, s, du, _, _ in d["ranges"] if s is not None and du is not None]
        ahead = ""
        if ends and d["pt"] not in (None, LIVE_EDGE_SENTINEL):
            ra = max(ends) - d["pt"]
            readaheads.append(ra)
            ahead = f"{ra / 1000:.1f}"
        sel = ",".join(map(str, d["selected"])) or "-"
        pv = ",".join(map(str, d["pref_v"])) or "-"
        pa = ",".join(map(str, d["pref_a"])) or "-"
        vp = f"{d['vw']}x{d['vh']}" if d["vw"] else "-"
        rng = " ".join(
            f"{itag}({du / 1000:.0f}s,{a}-{b})" if du is not None else f"{itag}(?,{a}-{b})"
            for itag, _, du, a, b in d["ranges"] if itag is not None
        ) or "-"
        vals = "".join(f" {d['abr'].get(f, '-')!s:>10}" for f in extra)
        print(f"{r.index:4} {r.t - t0:7.1f} {r.host:>14} {r.cpn[:6]:>6} {fmt_pt(d['pt']):>14} {ahead:>7} "
              f"{sel:>10} {pv:>8} {pa:>6} {d['vis']!s:>3} {vp:>10}{vals}  {rng}")
    if readaheads:
        rs = sorted(readaheads)
        print(f"\nread-ahead (ms): min={rs[0]} median={rs[len(rs) // 2]} "
              f"p95={rs[int(len(rs) * 0.95)]} max={rs[-1]}", file=sys.stderr)


def flat(payload, depth=0):
    out = []
    try:
        for f, w, v in fields(payload):
            if w == 2 and depth < 3:
                inner = flat(v, depth + 1)
                out.append(f"{f}:{{{inner}}}" if inner and "?" not in inner else f"{f}:{v[:48]!r}")
            else:
                out.append(f"{f}:{v if w == 0 else v.hex()}")
    except (ValueError, IndexError):
        return "?"
    return " ".join(out)


def cmd_responses(args):
    raw_types = set(int_list(args.raw))
    rows = [r for r in rounds(args.src) if r.resp is not None and (r.live or not args.live)]
    t0 = rows[0].t if rows else 0
    next_on_cpn = {}
    for r in reversed(rows):
        r.next = next_on_cpn.get(r.cpn)
        next_on_cpn[r.cpn] = r
    empty = 0
    for r in rows:
        try:
            parts = ump_parts(r.resp)
            pt = parse_request(r.req)["pt"]
        except (ValueError, IndexError):
            continue
        has_media = any(t == MEDIA for t, _ in parts)
        if args.empty and (has_media or pt in (None, LIVE_EDGE_SENTINEL)):
            continue
        then = ""
        if args.empty:
            nxt = r.next
            if nxt is None:
                then = " -> end of cpn"
            else:
                npt = parse_request(nxt.req)["pt"]
                dpt = "EDGE" if npt == LIVE_EDGE_SENTINEL else (npt - pt if npt is not None else "-")
                then = f" -> next +{(nxt.t - r.t) * 1000:.0f}ms dpt={dpt}"
        empty += not has_media
        segs, media_bytes, notes = [], 0, []
        for t, p in parts:
            kv = {}
            if t in (MEDIA_HEADER, LIVE_METADATA, NEXT_REQUEST_POLICY):
                kv = {f: v for f, w, v in fields(p) if w == 0}
            if t == MEDIA_HEADER and not kv.get(8):
                segs.append(f"it{kv.get(3)}#{kv.get(9)}@{kv.get(11)}+{kv.get(12)}")
            elif t == MEDIA:
                media_bytes += len(p) - 1
            elif t == LIVE_METADATA:
                notes.append(f"head_seq={kv.get(3)} head_ms={kv.get(4)} "
                             f"min_seek={kv.get(12)}/{kv.get(13)} max_seek={kv.get(14)}/{kv.get(15)}")
            elif t == NEXT_REQUEST_POLICY and 4 in kv:
                notes.append(f"backoff={kv[4]}")
            if t in raw_types:
                notes.append(f"[{t}] {flat(p)}")
        types = sorted({t for t, _ in parts})
        print(f"{r.index:4} {r.t - t0:7.1f} cpn={r.cpn[:6]} pt={fmt_pt(pt)} resp={len(r.resp)}B "
              f"media={media_bytes}B parts={types} segs={' '.join(segs) or '-'} | {' | '.join(notes)}{then}")
    print(f"{len(rows)} rounds, {empty} without media", file=sys.stderr)


SSAP = {1: "ad_cpns", 2: "ads_config", 3: "dai_state", 4: "skip_sequence", 5: "f5", 6: "f6",
        7: "cuepoint_id", 8: "state", 9: "tile_context", 10: "start_time_ms", 11: "duration_ms"}
SSAP_STATE = {0: "UNKNOWN", 1: "TIMEOUT", 2: "RATECONTROL", 3: "SERVER_ERROR", 4: "DELETED", 5: "WAITING",
              6: "READY", 7: "SSDISABLED", 8: "INACTIVE", 9: "SSDISABLED_USTREAMER",
              10: "SSDISABLED_NO_CONFIG", 11: "RATECONTROL_CLIENT", 12: "NO_DAI_CONFIG_RETURNED_IN_GAB",
              13: "CACHE_MISS"}
CUE_EVENT = {1: "START", 2: "CONTINUE", 3: "STOP", 4: "INSERTION", 5: "PREDICT_START", 6: "PREFETCH"}


def ssap(p):
    out = []
    for f, w, v in fields(p):
        if f == 8:
            v = SSAP_STATE.get(v, v)
        elif w == 2:
            v = v.decode(errors="replace")[:60]
        out.append(f"{SSAP.get(f, f'f{f}')}={v}")
    return " ".join(out)


def cue(p):
    out = []
    for f, w, v in fields(p):
        if f != 1 or w != 2:
            continue
        info = {f2: v2 for f2, _, v2 in fields(v)}
        c = {f3: v3 for f3, _, v3 in fields(info.get(1, b""))}

        def dbl(k):
            return round(struct.unpack("<d", c[k])[0], 2) if k in c else None

        out.append(f"type={c.get(1)} event={CUE_EVENT.get(c.get(2), c.get(2))} total={dbl(3)} "
                   f"playhead={dbl(4)} id={c.get(6, b'').decode(errors='replace')} "
                   f"start_seq={c.get(9)} track={info.get(2)} seq={info.get(3)}")
    return " | ".join(out)


def cmd_ssap(args):
    for r in rounds(args.src):
        pt = parse_request(r.req)["pt"]
        for f, w, v in fields(r.req):
            if f == 24 and w == 2:
                print(f"{r.index:4} cpn={r.cpn[:6]} pt={fmt_pt(pt)} REQ f24: {ssap(v)}")
        parts = ump_parts(r.resp or b"")
        media = sum(len(p) for t, p in parts if t == MEDIA)
        for t, p in parts:
            if t == CUEPOINT_LIST:
                print(f"{r.index:4} cpn={r.cpn[:6]} pt={fmt_pt(pt)} media={media}B RESP cue: {cue(p)}")


def cmd_extract(args):
    def summary(body):
        d = parse_request(body)
        return (f"{len(body)}B player_t={fmt_pt(d['pt'])} sel={'y' if d['selected'] else 'n'} "
                f"viewport={d['vw']}x{d['vh']}")

    for r in rounds(args.src):
        if args.index is None:
            print(f"{r.index:4} {summary(r.req)}")
        elif r.index == args.index:
            with open(args.out, "wb") as fh:
                fh.write(r.req)
            print(f"wrote {args.out}: {summary(r.req)}")
            return 0
    if args.index is not None:
        print(f"no request at index {args.index}", file=sys.stderr)
        return 1
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("requests", help="one line per POST")
    p.add_argument("src")
    p.add_argument("--fields", default="", help="extra ClientAbrState varint fields, e.g. 20,23,43")
    p.set_defaults(fn=cmd_requests)
    p = sub.add_parser("responses", help="one line per round")
    p.add_argument("src")
    p.add_argument("--raw", default="", help="part types to dump field by field, e.g. 69,47")
    p.add_argument("--empty", action="store_true", help="only rounds without MEDIA, with the next request")
    p.add_argument("--live", action="store_true", help="only live rounds")
    p.set_defaults(fn=cmd_responses)
    p = sub.add_parser("ssap", help="SsapPlaybackInfo and cuepoints")
    p.add_argument("src")
    p.set_defaults(fn=cmd_ssap)
    p = sub.add_parser("extract", help="list POSTs or write one request body")
    p.add_argument("src")
    p.add_argument("index", nargs="?", type=int)
    p.add_argument("out", nargs="?")
    p.set_defaults(fn=cmd_extract)
    args = ap.parse_args()
    if args.cmd == "extract" and (args.index is None) != (args.out is None):
        ap.error("extract takes both <index> and <out.bin>, or neither")
    return args.fn(args) or 0


if __name__ == "__main__":
    sys.exit(main())
