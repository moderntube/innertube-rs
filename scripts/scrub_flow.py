#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["mitmproxy"]
# ///
"""Scrub a mitmproxy .flow capture of auth tokens / credentials / PII.

Cleans a logged-in capture *without a human reading the secrets*: it collects identity
literals (client IP, visitorData) mechanically from headers/params, then:

  - DROPS credential-exchange hosts entirely (accounts.google.com, autofill, ...)
  - REDACTS sensitive request/response header VALUES (cookie, authorization, ...)
  - REDACTS sensitive URL query-param VALUES (pot, sig, n, key, ip, ...)
  - On youtube.com TEXT bodies: literal + key-based + high-entropy token redaction
  - STRIPS all other bodies (media/binary/static) to empty

Verification (`--verify`) re-reads the output and reports only MATCH COUNTS for
secret indicators — never their contents — so "zero secrets" can be confirmed
blind. Iterate until all counts are 0.

Usage:
  scrub_flow.py <in.flow> <out.flow>     # scrub
  scrub_flow.py --verify <out.flow>      # blind verification (counts only)

PII scope: auth tokens/creds + email + client IP + visitorData.
Public identity (display name, channel id, avatar, history titles) is LEFT INTACT.
"""
import sys, re
from mitmproxy.io import FlowReader, FlowWriter

# ---- host classification ----------------------------------------------------
DROP_SUBSTR = (
    "accounts.google", "accounts.youtube", "content-autofill",
    "android.clients.google", "play.google", "optimizationguide-pa",
    "ogads-pa", "update.googleapis", "signaler-pa.googleapis",
    "clients6.google.com",  # ads/services; suggestqueries-*.youtube.com is kept (different domain)
)
DROP_EXACT = {"www.google.com"}

def host_dropped(h):
    return h in DROP_EXACT or any(s in h for s in DROP_SUBSTR)

def keeps_text_body(h):
    return h.endswith("youtube.com")

TEXT_CT = ("text/html", "application/json", "text/javascript",
           "application/javascript", "application/x-www-form-urlencoded", "text/plain")

# ---- header / param denylists ----------------------------------------------
SENS_HEADER_RE = re.compile(
    r"(cookie|authorization|auth|token|sapisid|secure|visitor|session|csrf|xsrf|"
    r"identity|x-goog-pageid|x-goog-authuser|x-client-data|x-goog-device|"
    r"x-goog-iam|x-goog-acl|x-browser|x-chrome|x-goog-meta|bearer)", re.I)
SAFE_HEADER = {"content-type", "content-length", "date", "server", "vary",
               "cache-control", "expires", "last-modified", "etag",
               "content-encoding", "accept-ranges", "age", "alt-svc",
               ":authority", ":method", ":path", ":scheme", ":status"}
SENS_PARAMS = {"pot", "sig", "sig2", "signature", "lsig", "lsig2", "n", "s",
               "key", "ip", "pllat", "pllng", "msig", "ei2"}

# ---- body redaction patterns (applied to bytes) ----------------------------
ENTROPY_PATTERNS = [
    re.compile(rb"AIzaSy[0-9A-Za-z_\-]{20,}"),                 # Google API key
    re.compile(rb"ya29\.[0-9A-Za-z_\-]{20,}"),                 # OAuth access token
    re.compile(rb"1//[0-9A-Za-z_\-]{20,}"),                    # OAuth refresh token
    re.compile(rb"SAPISIDHASH\s+[0-9A-Za-z_\-]+"),
    re.compile(rb"(?i)(Bearer\s+)[0-9A-Za-z._\-]{20,}"),
    re.compile(rb"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}"),  # any email
]
SENS_JSON_KEYS = ("visitorData", "poToken", "potoken", "sessionToken",
                  "xsrfToken", "xsrf_token", "idToken", "id_token",
                  "accessToken", "access_token", "refreshToken", "refresh_token",
                  "datasyncId", "datasync_id", "loginToken", "sessionIndex",
                  "innertubeApiKey", "INNERTUBE_API_KEY", "apiKey",
                  "serializedExperimentIds", "ID_TOKEN", "csn")
JSON_KEY_RE = re.compile(
    (r'("(?:' + "|".join(SENS_JSON_KEYS) + r')"\s*:\s*")([^"]*)(")').encode())
REDACT = b"REDACTED"


def collect_literals(path):
    """Pass 1: gather IP + visitorData literals from headers/params only. Silent."""
    lits = set()
    with open(path, "rb") as f:
        for fl in FlowReader(f).stream():
            if not hasattr(fl, "request"):
                continue
            r = fl.request
            vid = r.headers.get("X-Goog-Visitor-Id")
            if vid:
                lits.add(vid)
            for k, v in r.query.items(multi=True) if hasattr(r.query, "items") else []:
                if k in ("ip", "visitor_data", "visitorData") and v:
                    lits.add(v)
            resp = fl.response
            if resp:
                vid2 = resp.headers.get("X-Goog-Visitor-Id")
                if vid2:
                    lits.add(vid2)
    # turn into bytes literals, longest first so substrings don't pre-empt
    return sorted({s.encode() for s in lits if len(s) >= 6}, key=len, reverse=True)


def redact_headers(msg):
    for name in list(msg.headers.keys()):
        ln = name.lower()
        if ln in SAFE_HEADER:
            continue
        if SENS_HEADER_RE.search(ln) or "api-key" in ln or ln.endswith("-key"):
            msg.headers[name] = "REDACTED"
            continue
        val = msg.headers[name].encode()
        for pat in ENTROPY_PATTERNS:
            val = pat.sub(REDACT, val)
        msg.headers[name] = val.decode("latin1")


def redact_query(req):
    if not req.query:
        return
    for k in list(req.query.keys()):
        if k in SENS_PARAMS:
            req.query[k] = "REDACTED"


def scrub_text_body(msg, lits):
    try:
        body = msg.content  # decoded (gunzipped/brotli) bytes
    except Exception:
        return
    if not body:
        return
    for lit in lits:
        if lit in body:
            body = body.replace(lit, REDACT)
    body = JSON_KEY_RE.sub(rb"\1REDACTED\3", body)
    for pat in ENTROPY_PATTERNS:
        body = pat.sub(REDACT, body)
    msg.content = body  # re-encodes per content-encoding


def scrub(in_path, out_path):
    lits = collect_literals(in_path)
    sys.stderr.write(f"[pass1] collected {len(lits)} identity literals (values not shown)\n")
    n_in = n_out = n_drop = n_strip = n_textscrub = 0
    with open(in_path, "rb") as fin, open(out_path, "wb") as fout:
        w = FlowWriter(fout)
        for fl in FlowReader(fin).stream():
            n_in += 1
            if not hasattr(fl, "request"):
                # websocket/other: drop to be safe
                n_drop += 1
                continue
            host = fl.request.host
            if host_dropped(host):
                n_drop += 1
                continue
            redact_query(fl.request)
            redact_headers(fl.request)
            if fl.response:
                redact_headers(fl.response)
            ct = (fl.request.headers.get("content-type", "") or "").split(";")[0]
            rct = (fl.response.headers.get("content-type", "") or "").split(";")[0] if fl.response else ""
            if keeps_text_body(host):
                if any(ct.startswith(t) for t in TEXT_CT):
                    scrub_text_body(fl.request, lits); n_textscrub += 1
                else:
                    fl.request.content = b""
                if fl.response:
                    if any(rct.startswith(t) for t in TEXT_CT):
                        scrub_text_body(fl.response, lits); n_textscrub += 1
                    else:
                        fl.response.content = b""; n_strip += 1
            else:
                if fl.request.content:
                    fl.request.content = b""
                if fl.response and fl.response.content:
                    fl.response.content = b""
                n_strip += 1
            w.add(fl)
            n_out += 1
    sys.stderr.write(
        f"[pass2] in={n_in} written={n_out} dropped={n_drop} "
        f"bodies_stripped={n_strip} text_bodies_scrubbed={n_textscrub}\n")


def verify(path):
    """Re-read output; report MATCH COUNTS only (never contents)."""
    indicators = {
        "google_api_key (AIzaSy)": re.compile(rb"AIzaSy[0-9A-Za-z_\-]{20,}"),
        "oauth_access (ya29.)": re.compile(rb"ya29\.[0-9A-Za-z_\-]{20,}"),
        "oauth_refresh (1//)": re.compile(rb"1//[0-9A-Za-z_\-]{20,}"),
        "SAPISIDHASH": re.compile(rb"SAPISIDHASH\s+\S"),
        "Bearer <tok>": re.compile(rb"(?i)Bearer\s+[0-9A-Za-z._\-]{20,}"),
        "any email @": re.compile(rb"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}"),
        'json secret-key w/ REAL value':
            re.compile((r'"(?:' + "|".join(SENS_JSON_KEYS) + r')"\s*:\s*"(?!REDACTED)[^"]+"').encode()),
        "Cookie header w/ REAL value": re.compile(rb"(?i)\bcookie:\s*(?!REDACTED)\S"),
        "set-cookie w/ REAL value": re.compile(rb"(?i)set-cookie:\s*(?!REDACTED)\S"),
    }
    counts = {k: 0 for k in indicators}
    drop_hosts = 0
    with open(path, "rb") as f:
        for fl in FlowReader(f).stream():
            if not hasattr(fl, "request"):
                continue
            if host_dropped(fl.request.host):
                drop_hosts += 1
            blobs = []
            for m in (fl.request, fl.response):
                if not m:
                    continue
                blobs.append(b"\n".join(b"%s: %s" % (k.encode(), v.encode())
                                        for k, v in m.headers.items()))
                try:
                    blobs.append(m.content or b"")
                except Exception:
                    pass
            data = b"\n".join(blobs)
            for k, pat in indicators.items():
                counts[k] += len(pat.findall(data))
    print("=== blind verification (match COUNTS only) ===")
    for k, c in counts.items():
        flag = "OK " if c == 0 else "!! "
        print(f"  {flag}{c:6d}  {k}")
    print(f"  --   {drop_hosts:6d}  credential hosts still present (should be 0)")
    bad = sum(counts.values()) + drop_hosts
    print(f"\n{'CLEAN' if bad == 0 else 'NOT CLEAN — ' + str(bad) + ' indicators remain'}")
    return bad


if __name__ == "__main__":
    if sys.argv[1] == "--verify":
        sys.exit(1 if verify(sys.argv[2]) else 0)
    scrub(sys.argv[1], sys.argv[2])
