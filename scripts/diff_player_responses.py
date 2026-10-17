#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""Diff two raw `player()` responses.

The ustreamer config is fixed inside `player()`, before the first SABR request, so two
sessions that behave differently are compared here first.

Reports keys present in only one side, and scalar values that differ. Long strings are
elided to their length and a prefix so a signature or a token does not dominate the output,
and `--secrets` is required to print them in full.

Structural difference is what to read here: a key present on one side and absent on the
other. Value differences are mostly session noise and are printed last.

**Compare the same video, or the output is unusable.** Two different videos differ in their
format ladder, recommendations and metadata, which swamps the signal. For a cross-video
comparison, decode the `videoPlaybackUstreamerConfig` out of each response and diff those
with `diff_sabr_requests.py`.

`it player <video_id> -o out.json` writes one response.

Usage: diff_player_responses.py a.json b.json [--max-value 60] [--only-structural]
"""
import argparse
import json


def walk(node, prefix=""):
    """-> {path: scalar}. Lists index by position; that is right for the ladders here,
    which are order-stable per response."""
    out = {}
    if isinstance(node, dict):
        for k, v in node.items():
            out.update(walk(v, f"{prefix}.{k}" if prefix else k))
    elif isinstance(node, list):
        for i, v in enumerate(node):
            out.update(walk(v, f"{prefix}[{i}]"))
    else:
        out[prefix] = node
    return out


def show(v, cap):
    s = v if isinstance(v, str) else json.dumps(v)
    if isinstance(v, str) and len(s) > cap:
        return f"<{len(s)} chars: {s[:cap]}...>"
    return s


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("a")
    ap.add_argument("b")
    ap.add_argument("--max-value", type=int, default=60)
    ap.add_argument("--only-structural", action="store_true")
    args = ap.parse_args()

    fa = walk(json.load(open(args.a)))
    fb = walk(json.load(open(args.b)))
    only_a = sorted(set(fa) - set(fb))
    only_b = sorted(set(fb) - set(fa))
    both = sorted(set(fa) & set(fb))
    diff = [k for k in both if fa[k] != fb[k]]

    print(f"paths: A={len(fa)}  B={len(fb)}  shared={len(both)}  "
          f"A-only={len(only_a)}  B-only={len(only_b)}  differing={len(diff)}\n")

    if only_a:
        print(f"--- present ONLY in A ({args.a}) ---")
        for k in only_a:
            print(f"  {k} = {show(fa[k], args.max_value)}")
    if only_b:
        print(f"\n--- present ONLY in B ({args.b}) ---")
        for k in only_b:
            print(f"  {k} = {show(fb[k], args.max_value)}")
    if diff and not args.only_structural:
        print(f"\n--- differing values ({len(diff)}) ---")
        for k in diff:
            print(f"  {k}")
            print(f"      A={show(fa[k], args.max_value)}")
            print(f"      B={show(fb[k], args.max_value)}")


if __name__ == "__main__":
    main()
