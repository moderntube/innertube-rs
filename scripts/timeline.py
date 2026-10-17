#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["mitmproxy"]
# ///
"""The control-plane requests in a mitmproxy capture: everything except media and assets.

  timeline.py requests  <capture.flow>                 in time order; youtubei POSTs show
                                                        their body keys, log_event its events
  timeline.py inventory <capture.flow> [path-substr ...]
                                                        host+path counts, then the query keys
                                                        of each endpoint with an example value
  timeline.py stats     <capture.flow> [docid ...]     /api/stats/{playback,watchtime} params
"""
import argparse
import json
from collections import Counter, defaultdict
from urllib.parse import parse_qs, parse_qsl, urlparse

from mitmproxy import io

SKIP = ("videoplayback", "i.ytimg.com", "yt3.ggpht", "yt3.googleusercontent", ".js", ".css", ".woff",
        "/s/player/", "fonts.g", "/s/_/", "/s/desktop/", "gstatic.com/images")
STATS_KEYS = ("el", "docid", "state", "cmt", "st", "et", "final", "len", "rtn", "referrer", "list", "plid")


def flows(path):
    with open(path, "rb") as fh:
        for fl in io.FlowReader(fh).stream():
            req = getattr(fl, "request", None)
            if req is not None:
                yield fl, req


def describe(req):
    u = urlparse(req.pretty_url)
    q = parse_qs(u.query)
    extra = ""
    body = req.content or b""
    if "/youtubei/" in u.path and body:
        try:
            j = json.loads(body)
            if u.path.endswith("log_event"):
                names = [k for ev in j.get("events", []) for k in ev if k not in ("eventTimeMs", "context", "eventId")]
                extra = "events=" + ",".join(names)
            else:
                extra = "keys=" + ",".join(k for k in j if k != "context")
                if "videoId" in j:
                    extra += f" videoId={j['videoId']}"
        except (ValueError, AttributeError):
            extra = f"body={len(body)}B"
    elif "/api/stats/" in u.path:
        extra = " ".join(f"{k}={q[k][0][:24]}" for k in ("docid", "cmt", "st", "et", "state", "final", "rtn") if k in q)
    elif body:
        extra = f"body={len(body)}B"
    return f"{req.method:4} {u.netloc}{u.path}", extra


def cmd_requests(args):
    rows = []
    for fl, req in flows(args.flow):
        if any(s in req.pretty_url for s in SKIP):
            continue
        status = fl.response.status_code if getattr(fl, "response", None) else "-"
        rows.append((req.timestamp_start, status, *describe(req)))
    rows.sort()
    t0 = rows[0][0] if rows else 0
    for t, status, name, extra in rows:
        print(f"{t - t0:8.2f}s {status!s:>4} {name}  {extra}")


def cmd_inventory(args):
    paths = Counter()
    keys = defaultdict(dict)
    for _, req in flows(args.flow):
        url = req.pretty_url
        if any(s in url for s in SKIP):
            continue
        u = urlparse(url)
        name = f"{req.method} {u.netloc}{u.path}"
        paths[name] += 1
        if args.focus and not any(f in u.path for f in args.focus):
            continue
        for k, v in parse_qsl(u.query, keep_blank_values=True):
            keys[name].setdefault(k, v[:60])
    for name, n in paths.most_common(60):
        print(f"{n:5}  {name}")
    for name, kv in keys.items():
        print(f"\n== {name}\n  " + " ".join(f"{k}={v}" for k, v in kv.items()))


def cmd_stats(args):
    ids = set(args.docids)
    rows = []
    for _, req in flows(args.flow):
        if "/api/stats/" not in req.path:
            continue
        u = urlparse(req.pretty_url)
        kind = u.path.rsplit("/", 1)[-1]
        if kind not in ("playback", "watchtime"):
            continue
        q = {k: v[0] for k, v in parse_qs(u.query).items()}
        if ids and q.get("docid") not in ids:
            continue
        rows.append((req.timestamp_start, kind, q))
    rows.sort(key=lambda r: r[0])
    t0 = rows[0][0] if rows else 0
    for t, kind, q in rows:
        print(f"{t - t0:8.2f}s {kind:9} " + " ".join(f"{k}={q[k][:60]}" for k in STATS_KEYS if k in q))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("requests", help="control-plane requests in time order")
    p.add_argument("flow")
    p.set_defaults(fn=cmd_requests)
    p = sub.add_parser("inventory", help="endpoint counts and query keys")
    p.add_argument("flow")
    p.add_argument("focus", nargs="*", help="only list query keys for paths containing one of these")
    p.set_defaults(fn=cmd_inventory)
    p = sub.add_parser("stats", help="playback and watchtime stats pings")
    p.add_argument("flow")
    p.add_argument("docids", nargs="*")
    p.set_defaults(fn=cmd_stats)
    args = ap.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
