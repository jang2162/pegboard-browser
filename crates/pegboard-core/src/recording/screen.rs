//! The screen of a session's tabs: which frames may be written (`may_record`, `ScreenGate`),
//! how many (`Throttle`, `ScreenBudget`), and the files they go to (`ScreenWriter`:
//! `screen/<targetId>/000001.jpg`, ... and `index.jsonl`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::actionlog::url_for_log;
use crate::recording::config::RecordingConfig;
use crate::recording::format::{
    create_private_dir, write_private_file, FrameKind, IndexLine, JsonlWriter,
};

pub const MIN_FRAME_GAP: Duration = Duration::from_millis(500); // at most 2 frames a second

/// How long stream frames stay out after a back/forward-cache restore that left a
/// screen-off page: no lifecycle events come for it (spike F5).
pub const RESTORE_HOLD: Duration = Duration::from_millis(250);

/// Leading-edge throttle that keeps the latest dropped item and hands it out once the gap
/// has passed without a newer one, so the settled screen is always the last frame.
///
/// An item offered at least `gap` after the last one handed out goes out at once. Any other
/// is held, replacing the one held before; the held item goes out (`tick`) once `gap` has
/// passed since it was offered with no newer offer, i.e. when the screen has been still.
pub struct Throttle<T> {
    gap: Duration,
    last_out: Option<Instant>,
    /// The item held back, and when it was offered.
    pending: Option<(Instant, T)>,
}

impl<T> Throttle<T> {
    pub fn new(gap: Duration) -> Self {
        Throttle {
            gap,
            last_out: None,
            pending: None,
        }
    }

    /// Some: write it now.
    pub fn offer(&mut self, now: Instant, item: T) -> Option<T> {
        let free = self
            .last_out
            .is_none_or(|last| now.saturating_duration_since(last) >= self.gap);
        if free {
            self.pending = None;
            self.last_out = Some(now);
            Some(item)
        } else {
            self.pending = Some((now, item));
            None
        }
    }

    /// When `tick` would release the pending item.
    pub fn due(&self) -> Option<Instant> {
        self.pending.as_ref().map(|(at, _)| *at + self.gap)
    }

    pub fn tick(&mut self, now: Instant) -> Option<T> {
        if self.due().is_some_and(|due| now >= due) {
            self.last_out = Some(now);
            return self.pending.take().map(|(_, item)| item);
        }
        None
    }

    /// The pending item, at the end of the recording.
    pub fn flush(&mut self) -> Option<T> {
        self.pending.take().map(|(_, item)| item)
    }

    /// Forgets the pending item (the screen it shows may not be written).
    pub fn discard(&mut self) {
        self.pending = None;
    }
}

/// Width and height of a JPEG, from its SOF0/SOF2 marker (SOF1 too: the same layout).
pub fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let b = bytes;
    if b.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut i = 2;
    loop {
        if *b.get(i)? != 0xFF {
            return None;
        }
        // Fill bytes: any number of 0xFF before the marker.
        while *b.get(i + 1)? == 0xFF {
            i += 1;
        }
        let marker = *b.get(i + 1)?;
        i += 2;
        match marker {
            // Markers without a length.
            0x01 | 0xD0..=0xD7 => continue,
            // The image data or its end came before a frame marker.
            0xD8..=0xDA => return None,
            _ => {}
        }
        let len = usize::from(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]));
        if len < 2 {
            return None;
        }
        let segment = b.get(i..i + len)?;
        if matches!(marker, 0xC0..=0xC2) {
            // length(2) precision(1) height(2) width(2)
            let h = u16::from_be_bytes([*segment.get(3)?, *segment.get(4)?]);
            let w = u16::from_be_bytes([*segment.get(5)?, *segment.get(6)?]);
            if w == 0 || h == 0 {
                return None;
            }
            return Some((u32::from(w), u32::from(h)));
        }
        i += len;
    }
}

/// The daemon records a target only in the session's own context, never in the human's.
pub fn may_record(target_context: &str, session_context: &str, human_context: &str) -> bool {
    !target_context.is_empty()
        && target_context == session_context
        && target_context != human_context
}

/// Why stream frames stay out after leaving a screen-off page (spike F5): the old page's
/// frames still come after the new main-frame `frameNavigated`, until the new document's
/// first contentful paint; a back/forward-cache restore has no lifecycle events.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Hold {
    /// Until `firstContentfulPaint` of this loader. Never comes: the tab stays unrecorded.
    Paint(String),
    Until(Instant),
}

/// Whether frames of a target may be written now: the main frame's URL is known and not
/// screen-off, and no navigation to a screen-off URL is under way (rule fixed by F5).
///
/// `open` is the URL rule, which a direct capture follows (one sent after the current
/// navigation committed, see `capture_ticket`); stream frames follow `stream_open`, which
/// also holds them out after leaving a screen-off page until the new document painted.
pub struct ScreenGate {
    current: Option<String>,
    pending: Option<String>,
    hold: Option<Hold>,
    /// Main-frame documents committed so far.
    navigations: u64,
}

impl ScreenGate {
    pub fn new(url: Option<&str>) -> Self {
        ScreenGate {
            current: url.map(String::from),
            pending: None,
            hold: None,
            navigations: 0,
        }
    }

    /// The main frame started to navigate to `url` (it may still be redirected).
    pub fn navigation_started(&mut self, url: &str) {
        self.pending = Some(url.to_string());
    }

    /// The main frame committed a document at `url`.
    pub fn committed(&mut self, url: &str) {
        self.current = Some(url.to_string());
        self.pending = None;
        self.navigations += 1;
    }

    pub fn open(&self, cfg: &RecordingConfig) -> bool {
        self.current.as_deref().is_some_and(|u| !cfg.screen_off(u))
            && !self.pending.as_deref().is_some_and(|u| cfg.screen_off(u))
    }

    /// The main frame's `Page.frameNavigated`: `committed`, and when the screen was closed
    /// (a screen-off or unknown page, or still held), stream frames stay out until the new
    /// document (`loader_id`) paints, or for `RESTORE_HOLD` after a back/forward-cache
    /// restore (`restored`).
    pub fn navigated(
        &mut self,
        cfg: &RecordingConfig,
        url: &str,
        loader_id: &str,
        restored: bool,
        now: Instant,
    ) {
        let was_closed =
            !self.current.as_deref().is_some_and(|u| !cfg.screen_off(u)) || self.held(now);
        self.committed(url);
        self.hold = if cfg.screen_off(url) || !was_closed {
            None
        } else if restored {
            Some(Hold::Until(now + RESTORE_HOLD))
        } else {
            Some(Hold::Paint(loader_id.to_string()))
        };
    }

    /// A `Page.lifecycleEvent` of the main frame.
    pub fn lifecycle(&mut self, loader_id: &str, name: &str) {
        if name == "firstContentfulPaint" && self.hold == Some(Hold::Paint(loader_id.to_string())) {
            self.hold = None;
        }
    }

    fn held(&self, now: Instant) -> bool {
        match &self.hold {
            None => false,
            Some(Hold::Paint(_)) => true,
            Some(Hold::Until(t)) => now < *t,
        }
    }

    /// Whether a stream frame received now may be kept.
    pub fn stream_open(&self, cfg: &RecordingConfig, now: Instant) -> bool {
        self.open(cfg) && !self.held(now)
    }

    /// Before a direct capture is sent: `None` when it may not be taken now; otherwise the
    /// ticket its result is checked with (`capture_valid`).
    pub fn capture_ticket(&self, cfg: &RecordingConfig) -> Option<u64> {
        self.open(cfg).then_some(self.navigations)
    }

    /// When a capture's result came: whether it may be written. Only a capture sent after
    /// the current document committed counts (spike F5: none of those showed the old page).
    pub fn capture_valid(&self, cfg: &RecordingConfig, ticket: u64) -> bool {
        self.open(cfg) && self.navigations == ticket
    }

    /// The main frame's URL as known now (the one under way first), for an `Off` marker.
    pub fn url(&self) -> Option<&str> {
        self.pending.as_deref().or(self.current.as_deref())
    }
}

/// The screen bytes one recording may still write (max_per_recording_bytes).
pub struct ScreenBudget {
    limit: u64,
    used: AtomicU64,
    capped: AtomicBool,
}

impl ScreenBudget {
    pub fn new(limit: u64) -> Arc<Self> {
        Arc::new(ScreenBudget {
            limit,
            used: AtomicU64::new(0),
            capped: AtomicBool::new(false),
        })
    }

    /// False (and capped from then on) once used + n > limit.
    pub fn try_take(&self, n: u64) -> bool {
        let mut used = self.used.load(Ordering::SeqCst);
        loop {
            if self.capped() {
                return false;
            }
            match used.checked_add(n).filter(|total| *total <= self.limit) {
                None => {
                    self.capped.store(true, Ordering::SeqCst);
                    return false;
                }
                Some(total) => match self.used.compare_exchange_weak(
                    used,
                    total,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => return true,
                    Err(now) => used = now,
                },
            }
        }
    }

    /// Gives back bytes taken for a frame that could not be written.
    fn give_back(&self, n: u64) {
        let mut used = self.used.load(Ordering::SeqCst);
        while let Err(now) = self.used.compare_exchange_weak(
            used,
            used.saturating_sub(n),
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            used = now;
        }
    }

    pub fn capped(&self) -> bool {
        self.capped.load(Ordering::SeqCst)
    }

    pub fn used(&self) -> u64 {
        self.used.load(Ordering::SeqCst)
    }
}

/// A target id as the browser makes them (32 upper-case hex digits): the name of the
/// target's directory under `screen/`.
pub fn valid_target_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'F'))
}

/// `<targetId>/<file>` as `ScreenWriter::write_frame` names a frame: one separator, a
/// valid target id, a numbered `.jpg` file. Nothing else may be read or written as a frame.
pub fn valid_frame_name(name: &str) -> bool {
    let Some((target, file)) = name.split_once('/') else {
        return false;
    };
    valid_target_id(target) && valid_frame_file(file)
}

/// `000001.jpg`: six digits or more (at most ten) and `.jpg`.
fn valid_frame_file(file: &str) -> bool {
    file.strip_suffix(".jpg")
        .is_some_and(|n| (6..=10).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit()))
}

fn invalid(message: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message)
}

/// screen/<targetId>/: numbered JPEGs (000001.jpg, ...) and index.jsonl.
///
/// Created once per target and recording (it refuses an existing index): whoever writes a
/// recording's screen keeps it for the whole recording, also across reconnects.
pub struct ScreenWriter {
    dir: PathBuf,
    target: String,
    next: u32,
    index: JsonlWriter,
    budget: Arc<ScreenBudget>,
    capped_marked: bool,
}

impl ScreenWriter {
    pub fn create(
        screen_dir: &Path,
        target_id: &str,
        budget: Arc<ScreenBudget>,
    ) -> std::io::Result<ScreenWriter> {
        if !valid_target_id(target_id) {
            return Err(invalid(format!("{target_id:?} is not a target id")));
        }
        let dir = screen_dir.join(target_id);
        create_private_dir(screen_dir)?;
        create_private_dir(&dir)?;
        let index = JsonlWriter::create(&dir.join("index.jsonl"))?;
        Ok(ScreenWriter {
            dir,
            target: target_id.to_string(),
            next: 1,
            index,
            budget,
            capped_marked: false,
        })
    }

    /// Writes the frame and its index line; returns "<targetId>/<file>", or None when the
    /// budget is spent (a Capped marker is written once). On an error nothing of the frame
    /// is left (its number is not used again).
    pub fn write_frame(
        &mut self,
        ts: &str,
        url: Option<&str>,
        jpeg: &[u8],
        kind: FrameKind,
    ) -> std::io::Result<Option<String>> {
        let n = jpeg.len() as u64;
        if !self.budget.try_take(n) {
            self.mark_capped(ts)?;
            return Ok(None);
        }
        let file = format!("{:06}.jpg", self.next);
        self.next = self.next.saturating_add(1);
        let name = format!("{}/{file}", self.target);
        if !valid_frame_name(&name) {
            self.budget.give_back(n);
            return Err(invalid(format!("{name:?} is not a frame name")));
        }
        let path = self.dir.join(&file);
        if let Err(e) = write_private_file(&path, jpeg) {
            // A file it may have created is not a frame.
            let _ = std::fs::remove_file(&path);
            self.budget.give_back(n);
            return Err(e);
        }
        let (width, height) = jpeg_size(jpeg).unzip();
        let line = IndexLine {
            ts: ts.to_string(),
            file: Some(file),
            url: url.map(url_for_log),
            width,
            height,
            kind,
        };
        if let Err(e) = self.index.append(&line) {
            let _ = std::fs::remove_file(&path);
            self.budget.give_back(n);
            return Err(e);
        }
        Ok(Some(name))
    }

    /// Off or Capped.
    pub fn mark(&mut self, ts: &str, kind: FrameKind, url: Option<&str>) -> std::io::Result<()> {
        if kind == FrameKind::Capped {
            return self.mark_capped(ts);
        }
        self.index.append(&IndexLine {
            ts: ts.to_string(),
            file: None,
            url: url.map(url_for_log),
            width: None,
            height: None,
            kind,
        })?;
        Ok(())
    }

    fn mark_capped(&mut self, ts: &str) -> std::io::Result<()> {
        if self.capped_marked {
            return Ok(());
        }
        self.index.append(&IndexLine {
            ts: ts.to_string(),
            file: None,
            url: None,
            width: None,
            height: None,
            kind: FrameKind::Capped,
        })?;
        self.capped_marked = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::format::read_jsonl;
    use std::os::unix::fs::PermissionsExt;

    const T1: &str = "0123456789ABCDEF0123456789ABCDEF";

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn throttle_lets_two_a_second_through_and_keeps_the_last() {
        let t0 = Instant::now();
        let mut th = Throttle::new(MIN_FRAME_GAP);
        let mut out = Vec::new();
        for i in 0..20u64 {
            let now = t0 + ms(i * 100);
            if let Some(x) = th.offer(now, i * 100) {
                out.push((i * 100, x));
            }
            // Nothing is due while the screen keeps changing.
            assert!(th.tick(now).is_none());
        }
        assert_eq!(
            out,
            vec![(0, 0), (500, 500), (1000, 1000), (1500, 1500)],
            "released on offer"
        );
        assert_eq!(th.due(), Some(t0 + ms(2400)));
        assert!(th.tick(t0 + ms(2399)).is_none());
        assert_eq!(th.tick(t0 + ms(2400)), Some(1900));
        // Nothing is released twice.
        assert!(th.tick(t0 + ms(5000)).is_none());
        assert!(th.flush().is_none());
        assert_eq!(th.due(), None);

        // flush() after the last offer gives the newest pending item.
        let mut th = Throttle::new(MIN_FRAME_GAP);
        assert_eq!(th.offer(t0, 1), Some(1));
        assert_eq!(th.offer(t0 + ms(10), 2), None);
        assert_eq!(th.offer(t0 + ms(20), 3), None);
        assert_eq!(th.flush(), Some(3));
        assert!(th.flush().is_none());
        assert!(th.tick(t0 + ms(10_000)).is_none());
        // discard() forgets it.
        assert_eq!(th.offer(t0 + ms(30), 4), None);
        th.discard();
        assert_eq!(th.due(), None);
        assert!(th.flush().is_none());
    }

    #[test]
    fn throttle_releases_the_settled_frame_after_the_gap() {
        let t0 = Instant::now();
        let mut th = Throttle::new(MIN_FRAME_GAP);
        assert_eq!(th.offer(t0, "a"), Some("a"));
        assert_eq!(th.offer(t0 + ms(100), "b"), None);
        assert!(th.tick(t0 + ms(599)).is_none());
        assert_eq!(th.tick(t0 + ms(600)), Some("b"));
        // The gap counts from what went out last.
        assert_eq!(th.offer(t0 + ms(700), "c"), None);
        assert_eq!(th.offer(t0 + ms(1100), "d"), Some("d"));
    }

    /// A JPEG header up to its frame marker (`sof`): enough for `jpeg_size`.
    fn jpeg(sof: u8, w: u16, h: u16) -> Vec<u8> {
        let mut b = vec![0xFF, 0xD8];
        // APP0 (JFIF), 16 bytes.
        b.extend([0xFF, 0xE0, 0x00, 0x10]);
        b.extend(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        // A quantization table segment, length 5.
        b.extend([0xFF, 0xDB, 0x00, 0x05, 0x00, 0x01, 0x02]);
        // Fill bytes before a marker are allowed.
        b.extend([0xFF, 0xFF]);
        b.extend([0xFF, sof, 0x00, 0x11, 0x08]);
        b.extend(h.to_be_bytes());
        b.extend(w.to_be_bytes());
        b.extend([0x03, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        b.extend([0xFF, 0xDA, 0x00, 0x08, 1, 1, 0, 0, 0x3F, 0]);
        b.extend([0x12, 0x34, 0xFF, 0xD9]);
        b
    }

    #[test]
    fn jpeg_size_reads_baseline_and_progressive() {
        assert_eq!(jpeg_size(&jpeg(0xC0, 1280, 717)), Some((1280, 717)));
        assert_eq!(jpeg_size(&jpeg(0xC2, 800, 600)), Some((800, 600)));
        let full = jpeg(0xC0, 1280, 717);
        for garbage in [
            &b""[..],
            b"\xff\xd8",
            b"GIF89a",
            &full[..20],
            b"\xff\xd8\xff\xda\x00\x08",
            b"\xff\xd8\xff\xe0\x00\x01",
        ] {
            assert_eq!(jpeg_size(garbage), None, "{garbage:?}");
        }
        // A frame marker without a size is not a size.
        assert_eq!(jpeg_size(&jpeg(0xC0, 0, 717)), None);
    }

    #[test]
    fn may_record_only_in_the_session_context() {
        assert!(may_record("ctxA", "ctxA", "human"));
        assert!(!may_record("human", "human", "human"));
        assert!(!may_record("ctxB", "ctxA", "human"));
        assert!(!may_record("", "ctxA", "human"));
        assert!(!may_record("", "", "human"));
    }

    fn off_list() -> RecordingConfig {
        RecordingConfig {
            screen_off_domains: vec!["screen-off.test".into()],
            ..RecordingConfig::default()
        }
    }

    #[test]
    fn screen_gate_closes_before_an_off_page_can_paint() {
        let cfg = off_list();
        let mut g = ScreenGate::new(Some("http://a.test/"));
        assert!(g.open(&cfg));
        g.navigation_started("http://screen-off.test/");
        assert!(!g.open(&cfg));
        g.committed("http://screen-off.test/");
        assert!(!g.open(&cfg));
        g.navigation_started("http://a.test/");
        assert!(!g.open(&cfg));
        g.committed("http://a.test/");
        assert!(g.open(&cfg));
        // Unknown URL.
        assert!(!ScreenGate::new(None).open(&cfg));
        // A redirect: started on a.test, committed on the off domain.
        let mut g = ScreenGate::new(Some("http://a.test/"));
        g.navigation_started("http://a.test/to-off");
        assert!(g.open(&cfg));
        g.committed("http://screen-off.test/off");
        assert!(!g.open(&cfg));
        // An empty list: every known URL is open.
        let mut g = ScreenGate::new(Some("http://screen-off.test/"));
        assert!(g.open(&RecordingConfig::default()));
        g.navigation_started("http://screen-off.test/x");
        assert!(g.open(&RecordingConfig::default()));
    }

    #[test]
    fn leaving_an_off_page_waits_for_the_new_documents_paint() {
        let cfg = off_list();
        let t0 = Instant::now();
        let mut g = ScreenGate::new(Some("http://screen-off.test/"));
        assert!(!g.stream_open(&cfg, t0));
        g.navigated(&cfg, "http://a.test/", "L1", false, t0);
        // The URL rule is open (captures sent from now are fine) ...
        assert!(g.open(&cfg));
        // ... stream frames are not until the new document painted.
        assert!(!g.stream_open(&cfg, t0 + ms(5000)));
        g.lifecycle("L0", "firstContentfulPaint");
        g.lifecycle("L1", "load");
        assert!(!g.stream_open(&cfg, t0 + ms(5000)));
        g.lifecycle("L1", "firstContentfulPaint");
        assert!(g.stream_open(&cfg, t0 + ms(5000)));
        // Between pages that are not off, nothing is held.
        g.navigated(&cfg, "http://b.test/", "L2", false, t0);
        assert!(g.stream_open(&cfg, t0));

        // A back/forward-cache restore has no lifecycle events: 250 ms.
        g.navigated(&cfg, "http://screen-off.test/", "L3", false, t0);
        assert!(!g.stream_open(&cfg, t0));
        g.navigated(&cfg, "http://a.test/", "L4", true, t0);
        assert!(!g.stream_open(&cfg, t0 + ms(249)));
        assert!(g.stream_open(&cfg, t0 + RESTORE_HOLD));

        // No paint ever: the tab stays unrecorded, also after the next navigation.
        g.navigated(&cfg, "http://screen-off.test/", "L5", false, t0);
        g.navigated(&cfg, "http://a.test/", "L6", false, t0);
        g.navigated(&cfg, "http://b.test/", "L7", false, t0);
        assert!(!g.stream_open(&cfg, t0 + ms(60_000)));
        g.lifecycle("L6", "firstContentfulPaint");
        assert!(!g.stream_open(&cfg, t0 + ms(60_000)));
        g.lifecycle("L7", "firstContentfulPaint");
        assert!(g.stream_open(&cfg, t0 + ms(60_000)));

        // A screen-off navigation under way closes the stream too.
        g.navigation_started("http://screen-off.test/");
        assert!(!g.stream_open(&cfg, t0 + ms(60_000)));
    }

    #[test]
    fn a_capture_counts_only_when_sent_after_the_current_navigation() {
        let cfg = off_list();
        let t0 = Instant::now();
        let mut g = ScreenGate::new(Some("http://a.test/"));
        let ticket = g.capture_ticket(&cfg).unwrap();
        assert!(g.capture_valid(&cfg, ticket));
        g.navigated(&cfg, "http://b.test/", "L1", false, t0);
        // Sent before the navigation: dropped, whatever it shows.
        assert!(!g.capture_valid(&cfg, ticket));
        let ticket = g.capture_ticket(&cfg).unwrap();
        g.navigation_started("http://screen-off.test/");
        assert!(g.capture_ticket(&cfg).is_none());
        assert!(!g.capture_valid(&cfg, ticket));
        g.navigated(&cfg, "http://screen-off.test/", "L2", false, t0);
        assert!(g.capture_ticket(&cfg).is_none());
        // Leaving the off page: a capture sent after frameNavigated is fine at once.
        g.navigated(&cfg, "http://a.test/", "L3", false, t0);
        let ticket = g.capture_ticket(&cfg).unwrap();
        assert!(g.capture_valid(&cfg, ticket));
    }

    #[test]
    fn budget_caps_once() {
        let b = ScreenBudget::new(10);
        assert!(b.try_take(6));
        assert!(!b.capped());
        assert!(!b.try_take(5));
        assert!(b.capped());
        assert!(!b.try_take(1));
        assert_eq!(b.used(), 6);
        // Exactly the limit is still within it.
        let b = ScreenBudget::new(10);
        assert!(b.try_take(10));
        assert!(!b.capped());
        assert!(!b.try_take(1));
    }

    #[test]
    fn frame_names_are_a_target_and_a_numbered_jpeg() {
        assert!(valid_target_id(T1));
        for bad in [
            "",
            "0123456789abcdef0123456789abcdef",
            "0123456789ABCDEF0123456789ABCDE",
            "0123456789ABCDEF0123456789ABCDEF0",
            "..",
            "0123456789ABCDEF/123456789ABCDEF",
        ] {
            assert!(!valid_target_id(bad), "{bad}");
        }
        assert!(valid_frame_name(&format!("{T1}/000001.jpg")));
        assert!(valid_frame_name(&format!("{T1}/1234567.jpg")));
        for bad in [
            format!("{T1}/../000001.jpg"),
            format!("{T1}/000001.jpg/x"),
            format!("../{T1}/000001.jpg"),
            format!("{T1}/index.jsonl"),
            format!("{T1}/00001.jpg"),
            format!("{T1}/000001.png"),
            format!("{T1}//000001.jpg"),
            "000001.jpg".to_string(),
            format!("/{T1}/000001.jpg"),
            format!("{}/000001.jpg", T1.to_lowercase()),
        ] {
            assert!(!valid_frame_name(&bad), "{bad}");
        }
    }

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn writer_numbers_files_and_indexes_them_with_0600() {
        let tmp = tempfile::tempdir().unwrap();
        let screen = tmp.path().join("screen");
        let mut w = ScreenWriter::create(&screen, T1, ScreenBudget::new(1_000_000)).unwrap();
        // A target id that could leave the directory is refused.
        assert!(ScreenWriter::create(&screen, "../x", ScreenBudget::new(10)).is_err());
        assert!(!tmp.path().join("x").exists());
        let a = jpeg(0xC0, 1280, 717);
        let b = jpeg(0xC2, 640, 480);
        let n1 = w
            .write_frame(
                "t1",
                Some("http://a.test/x?secret=1"),
                &a,
                FrameKind::Change,
            )
            .unwrap();
        assert_eq!(n1, Some(format!("{T1}/000001.jpg")));
        w.mark("t2", FrameKind::Off, Some("http://screen-off.test/"))
            .unwrap();
        let n2 = w.write_frame("t3", None, &b, FrameKind::After).unwrap();
        assert_eq!(n2, Some(format!("{T1}/000002.jpg")));
        let dir = screen.join(T1);
        assert_eq!(mode(&screen), 0o700);
        assert_eq!(mode(&dir), 0o700);
        for f in ["000001.jpg", "000002.jpg", "index.jsonl"] {
            assert_eq!(mode(&dir.join(f)), 0o600, "{f}");
        }
        assert_eq!(std::fs::read(dir.join("000001.jpg")).unwrap(), a);
        let lines: Vec<IndexLine> = read_jsonl(&dir.join("index.jsonl")).unwrap();
        assert_eq!(
            lines,
            vec![
                IndexLine {
                    ts: "t1".into(),
                    file: Some("000001.jpg".into()),
                    url: Some("http://a.test/x".into()),
                    width: Some(1280),
                    height: Some(717),
                    kind: FrameKind::Change,
                },
                IndexLine {
                    ts: "t2".into(),
                    file: None,
                    url: Some("http://screen-off.test/".into()),
                    width: None,
                    height: None,
                    kind: FrameKind::Off,
                },
                IndexLine {
                    ts: "t3".into(),
                    file: Some("000002.jpg".into()),
                    url: None,
                    width: Some(640),
                    height: Some(480),
                    kind: FrameKind::After,
                },
            ]
        );
    }

    #[test]
    fn writer_stops_at_the_budget_with_one_capped_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let screen = tmp.path().join("screen");
        let frame = jpeg(0xC0, 10, 10);
        let n = frame.len() as u64;
        let budget = ScreenBudget::new(2 * n + n / 2);
        let mut w = ScreenWriter::create(&screen, T1, budget.clone()).unwrap();
        let mut names = Vec::new();
        for i in 0..5 {
            names.push(
                w.write_frame(&format!("t{i}"), None, &frame, FrameKind::Change)
                    .unwrap(),
            );
        }
        assert_eq!(names.iter().filter(|n| n.is_some()).count(), 2);
        assert!(names[2..].iter().all(Option::is_none));
        assert!(budget.capped());
        assert_eq!(budget.used(), 2 * n);
        // Another target of the same recording shares the budget.
        let other = "FEDCBA9876543210FEDCBA9876543210";
        let mut w2 = ScreenWriter::create(&screen, other, budget.clone()).unwrap();
        assert_eq!(
            w2.write_frame("u", None, &frame, FrameKind::After).unwrap(),
            None
        );
        let lines: Vec<IndexLine> = read_jsonl(&screen.join(T1).join("index.jsonl")).unwrap();
        let kinds: Vec<FrameKind> = lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![FrameKind::Change, FrameKind::Change, FrameKind::Capped]
        );
        let jpgs = std::fs::read_dir(screen.join(T1))
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".jpg")
            })
            .count();
        assert_eq!(jpgs, 2);
        let lines: Vec<IndexLine> = read_jsonl(&screen.join(other).join("index.jsonl")).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].kind, FrameKind::Capped);
    }

    #[test]
    fn write_errors_drop_frames_only() {
        // Root can write into a read-only directory.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let screen = tmp.path().join("screen");
        let budget = ScreenBudget::new(1_000_000);
        let mut w = ScreenWriter::create(&screen, T1, budget.clone()).unwrap();
        let frame = jpeg(0xC0, 10, 10);
        let dir = screen.join(T1);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert!(w
            .write_frame("t1", None, &frame, FrameKind::Change)
            .is_err());
        // Its bytes were given back.
        assert_eq!(budget.used(), 0);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let name = w
            .write_frame("t2", None, &frame, FrameKind::Change)
            .unwrap()
            .unwrap();
        let file = name.rsplit('/').next().unwrap().to_string();
        assert!(dir.join(&file).exists());
        let lines: Vec<IndexLine> = read_jsonl(&dir.join("index.jsonl")).unwrap();
        // Every index line names a file that exists, and every file has its line.
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].file.as_deref(), Some(file.as_str()));
        let jpgs: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".jpg"))
            .collect();
        assert_eq!(jpgs, vec![file]);
    }
}
