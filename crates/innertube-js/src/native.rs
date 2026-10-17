//! Native n-sig pass, the fast path.
//!
//! The vendored `ejs` solver is player-agnostic because it locates YouTube's URL-signing
//! function structurally rather than by regex, then drives that function instead of
//! reimplementing it. We keep that design. ejs parses the ~2.5 MB `base.js` with meriyah (a
//! JavaScript parser written in JavaScript) inside QuickJS, then regenerates the whole
//! player with astring. That AST round-trip is ~8.7 s of a ~9.3 s cold solve.
//!
//! This module does the same analysis with a native parser (`oxc`) in ~90 ms and hands
//! QuickJS the original source with two edits. A cold solve takes ~0.5 s, faster than ejs's
//! cached replay, so the fast path needs no cache.
//!
//! On any failure it returns `None` and the caller falls back to ejs. The two are
//! differential-tested against each other.

use oxc_ast::ast::*;
use oxc_span::GetSpan;
use crate::JsError;

/// Browser globals the player touches while defining itself. Same set ejs emits
/// ahead of its preprocessed player.
const SHIMS: &str = r#"
if (typeof globalThis.XMLHttpRequest === "undefined") globalThis.XMLHttpRequest = { prototype: {} };
if (typeof URL === "undefined") {
  globalThis.location = { hash:"", host:"www.youtube.com", hostname:"www.youtube.com",
    href:"https://www.youtube.com/watch?v=yt-dlp-wins", origin:"https://www.youtube.com",
    password:"", pathname:"/watch", port:"", protocol:"https:", search:"?v=yt-dlp-wins",
    username:"" };
} else {
  globalThis.location = new URL("https://www.youtube.com/watch?v=yt-dlp-wins");
}
if (typeof globalThis.document  === "undefined") globalThis.document  = Object.create(null);
if (typeof globalThis.navigator === "undefined") globalThis.navigator = Object.create(null);
if (typeof globalThis.self      === "undefined") globalThis.self      = globalThis;
if (typeof globalThis.window    === "undefined") globalThis.window    = globalThis;
"#;

/// Build a signing object, set `n` on it, invoke the one non-public prototype method (the
/// player's own transform), and read `n` back. Lifted from ejs's generated harness so both
/// paths drive the player identically.
const HARNESS: &str = r#"
(function(n){
  var url = globalThis.__nsig("https://youtube.com/watch?v=yt-dlp-wins", "s", undefined);
  url.set("n", n);
  var proto = Object.getPrototypeOf(url);
  var keys = Object.keys(proto).concat(Object.getOwnPropertyNames(proto));
  for (var i = 0; i < keys.length; i++) {
    var k = keys[i];
    if (["constructor","set","get","clone"].indexOf(k) === -1) { url[k](); break; }
  }
  return url.get("n");
})
"#;

/// A QuickJS runtime with one player already evaluated, kept alive between solves.
///
/// A browser parses `base.js` once per page session and later videos call the descrambling
/// function for free. Per-solve costs:
///
/// ```text
/// parse   :  ~50 ms   oxc
/// eval    : ~350 ms   QuickJS ingesting the player
/// solve   :   ~2 ms   running the transform, the only per-video part
/// ```
///
/// Holding the context makes the second and later solves ~2 ms.
///
/// `rquickjs::Context` is `!Send`, so one dedicated thread owns it and serves solve
/// requests over a channel.
mod resident {
    use super::{JsError, HARNESS, SHIMS};
    use rquickjs::{CatchResultExt, Context, Function as JsFn, Object, Runtime};
    use std::sync::{mpsc, OnceLock};

    pub(super) struct Job {
        pub key: u64,
        /// Read only on a key miss; building it costs a parse that a loaded player skips.
        pub prepared: Box<dyn FnOnce() -> Option<String> + Send>,
        pub challenges: Vec<String>,
        pub reply: mpsc::Sender<Result<Vec<String>, JsError>>,
    }

    fn spawn() -> mpsc::Sender<Job> {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("nsig-player".into())
            .spawn(move || {
                // (key, runtime, context) for the player currently evaluated here.
                let mut loaded: Option<(u64, Runtime, Context)> = None;
                while let Ok(job) = rx.recv() {
                    if loaded.as_ref().map(|(k, _, _)| *k) != Some(job.key) {
                        match load(job.key, job.prepared) {
                            Ok(entry) => loaded = Some(entry),
                            Err(e) => {
                                loaded = None;
                                let _ = job.reply.send(Err(e));
                                continue;
                            }
                        }
                    }
                    let (_, _, ctx) = loaded.as_ref().expect("just loaded");
                    let _ = job.reply.send(run(ctx, &job.challenges));
                }
            })
            .expect("spawn nsig-player thread");
        tx
    }

    fn load(
        key: u64,
        prepared: Box<dyn FnOnce() -> Option<String> + Send>,
    ) -> Result<(u64, Runtime, Context), JsError> {
        let src = prepared().ok_or_else(|| JsError::Solver("player declined".into()))?;
        let to_rt = |e: rquickjs::Error| JsError::Runtime(e.to_string());
        let rt = Runtime::new().map_err(to_rt)?;
        let ctx = Context::full(&rt).map_err(to_rt)?;
        ctx.with(|ctx| -> Result<(), JsError> {
            // quickjs-ng ships no console; the player calls it.
            let console = Object::new(ctx.clone()).map_err(to_rt)?;
            let noop = JsFn::new(ctx.clone(), || {}).map_err(to_rt)?;
            for m in ["log", "warn", "error", "debug", "info"] {
                console.set(m, noop.clone()).map_err(to_rt)?;
            }
            ctx.globals().set("console", console).map_err(to_rt)?;
            ctx.eval::<(), _>(SHIMS).catch(&ctx).map_err(|e| JsError::Runtime(e.to_string()))?;
            ctx.eval::<(), _>(src.as_bytes())
                .catch(&ctx)
                .map_err(|e| JsError::Solver(e.to_string()))?;
            Ok(())
        })?;
        Ok((key, rt, ctx))
    }

    fn run(ctx: &Context, challenges: &[String]) -> Result<Vec<String>, JsError> {
        ctx.with(|ctx| {
            let f: JsFn = ctx
                .eval(HARNESS.as_bytes())
                .catch(&ctx)
                .map_err(|e| JsError::Runtime(e.to_string()))?;
            challenges
                .iter()
                .map(|c| f.call((c.as_str(),)).catch(&ctx).map_err(|e| JsError::Solver(e.to_string())))
                .collect()
        })
    }

    /// Solve on the resident thread, evaluating `prepared()` only on a cache miss.
    pub(super) fn solve(
        key: u64,
        prepared: Box<dyn FnOnce() -> Option<String> + Send>,
        challenges: &[String],
    ) -> Result<Vec<String>, JsError> {
        static TX: OnceLock<mpsc::Sender<Job>> = OnceLock::new();
        let (reply, wait) = mpsc::channel();
        TX.get_or_init(spawn)
            .send(Job { key, prepared, challenges: challenges.to_vec(), reply })
            .map_err(|_| JsError::Runtime("nsig-player thread gone".into()))?;
        wait.recv().map_err(|_| JsError::Runtime("nsig-player thread died".into()))?
    }
}

/// Solve `challenges` against `player_js`, reusing an already-evaluated player when
/// this is the same build we last saw.
pub(crate) fn solve_resident(
    player_js: &str,
    challenges: &[String],
) -> Result<Vec<String>, JsError> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    player_js.hash(&mut h);
    let key = h.finish();
    let owned = player_js.to_string();
    resident::solve(key, Box::new(move || prepare(&owned)), challenges)
}

/// The marker ejs keys on: a function whose body contains, as a direct statement, a call
/// `<Identifier>.<prop>("alr", "yes")`.
///
/// In a typical build the literal pair occurs four times; only one is the signing function:
///   * `D.get("alr")||D.set("alr","yes")` is a LogicalExpression, not a call statement
///   * `mR=async function(…){ H.set("alr","yes") }` is excluded as async
///   * `this.Zq.set("alr","yes")` has `this` as callee object, not an Identifier
///   * `V3=function(…){ …J.set("alr","yes")… }` is the one we want
fn has_marker(body: &FunctionBody) -> bool {
    body.statements.iter().any(|st| {
        let Statement::ExpressionStatement(es) = st else { return false };
        let Expression::CallExpression(call) = &es.expression else { return false };
        let ident_object = match &call.callee {
            Expression::StaticMemberExpression(m) => matches!(m.object, Expression::Identifier(_)),
            Expression::ComputedMemberExpression(m) => matches!(m.object, Expression::Identifier(_)),
            _ => false,
        };
        if !ident_object || call.arguments.len() != 2 {
            return false;
        }
        fn lit<'a>(a: &'a Argument<'a>) -> Option<&'a str> {
            match a {
                Argument::StringLiteral(s) => Some(s.value.as_str()),
                _ => None,
            }
        }
        lit(&call.arguments[0]) == Some("alr") && lit(&call.arguments[1]) == Some("yes")
    })
}

/// Every `(name, body)` a top-level statement introduces for a non-async function.
///
/// A statement can introduce several: minified players emit comma-joined `var` lists like
/// `var pad=1,SIGN=function(u){…};`. ejs's `extract` pushes every declarator into its
/// candidate list, so we do too. Taking only the first would bind the wrong name or miss
/// the signing function.
fn candidates<'a>(st: &'a Statement<'a>, src: &'a str) -> Vec<(&'a str, &'a FunctionBody<'a>)> {
    let non_async = |f: &'a Function<'a>| (!f.r#async).then_some(f.body.as_deref()).flatten();
    match st {
        Statement::FunctionDeclaration(f) => match (&f.id, non_async(f)) {
            (Some(id), Some(b)) => vec![(id.name.as_str(), b)],
            _ => vec![],
        },
        Statement::ExpressionStatement(es) => match &es.expression {
            Expression::AssignmentExpression(a) => match &a.right {
                Expression::FunctionExpression(f) => non_async(f)
                    .map(|b| vec![(a.left.span().source_text(src), b)])
                    .unwrap_or_default(),
                _ => vec![],
            },
            _ => vec![],
        },
        Statement::VariableDeclaration(vd) => vd
            .declarations
            .iter()
            .filter_map(|d| match &d.init {
                Some(Expression::FunctionExpression(f)) => {
                    non_async(f).map(|b| (d.id.span().source_text(src), b))
                }
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

/// A top-level statement is dropped if it is an ExpressionStatement that is neither an
/// assignment nor a bare literal, i.e. the player's bootstrapping calls. Mirrors ejs
/// `modifyPlayer`. It keeps the player defined but not run (running it reaches for `Intl`
/// and `location` and throws).
fn is_bootstrap_call(st: &Statement) -> bool {
    match st {
        Statement::ExpressionStatement(es) => !matches!(
            es.expression,
            Expression::AssignmentExpression(_)
                | Expression::StringLiteral(_)
                | Expression::NumericLiteral(_)
                | Expression::BooleanLiteral(_)
                | Expression::NullLiteral(_)
                | Expression::BigIntLiteral(_)
                | Expression::RegExpLiteral(_)
        ),
        _ => false,
    }
}

/// Is this the IIFE's opening `var window=this;`?
///
/// QuickJS evaluates scripts with `this === undefined`, so keeping the statement makes the
/// player throw "Context has not been set and window is undefined". ejs drops it
/// (`modifyPlayer`'s `splice(0, 1)`) and lets its `globalThis.window = globalThis` shim
/// satisfy the reference; we do the same. Detection is on the AST, so a whitespace or
/// minifier change cannot turn it into a no-op.
fn is_window_this_binding(st: &Statement) -> bool {
    let Statement::VariableDeclaration(vd) = st else { return false };
    vd.declarations.iter().any(|d| {
        d.id.get_identifier_name().is_some_and(|n| n == "window")
            && matches!(d.init, Some(Expression::ThisExpression(_)))
    })
}

/// The player's outer IIFE. ejs `modifyPlayer` accepts two shapes:
/// `(function(g){…})(_yt_player)` and `(function(){…}).call(this)`.
fn player_iife<'a>(program: &'a Program<'a>) -> Option<&'a Function<'a>> {
    program.body.iter().rev().find_map(|st| {
        let Statement::ExpressionStatement(es) = st else { return None };
        let Expression::CallExpression(c) = &es.expression else { return None };
        match &c.callee {
            Expression::FunctionExpression(f) => Some(&**f),
            // `(function(){…}).call(this)`: the callee is a member access on the function.
            Expression::StaticMemberExpression(m) => match &m.object {
                Expression::FunctionExpression(f) => Some(&**f),
                _ => None,
            },
            _ => None,
        }
    })
}

/// Rewrite `player_js` so the signing function is reachable as `globalThis.__nsig` and
/// evaluating the player defines it without running it. Returns `None` if the player does
/// not have the expected shape; the caller then falls back to ejs.
pub(crate) fn prepare(player_js: &str) -> Option<String> {
    let allocator = oxc_allocator::Allocator::default();
    // `preserve_parens: false` exposes `(function(g){…})(x)` as a FunctionExpression callee,
    // the shape ejs/meriyah sees.
    let ret = oxc_parser::Parser::new(&allocator, player_js, oxc_span::SourceType::cjs())
        .with_options(oxc_parser::ParseOptions { preserve_parens: false, ..Default::default() })
        .parse();
    // oxc error-recovers, so a truncated or corrupt player still yields a program.
    // Rewriting one by span would mangle it silently; defer to ejs instead.
    if ret.panicked || !ret.diagnostics.is_empty() {
        return None;
    }

    let body = player_iife(&ret.program)?.body.as_ref()?;

    // Exactly one candidate must match. Zero means the player changed shape;
    // several means we cannot tell which is the real one. Either way, guessing
    // would silently produce wrong `n` values (→ 403s), so defer to ejs.
    let mut ident: Option<&str> = None;
    let mut matches = 0usize;
    for st in body.statements.iter() {
        for (name, fn_body) in candidates(st, player_js) {
            if has_marker(fn_body) {
                matches += 1;
                ident = Some(name);
            }
        }
    }
    if matches != 1 {
        return None;
    }
    let ident = ident?;

    // Rebuild the IIFE body from the surviving statements, then expose the fn.
    let mut inner = String::with_capacity(player_js.len());
    for st in body.statements.iter() {
        if is_window_this_binding(st) || is_bootstrap_call(st) {
            continue;
        }
        inner.push_str(st.span().source_text(player_js));
        inner.push(';');
    }
    inner.push_str("globalThis.__nsig=");
    inner.push_str(ident);
    inner.push(';');

    let (open, close) = (body.span().start as usize + 1, body.span().end as usize - 1);
    let mut out = String::with_capacity(inner.len() + 4096);
    out.push_str(&player_js[..open]);
    out.push_str(&inner);
    out.push_str(&player_js[close..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A miniature player with the same shape as the real one: an IIFE holding
    /// several candidates, only one of which matches ejs's pattern.
    const MINI: &str = r#"var _yt_player={};(function(g){var window=this;
var decoy1=function(a){var D=new g.U(a);D.get("alr")||D.set("alr","yes");return D};
mR=async function(a){var H=new g.U(a);H.set("alr","yes");return H};
Klass=function(){};Klass.prototype.go=function(){this.Zq.set("alr","yes")};
SIGN=function(u,w,h){var J=new g.U(u,!0);J.set("alr","yes");return J};
sideEffect();
})(_yt_player);"#;

    fn ident_of(src: &str) -> Option<String> {
        let out = prepare(src)?;
        let at = out.find("globalThis.__nsig=")? + "globalThis.__nsig=".len();
        Some(out[at..].split(';').next()?.to_string())
    }

    #[test]
    fn the_signing_function_is_found_among_lookalikes() {
        assert_eq!(ident_of(MINI).as_deref(), Some("SIGN"));
    }

    #[test]
    fn the_window_this_binding_quickjs_cannot_evaluate_is_dropped() {
        let out = prepare(MINI).unwrap();
        assert!(!out.contains("var window=this"), "window=this survived: {out}");
    }

    /// A comma-joined `var` list binds the declarator that holds the signing function, not
    /// the first one.
    #[test]
    fn the_signing_declarator_is_bound_in_a_multi_declarator_var() {
        let src = r#"var _yt_player={};(function(g){var window=this;
var pad=1,SIGN=function(u){var J=new g.U(u);J.set("alr","yes");return J};
})(_yt_player);"#;
        assert_eq!(ident_of(src).as_deref(), Some("SIGN"));
    }

    /// The signing function can follow a function-valued decoy declarator.
    #[test]
    fn a_candidate_after_a_decoy_in_the_same_var_list_is_found() {
        let src = r#"var _yt_player={};(function(g){var window=this;
var decoy=function(x){return x},SIGN=function(u){var J=new g.U(u);J.set("alr","yes");return J};
})(_yt_player);"#;
        assert_eq!(ident_of(src).as_deref(), Some("SIGN"));
    }

    #[test]
    fn the_dot_call_iife_shape_is_found() {
        let src = r#"(function(){var window=this;
SIGN=function(u){var J=new U(u);J.set("alr","yes");return J};
}).call(this);"#;
        assert_eq!(ident_of(src).as_deref(), Some("SIGN"));
    }

    /// A truncated player parses under oxc error recovery. Rewriting it by span would mangle it.
    #[test]
    fn a_truncated_player_is_declined() {
        let truncated = &MINI[..MINI.len() - 40];
        assert!(prepare(truncated).is_none());
    }
}
