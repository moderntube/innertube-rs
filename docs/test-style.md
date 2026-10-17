# Test style

What gets a test in this codebase, and what does not. Test comments follow
`docs/comment-style.md`.

## The rule

A test runs real inputs through our logic and checks the result against
something outside the code. Here that is almost always the reference client: a
captured request, a captured response, a scrubbed player response, or a
container spec. If the expected value can only come from reading the code (a
field number, a constant, a log line, the function's own output pasted back),
the test restates the code. It fails when someone changes the value on purpose
and never catches a mistake. Those tests do not get written.

A test fails only on something that would always be a bug. Behaviour that may
change on purpose (a parity fix, a tuning value, a new request field) never
breaks a test.

The default is no test. Coding agents tend to add one test per branch, one per
bug and one per constant; this guide exists to stop that.

## What gets tested

1. **Codecs against captured bytes.** UMP framing and the SABR protobufs decode
   a captured response and encode a request that matches a captured one, field
   for field. One fixture per message shape, not one per field.

2. **The engine, through captured sessions.** Feed `SabrEngine` a captured
   response sequence and compare the requests it produces with the ones the
   reference client sent. This covers live-edge join, seeks, backoff, context
   updates and itag acks. Internal engine functions (the gate, `player_time`,
   segment duration) are not tested on their own: if one breaks, a replayed
   request changes, and that is the signal. Engine output gets checked the same
   way: run a captured session and assert properties of what comes out (each
   track starts at a container init, segments are never emitted twice). The
   engine is never fed hand-built bytes to test its logic.

3. **The two sanctioned divergences.** The byte-addressed stream is ours, so it
   has no capture to compare with. Test its properties directly: the output
   parses, PTS is monotonic across an itag switch and a seek, and every `Cues`
   entry points at a cluster that starts at that time.

4. **InnerTube JSON parsing.** Scrubbed real responses in `notes/fixtures/` or
   `crates/*/tests/fixtures/`, one per response shape, asserting the fields the
   app uses.

5. **n-sig and PO-token plumbing, offline.** Known input and output pairs from
   yt-dlp's own test vectors, or from a captured session.

6. **A property that holds everywhere.** "Splitting a body at any offset ingests
   the same as the whole body" is worth one test over every offset.

7. **A fix that looks removable.** A bug fix gets no test by default. It gets one
   only when the fixed code looks wrong or pointless at a glance, so a later
   cleanup would plausibly undo it. The test names the input it guards; the
   story stays in the commit.

## What never gets a test

- **Constants.** Parity constants, field numbers, locale strings, default
  timeouts. The capture and the code are the record.
- **Mock calls.** That a mocked function was called, or called with X.
- **Wording.** Log lines, error messages, CLI output.
- **CLI probes and scripts.** `innertube-cli` binaries and `scripts/` are tools.
- **Language and library behaviour.** That `prost` round-trips, that `reqwest`
  sends a header we set.
- **Contrived input.** Values the server cannot send and the app cannot produce.
- **Current differences from the reference.** Which fields we send and the
  reference does not is a list that shrinks as parity work lands. It lives in
  the replay harness's allow-list, never in an assertion.
- **Performance numbers.** Allocation counts, byte budgets and timings change
  with any new field. A property that is always a bug stays testable: a 2 MiB
  payload is never copied on parse.
- **Code production does not run.** A path no caller uses gets deleted, not
  tested.

## Network tests

Anything that contacts YouTube is `#[ignore]` with a reason, so
`cargo test --workspace` stays offline. It follows the traffic rule in
`AGENTS.md` (method rule 8). A network test asserts a property ("the served
height is at least 1080", "protection status reaches 1"), never a number from
one run. A measurement without an assertion is a probe and belongs in
`innertube-cli`.

## Numbers in a test

An expected number names its source on the line or once at the top of the
module: a capture and round, a spec section, a yt-dlp vector. A number copied
from the code's output is not a source. When there is none, assert the property
instead, or drop the assertion.

## Form

- Name the test for what is true, in protocol words: "a live seek is adopted as
  the next player time". No issue numbers in names.
- Unit tests in `#[cfg(test)]` modules are for the kinds above, same as
  integration tests. Being next to the code is not a reason to exist.
- Shared fixtures and replay helpers live in one place per crate.

## Deciding on an existing test

For each test, in order:

1. Is it one of the seven kinds above, or a network test that asserts a
   property? If not, cut it.
2. Does its expected value come from outside the code? If not, assert the
   property or cut it.
3. Does another test already check the same result through the same path? Keep
   one.
4. For the engine, is it an internal function? Cut it, unless no captured
   session exercises that path; then the fix is a new replay fixture, not the
   unit test.

A cut needs one line of reason in the commit. Nothing is protected by being old.

## Sources

- Andre Hora and Romain Robbes, "Are Coding Agents Generating Over-Mocked Tests?
  An Empirical Study", MSR 2026: https://arxiv.org/abs/2602.00409
- Alex Eagle, "Testing on the Toilet: Change-Detector Tests Considered Harmful",
  Google Testing Blog, 2015:
  https://testing.googleblog.com/2015/01/testing-on-toilet-change-detector-tests.html
- *Software Engineering at Google*, chapter 12 "Unit Testing":
  https://abseil.io/resources/swe-book/html/ch12.html
