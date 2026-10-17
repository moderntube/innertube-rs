//! Watch reporting: the `/api/stats/playback` and `/api/stats/watchtime` reports the web
//! player sends while a video plays. They put the watch, with its progress, into the
//! account's history.
//!
//! Every report is a POST with the form body `session_token=<ytcfg XSRF_TOKEN>` (IT-017).
//! `playback` goes once when playback starts. `watchtime` goes at 7, 17, 27, 41 and 81 s
//! after it and every 40 s from there, each carrying the media spans played since the
//! previous report (`st`/`et`, comma-joined after a seek). A last `state=paused&final=1`
//! report goes on leaving (IT-017).

use std::sync::Arc;
use std::time::Duration;

use crate::client::Session;
use tokio::sync::watch;

/// One playhead sample from the consumer.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Sample {
    media_s: f64,
    playing: bool,
}

/// Seconds after the `playback` report at which the reference player sends `watchtime`.
fn report_times() -> impl Iterator<Item = f64> {
    [7.0, 17.0, 27.0, 41.0].into_iter().chain((0..).map(|i| 81.0 + 40.0 * i as f64))
}

/// The Shorts page reports about every 10 s, the first report 4.6 to 8.2 s in (IT-017).
fn short_report_times() -> impl Iterator<Item = f64> {
    (0..).map(|i| 8.0 + 10.0 * i as f64)
}

/// Largest playhead step between two one-second samples that still counts as continuous play
/// (covers playback rates up to 4x plus sampling jitter).
const MAX_STEP_S: f64 = 5.0;

/// The media spans actually played, built from periodic playhead samples.
#[derive(Debug, Default)]
struct Spans {
    done: Vec<(f64, f64)>,
    open: Option<(f64, f64)>,
}

impl Spans {
    fn sample(&mut self, s: Sample) {
        let continues = self.open.is_some_and(|(_, et)| s.playing && s.media_s >= et && s.media_s - et <= MAX_STEP_S);
        match (&mut self.open, s.playing) {
            (Some(span), true) if continues => span.1 = s.media_s,
            (open, playing) => {
                if let Some(span) = open.take().filter(|(st, et)| et > st) {
                    self.done.push(span);
                }
                *open = playing.then_some((s.media_s, s.media_s));
            }
        }
    }

    /// The spans since the last call. An open span is split at its current end, so the next
    /// report starts where this one stopped.
    fn take(&mut self) -> Vec<(f64, f64)> {
        let mut out = std::mem::take(&mut self.done);
        if let Some((st, et)) = self.open {
            if et > st {
                out.push((st, et));
            }
            self.open = Some((et, et));
        }
        out
    }
}

/// What the watch page supplies for reporting.
#[derive(Debug, Clone)]
struct PageContext {
    session_token: String,
    visitor_id: Option<String>,
    client_version: String,
    playback_url: String,
    watchtime_url: String,
    length_s: f64,
}

/// The JSON string literal that follows `key` in `html`, unescaped.
fn json_str_after(html: &str, key: &str) -> Option<String> {
    let start = html.find(key)? + key.len();
    let bytes = html.as_bytes();
    let mut i = start;
    while i < bytes.len() && bytes[i] != b'"' {
        i += if bytes[i] == b'\\' { 2 } else { 1 };
    }
    serde_json::from_str(&format!("\"{}\"", html.get(start..i)?)).ok()
}

impl PageContext {
    fn from_watch_html(html: &str) -> Option<Self> {
        Some(Self {
            session_token: json_str_after(html, "\"XSRF_TOKEN\":\"")?,
            visitor_id: crate::client::extract_visitor_data(html),
            client_version: json_str_after(html, "\"INNERTUBE_CLIENT_VERSION\":\"")?,
            playback_url: json_str_after(html, "\"videostatsPlaybackUrl\":{\"baseUrl\":\"")?,
            watchtime_url: json_str_after(html, "\"videostatsWatchtimeUrl\":{\"baseUrl\":\"")?,
            length_s: json_str_after(html, "\"lengthSeconds\":\"")?.parse().ok()?,
        })
    }
}

fn encode(v: &str) -> String {
    v.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `base` with each of `extra` appended unless `base` already carries that key.
fn with_params(base: &str, extra: &[(&str, String)]) -> String {
    let query = base.split_once('?').map_or("", |(_, q)| q);
    let has = |k: &str| query.split('&').any(|p| p.split('=').next() == Some(k));
    let mut out = base.to_string();
    let mut sep = if base.contains('?') { '&' } else { '?' };
    for (k, v) in extra.iter().filter(|(k, _)| !has(k)) {
        out.push(sep);
        out.push_str(k);
        out.push('=');
        out.push_str(&encode(v));
        sep = '&';
    }
    out
}

fn join(spans: &[(f64, f64)], pick: impl Fn(&(f64, f64)) -> f64) -> String {
    spans.iter().map(|s| format!("{:.3}", pick(s))).collect::<Vec<_>>().join(",")
}

/// Reports one video's watch to the signed-in account. Feed it the playhead with
/// [`WatchReport::update`]; dropping it sends the final report.
pub struct WatchReport {
    tx: watch::Sender<Option<Sample>>,
}

impl WatchReport {
    /// The handle, and the task that does the reporting (spawn it on a tokio runtime). The task
    /// reports nothing for a signed-out session, and ends after the final report.
    pub fn new(session: Arc<Session>, video_id: &str) -> (Self, impl std::future::Future<Output = ()> + Send + 'static) {
        let (tx, rx) = watch::channel(None);
        (Self { tx }, run(session, video_id.to_string(), rx, false))
    }

    /// Like [`WatchReport::new`], for a Short watched on the Shorts page.
    pub fn new_short(session: Arc<Session>, video_id: &str) -> (Self, impl std::future::Future<Output = ()> + Send + 'static) {
        let (tx, rx) = watch::channel(None);
        (Self { tx }, run(session, video_id.to_string(), rx, true))
    }

    pub fn update(&self, media_s: f64, playing: bool) {
        self.tx.send_replace(Some(Sample { media_s, playing }));
    }
}

async fn run(session: Arc<Session>, video_id: String, mut rx: watch::Receiver<Option<Sample>>, short: bool) {
    if !session.is_authenticated() {
        return;
    }
    let watch_url = format!("https://www.youtube.com/watch?v={video_id}");
    let ctx = match session.get_text(&watch_url).await.ok().as_deref().and_then(PageContext::from_watch_html) {
        Some(c) => c,
        None => {
            tracing::warn!("[watch-report] {video_id}: no reporting context on the watch page");
            return;
        }
    };
    let cpn = crate::client::client_playback_nonce();
    let common = |cmt: f64, rt: f64| -> Vec<(&'static str, String)> {
        vec![
            ("ns", "yt".into()),
            ("el", if short { "shortspage" } else { "detailpage" }.into()),
            ("cpn", cpn.clone()),
            ("ver", "2".into()),
            ("cmt", format!("{cmt:.3}")),
            ("fs", "0".into()),
            ("rt", format!("{rt:.3}")),
            ("euri", String::new()),
            ("lact", format!("{}", (rt * 1000.0) as u64)),
            ("volume", "100".into()),
            ("cbr", "Chrome".into()),
            ("cbrver", crate::client::WEB_BROWSER_VERSION.into()),
            ("c", "WEB".into()),
            ("cver", ctx.client_version.clone()),
            ("cplayer", "UNIPLAYER".into()),
            ("cos", "X11".into()),
            ("cplatform", "DESKTOP".into()),
            ("hl", "en_US".into()),
            ("cr", "US".into()),
            ("len", format!("{:.3}", ctx.length_s)),
            ("muted", "0".into()),
        ]
    };
    let post = |name: &'static str, url: String| {
        let (session, ctx, watch_url, video_id) = (session.clone(), ctx.clone(), watch_url.clone(), video_id.clone());
        async move {
            if let Err(e) = session.post_stats(&url, &ctx.session_token, ctx.visitor_id.as_deref(), &watch_url).await {
                tracing::warn!("[watch-report] {video_id} {name}: {e}");
            }
        }
    };

    // `playback` goes when the playhead first moves.
    let mut spans = Spans::default();
    let first = loop {
        if rx.changed().await.is_err() {
            return;
        }
        let s = *rx.borrow_and_update();
        if let Some(s) = s.filter(|s| s.playing) {
            break s;
        }
    };
    spans.sample(first);
    let t0 = tokio::time::Instant::now();
    let schedule: Box<dyn Iterator<Item = f64> + Send> =
        if short { Box::new(short_report_times()) } else { Box::new(report_times()) };
    let mut times = schedule.peekable();
    let mut q = common(first.media_s, 0.0);
    q.extend([("mos", "0".into()), ("state", "playing".into()), ("rtn", "7".into())]);
    post("playback", with_params(&ctx.playback_url, &q)).await;
    tracing::info!("[watch-report] {video_id}: reporting");

    let mut last = first;
    let mut prev_rt = 0.0;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut closed = false;
    loop {
        let due = *times.peek().expect("endless schedule");
        tokio::select! {
            _ = tick.tick() => {}
            r = rx.changed() => closed = r.is_err(),
        }
        if let Some(s) = *rx.borrow_and_update() {
            last = s;
        }
        spans.sample(last);
        let rt = t0.elapsed().as_secs_f64();
        if !closed && rt < due {
            continue;
        }
        if !closed {
            times.next();
        }
        let played = spans.take();
        if played.is_empty() && !closed {
            prev_rt = rt;
            continue;
        }
        let mut q = common(last.media_s, rt);
        q.extend([
            ("state", if closed || !last.playing { "paused" } else { "playing" }.into()),
            ("rtn", format!("{}", times.peek().copied().unwrap_or(rt + 40.0))),
            ("rti", format!("{}", prev_rt.round())),
            ("st", join(&played, |s| s.0)),
            ("et", join(&played, |s| s.1)),
        ]);
        if closed {
            q.push(("final", "1".into()));
        }
        post("watchtime", with_params(&ctx.watchtime_url, &q)).await;
        prev_rt = rt;
        if closed {
            tracing::info!("[watch-report] {video_id}: final report sent");
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(spans: &mut Spans, samples: &[(f64, bool)]) {
        for &(media_s, playing) in samples {
            spans.sample(Sample { media_s, playing });
        }
    }

    #[test]
    fn continuous_play_is_reported_as_adjacent_spans() {
        let mut s = Spans::default();
        feed(&mut s, &[(0.0, true), (1.0, true), (2.0, true)]);
        assert_eq!(s.take(), vec![(0.0, 2.0)]);
        feed(&mut s, &[(3.0, true), (4.0, true)]);
        assert_eq!(s.take(), vec![(2.0, 4.0)]);
    }

    #[test]
    fn a_seek_reports_the_span_before_and_after_it_separately() {
        let mut s = Spans::default();
        feed(&mut s, &[(40.0, true), (41.0, true), (360.0, true), (362.0, true), (15.0, true), (16.0, true)]);
        assert_eq!(s.take(), vec![(40.0, 41.0), (360.0, 362.0), (15.0, 16.0)]);
    }

    #[test]
    fn paused_time_is_not_reported() {
        let mut s = Spans::default();
        feed(&mut s, &[(5.0, true), (6.0, true), (6.0, false), (6.0, false)]);
        assert_eq!(s.take(), vec![(5.0, 6.0)]);
        feed(&mut s, &[(6.0, false), (6.0, false)]);
        assert!(s.take().is_empty());
        feed(&mut s, &[(6.0, true), (7.0, true)]);
        assert_eq!(s.take(), vec![(6.0, 7.0)]);
    }

    #[test]
    fn the_report_context_is_read_from_the_watch_page() {
        let html = r#"ytcfg.set({"XSRF_TOKEN":"QUFF=","INNERTUBE_CLIENT_VERSION":"2.20260928.00.00","visitorData":"CgtBQUFBQUFBQUFBQSiA8tbKBjIK"});
            var ytInitialPlayerResponse = {"playbackTracking":{"videostatsPlaybackUrl":{"baseUrl":"https://s.youtube.com/api/stats/playback?cl=1&docid=x"},
            "videostatsWatchtimeUrl":{"baseUrl":"https://s.youtube.com/api/stats/watchtime?cl=1&docid=x"}},"videoDetails":{"lengthSeconds":"763"}};"#;
        let c = PageContext::from_watch_html(html).expect("context");
        assert_eq!(c.session_token, "QUFF=");
        assert_eq!(c.playback_url, "https://s.youtube.com/api/stats/playback?cl=1&docid=x");
        assert_eq!(c.length_s, 763.0);
        assert!(c.visitor_id.is_some());
    }

}
