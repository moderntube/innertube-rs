#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""Diff two captured `VideoPlaybackAbrRequest` bodies field by field.

Reads the raw wire format rather than the generated types on purpose: the question these
dumps exist to answer is which request the server answers with media, and a schema-aware
decode would silently drop any field we do not already model -- which is exactly where an
unexplained difference would hide.

Nested messages are detected by trying to parse a length-delimited value as a message and
keeping the result only if it consumes the buffer exactly. That heuristic can misread a
string as a message; `--raw` turns it off for a field path when a value looks wrong.

Usage:
  diff_sabr_requests.py a.bin b.bin [--raw 1.2] [--context]
  diff_sabr_requests.py --dump a.bin
"""
import argparse
import sys

WIRE = {0: "varint", 1: "i64", 2: "bytes", 5: "i32"}


def read_varint(buf, i):
    shift = result = 0
    while i < len(buf):
        b = buf[i]
        result |= (b & 0x7F) << shift
        i += 1
        if not b & 0x80:
            return result, i
        shift += 7
        if shift > 63:
            return None
    return None


def parse(buf, raw_paths=(), prefix=""):
    """-> list of (path, wire_type, value). Returns None if `buf` is not a whole message."""
    out, i = [], 0
    while i < len(buf):
        got = read_varint(buf, i)
        if got is None:
            return None
        tag, i = got
        fnum, wt = tag >> 3, tag & 7
        if fnum == 0 or wt not in WIRE:
            return None
        path = f"{prefix}{fnum}"
        if wt == 0:
            got = read_varint(buf, i)
            if got is None:
                return None
            val, i = got
            out.append((path, wt, val))
        elif wt == 1:
            if i + 8 > len(buf):
                return None
            out.append((path, wt, buf[i:i + 8].hex()))
            i += 8
        elif wt == 5:
            if i + 4 > len(buf):
                return None
            out.append((path, wt, buf[i:i + 4].hex()))
            i += 4
        else:
            got = read_varint(buf, i)
            if got is None:
                return None
            ln, i = got
            if i + ln > len(buf):
                return None
            payload = buf[i:i + ln]
            i += ln
            sub = None if path in raw_paths else parse(payload, raw_paths, path + ".")
            if sub is not None and ln > 0:
                out.extend(sub)
            else:
                # Not a message (or forced raw): show it as text when it is printable,
                # otherwise as a length + hex prefix, so a 200-byte token does not
                # dominate the diff.
                try:
                    s = payload.decode("utf-8")
                    printable = s.isprintable()
                except UnicodeDecodeError:
                    printable = False
                out.append((path, wt, f'"{s}"' if printable
                            else f"<{ln}B {payload[:16].hex()}{'...' if ln > 16 else ''}>"))
    return out


def flat(path_):
    d = {}
    for p, wt, v in path_:
        d.setdefault(p, []).append(v)
    return d


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("a")
    ap.add_argument("b", nargs="?")
    ap.add_argument("--raw", action="append", default=[],
                    help="field path to NOT recurse into (repeatable)")
    ap.add_argument("--dump", action="store_true", help="print one file's fields")
    args = ap.parse_args()

    pa = parse(open(args.a, "rb").read(), tuple(args.raw))
    if pa is None:
        sys.exit(f"{args.a}: not a parseable protobuf message")
    if args.dump or not args.b:
        for p, wt, v in pa:
            print(f"  {p:<14} {WIRE[wt]:<7} {v}")
        return
    pb = parse(open(args.b, "rb").read(), tuple(args.raw))
    if pb is None:
        sys.exit(f"{args.b}: not a parseable protobuf message")

    fa, fb = flat(pa), flat(pb)
    keys = sorted(set(fa) | set(fb), key=lambda k: [int(x) for x in k.split(".")])
    same = diff = 0
    for k in keys:
        va, vb = fa.get(k), fb.get(k)
        if va == vb:
            same += 1
            continue
        diff += 1
        print(f"  {k:<14} A={va if va is not None else '(absent)'}")
        print(f"  {'':<14} B={vb if vb is not None else '(absent)'}")
    print(f"\n{len(keys)} field paths: {same} identical, {diff} differing")
    print(f"A={args.a} ({len(open(args.a,'rb').read())} B)  B={args.b} "
          f"({len(open(args.b,'rb').read())} B)")


if __name__ == "__main__":
    main()
