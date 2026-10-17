//! Lockstep replay of a captured SABR session through `SabrEngine`.
//!
//! A fixture (`tests/fixtures/sessions/*.replay`, written by
//! `scripts/extract_replay_session.py`) holds the reference client's requests and the
//! server's responses in order. The harness takes the player's own inputs from
//! reference request n (playhead, viewport, bandwidth estimate, PO token, and in VOD the
//! buffered ranges, which come from the player's segment store), builds our request n,
//! and diffs it with the reference field by field. It then feeds response n to the
//! engine. A response answers the request that produced it, so the first round whose
//! request differs ends the replay.
//!
//! An empty string field compares equal to an absent one: the reference sends an empty
//! `xtags` in every `FormatId` that has none, and the engine omits it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use innertube_sabr::{
    parse, write_part, write_varint, Ingest, SabrClientInfo, SabrEngine, SelectedFormat,
    VodBufferedRange, LIVE_EDGE_SEEK_MS,
};

const META: u32 = 1;
const REQUEST: u32 = 2;
const RESPONSE: u32 = 3;
const MEDIA: u32 = 21;
const MEDIA_ELIDED: u32 = 1021;

/// Audio itags YouTube serves, for media headers whose itag no preference list names.
const AUDIO_ITAGS: &[i32] = &[139, 140, 141, 171, 249, 250, 251, 256, 258, 327, 338, 599, 600];

pub struct Session {
    pub meta: BTreeMap<String, String>,
    pub rounds: Vec<Round>,
}

pub struct Round {
    pub request: Vec<u8>,
    pub response: Vec<u8>,
}

impl Session {
    pub fn load(name: &str) -> Session {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sessions").join(name);
        let data = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let mut meta = BTreeMap::new();
        let mut rounds = Vec::new();
        let mut request = None;
        for rec in parse(&data[..]) {
            match rec.part_type {
                META => {
                    for line in std::str::from_utf8(&rec.payload).unwrap().lines() {
                        if let Some((k, v)) = line.split_once('=') {
                            meta.insert(k.to_string(), v.to_string());
                        }
                    }
                }
                REQUEST => request = Some(rec.payload.to_vec()),
                RESPONSE => rounds.push(Round {
                    request: request.take().expect("RESPONSE before REQUEST"),
                    response: expand_media(&rec.payload),
                }),
                t => panic!("unknown record {t}"),
            }
        }
        Session { meta, rounds }
    }

    pub fn is_live(&self) -> bool {
        self.meta.get("live").map(String::as_str) == Some("1")
    }
}

/// Rebuild MEDIA parts from MEDIA_ELIDED: header id, then `[literal][zero run]` pairs.
fn expand_media(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    for part in parse(body) {
        if part.part_type != MEDIA_ELIDED {
            write_part(&mut out, part.part_type, &part.payload);
            continue;
        }
        let p = &part.payload[..];
        let (header_id, mut i) = ump_varint(p, 0);
        let mut media = Vec::new();
        write_varint(&mut media, header_id);
        while i < p.len() {
            let (n, j) = ump_varint(p, i);
            media.extend_from_slice(&p[j..j + n as usize]);
            let (zeros, k) = ump_varint(p, j + n as usize);
            media.resize(media.len() + zeros as usize, 0);
            i = k;
        }
        write_part(&mut out, MEDIA, &media);
    }
    out
}

fn ump_varint(p: &[u8], i: usize) -> (u32, usize) {
    let b = |k: usize| u32::from(p[i + k]);
    match p[i] {
        0..=0x7f => (b(0), i + 1),
        0x80..=0xbf => ((b(0) & 0x3f) | b(1) << 6, i + 2),
        0xc0..=0xdf => ((b(0) & 0x1f) | b(1) << 5 | b(2) << 13, i + 3),
        0xe0..=0xef => ((b(0) & 0x0f) | b(1) << 4 | b(2) << 12 | b(3) << 20, i + 4),
        _ => (u32::from_le_bytes(p[i + 1..i + 5].try_into().unwrap()), i + 5),
    }
}

/// A protobuf value read without a schema.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Val {
    Int(u64),
    Fixed(Vec<u8>),
    Bytes(Vec<u8>),
    Msg(Vec<(u32, Val)>),
}

fn pb_varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let x = *b.get(*i)?;
        *i += 1;
        v |= u64::from(x & 0x7f) << shift;
        if x & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// Decode `b` as a message, or `None` if it is not one. Length-delimited fields are
/// decoded as messages when they parse as one, and kept as bytes otherwise.
pub fn decode(b: &[u8]) -> Option<Vec<(u32, Val)>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let key = pb_varint(b, &mut i)?;
        let (field, wire) = ((key >> 3) as u32, key & 7);
        if field == 0 {
            return None;
        }
        let v = match wire {
            0 => Val::Int(pb_varint(b, &mut i)?),
            1 | 5 => {
                let n = if wire == 1 { 8 } else { 4 };
                let v = Val::Fixed(b.get(i..i + n)?.to_vec());
                i += n;
                v
            }
            2 => {
                let n = pb_varint(b, &mut i)? as usize;
                let s = b.get(i..i + n)?;
                i += n;
                match decode(s) {
                    Some(m) if !s.is_empty() => Val::Msg(m),
                    _ => Val::Bytes(s.to_vec()),
                }
            }
            _ => return None,
        };
        out.push((field, v));
    }
    Some(out)
}

/// Every value at a dotted field path (`"19.1.17"`).
pub fn get<'a>(msg: &'a [(u32, Val)], path: &str) -> Vec<&'a Val> {
    let mut cur: Vec<&[(u32, Val)]> = vec![msg];
    let fields: Vec<u32> = path.split('.').map(|f| f.parse().unwrap()).collect();
    let (last, inner) = fields.split_last().unwrap();
    for f in inner {
        cur = cur
            .iter()
            .flat_map(|m| m.iter().filter(|(k, _)| k == f))
            .filter_map(|(_, v)| if let Val::Msg(m) = v { Some(&m[..]) } else { None })
            .collect();
    }
    cur.iter().flat_map(|m| m.iter().filter(|(k, _)| k == last).map(|(_, v)| v)).collect()
}

fn int(msg: &[(u32, Val)], path: &str) -> Option<u64> {
    get(msg, path).into_iter().find_map(|v| if let Val::Int(n) = v { Some(*n) } else { None })
}

fn raw(v: &Val) -> Vec<u8> {
    match v {
        Val::Bytes(b) | Val::Fixed(b) => b.clone(),
        Val::Msg(m) => encode(m),
        Val::Int(_) => panic!("not length-delimited"),
    }
}

fn encode(m: &[(u32, Val)]) -> Vec<u8> {
    fn varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }
    let mut out = Vec::new();
    for (f, v) in m {
        let wire = match v {
            Val::Int(_) => 0,
            Val::Fixed(b) if b.len() == 8 => 1,
            Val::Fixed(_) => 5,
            _ => 2,
        };
        varint(&mut out, u64::from(*f) << 3 | wire);
        match v {
            Val::Int(n) => varint(&mut out, *n),
            Val::Fixed(b) => out.extend(b),
            Val::Bytes(_) | Val::Msg(_) => {
                let b = raw(v);
                varint(&mut out, b.len() as u64);
                out.extend(b);
            }
        }
    }
    out
}

/// Leaves of a message as `(path, value)`. Each element of a repeated message field is
/// one leaf holding its own sorted leaves, so elements compare whole and in any order.
fn leaves(msg: &[(u32, Val)], prefix: &str, skip: &dyn Fn(&str) -> bool, out: &mut Vec<(String, String)>) {
    let mut seen_msg: BTreeMap<u32, usize> = BTreeMap::new();
    for (f, v) in msg {
        if let Val::Msg(_) = v {
            *seen_msg.entry(*f).or_default() += 1;
        }
    }
    for (f, v) in msg {
        let path = if prefix.is_empty() { f.to_string() } else { format!("{prefix}.{f}") };
        if skip(&path) {
            continue;
        }
        match v {
            Val::Msg(m) if seen_msg[f] > 1 => {
                let mut inner = Vec::new();
                leaves(m, &path, skip, &mut inner);
                inner.sort();
                let s = inner
                    .iter()
                    .map(|(p, v)| format!("{}={v}", &p[path.len() + 1..]))
                    .collect::<Vec<_>>()
                    .join(" ");
                out.push((path, format!("{{{s}}}")));
            }
            Val::Msg(m) => leaves(m, &path, skip, out),
            Val::Int(n) => out.push((path, n.to_string())),
            Val::Fixed(b) => out.push((path, format!("0x{}", hex(b)))),
            Val::Bytes(b) if b.is_empty() => {}
            Val::Bytes(b) => out.push((path, show(b))),
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn show(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) if s.len() <= 48 && s.chars().all(|c| !c.is_control()) => format!("{s:?}"),
        _ if b.len() <= 24 => format!("0x{}", hex(b)),
        _ => format!("<{} bytes {:016x}>", b.len(), fnv(b)),
    }
}

fn fnv(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325, |h, x| (h ^ u64::from(*x)).wrapping_mul(0x100_0000_01b3))
}

/// One field path whose values differ between our request and the reference's.
#[derive(Debug)]
pub struct FieldDiff {
    pub path: String,
    pub ours: Vec<String>,
    pub reference: Vec<String>,
}

/// Field paths the harness does not compare, each with the reason. A path covers
/// everything under it.
pub type Allowed<'a> = &'a [(&'a str, &'a str)];

pub fn diff(ours: &[u8], reference: &[u8], allowed: Allowed) -> Vec<FieldDiff> {
    let skip = |p: &str| allowed.iter().any(|(a, _)| p == *a || p.starts_with(&format!("{a}.")));
    let collect = |b: &[u8]| {
        let mut l = Vec::new();
        leaves(&decode(b).expect("request is not a message"), "", &skip, &mut l);
        let mut by: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (p, v) in l {
            by.entry(p).or_default().push(v);
        }
        for v in by.values_mut() {
            v.sort();
        }
        by
    };
    let (a, b) = (collect(ours), collect(reference));
    let paths: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    paths
        .into_iter()
        .filter(|p| a.get(*p) != b.get(*p))
        .map(|p| FieldDiff {
            path: p.clone(),
            ours: a.get(p).cloned().unwrap_or_default(),
            reference: b.get(p).cloned().unwrap_or_default(),
        })
        .collect()
}

/// Where a replay stopped.
#[derive(Debug)]
pub struct Divergence {
    pub round: usize,
    pub diffs: Vec<FieldDiff>,
}

impl std::fmt::Display for Divergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "request {} differs from the reference:", self.round)?;
        for d in &self.diffs {
            writeln!(f, "  {}\n    ours: {:?}\n    ref:  {:?}", d.path, d.ours, d.reference)?;
        }
        Ok(())
    }
}

fn formats(prefs: &[&Val], is_video: bool) -> Vec<SelectedFormat> {
    prefs
        .iter()
        .filter_map(|v| if let Val::Msg(m) = v { Some(m) } else { None })
        .map(|m| SelectedFormat {
            itag: int(m, "1").unwrap_or(0) as i32,
            last_modified: int(m, "2").unwrap_or(0),
            is_video,
            xtags: get(m, "3").first().map(|v| String::from_utf8(raw(v)).unwrap()).filter(|s| !s.is_empty()),
        })
        .collect()
}

/// The engine as the app builds it for this session, from what the reference's first
/// request and the responses' media headers say about the formats.
fn engine_for(session: &Session) -> SabrEngine {
    let req0 = decode(&session.rounds[0].request).unwrap();
    let mut selected = formats(&get(&req0, "17"), true);
    let video: BTreeSet<i32> = selected.iter().map(|f| f.itag).collect();
    selected.extend(formats(&get(&req0, "16"), false));

    let mut all = selected.clone();
    for round in &session.rounds {
        for part in parse(&round.response[..]).into_iter().filter(|p| p.part_type == 20) {
            let h = decode(&part.payload).unwrap_or_default();
            let (Some(itag), Some(lmt)) = (int(&h, "3"), int(&h, "4")) else { continue };
            let itag = itag as i32;
            if !all.iter().any(|f| f.itag == itag) {
                let is_video = video.contains(&itag) || !AUDIO_ITAGS.contains(&itag);
                all.push(SelectedFormat { itag, last_modified: lmt, is_video, xtags: None });
            }
        }
    }
    if session.is_live() {
        for f in selected.iter_mut().chain(all.iter_mut()) {
            if f.last_modified == 0 {
                f.last_modified = all_lmt(session, f.itag).unwrap_or(0);
            }
        }
    }

    let config = get(&req0, "5").first().map(|v| raw(v)).unwrap_or_default();
    let mut engine = SabrEngine::new(config, Vec::new(), selected, &all);
    engine.set_live(session.is_live());
    engine.set_collect_segments(true);
    let ci = |p: &str| get(&req0, &format!("19.1.{p}")).first().map(|v| String::from_utf8(raw(v)).unwrap());
    engine.set_client_info(SabrClientInfo {
        client_name: int(&req0, "19.1.16").unwrap_or(0) as i32,
        client_version: ci("17").unwrap_or_default(),
        os_name: ci("18").unwrap_or_default(),
        os_version: ci("19").unwrap_or_default(),
    });
    if let Some(cap) = int(&req0, "1.72.2") {
        engine.set_res_ceiling(cap as i32);
    }
    engine.set_inline(int(&req0, "1.34") == Some(5));
    if let Some(itag) = session.rounds.iter().find_map(|r| int(&decode(&r.request).unwrap(), "24.5")) {
        engine.set_ad_report(Some(itag as i32));
    }
    if session.is_live() {
        engine.set_live_segment_ms(live_segment_ms(session));
    }
    engine
}

fn all_lmt(session: &Session, itag: i32) -> Option<u64> {
    session.rounds.iter().find_map(|r| {
        parse(&r.response[..]).into_iter().filter(|p| p.part_type == 20).find_map(|p| {
            let h = decode(&p.payload)?;
            (int(&h, "3")? as i32 == itag).then(|| int(&h, "4")).flatten()
        })
    })
}

/// The format's `targetDurationSec` in ms, which the web client reports as the length
/// of a one-segment live range.
fn live_segment_ms(session: &Session) -> i64 {
    session
        .rounds
        .iter()
        .flat_map(|r| {
            let m = decode(&r.request).unwrap();
            get(&m, "3")
                .into_iter()
                .filter_map(|v| if let Val::Msg(b) = v { Some(b.clone()) } else { None })
                .collect::<Vec<_>>()
        })
        .find(|b| int(b, "4") == int(b, "5"))
        .and_then(|b| int(&b, "3"))
        .unwrap_or(0) as i64
}

/// Apply the player's inputs from reference request `req` and build our request.
fn our_request(engine: &mut SabrEngine, req: &[(u32, Val)], live: bool, anchored: bool) -> Vec<u8> {
    if let Some(Val::Bytes(t) | Val::Fixed(t)) = get(req, "19.2").first() {
        engine.set_po_token(t.clone());
    } else if let Some(Val::Msg(m)) = get(req, "19.2").first() {
        engine.set_po_token(encode(m));
    }
    if let (Some(w), Some(h)) = (int(req, "1.18"), int(req, "1.19")) {
        engine.set_viewport(w as i32, h as i32);
    }
    if let Some(bw) = int(req, "1.23") {
        engine.set_bandwidth_estimate(bw as i64);
    }
    let rate = get(req, "1.35").first().map(|v| f32::from_le_bytes(raw(v)[..4].try_into().unwrap()));
    engine.set_playback_rate(rate.unwrap_or(1.0));
    let ref_pt = int(req, "1.28").map(|v| v as i64);
    if live {
        engine.set_keep_ranges_on_seek(anchored);
        let playhead = ref_pt.filter(|&p| p != LIVE_EDGE_SEEK_MS);
        let pt = engine.next_player_time(playhead);
        engine.request_bytes(pt)
    } else {
        let ranges = get(req, "3")
            .into_iter()
            .filter_map(|v| if let Val::Msg(m) = v { Some(m) } else { None })
            .map(|m| VodBufferedRange {
                itag: int(m, "1.1").unwrap_or(0) as i32,
                start_time_ms: int(m, "2").unwrap_or(0) as i64,
                duration_ms: int(m, "3").unwrap_or(0) as i64,
                start_segment_index: int(m, "4").unwrap_or(0) as i32,
                end_segment_index: int(m, "5").unwrap_or(0) as i32,
            })
            .collect();
        engine.set_vod_buffered_ranges(ranges);
        engine.request_bytes(ref_pt.unwrap_or(0))
    }
}

/// The engine clamps a live `player_time` to `max_seekable`; the reference sends its
/// playhead past it. Only that case is allowed, so the join's seek target still compares.
fn is_max_seekable_clamp(d: &FieldDiff, max_seekable: Option<i64>) -> bool {
    let one = |v: &[String]| v.first().and_then(|s| s.parse::<i64>().ok());
    match (d.path.as_str(), one(&d.ours), one(&d.reference), max_seekable) {
        ("1.28", Some(ours), Some(theirs), Some(max)) => ours == max && theirs > max,
        _ => false,
    }
}

/// Visibility 3 is the reference's tab in the background, a player state the app has no
/// counterpart for.
fn is_background_tab(d: &FieldDiff) -> bool {
    d.path == "1.34" && d.reference == ["3"]
}

/// A completed replay: every request matched, and `ingests[n]` is what the engine
/// emitted for response n.
pub struct Replay {
    pub ingests: Vec<Ingest>,
}

/// The engine after the first `rounds` rounds of `session`, for fault injection.
pub fn engine_after(session: &Session, rounds: usize) -> SabrEngine {
    let mut engine = engine_for(session);
    let mut anchored = false;
    for round in &session.rounds[..rounds] {
        let req = decode(&round.request).unwrap();
        our_request(&mut engine, &req, session.is_live(), anchored);
        engine.ingest(&round.response[..]);
        anchored |= engine.buffered_end_ms().is_some();
    }
    engine
}

/// Replay `session` in lockstep, recording what the engine emits for each response.
pub fn replay(session: &Session, allowed: Allowed) -> Result<Replay, Divergence> {
    let mut engine = engine_for(session);
    let live = session.is_live();
    let mut anchored = false;
    let mut ingests = Vec::new();
    for (n, round) in session.rounds.iter().enumerate() {
        let req = decode(&round.request).unwrap();
        let ours = our_request(&mut engine, &req, live, anchored);
        let mut diffs = diff(&ours, &round.request, allowed);
        diffs.retain(|d| !is_max_seekable_clamp(d, engine.max_seekable_ms()) && !is_background_tab(d));
        if !diffs.is_empty() {
            return Err(Divergence { round: n, diffs });
        }
        ingests.push(engine.ingest(&round.response[..]));
        anchored |= engine.buffered_end_ms().is_some();
    }
    Ok(Replay { ingests })
}
