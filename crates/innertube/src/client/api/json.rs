//! Generic walkers over InnerTube renderer JSON.

use super::*;

/// Text from a `{simpleText}` or `{runs:[{text}]}` renderer.
pub(super) fn text(v: &Value) -> Option<String> {
    if let Some(s) = v.get("simpleText").and_then(Value::as_str) {
        return Some(s.to_string());
    }
    let runs = v.get("runs")?.as_array()?;
    let s: String = runs.iter().filter_map(|r| r["text"].as_str()).collect();
    (!s.is_empty()).then_some(s)
}

/// The continuation token inside a `continuationItemRenderer`. Auto-load expanders (next
/// page of search/comments) put it under `continuationEndpoint`; "show more replies"
/// buttons put it under `button.buttonRenderer.command`. Both are read, or reply-thread
/// pagination stops after the first page.
pub(super) fn continuation_token(v: &Value) -> Option<String> {
    let cir = &v["continuationItemRenderer"];
    cir["continuationEndpoint"]["continuationCommand"]["token"]
        .as_str()
        .or_else(|| cir["button"]["buttonRenderer"]["command"]["continuationCommand"]["token"].as_str())
        .map(str::to_string)
}

/// First string value for `key` found anywhere in the tree (DFS, earliest wins).
/// Used to pluck `playlistId` / `createCommentParams` out of a response.
pub fn find_str(v: &Value, key: &str) -> Option<String> {
    let mut found: Option<String> = None;
    walk(v, &mut |node| {
        if found.is_none() {
            if let Some(s) = node.get(key).and_then(Value::as_str) {
                found = Some(s.to_string());
            }
        }
    });
    found
}

/// Depth-first visit of every object node, calling `f` on each.
pub(super) fn walk(v: &Value, f: &mut impl FnMut(&Value)) {
    match v {
        Value::Object(m) => {
            f(v);
            for x in m.values() {
                walk(x, f);
            }
        }
        Value::Array(a) => {
            for x in a {
                walk(x, f);
            }
        }
        _ => {}
    }
}

/// [`walk`], but `f` returning `false` skips that object's children.
pub(super) fn walk_pruned(v: &Value, f: &mut impl FnMut(&Value) -> bool) {
    match v {
        Value::Object(m) if f(v) => {
            for x in m.values() {
                walk_pruned(x, f);
            }
        }
        Value::Array(a) => {
            for x in a {
                walk_pruned(x, f);
            }
        }
        _ => {}
    }
}
