//! YouTube player-JS concerns: the n-sig descrambler.
//!
//! Media requests whose `n` query param is not transformed by a function in the obfuscated
//! player JS are throttled or refused (PLAYER-005). The transform comes from yt-dlp's
//! public-domain `ejs` solver (AST-based, player-agnostic), run in-process via [`rquickjs`]
//! (quickjs-ng). The SABR url's `n` is fixed per session (PLAYER-006), so this runs once
//! per stream.
//!
//! The solver is two vendored JS blobs: `yt.solver.lib.js` bundles the meriyah
//! parser + astring generator (`var lib = { meriyah, astring }`), and
//! `yt.solver.core.js` is the AST solver itself (`var jsc = (…)(meriyah, astring)`).
//! We eval `lib`, hoist it onto `globalThis`, eval `core`, then call `jsc(input)`
//! and read the JSON result back. quickjs-ng provides `globalThis` natively; the
//! only env gap is `console`, which we no-op shim.

mod native;

use std::time::Instant;

use rquickjs::{CatchResultExt, Context, Function, Object, Runtime};

const SOLVER_LIB: &str = include_str!("../vendor/yt.solver.lib.js");
const SOLVER_CORE: &str = include_str!("../vendor/yt.solver.core.js");

/// Errors descrambling `n`.
#[derive(Debug, thiserror::Error)]
pub enum JsError {
    #[error("js engine error: {0}")]
    Runtime(String),
    #[error("solver threw: {0}")]
    Solver(String),
    #[error("invalid solver output: {0}")]
    Json(#[from] serde_json::Error),
    #[error("solver returned no result for a challenge")]
    Missing,
}

/// Find the player JS URL (`/s/player/.../base.js`) in watch-page HTML.
pub fn extract_player_js_url(html: &str) -> Option<String> {
    let pos = html.find("/s/player/").or_else(|| html.find("\\/s\\/player\\/"))?;
    let window = html[pos..(pos + 256).min(html.len())].replace("\\/", "/");
    let start = window.find("/s/player/")?;
    let end = window[start..].find("base.js")? + start + "base.js".len();
    Some(format!("https://www.youtube.com{}", &window[start..end]))
}

/// Candidate player JS URLs from the `/iframe_api` bootstrap script. This is the fallback
/// when the watch page is unavailable: a `google.com/sorry` 429 wall flags `/watch` HTML
/// while the API endpoints and static player JS stay reachable (IT-024). The script embeds
/// the current player build id in a `/s/player/<id>/www-widgetapi.vflset/...` URL; the full
/// player lives at the same id under `player_es6.vflset` (what a browser-like WEB session
/// is served, PLAYER-002) or `player_ias.vflset`. Tried in that order.
pub fn player_js_urls_from_iframe_api(js: &str) -> Option<[String; 2]> {
    let pos = js.find("/s/player/").or_else(|| js.find("\\/s\\/player\\/"))?;
    let window = js[pos..(pos + 128).min(js.len())].replace("\\/", "/");
    let id = window.strip_prefix("/s/player/")?.split('/').next()?;
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some([
        format!("https://www.youtube.com/s/player/{id}/player_es6.vflset/en_US/base.js"),
        format!("https://www.youtube.com/s/player/{id}/player_ias.vflset/en_US/base.js"),
    ])
}

/// Extract the `signatureTimestamp` (`sts`) from player JS. The `player()` request must
/// send the sts of the player used for n-sig, or the streaming URL is signed for another
/// build and POSTs to it fail with HTTP 403 (PLAYER-004).
pub fn extract_signature_timestamp(player_js: &str) -> Option<i64> {
    let marker = "signatureTimestamp:";
    let pos = player_js.find(marker)? + marker.len();
    let digits: String = player_js[pos..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Descrambles `n` query params using the ejs solver in-process (rquickjs).
///
/// Parsing the multi-MB `base.js` with meriyah is about 95% of the solve (5 to 10 s). The
/// n-function is stable per player build, so the first solve emits the solver's
/// `preprocessed_player` (ejs `output_preprocessed` mode) and caches it on disk keyed by a
/// hash of `base.js`. Later solves replay the cached form and skip the parse.
#[derive(Clone)]
pub struct NSigSolver {
    cache_dir: Option<std::path::PathBuf>,
    mode: SolverMode,
}

/// Which implementation(s) [`NSigSolver::solve_n`] may use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SolverMode {
    /// Native fast path, falling back to ejs on decline or failure. The default.
    Auto,
    /// Vendored ejs only: the oracle the native path is checked against.
    EjsOnly,
    /// Native only, erroring rather than falling back. Tests use this so a
    /// decline is a visible failure: under `Auto` the fallback would make a
    /// native-vs-ejs comparison silently compare ejs against itself.
    NativeOnly,
}

impl Default for NSigSolver {
    fn default() -> Self {
        Self::new()
    }
}

impl NSigSolver {
    /// Disk-cached solver (cache under `$XDG_CACHE_HOME`/`~/.cache`/`$TMPDIR`).
    pub fn new() -> Self {
        Self { cache_dir: default_nsig_cache_dir(), mode: SolverMode::Auto }
    }

    /// Vendored ejs only, bypassing the native fast path: the oracle side of the
    /// differential test.
    pub fn ejs_only() -> Self {
        Self { cache_dir: None, mode: SolverMode::EjsOnly }
    }

    /// Native fast path only, with no ejs fallback: a decline or a JS throw
    /// becomes an error instead of silently costing ~9s. Tests use this so the
    /// differential actually compares the two implementations.
    pub fn native_only() -> Self {
        Self { cache_dir: None, mode: SolverMode::NativeOnly }
    }

    /// Transform each `n` in `challenges` using `player_js`, preserving order.
    /// Uses the on-disk preprocessed-player cache when present (skips the base.js
    /// parse); on a miss, solves fully and populates the cache.
    pub async fn solve_n(
        &self,
        player_js: &str,
        challenges: &[String],
    ) -> Result<Vec<String>, JsError> {
        let challenges = challenges.to_vec();

        // Fast path: a native parse locates the signing function in ~90 ms, where ejs's
        // meriyah-in-QuickJS pass takes ~8.7 s. It is faster cold than ejs is warm, so it does
        // not touch the cache. Any failure (unfamiliar player shape, ambiguous match, JS
        // throw) falls back to ejs below.
        if self.mode != SolverMode::EjsOnly {
            let (pjs, chals) = (player_js.to_string(), challenges.clone());
            let t = Instant::now();
            // The evaluated player stays resident between calls, so only the first
            // solve for a given build pays the ~350ms QuickJS eval; later ones are
            // the ~2ms transform alone.
            let native = tokio::task::spawn_blocking(move || native::solve_resident(&pjs, &chals))
                .await
                .map_err(|e| JsError::Runtime(e.to_string()))?;
            let native = match native {
                Err(JsError::Solver(ref m)) if m == "player declined" => None,
                other => Some(other),
            };
            match native {
                Some(Ok(out)) => {
                    tracing::debug!("    [nsig] native solve: {:.2}s", t.elapsed().as_secs_f64());
                    return Ok(out);
                }
                // The native pass matched a function and then threw, which means player drift.
                // It costs ~9 s of ejs work on the stream-start path, so always log it.
                Some(Err(e)) => {
                    if self.mode == SolverMode::NativeOnly {
                        return Err(e);
                    }
                    tracing::warn!("    [nsig] native solve failed ({e}) -> falling back to ejs");
                }
                None => {
                    if self.mode == SolverMode::NativeOnly {
                        return Err(JsError::Solver(
                            "native pass declined this player".into(),
                        ));
                    }
                    tracing::debug!("    [nsig] native pass declined this player -> falling back to ejs");
                }
            }
        }

        let cache_path = self.cache_path(player_js);

        // Cache hit: replay the cached preprocessed player (no base.js parse). A
        // stale/corrupt entry (e.g. after a solver bump) just falls through to a
        // full solve, which rewrites it.
        if let Some(path) = &cache_path {
            if let Ok(pp) = std::fs::read_to_string(path) {
                let (chals, out) = (challenges.clone(), {
                    let chals = challenges.clone();
                    tokio::task::spawn_blocking(move || run_preprocessed(&pp, &chals))
                        .await
                        .map_err(|e| JsError::Runtime(e.to_string()))?
                });
                if let Ok(out) = out {
                    return parse_solved(&out, &chals);
                }
                let _ = std::fs::remove_file(path);
            }
        }

        // Cache miss: full solve, emitting the preprocessed player to cache.
        let (pjs, chals) = (player_js.to_string(), challenges.clone());
        let out = tokio::task::spawn_blocking(move || run_full(&pjs, &chals))
            .await
            .map_err(|e| JsError::Runtime(e.to_string()))??;
        let value: serde_json::Value = serde_json::from_str(&out)?;
        if let Some(path) = &cache_path {
            if let Some(pp) = value.get("preprocessed_player").filter(|p| !p.is_null()) {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(path, pp.to_string());
            }
        }
        parse_solved_value(&value, &challenges)
    }

    fn cache_path(&self, player_js: &str) -> Option<std::path::PathBuf> {
        self.cache_dir.as_ref().map(|d| d.join(format!("nsig-{}.json", cache_key(player_js))))
    }
}

/// Cache key: a hash of `base.js` *and* our vendored solver source, so a player
/// rotation or a solver bump both invalidate it. Collisions only cost a re-solve.
fn cache_key(player_js: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    SOLVER_CORE.hash(&mut h);
    player_js.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn default_nsig_cache_dir() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    Some(base.join("innertube-rust").join("nsig"))
}

/// Eval the bundled lib (meriyah + astring) onto `globalThis`, then the solver core.
fn solver_setup() -> String {
    format!("{SOLVER_LIB}\nObject.assign(globalThis, lib);\n{SOLVER_CORE}\n")
}

/// Full solve: parse `base.js` and emit `preprocessed_player` for caching.
fn run_full(player_js: &str, challenges: &[String]) -> Result<String, JsError> {
    let data = serde_json::json!({
        "type": "player",
        "player": player_js,
        "requests": [{ "type": "n", "challenges": challenges }],
        "output_preprocessed": true,
    });
    run_quickjs(&solver_setup(), &format!("JSON.stringify(jsc({data}));"), None)
}

/// Cached solve: replay a preprocessed player (passed as the `__pp` global to avoid
/// embedding the multi-MB blob in JS source), skipping the base.js parse.
fn run_preprocessed(pp_json: &str, challenges: &[String]) -> Result<String, JsError> {
    let data = serde_json::json!({
        "type": "preprocessed",
        "requests": [{ "type": "n", "challenges": challenges }],
    });
    let expr = format!(
        "JSON.stringify((function(){{var i={data};i.preprocessed_player=JSON.parse(globalThis.__pp);return jsc(i);}})());"
    );
    run_quickjs(&solver_setup(), &expr, Some(pp_json))
}

/// In a fresh quickjs-ng runtime (no `console` → no-op shim), eval `setup` (for
/// its side effects: defining globals), then eval `expr` and return its String
/// completion value. JS exceptions surface as [`JsError::Solver`].
fn run_quickjs(setup: &str, expr: &str, pp_global: Option<&str>) -> Result<String, JsError> {
    let rt = Runtime::new().map_err(|e| JsError::Runtime(e.to_string()))?;
    let ctx = Context::full(&rt).map_err(|e| JsError::Runtime(e.to_string()))?;
    ctx.with(|ctx| {
        install_console(&ctx)?;
        // A cached preprocessed player is injected as a global string (parsed inside
        // JS via JSON.parse) rather than interpolated into `expr`, so quickjs never
        // parses the multi-MB blob as source.
        if let Some(pp) = pp_global {
            ctx.globals().set("__pp", pp).map_err(|e| JsError::Runtime(e.to_string()))?;
        }
        if !setup.is_empty() {
            let t = Instant::now();
            ctx.eval::<(), _>(setup).catch(&ctx).map_err(|e| JsError::Solver(e.to_string()))?;
            tracing::debug!("    [nsig] compile lib+core: {:.2}s", t.elapsed().as_secs_f64());
        }
        let t = Instant::now();
        let out = ctx
            .eval::<String, _>(expr)
            .catch(&ctx)
            .map_err(|e| JsError::Solver(e.to_string()))?;
        tracing::debug!("    [nsig] run solver (parse base.js + solve): {:.2}s", t.elapsed().as_secs_f64());
        Ok(out)
    })
}

/// Install a no-op global `console` so solver diagnostics don't throw under
/// quickjs-ng (which has no `console` by default).
fn install_console(ctx: &rquickjs::Ctx<'_>) -> Result<(), JsError> {
    let to_rt = |e: rquickjs::Error| JsError::Runtime(e.to_string());
    let console = Object::new(ctx.clone()).map_err(to_rt)?;
    let noop = Function::new(ctx.clone(), || {}).map_err(to_rt)?;
    for method in ["log", "warn", "error", "debug", "info"] {
        console.set(method, noop.clone()).map_err(to_rt)?;
    }
    ctx.globals().set("console", console).map_err(to_rt)
}

/// Parse the solver's JSON output, pulling each challenge's result in order.
/// The solver wraps each request's outcome in `responses[i]`; a `type:"error"`
/// response carries the JS-side failure message.
fn parse_solved(output: &str, challenges: &[String]) -> Result<Vec<String>, JsError> {
    let parsed: serde_json::Value = serde_json::from_str(output)?;
    parse_solved_value(&parsed, challenges)
}

/// Like [`parse_solved`] but on an already-parsed value (avoids re-parsing the large
/// `output_preprocessed` output, which also carries the preprocessed player).
fn parse_solved_value(parsed: &serde_json::Value, challenges: &[String]) -> Result<Vec<String>, JsError> {
    let response = &parsed["responses"][0];
    if response["type"] == "error" {
        let msg = response["error"].as_str().unwrap_or("unknown solver error");
        return Err(JsError::Solver(msg.to_string()));
    }
    let data = &response["data"];
    challenges
        .iter()
        .map(|c| data[c].as_str().map(str::to_string).ok_or(JsError::Missing))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `/iframe_api` shape: a trustedTypes wrapper around an escaped URL.
    #[test]
    fn iframe_api_yields_both_player_js_urls() {
        let js = r#"var scriptUrl = 'https:\/\/www.youtube.com\/s\/player\/4918c89a\/www-widgetapi.vflset\/www-widgetapi.js';try{var ttPolicy=window.trustedTypes"#;
        let urls = player_js_urls_from_iframe_api(js).expect("must extract the build id");
        assert_eq!(urls[0], "https://www.youtube.com/s/player/4918c89a/player_es6.vflset/en_US/base.js");
        assert_eq!(urls[1], "https://www.youtube.com/s/player/4918c89a/player_ias.vflset/en_US/base.js");
    }

    /// The bundled solver lib (meriyah and astring) loads in quickjs-ng and round-trips a snippet.
    #[test]
    fn the_bundled_solver_lib_runs_in_quickjs() {
        let setup = format!("{SOLVER_LIB}\nObject.assign(globalThis, lib);\n");
        let out = run_quickjs(&setup, "astring.generate(meriyah.parse('const x = 1+2;'))", None)
            .expect("lib should run in quickjs");
        assert!(out.contains("const x = 1 + 2"), "got: {out}");
    }

    #[test]
    fn solved_challenges_come_back_in_request_order() {
        let output = r#"{"type":"result","responses":[{"type":"result","data":{"AAA":"xxx","BBB":"yyy"}}]}"#;
        let challenges = vec!["AAA".to_string(), "BBB".to_string()];
        assert_eq!(parse_solved(output, &challenges).unwrap(), vec!["xxx", "yyy"]);
    }

}
