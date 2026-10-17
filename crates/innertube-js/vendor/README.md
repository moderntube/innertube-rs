# Vendored n-sig solver

`yt.solver.core.js` + `yt.solver.lib.js` are vendored from
**[yt-dlp/ejs](https://github.com/yt-dlp/ejs)** v0.8.0 (release assets),
**SPDX-License-Identifier: Unlicense** (public domain).

`core` is an AST-based (meriyah + astring) solver that extracts and evaluates the
player's n-sig / signature transforms — player-agnostic, so it survives player
rotation without per-player regex maintenance. `lib` is the **fully-bundled**
variant: `var lib = { meriyah, astring }` with meriyah 6.1.4 + astring 1.9.0
inlined (no npm imports, no top-level await, no ESM exports) — so it runs
unchanged in a bare JS engine.

`innertube-js` evals `lib + core + a challenge harness` **in-process** via
`rquickjs` (quickjs-ng) and reads the solved values back (see `src/lib.rs`). To
update: re-download `yt.solver.core.js` + `yt.solver.lib.js` from a newer ejs
release. (The `bun`/`deno` lib variants resolve meriyah/astring via npm and are
*not* used here.)
