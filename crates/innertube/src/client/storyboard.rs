//! Seek-preview storyboards: VOD sheets by time, live sheets by segment sequence.

/// Seek-preview thumbnails: YouTube ships them as sprite sheets (a grid of small
/// frames per image) at several zoom levels, described by `playerStoryboardSpecRenderer`.
/// [`Storyboard::thumbnail_at`] maps a playback time to the sheet URL + crop rect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Storyboard {
    levels: Vec<StoryboardLevel>,
    /// Index into `levels` to use for scrub previews (server's fine-scrubbing hint).
    default_level: usize,
}

/// One zoom level of a storyboard.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StoryboardLevel {
    thumb_w: u32,
    thumb_h: u32,
    cols: u32,
    rows: u32,
    /// Time between consecutive thumbnails (ms). 0 on the legacy level-0 "default" sheet.
    interval_ms: u32,
    total_frames: u32,
    /// Base url with `$L` already substituted to this level; still has `$N`/`?sqp=`.
    base: String,
    /// `$N` replacement (contains `$M`, the sheet index).
    name: String,
    /// The `sigh` value, appended whole including its `rs$` prefix.
    sigh: String,
}

/// A single seek-preview thumbnail: which sheet to fetch + the cell to crop out of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryboardThumb {
    pub url: String,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Storyboard {
    /// Parse a `playerStoryboardSpecRenderer.spec` string (`base|level0|level1|…`,
    /// each level `w#h#totalFrames#cols#rows#intervalMs#name#sigh`). `fine_level` is the
    /// renderer's `fineScrubbingRecommendedLevel`/`recommendedLevel` (clamped).
    pub fn parse(spec: &str, fine_level: Option<usize>) -> Option<Storyboard> {
        let mut parts = spec.split('|');
        let base_tpl = parts.next()?;
        let mut levels = Vec::new();
        for (i, p) in parts.enumerate() {
            let f: Vec<&str> = p.split('#').collect();
            if f.len() != 8 {
                continue;
            }
            levels.push(StoryboardLevel {
                thumb_w: f[0].parse().ok()?,
                thumb_h: f[1].parse().ok()?,
                total_frames: f[2].parse().ok()?,
                cols: f[3].parse().ok()?,
                rows: f[4].parse().ok()?,
                interval_ms: f[5].parse().ok()?,
                base: base_tpl.replace("$L", &i.to_string()),
                name: f[6].to_string(),
                sigh: f[7].to_string(),
            });
        }
        if levels.is_empty() {
            return None;
        }
        // Prefer the fine-scrub level; clamp to a real level; else the finest non-legacy.
        let last = levels.len() - 1;
        let default_level = fine_level.filter(|&l| l < levels.len()).unwrap_or(last).max(1).min(last);
        Some(Storyboard { levels, default_level })
    }

    /// The thumbnail covering playback position `time_ms` (using the scrub-preview level).
    pub fn thumbnail_at(&self, time_ms: i64) -> Option<StoryboardThumb> {
        let lvl = self.levels.get(self.default_level)?;
        if lvl.interval_ms == 0 || lvl.cols == 0 || lvl.rows == 0 {
            return None;
        }
        let per_sheet = lvl.cols * lvl.rows;
        let frame = ((time_ms.max(0) as u32) / lvl.interval_ms).min(lvl.total_frames.saturating_sub(1));
        let (sheet, within) = (frame / per_sheet, frame % per_sheet);
        let name = lvl.name.replace("$M", &sheet.to_string());
        Some(StoryboardThumb {
            url: format!("{}&sigh={}", lvl.base.replace("$N", &name), lvl.sigh),
            x: (within % lvl.cols) * lvl.thumb_w,
            y: (within / lvl.cols) * lvl.thumb_h,
            w: lvl.thumb_w,
            h: lvl.thumb_h,
        })
    }
}

/// Live seek-preview thumbnails (`playerLiveStoryboardSpecRenderer.spec`, `url#w#h#cols#rows`
/// with `$M` the sheet). One frame per live segment, numbered by the segment's sequence
/// number (base.js `IST.G` returns the segment index of a time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveStoryboard {
    template: String,
    thumb_w: u32,
    thumb_h: u32,
    cols: u32,
    rows: u32,
}

impl LiveStoryboard {
    pub fn parse(spec: &str) -> Option<LiveStoryboard> {
        let f: Vec<&str> = spec.split('#').collect();
        let [template, w, h, cols, rows] = f.as_slice() else { return None };
        let sb = LiveStoryboard {
            template: template.to_string(),
            thumb_w: w.parse().ok()?,
            thumb_h: h.parse().ok()?,
            cols: cols.parse().ok()?,
            rows: rows.parse().ok()?,
        };
        (sb.cols > 0 && sb.rows > 0 && sb.template.contains("$M")).then_some(sb)
    }

    /// Frames per sheet. base.js holds back the newest two sheets' worth, which the server
    /// has not published yet.
    pub fn frames_per_sheet(&self) -> i64 {
        i64::from(self.cols * self.rows)
    }

    /// The thumbnail of the live segment with sequence number `seq`.
    pub fn thumbnail_for_sequence(&self, seq: i64) -> Option<StoryboardThumb> {
        if seq < 0 {
            return None;
        }
        let per_sheet = self.frames_per_sheet();
        let (sheet, within) = (seq / per_sheet, (seq % per_sheet) as u32);
        Some(StoryboardThumb {
            url: self.template.replace("$M", &sheet.to_string()),
            x: (within % self.cols) * self.thumb_w,
            y: (within / self.cols) * self.thumb_h,
            w: self.thumb_w,
            h: self.thumb_h,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The spec grammar of `playerStoryboardSpecRenderer`; the `sigh` keeps its `rs$` prefix.
    const SPEC: &str = "https://i.ytimg.com/sb/X/storyboard3_L$L/$N.jpg?sqp=Q\
        |48#27#100#10#10#0#default#rs$S0\
        |80#45#108#10#10#2000#M$M#rs$S1\
        |160#90#108#5#5#2000#M$M#rs$S2\
        |320#180#108#3#3#2000#M$M#rs$S3";

    fn sheet_of(url: &str) -> u32 {
        let (_, tail) = url.rsplit_once("/M").expect("sheet name");
        tail.split_once('.').expect("extension").0.parse().expect("sheet index")
    }

    #[test]
    fn every_playback_time_maps_to_a_cell_inside_a_sheet_that_never_goes_backwards() {
        let sb = Storyboard::parse(SPEC, Some(2)).expect("parse");
        let (cols, rows, w, h) = (5, 5, 160, 90);
        let mut last_sheet = 0;
        for t in (-1_000..=300_000).step_by(250) {
            let th = sb.thumbnail_at(t).expect("a thumbnail for every time");
            assert_eq!((th.w, th.h), (w, h));
            assert!(th.x.is_multiple_of(w) && th.y.is_multiple_of(h) && th.x < cols * w && th.y < rows * h, "{th:?}");
            assert!(th.url.ends_with("&sigh=rs$S2"), "{}", th.url);
            let sheet = sheet_of(&th.url);
            assert!(sheet >= last_sheet, "sheet {sheet} after {last_sheet} at {t} ms");
            last_sheet = sheet;
        }
    }

    #[test]
    fn every_live_sequence_gets_its_own_cell_inside_the_sheet() {
        let spec = "https://i.ytimg.com/sb/X/storyboard_live_90_3x3_b1/M$M.jpg?rs=R#159#90#3#3";
        let sb = LiveStoryboard::parse(spec).expect("parse");
        let mut seen = std::collections::HashSet::new();
        for seq in 0..200 {
            let th = sb.thumbnail_for_sequence(seq).expect("a thumbnail for every sequence");
            assert!(th.x < 3 * 159 && th.y < 3 * 90, "{th:?}");
            assert!(seen.insert((th.url, th.x, th.y)), "sequence {seq} reuses a cell");
        }
    }
}
