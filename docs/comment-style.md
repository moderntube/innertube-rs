# Comment style

How comments in this codebase are written. A reader who was not here for the
investigation should be able to read a comment and trust it.

Applies to `crates/` and `scripts/`. The provenance section is specific to
protocol reverse engineering.

## The main problem: saying too much

Most comments here carry a real fact: a wire field, a server behaviour, a
measured number. The larger problem is the investigation that comes with it.
Comments retell how a bug was found, which hypotheses failed, which run proved
it, and what the code did before. That story belongs in the commit, and in
`notes/` or `docs/` when it is worth keeping.

A person writes a comment when the code alone would mislead or leave the reader
stuck. The default is no comment.

- **Assume a competent Rust reader.** A `?`, a `match` fallback, a `Drop`, a
  `tokio::select!`: the reader knows what these do. Comment only what is
  specific to this protocol or this decision.
- **One reason is enough.** If the comment states a fact and then argues for it,
  cut the argument.
- **When a comment gives a reason, keep only the reason.** Drop the sentence
  around it that describes what the code does.
- **Don't prove it.** A worked example or a decoded capture belongs in a test or
  in `notes/`. Keep a number in a comment only when the number is the point: a
  field number, an announced backoff, a byte offset.
- **Nothing that goes stale.** Test counts, line numbers, session counts ("6 of
  6 runs") and timings change without anyone updating the comment. A player
  build hash is the exception when it dates a workaround: it tells the reader
  when to recheck.
- **Short.** Most comments that earn their place fit in one or two lines.

## What earns a comment

1. **Why.** The reason for a choice that looks wrong or arbitrary: a header the
   server gates on, an ordering that looks backwards.
2. **Where a fact came from.** A capture, a `base.js` branch, an upstream
   source. See [Provenance](#provenance).
3. **The protocol.** What a part type, field or policy means, for a reader who
   has not decoded SABR. Field numbers go with their names: `field 72
   (MediaCapabilityCaps)`.
4. **The contract of a public item.** What it returns, its units (ms, bytes,
   segments), whether a time is absolute or relative to the stream start.
5. **A trap.** Code that looks wrong, or a change that has to be made in two
   places, where getting it wrong is not caught by a test.

Units go in any comment that mentions a quantity: ms or s, bytes, segments,
and whether a timestamp is media time, wall clock or live-absolute.

## What does not

**Restating the next line.** Delete it.

**History.** "This used to send sd=480", "Walked back from the cushion model",
"WITHDRAWN". Keep the constraint that resulted. The story goes in the commit.

**Investigation results.** "Measured 2026-08-29: 6 of 6 opens held prot=1".
The comment states the fact ("The attestation binding is per session"); the
measurement lives in `notes/` or the commit that established it.

**Own issue numbers.** `(#NN)` points at a tracker the reader may not have. Write the fact the
issue held, or leave it out. `git blame` leads from any line to its commit.

**Commented-out code.** Git keeps it.

**TODOs without a decider.** A TODO names what is left and what decides it.

## Wording

- **Say what the code does or is.** Don't set up a misreading to knock it down.
- **No em dashes.** Use a period, a comma, a colon or parentheses.
- **No capitals for emphasis.** Constants and env vars keep their capitals.
- **No "not X, but Y" or "X, not Y" constructions.** Say what is true.
- **No labelled lead-ins** (`NOTE:`, `WHY:`, `CORRECTION (date).`, `RESOLVED`).
- **Cut intensifiers**: exactly, actually, deliberately, genuinely, crucially.
- **Few semicolons.** Two short sentences read better than one joined by one.

## Form

- `///` on public items when the name and signature leave something out. The
  first sentence says what it is or returns.
- `//!` module header: what the module is for and what it assumes, in a few
  lines. Long derivations go in `docs/` or `notes/` with a one-line pointer.
- `//` inside bodies, for why and for traps.

## Provenance

The ground truth here is the reference client, so a protocol fact cites where in
the reference it was seen.

- **A capture.** `dumps/<file>.flow`, plus enough to find it again: the round,
  the part type, the field. "Round 0 is `[35, 57, 67]` with no media when the
  content-ads gate is on (`full-session.flow`)."
- **Player source.** The behaviour and the player build, since minified names
  rotate: "`base.js` 9fc68080 ignores the backoff only when client-side DAI is
  up."
- **Upstream.** Public issues and source are fine to cite by reference:
  `LuanRT/BgUtils#44`, yt-dlp's extractor, googlevideo's protos.
- **Our own measurement.** State the fact; point at the `notes/` file that holds
  the experiment. No run counts, no dates in the code.

## Tests

Test comments follow the same rules, with two differences.

- Say why a case exists when its name does not: the input it guards or the
  failure it pins. Drop the story of the bug that led to it.
- A decoded value can stay when it explains the asserted value. Keep it to the
  line that derives it.

## Checking

```bash
git grep -nE '^\s*//.*—' -- '*.rs'
git grep -nE '^\s*//.*#[0-9]{2,4}\b' -- '*.rs'
git grep -nE '^\s*//.*\b(NOT|ONLY|NEVER|MUST|ALWAYS|SAME|BOTH)\b' -- '*.rs'
git grep -nE '^\s*//.*20[0-9]{2}-[0-9]{2}-[0-9]{2}' -- '*.rs'
git grep -niE '^\s*//.*\b(previously|used to|no longer|withdrawn|exactly|actually|deliberately|genuinely)\b' -- '*.rs'
```

A comment-only change leaves behaviour alone. One module per commit, so each
diff can be read in one sitting.
