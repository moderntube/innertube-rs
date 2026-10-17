//! Comments, replies and the comment-action tokens the write methods take.

use super::*;

/// `performCommentActionEndpoint.action` params of a given type from the comments page.
/// The param is URL-encoded base64 of a protobuf: field 1 is the action type (4 remove
/// like, 5 like, 6 dislike, 7 remove dislike), field 3 the target comment id. Pass one to
/// [`crate::Session::perform_comment_action`].
pub fn find_comment_actions(comments_page: &Value, action_type: u8) -> Vec<String> {
    let mut out = Vec::new();
    walk(comments_page, &mut |node| {
        if let Some(a) = node["performCommentActionEndpoint"]["action"].as_str() {
            if decode_comment_action(a).map(|(t, _)| t) == Some(action_type) {
                out.push(a.to_string());
            }
        }
    });
    out
}

/// Find the action param of `action_type` that targets `comment_id` (e.g. the
/// type-5 "like" action for a specific comment).
pub fn find_comment_action_for(comments_page: &Value, comment_id: &str, action_type: u8) -> Option<String> {
    let mut found = None;
    walk(comments_page, &mut |node| {
        if found.is_none() {
            if let Some(a) = node["performCommentActionEndpoint"]["action"].as_str() {
                if decode_comment_action(a) == Some((action_type, comment_id.to_string())) {
                    found = Some(a.to_string());
                }
            }
        }
    });
    found
}

/// Decode a URL-encoded base64 perform-comment-action param → (action type, comment id).
fn decode_comment_action(action: &str) -> Option<(u8, String)> {
    let bytes = crate::client::decode_base64_lenient(&percent_decode(action))?;
    let (mut atype, mut cid) = (None, None);
    let mut i = 0;
    while i < bytes.len() {
        let (tag, ni) = read_pb_varint(&bytes, i)?;
        i = ni;
        let (field, wire) = (tag >> 3, tag & 7);
        match wire {
            0 => {
                let (v, ni) = read_pb_varint(&bytes, i)?;
                i = ni;
                if field == 1 {
                    atype = Some(v as u8);
                }
            }
            2 => {
                let (len, ni) = read_pb_varint(&bytes, i)?;
                i = ni + len as usize;
                if field == 3 {
                    cid = std::str::from_utf8(bytes.get(ni..i)?).ok().map(str::to_string);
                }
            }
            1 => i += 8,
            5 => i += 4,
            _ => return None,
        }
    }
    Some((atype?, cid?))
}

/// Read a protobuf varint at `pos`; returns (value, next position).
pub(super) fn read_pb_varint(bytes: &[u8], pos: usize) -> Option<(u64, usize)> {
    let (mut v, mut shift, mut i) = (0u64, 0u32, pos);
    loop {
        let b = *bytes.get(i)?;
        v |= ((b & 0x7f) as u64) << shift;
        i += 1;
        if b & 0x80 == 0 {
            return Some((v, i));
        }
        shift += 7;
        // A malformed token (more than 9 continuation bytes) would shift past 64: a panic
        // in debug, a wrap in release. Return None.
        if shift >= 64 {
            return None;
        }
    }
}

/// Decode `%XX` percent-encoding (YouTube URL-encodes the base64 `=` padding etc.).
pub(super) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(h) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16) {
                out.push(h);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `createReplyParams` tokens from the comments page (one per reply-able comment).
/// Pass one to [`crate::Session::create_comment_reply`].
pub fn find_reply_params(comments_page: &Value) -> Vec<String> {
    let mut out = Vec::new();
    walk(comments_page, &mut |node| {
        if let Some(p) = node["createReplyParams"].as_str() {
            out.push(p.to_string());
        } else if let Some(p) = node["createCommentReplyEndpoint"]["createReplyParams"].as_str() {
            out.push(p.to_string());
        }
    });
    out
}

/// A single comment (modern `commentEntityPayload` entity-mutation format).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub channel_id: Option<String>,
    pub author_thumbnail_url: Option<String>,
    pub text: String,
    pub published: Option<String>,
    /// Like count as a display string (e.g. `"1.5K"`), as YouTube returns it.
    pub like_count: Option<String>,
    pub reply_count: Option<String>,
    pub is_creator: bool,
    pub is_verified: bool,
    /// "View replies" continuation token (feed to [`Session::comments_page`]); `None`
    /// when the comment has no replies. Joined from the `commentThreadRenderer` by id.
    pub replies_token: Option<String>,
    /// The "like this comment" action param (feed to [`Session::perform_comment_action`]).
    /// Present while the comment is not yet liked by the viewer.
    pub like_token: Option<String>,
    /// The toolbar's "remove like" action param, for undoing a like.
    pub unlike_token: Option<String>,
    /// `createReplyParams` for replying to this comment (feed to
    /// [`Session::create_comment_reply`]). Joined from the `commentThreadRenderer` by
    /// id; present only when the viewer is authed and the comment accepts replies.
    pub reply_params: Option<String>,
    /// 0 for a top-level comment, 1 for a reply, 2 for a reply to a reply.
    pub reply_level: u8,
}

/// A page of comments plus the token for the next page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comments {
    pub items: Vec<Comment>,
    pub continuation: Option<String>,
}

fn comment_from_payload(cep: &Value) -> Option<Comment> {
    let p = &cep["properties"];
    let a = &cep["author"];
    let t = &cep["toolbar"];
    Some(Comment {
        id: p["commentId"].as_str()?.to_string(),
        author: a["displayName"].as_str().unwrap_or_default().to_string(),
        channel_id: a["channelId"].as_str().map(str::to_string),
        author_thumbnail_url: a["avatarThumbnailUrl"]
            .as_str()
            .or_else(|| a["avatar"]["image"]["sources"][0]["url"].as_str())
            .map(str::to_string),
        text: p["content"]["content"].as_str().unwrap_or_default().to_string(),
        published: p["publishedTime"].as_str().map(str::to_string),
        like_count: t["likeCountNotliked"].as_str().map(str::to_string),
        reply_count: t["replyCount"].as_str().map(str::to_string),
        is_creator: a["isCreator"].as_bool().unwrap_or(false),
        is_verified: a["isVerified"].as_bool().unwrap_or(false),
        replies_token: None, // filled by the commentThreadRenderer join in parse_comments
        like_token: None,    // filled by the performCommentActionEndpoint join below
        unlike_token: None,
        reply_params: None,  // filled by the commentThreadRenderer join in parse_comments
        reply_level: p["replyLevel"].as_u64().unwrap_or(0).min(u8::MAX as u64) as u8,
    })
}

/// Parse a comments continuation page. Modern comments arrive as
/// `commentEntityPayload` entities under `frameworkUpdates.entityBatchUpdate`;
/// the next-page token is a `continuationItemRenderer` in the append action.
pub fn parse_comments(v: &Value) -> Comments {
    let mut items = Vec::new();
    if let Some(muts) = v["frameworkUpdates"]["entityBatchUpdate"]["mutations"].as_array() {
        for m in muts {
            let cep = &m["payload"]["commentEntityPayload"];
            if cep.is_object() {
                if let Some(c) = comment_from_payload(cep) {
                    items.push(c);
                }
            }
        }
    }
    // Join each comment to its "view replies" continuation: the contents tree has a
    // `commentThreadRenderer` per comment carrying the same commentId + a replies
    // continuation. Scoped to each thread's `.replies`, so it doesn't touch the
    // page-level next token below.
    if !items.is_empty() {
        let mut replies_by_id: std::collections::HashMap<String, String> = Default::default();
        let mut reply_params_by_id: std::collections::HashMap<String, String> = Default::default();
        walk(v, &mut |node| {
            if let Some(thread) = node.get("commentThreadRenderer") {
                if let Some(cid) = find_str(thread, "commentId") {
                    let mut tok = None;
                    walk(&thread["replies"], &mut |n| {
                        if tok.is_none() {
                            if let Some(t) = continuation_token(n) {
                                tok = Some(t);
                            }
                        }
                    });
                    if let Some(tok) = tok {
                        replies_by_id.entry(cid.clone()).or_insert(tok);
                    }
                    // The reply composer's `createReplyParams` lives in the thread's
                    // comment-reply renderer; scoped to this thread so each comment gets
                    // its own params (the top-level `find_reply_params` can't attribute).
                    let mut rp = None;
                    walk(thread, &mut |n| {
                        if rp.is_none() {
                            if let Some(p) = n["createReplyParams"].as_str() {
                                rp = Some(p.to_string());
                            } else if let Some(p) =
                                n["createCommentReplyEndpoint"]["createReplyParams"].as_str()
                            {
                                rp = Some(p.to_string());
                            }
                        }
                    });
                    if let Some(rp) = rp {
                        reply_params_by_id.entry(cid).or_insert(rp);
                    }
                }
            }
        });
        // In the signed-in shape, `createReplyParams` is not in `commentThreadRenderer`.
        // It arrives as an `engagementToolbarSurfaceEntityPayload` mutation (one per
        // comment) whose `key` embeds the comment id (base64url of a small proto whose
        // field 2 is "<commentId>/<n>", IT-018). Decode each key and substring-match the
        // parsed comment ids. The thread-renderer join above stays as a fallback.
        walk(v, &mut |node| {
            let Some(p) = node.get("engagementToolbarSurfaceEntityPayload") else { return };
            let Some(key) = p["key"].as_str() else { return };
            let mut rp = None;
            walk(&p["replyCommand"], &mut |n| {
                if rp.is_none() {
                    if let Some(s) = n["createReplyParams"].as_str() {
                        rp = Some(s.to_string());
                    }
                }
            });
            let Some(rp) = rp else { return };
            if let Some(decoded) = crate::client::decode_base64_lenient(&percent_decode(key)) {
                let hay = String::from_utf8_lossy(&decoded).into_owned();
                for c in &items {
                    if !c.id.is_empty() && hay.contains(&c.id) {
                        reply_params_by_id.entry(c.id.clone()).or_insert(rp);
                        break;
                    }
                }
            }
        });
        for c in &mut items {
            c.replies_token = replies_by_id.get(&c.id).cloned();
            c.reply_params = reply_params_by_id.get(&c.id).cloned();
        }

        // Per-comment "like" action (type-5 performCommentActionEndpoint), keyed by the
        // comment id the action param decodes to.
        let mut like_by_id: std::collections::HashMap<String, String> = Default::default();
        walk(v, &mut |node| {
            if let Some(a) = node["performCommentActionEndpoint"]["action"].as_str() {
                if let Some((5, cid)) = decode_comment_action(a) {
                    like_by_id.entry(cid).or_insert_with(|| a.to_string());
                }
            }
        });
        let mut unlike_by_id: std::collections::HashMap<String, String> = Default::default();
        walk(v, &mut |node| {
            let a = &node["engagementToolbarSurfaceEntityPayload"]["unlikeCommand"]["innertubeCommand"]
                ["performCommentActionEndpoint"]["action"];
            if let Some(a) = a.as_str() {
                if let Some((_, cid)) = decode_comment_action(a) {
                    unlike_by_id.entry(cid).or_insert_with(|| a.to_string());
                }
            }
        });
        for c in &mut items {
            c.like_token = like_by_id.get(&c.id).cloned();
            c.unlike_token = unlike_by_id.get(&c.id).cloned();
        }
    }

    let mut continuation = None;
    walk(v, &mut |node| {
        if let Some(t) = continuation_token(node) {
            continuation = Some(t); // last (page-level) token wins
        }
    });
    Comments { items, continuation }
}
