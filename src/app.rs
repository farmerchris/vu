//! Application state, cursor/scroll model and input handling.

use crate::analyzer::Analyzer;
use crate::cli::{Cli, Panel};
use crate::dsp::scale::FreqScale;
use crate::history::History;
use crate::recorder::{record_path, RecordFormat, Recorder};
use crate::source::{Control, SourceInfo, SourceKind};
use crate::ui::colormap::{ColorMap, ColorMode};
use crate::ui::SpecCache;
use anyhow::Result;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

/// Scroll/cursor state over the history. The charts show `span_secs` seconds
/// whatever their width, so each column covers a fractional number of frames.
/// Columns are buckets anchored at frame 0; the rightmost holds `view_end`.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    /// Follow live data (cursor pinned to the newest frame).
    pub follow: bool,
    pub cursor: usize,
    pub view_end: usize,
    /// Seconds shown across the chart width (the zoom level).
    pub span_secs: f64,
    /// Seconds per analysis frame.
    pub dt: f64,
    /// Chart width in columns.
    pub width: usize,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Nav {
    Left(usize),
    Right(usize),
    PageLeft,
    PageRight,
    Home,
    End,
    ToggleHold,
    ZoomIn,
    ZoomOut,
    Column(usize),
}

/// Zoom levels (seconds across the chart) stepped through by + and -.
pub const ZOOM_SPANS: [f64; 11] = [0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 30.0, 60.0, 120.0, 300.0, 600.0];
pub const DEFAULT_SPAN: f64 = 10.0;

/// Tolerance for float bucket arithmetic.
const EPS: f64 = 1e-9;

impl Default for View {
    fn default() -> Self {
        View { follow: true, cursor: 0, view_end: 0, span_secs: DEFAULT_SPAN, dt: 0.01, width: 1 }
    }
}

impl View {
    /// Frames per column (below 1 when zoomed in past one frame per column).
    pub fn frames_per_col(&self) -> f64 {
        self.span_secs / (self.width.max(1) as f64 * self.dt)
    }

    /// Frames spanned by the visible chart.
    pub fn span(&self) -> usize {
        ((self.span_secs / self.dt - EPS).ceil() as usize).max(1)
    }

    /// Bucket (absolute column index) holding frame `f`.
    fn bucket(&self, f: usize) -> usize {
        (f as f64 / self.frames_per_col() + EPS).floor() as usize
    }

    /// Frames `[lo, hi]` of bucket `b`. A bucket narrower than a frame holds no
    /// frame start of its own and shows the frame it falls within.
    pub fn bucket_frames(&self, b: usize) -> (usize, usize) {
        let fpc = self.frames_per_col();
        let lo = (b as f64 * fpc - EPS).ceil() as usize;
        let end = ((b + 1) as f64 * fpc - EPS).ceil() as usize;
        if end > lo {
            (lo, end - 1)
        } else {
            let f = (b as f64 * fpc + EPS).floor() as usize;
            (f, f)
        }
    }

    /// Last frame of the bucket containing frame `f`. Anchoring buckets at frame
    /// 0 keeps columns stable as live data scrolls in.
    pub fn bucket_end(&self, f: usize) -> usize {
        self.bucket_frames(self.bucket(f)).1
    }

    /// Frames moved by `n` columns of cursor movement (at least one).
    fn step(&self, n: usize) -> usize {
        ((n as f64 * self.frames_per_col()).round() as usize).max(1)
    }

    /// Re-establish invariants after data arrives or the width changes.
    pub fn sync(&mut self, first: usize, last: Option<usize>) {
        let Some(last) = last else {
            self.cursor = 0;
            self.view_end = 0;
            return;
        };
        if self.follow {
            self.cursor = last;
            self.view_end = self.bucket_end(last);
            return;
        }
        self.cursor = self.cursor.clamp(first, last);
        let span = self.span();
        let max_end = self.bucket_end(last);
        // Never scroll back past the oldest frame while there is data to fill the view.
        self.view_end = self.view_end.clamp((first + span - 1).min(max_end), max_end);
        if self.cursor > self.view_end {
            self.view_end = self.cursor;
        } else if self.cursor + span <= self.view_end {
            self.view_end = self.cursor + span - 1;
        }
    }

    pub fn apply(&mut self, nav: Nav, first: usize, last: Option<usize>) {
        let Some(last) = last else { return };
        match nav {
            Nav::Left(n) => {
                self.unfollow(last);
                self.cursor = self.cursor.saturating_sub(self.step(n)).max(first);
            }
            Nav::Right(n) => {
                if !self.follow {
                    let c = self.cursor + self.step(n);
                    if c >= last {
                        self.follow = true;
                    } else {
                        self.cursor = c;
                    }
                }
            }
            Nav::PageLeft => {
                self.unfollow(last);
                let s = self.span();
                self.cursor = self.cursor.saturating_sub(s).max(first);
                self.view_end = self.view_end.saturating_sub(s).max(self.cursor);
            }
            Nav::PageRight => {
                if !self.follow {
                    let s = self.span();
                    if self.cursor + s >= last {
                        self.follow = true;
                    } else {
                        self.cursor += s;
                        self.view_end = (self.view_end + s).min(last);
                    }
                }
            }
            Nav::Home => {
                self.follow = false;
                self.cursor = first;
                self.view_end = (first + self.span() - 1).min(last);
            }
            Nav::End => self.follow = true,
            Nav::ToggleHold => {
                if self.follow {
                    self.unfollow(last);
                } else {
                    self.follow = true;
                }
            }
            Nav::ZoomIn => {
                if let Some(&s) = ZOOM_SPANS.iter().rev().find(|&&s| s < self.span_secs - EPS) {
                    self.span_secs = s;
                }
            }
            Nav::ZoomOut => {
                if let Some(&s) = ZOOM_SPANS.iter().find(|&&s| s > self.span_secs + EPS) {
                    self.span_secs = s;
                }
            }
            Nav::Column(col) => {
                if let Some(f) = self.column_frame(col.min(self.width.saturating_sub(1))) {
                    self.unfollow(last);
                    self.cursor = f.max(first);
                }
            }
        }
        self.sync(first, Some(last));
    }

    fn unfollow(&mut self, last: usize) {
        if self.follow {
            self.follow = false;
            self.cursor = last;
            self.view_end = self.bucket_end(last);
        }
    }

    /// Frames `[lo, hi]` shown in column `col`, or `None` if it lies before frame 0.
    pub fn column_frames(&self, col: usize) -> Option<(usize, usize)> {
        if col >= self.width {
            return None;
        }
        let b = self.bucket(self.view_end).checked_sub(self.width - 1 - col)?;
        Some(self.bucket_frames(b))
    }

    /// Newest frame in column `col`'s bucket, or `None` if it lies before frame 0.
    pub fn column_frame(&self, col: usize) -> Option<usize> {
        self.column_frames(col).map(|(_, hi)| hi)
    }

    /// Column containing the cursor, if visible.
    pub fn cursor_column(&self) -> Option<usize> {
        self.frame_column(self.cursor)
    }

    /// Column whose bucket holds frame `f`, if visible. When zoomed in past one
    /// frame per column this is the first of the columns showing `f`.
    pub fn frame_column(&self, f: usize) -> Option<usize> {
        if f > self.view_end {
            return None;
        }
        let back = self.bucket(self.view_end) - self.bucket(f);
        (back < self.width).then(|| self.width - 1 - back)
    }
}

/// Highest sample peak and number of clipping events since the start or the
/// last reset (`x`), and the highest momentary loudness since the start or its
/// own reset (`R`). A run of consecutive clipped frames counts as one event.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub max_db: f32,
    pub clips: u64,
    in_clip: bool,
    /// Highest momentary loudness (EBU "max M"), LUFS.
    pub max_lufs_m: f32,
}

impl Default for Session {
    fn default() -> Self {
        Session { max_db: crate::dsp::level::FLOOR_DB, clips: 0, in_clip: false, max_lufs_m: f32::NEG_INFINITY }
    }
}

impl Session {
    pub fn add(&mut self, f: &crate::analyzer::Frame) {
        for &p in &f.peak[..f.n()] {
            self.max_db = self.max_db.max(p);
        }
        if f.clipped && !self.in_clip {
            self.clips += 1;
        }
        self.in_clip = f.clipped;
        self.max_lufs_m = self.max_lufs_m.max(f.lufs_m);
    }

    /// Start counting peaks and clips again. A clip run in progress at the
    /// reset is not counted a second time. The loudness maximum is kept.
    pub fn reset(&mut self) {
        *self = Session { in_clip: self.in_clip, max_lufs_m: self.max_lufs_m, ..Session::default() };
    }

    /// Start tracking the maximum momentary loudness again.
    pub fn reset_loudness(&mut self) {
        self.max_lufs_m = f32::NEG_INFINITY;
    }
}

/// How long CLIP stays lit after a clip, seconds.
pub const CLIP_SECS: f64 = 2.0;

/// How often the numeric readouts refresh while following live input.
pub const READOUT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Frame whose values the numeric readouts show. While live it is refreshed
/// every `READOUT_INTERVAL` so the digits are readable; when the cursor is
/// placed by hand it tracks the cursor exactly.
#[derive(Default)]
pub struct Readout {
    shown: Option<(usize, Instant)>,
}

impl Readout {
    pub fn pick(&mut self, cursor: usize, follow: bool, now: Instant, exists: impl Fn(usize) -> bool) -> usize {
        if follow {
            if let Some((f, at)) = self.shown {
                if now.duration_since(at) < READOUT_INTERVAL && f <= cursor && exists(f) {
                    return f;
                }
            }
            self.shown = Some((cursor, now));
        } else {
            self.shown = None;
        }
        cursor
    }
}

/// Redraws in the last second and the most recent draw time.
#[derive(Default)]
pub struct FpsMeter {
    draws: std::collections::VecDeque<Instant>,
    pub fps: usize,
    pub draw_ms: f64,
}

impl FpsMeter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tick(&mut self, now: Instant, draw_time: std::time::Duration) {
        self.draws.push_back(now);
        while self.draws.front().is_some_and(|&t| now.duration_since(t) >= std::time::Duration::from_secs(1)) {
            self.draws.pop_front();
        }
        self.fps = self.draws.len();
        self.draw_ms = draw_time.as_secs_f64() * 1000.0;
    }
}

/// Static facts about the analysis, needed for drawing.
pub struct AnalysisInfo {
    pub sample_rate: u32,
    pub channels: usize,
    pub fft_size: usize,
    pub hop: usize,
    pub bin_hz: f32,
    pub bins: usize,
    pub band_centers: Vec<f32>,
    pub band_labels: Vec<String>,
    pub band_major: Vec<bool>,
    pub third_octave: bool,
    pub frame_dt: f64,
    pub min_freq: f32,
    pub max_freq: f32,
    pub weighting: String,
    pub window: String,
}

pub struct Display {
    pub panels: [bool; 4],
    pub db_min: f32,
    pub db_max: f32,
    pub meter_floor: f32,
    pub colormap: ColorMap,
    pub freq_scale: FreqScale,
    pub color_mode: ColorMode,
    /// Frames the CLIP indicator stays lit after a clip.
    pub clip_frames: usize,
    /// Level bars show VU needles instead of RMS with a peak envelope.
    pub vu: bool,
    /// dBFS that reads 0 VU.
    pub vu_ref: f32,
}

impl Display {
    pub fn shown(&self, p: Panel) -> bool {
        self.panels[p as usize]
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    Redraw,
}

pub struct App {
    pub history: History,
    pub view: View,
    pub info: SourceInfo,
    pub analysis: AnalysisInfo,
    pub display: Display,
    pub control: Arc<Control>,
    pub show_help: bool,
    pub fps: FpsMeter,
    pub source_done: bool,
    pub ingest_done: bool,
    pub message: Option<(String, Instant)>,
    /// Persistent problem with a live input (e.g. it went digitally silent).
    pub input_warning: Option<String>,
    pub readout: Readout,
    pub session: Session,
    /// Times the live input was reopened after going silent.
    pub reopens: u32,
    pub recorder: Option<Recorder>,
    pub saved_recordings: Vec<PathBuf>,
    record_base: Option<PathBuf>,
    record_format: RecordFormat,
    record_count: usize,
    /// Chart rectangles (spectrogram, timeline) for mouse hit-testing.
    pub chart_rects: Vec<Rect>,
    pub spec_cache: SpecCache,
}

impl App {
    pub fn new(cli: &Cli, info: SourceInfo, analyzer: &Analyzer, control: Arc<Control>, color_mode: ColorMode) -> Self {
        let cfg = analyzer.config();
        let frame_dt = cfg.frame_dt();
        let mut cap = (cli.history / frame_dt).ceil() as usize;
        if let (SourceKind::File, Some(total), false) = (info.kind, info.total_frames, cli.looping) {
            // Keep whole files for browsing, up to an hour (~1.3 KB per frame).
            let hour = (3600.0 / frame_dt) as usize;
            cap = cap.max((total as usize / cfg.hop + 2).min(hour));
        }
        if let Some(d) = cli.duration {
            cap = cap.max((d / frame_dt).ceil() as usize + 2);
        }
        let mut panels = [false; 4];
        for &p in &cli.panels {
            panels[p as usize] = true;
        }
        App {
            history: History::new(cap),
            view: View { span_secs: cli.span, dt: frame_dt, ..View::default() },
            analysis: AnalysisInfo {
                sample_rate: cfg.sample_rate,
                channels: cfg.channels,
                fft_size: cfg.fft_size,
                hop: cfg.hop,
                bin_hz: cfg.bin_hz(),
                bins: analyzer.bins(),
                band_centers: analyzer.bands.centers.clone(),
                band_labels: analyzer.bands.labels.clone(),
                band_major: analyzer.bands.major.clone(),
                third_octave: analyzer.bands.third_octave,
                frame_dt,
                min_freq: cfg.min_freq,
                max_freq: cfg.top_freq(),
                weighting: format!("{:?}", cfg.weighting),
                window: format!("{:?}", cfg.window).to_lowercase(),
            },
            display: Display {
                panels,
                db_min: cli.db_min,
                db_max: cli.db_max,
                meter_floor: cli.meter_floor,
                colormap: cli.colormap,
                freq_scale: cli.freq_scale,
                color_mode,
                clip_frames: ((CLIP_SECS / frame_dt).round() as usize).max(1),
                vu: cli.vu,
                vu_ref: cli.vu_ref,
            },
            info,
            control,
            show_help: false,
            fps: FpsMeter::new(),
            source_done: false,
            ingest_done: false,
            message: None,
            input_warning: None,
            readout: Readout::default(),
            session: Session::default(),
            reopens: 0,
            recorder: None,
            saved_recordings: Vec::new(),
            record_base: cli.record.clone(),
            record_format: cli.record_format,
            record_count: 0,
            chart_rects: Vec::new(),
            spec_cache: SpecCache::default(),
        }
    }

    pub fn sync_view(&mut self) {
        self.view.sync(self.history.first(), self.history.last());
    }

    pub fn nav(&mut self, n: Nav) {
        self.view.apply(n, self.history.first(), self.history.last());
    }

    pub fn paused(&self) -> bool {
        self.control.is_paused()
    }

    /// Store a new analysis frame and fold it into the session statistics.
    pub fn push_frame(&mut self, f: crate::analyzer::Frame) {
        self.session.add(&f);
        self.history.push(f);
    }

    pub fn flash(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), Instant::now()));
    }

    pub fn start_recording(&mut self) -> Result<()> {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = record_path(self.record_base.as_deref(), self.record_count, secs);
        self.record_count += 1;
        let r = Recorder::create(&path, self.info.channels, self.info.sample_rate, self.record_format)?;
        self.flash(format!("recording to {}", path.display()));
        self.recorder = Some(r);
        Ok(())
    }

    pub fn stop_recording(&mut self) -> Result<()> {
        if let Some(mut r) = self.recorder.take() {
            r.finish()?;
            self.saved_recordings.push(r.path.clone());
            let secs = r.samples as f64 / (self.info.channels.max(1) as f64 * self.info.sample_rate as f64);
            self.flash(format!("saved {} ({secs:.1} s)", r.path.display()));
        }
        Ok(())
    }

    fn toggle_recording(&mut self) {
        let res = if self.recorder.is_some() { self.stop_recording() } else { self.start_recording() };
        if let Err(e) = res {
            self.flash(format!("record error: {e:#}"));
        }
    }

    fn toggle_panel(&mut self, p: Panel) {
        let i = p as usize;
        let on = !self.display.panels[i];
        if !on && self.display.panels.iter().filter(|&&b| b).count() == 1 {
            return;
        }
        self.display.panels[i] = on;
    }

    pub fn handle_event(&mut self, ev: Event) -> Action {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.handle_key(k),
            Event::Mouse(m) => {
                let hit = self
                    .chart_rects
                    .iter()
                    .any(|r| m.column >= r.x && m.column < r.x + r.width && m.row >= r.y && m.row < r.y + r.height);
                match m.kind {
                    MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) if hit => {
                        let x = self.chart_rects[0].x;
                        self.nav(Nav::Column(m.column.saturating_sub(x) as usize));
                        Action::Redraw
                    }
                    MouseEventKind::ScrollUp => {
                        self.nav(Nav::Left(4));
                        Action::Redraw
                    }
                    MouseEventKind::ScrollDown => {
                        self.nav(Nav::Right(4));
                        Action::Redraw
                    }
                    _ => Action::None,
                }
            }
            Event::Resize(..) => Action::Redraw,
            _ => Action::None,
        }
    }

    pub fn handle_key(&mut self, k: KeyEvent) -> Action {
        let fast = k.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL);
        let n = if fast { 10 } else { 1 };
        match k.code {
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return Action::Quit,
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Esc => {
                if self.show_help {
                    self.show_help = false;
                } else {
                    return Action::Quit;
                }
            }
            KeyCode::Left | KeyCode::Char('h') => self.nav(Nav::Left(n)),
            KeyCode::Right | KeyCode::Char('l') => self.nav(Nav::Right(n)),
            KeyCode::Char('H') => self.nav(Nav::Left(10)),
            KeyCode::Char('L') => self.nav(Nav::Right(10)),
            KeyCode::PageUp | KeyCode::Char('b') => self.nav(Nav::PageLeft),
            KeyCode::PageDown | KeyCode::Char('f') => self.nav(Nav::PageRight),
            KeyCode::Home | KeyCode::Char('g') => self.nav(Nav::Home),
            KeyCode::End | KeyCode::Char('G') => self.nav(Nav::End),
            KeyCode::Char(' ') => self.nav(Nav::ToggleHold),
            KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Up => self.nav(Nav::ZoomIn),
            KeyCode::Char('-') | KeyCode::Char('_') | KeyCode::Down => self.nav(Nav::ZoomOut),
            KeyCode::Char('p') => {
                let p = !self.paused();
                self.control.paused.store(p, Ordering::Relaxed);
                self.flash(if p { "input paused" } else { "input resumed" });
            }
            KeyCode::Char('r') => self.toggle_recording(),
            KeyCode::Char('x') => {
                self.session.reset();
                self.flash("max level and clip count reset");
            }
            KeyCode::Char('R') => {
                self.session.reset_loudness();
                self.flash("max loudness reset");
            }
            KeyCode::Char('1') => self.toggle_panel(Panel::Meter),
            KeyCode::Char('2') => self.toggle_panel(Panel::Eq),
            KeyCode::Char('3') => self.toggle_panel(Panel::Spectrogram),
            KeyCode::Char('4') => self.toggle_panel(Panel::Timeline),
            KeyCode::Char('c') => {
                self.display.colormap = self.display.colormap.next();
                self.flash(format!("colormap: {:?}", self.display.colormap).to_lowercase());
            }
            KeyCode::Char('s') => {
                self.display.freq_scale = self.display.freq_scale.next();
                self.flash(format!("frequency scale: {:?}", self.display.freq_scale).to_lowercase());
            }
            KeyCode::Char('v') => {
                self.display.vu = !self.display.vu;
                self.flash(if self.display.vu { "meters: VU needle" } else { "meters: RMS / peak" });
            }
            KeyCode::Char('[') => self.display.db_min = (self.display.db_min - 6.0).max(-120.0),
            KeyCode::Char(']') => self.display.db_min = (self.display.db_min + 6.0).min(self.display.db_max - 12.0),
            KeyCode::Char('{') => self.display.db_max = (self.display.db_max - 6.0).max(self.display.db_min + 12.0),
            KeyCode::Char('}') => self.display.db_max = (self.display.db_max + 6.0).min(6.0),
            KeyCode::Char('?') | KeyCode::F(1) => self.show_help = !self.show_help,
            _ => return Action::None,
        }
        Action::Redraw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame per column: 1 s frames, span equal to the width.
    fn view(width: usize) -> View {
        View { width, dt: 1.0, span_secs: width as f64, ..View::default() }
    }

    #[test]
    fn follow_pins_cursor_to_latest() {
        let mut v = view(10);
        v.sync(0, None);
        assert_eq!((v.cursor, v.view_end), (0, 0));
        v.sync(0, Some(99));
        assert_eq!((v.cursor, v.view_end), (99, 99));
        assert_eq!(v.cursor_column(), Some(9));
        assert_eq!(v.column_frame(9), Some(99));
        assert_eq!(v.column_frame(0), Some(90));
        assert_eq!(v.column_frame(10), None);
    }

    #[test]
    fn early_columns_are_empty() {
        let mut v = view(10);
        v.sync(0, Some(3));
        assert_eq!(v.column_frame(9), Some(3));
        assert_eq!(v.column_frame(6), Some(0));
        assert_eq!(v.column_frame(5), None);
    }

    #[test]
    fn left_right_and_return_to_live() {
        let mut v = view(10);
        v.sync(0, Some(99));
        v.apply(Nav::Left(1), 0, Some(99));
        assert!(!v.follow);
        assert_eq!(v.cursor, 98);
        assert_eq!(v.view_end, 99);
        assert_eq!(v.cursor_column(), Some(8));
        // New data does not move a held cursor.
        v.sync(0, Some(120));
        assert_eq!(v.cursor, 98);
        v.apply(Nav::Right(10), 0, Some(120));
        assert_eq!(v.cursor, 108);
        assert_eq!(v.view_end, 108);
        v.apply(Nav::Right(100), 0, Some(120));
        assert!(v.follow);
        assert_eq!(v.cursor, 120);
    }

    #[test]
    fn view_scrolls_to_keep_cursor_visible() {
        let mut v = view(10);
        v.sync(0, Some(99));
        v.apply(Nav::Left(15), 0, Some(99));
        assert_eq!(v.cursor, 84);
        assert_eq!(v.view_end, 93);
        assert_eq!(v.cursor_column(), Some(0));
    }

    #[test]
    fn paging_home_end() {
        let mut v = view(10);
        v.sync(0, Some(99));
        v.apply(Nav::PageLeft, 0, Some(99));
        assert_eq!((v.cursor, v.view_end), (89, 89));
        v.apply(Nav::PageLeft, 0, Some(99));
        assert_eq!((v.cursor, v.view_end), (79, 79));
        v.apply(Nav::PageRight, 0, Some(99));
        assert_eq!((v.cursor, v.view_end), (89, 89));
        v.apply(Nav::Home, 0, Some(99));
        assert_eq!((v.cursor, v.view_end), (0, 9));
        assert_eq!(v.cursor_column(), Some(0));
        v.apply(Nav::Left(5), 0, Some(99));
        assert_eq!(v.cursor, 0);
        v.apply(Nav::End, 0, Some(99));
        assert!(v.follow);
        assert_eq!(v.cursor, 99);
        v.apply(Nav::PageRight, 0, Some(99));
        assert!(v.follow);
    }

    #[test]
    fn paging_stops_at_oldest_frame() {
        let mut v = view(10);
        v.sync(0, Some(99));
        for _ in 0..20 {
            v.apply(Nav::PageLeft, 0, Some(99));
        }
        assert_eq!((v.cursor, v.view_end), (0, 9));
        assert_eq!(v.column_frame(0), Some(0));
        // With less data than the width, the view stays right-aligned.
        let mut v = view(10);
        v.sync(0, Some(4));
        v.apply(Nav::Home, 0, Some(4));
        assert_eq!(v.view_end, 4);
    }

    #[test]
    fn eviction_clamps_cursor() {
        let mut v = view(10);
        v.sync(0, Some(50));
        v.apply(Nav::Home, 0, Some(50));
        v.sync(20, Some(70));
        assert_eq!(v.cursor, 20);
        assert!(v.view_end >= 20 && v.view_end < 30);
    }

    #[test]
    fn hold_toggle() {
        let mut v = view(10);
        v.sync(0, Some(10));
        v.apply(Nav::ToggleHold, 0, Some(10));
        assert!(!v.follow);
        v.sync(0, Some(30));
        assert_eq!(v.cursor, 10);
        v.apply(Nav::ToggleHold, 0, Some(30));
        assert!(v.follow);
        assert_eq!(v.cursor, 30);
    }

    #[test]
    fn zoom_buckets() {
        let mut v = view(10);
        v.span_secs = 40.0;
        assert_eq!(v.frames_per_col(), 4.0);
        assert_eq!(v.span(), 40);
        v.sync(0, Some(999));
        assert_eq!(v.column_frame(8), Some(995));
        assert_eq!(v.column_frames(8), Some((992, 995)));
        v.apply(Nav::Left(1), 0, Some(999));
        assert_eq!(v.cursor, 995);
        assert_eq!(v.cursor_column(), Some(8));
        v.cursor = 992;
        assert_eq!(v.cursor_column(), Some(8));
        v.cursor = 991;
        assert_eq!(v.cursor_column(), Some(7));
    }

    #[test]
    fn zoom_steps_through_round_spans() {
        let mut v = View { width: 137, dt: 512.0 / 48000.0, ..View::default() };
        assert_eq!(v.span_secs, 10.0);
        v.apply(Nav::ZoomOut, 0, Some(9999));
        assert_eq!(v.span_secs, 20.0);
        v.apply(Nav::ZoomIn, 0, Some(9999));
        v.apply(Nav::ZoomIn, 0, Some(9999));
        assert_eq!(v.span_secs, 5.0);
        for _ in 0..20 {
            v.apply(Nav::ZoomIn, 0, Some(9999));
        }
        assert_eq!(v.span_secs, ZOOM_SPANS[0]);
        for _ in 0..20 {
            v.apply(Nav::ZoomOut, 0, Some(9999));
        }
        assert_eq!(v.span_secs, ZOOM_SPANS[ZOOM_SPANS.len() - 1]);
        // An off-list span (from --span) snaps to the neighbouring levels.
        v.span_secs = 7.0;
        v.apply(Nav::ZoomIn, 0, Some(9999));
        assert_eq!(v.span_secs, 5.0);
        v.span_secs = 7.0;
        v.apply(Nav::ZoomOut, 0, Some(9999));
        assert_eq!(v.span_secs, 10.0);
    }

    #[test]
    fn resizing_stretches_the_same_span() {
        let dt = 512.0 / 48000.0;
        for width in [37, 100, 141, 300] {
            let mut v = View { width, dt, ..View::default() };
            v.sync(0, Some(5000));
            // The columns tile exactly 10 s of frames, oldest to newest.
            let (lo, _) = v.column_frames(0).unwrap();
            let (_, hi) = v.column_frames(width - 1).unwrap();
            let shown = (hi - lo + 1) as f64 * dt;
            assert!((shown - 10.0).abs() <= 2.0 * dt * v.frames_per_col().max(1.0), "{width}: {shown}");
            for c in 1..width {
                let (a, b) = (v.column_frames(c - 1).unwrap(), v.column_frames(c).unwrap());
                assert_eq!(a.1 + 1, b.0, "{width}: columns {c} contiguous");
            }
            // Every frame maps back to the column that shows it.
            for c in 0..width {
                let (a, b) = v.column_frames(c).unwrap();
                assert_eq!(v.frame_column(a), Some(c));
                assert_eq!(v.frame_column(b), Some(c));
            }
        }
    }

    #[test]
    fn zoomed_in_past_one_frame_per_column() {
        // 0.5 s over 100 columns at 0.01 s frames: half a frame per column.
        let mut v = View { width: 100, dt: 0.01, span_secs: 0.5, ..View::default() };
        assert_eq!(v.frames_per_col(), 0.5);
        v.sync(0, Some(999));
        // Each frame fills two columns; the live edge shows the newest frame's first half.
        assert_eq!(v.column_frames(99), Some((999, 999)));
        assert_eq!(v.column_frames(98), Some((998, 998)));
        assert_eq!(v.column_frames(97), Some((998, 998)));
        assert_eq!(v.column_frames(96), Some((997, 997)));
        assert_eq!(v.cursor_column(), Some(99));
        v.apply(Nav::Left(1), 0, Some(999));
        assert_eq!(v.cursor, 998);
        assert_eq!(v.cursor_column(), Some(97));
    }

    #[test]
    fn live_buckets_are_aligned() {
        let mut v = view(10);
        v.span_secs = 40.0;
        v.sync(0, Some(100));
        assert_eq!(v.view_end, 103);
        assert_eq!(v.cursor_column(), Some(9));
        v.sync(0, Some(102));
        assert_eq!(v.view_end, 103);
        v.sync(0, Some(104));
        assert_eq!(v.view_end, 107);
        v.apply(Nav::ToggleHold, 0, Some(104));
        assert_eq!((v.cursor, v.view_end), (104, 107));
        assert_eq!(v.bucket_end(8), 11);
        // Fractional buckets: 2.5 frames per column.
        v.span_secs = 25.0;
        assert_eq!(v.bucket_end(0), 2);
        assert_eq!(v.bucket_end(3), 4);
        assert_eq!(v.bucket_end(5), 7);
    }

    #[test]
    fn readout_refreshes_every_interval_while_live() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + std::time::Duration::from_millis(n);
        let mut r = Readout::default();
        let all = |_| true;
        assert_eq!(r.pick(10, true, ms(0), all), 10);
        assert_eq!(r.pick(12, true, ms(50), all), 10, "held within the interval");
        assert_eq!(r.pick(15, true, ms(99), all), 10);
        assert_eq!(r.pick(19, true, ms(100), all), 19, "refreshed after it");
        assert_eq!(r.pick(20, true, ms(120), |i| i != 19), 20, "evicted frame is replaced");
        // Scrolling back shows the cursor's frame immediately.
        assert_eq!(r.pick(5, false, ms(130), all), 5);
        assert_eq!(r.pick(6, false, ms(131), all), 6);
        assert_eq!(r.pick(30, true, ms(140), all), 30, "returning to live refreshes at once");
    }

    #[test]
    fn session_tracks_max_and_clip_events() {
        use crate::analyzer::Frame;
        let floor = crate::dsp::level::FLOOR_DB;
        let frame = |peak: f32, clipped: bool| {
            let mut p = [floor; crate::analyzer::MAX_CHANNELS];
            p[0] = peak;
            p[1] = peak - 6.0;
            Frame {
                time: 0.0,
                channels: 2,
                peak: p,
                peak_decay: p,
                rms: p,
                rms_avg: p,
                hold: p,
                vu: p,
                vu_hold: p,
                lufs_m: peak - 3.0,
                lufs_s: floor,
                lufs_i: floor,
                peak_freq: None,
                clipped,
                spectrum: Box::new([]),
                bands: Box::new([]),
                band_caps: Box::new([]),
            }
        };
        let mut s = Session::default();
        s.add(&frame(-20.0, false));
        s.add(&frame(-3.0, false));
        s.add(&frame(-10.0, false));
        assert_eq!((s.max_db, s.clips), (-3.0, 0));
        // Two consecutive clipped frames are one event; a later one is another.
        s.add(&frame(0.0, true));
        s.add(&frame(0.0, true));
        s.add(&frame(-6.0, false));
        s.add(&frame(0.0, true));
        assert_eq!((s.max_db, s.clips), (0.0, 2));
        assert_eq!(s.max_lufs_m, -3.0);
        s.reset();
        assert_eq!((s.max_db, s.clips), (Session::default().max_db, 0));
        assert_eq!(s.max_lufs_m, -3.0, "x keeps the loudness maximum");
        s.reset_loudness();
        assert_eq!(s.max_lufs_m, f32::NEG_INFINITY);
        s.add(&frame(-20.0, false));
        assert_eq!(s.max_lufs_m, -23.0);
        // Resetting mid-clip does not count the same run again.
        s.add(&frame(0.0, true));
        s.reset();
        s.add(&frame(0.0, true));
        assert_eq!(s.clips, 0);
        s.add(&frame(-6.0, false));
        s.add(&frame(0.0, true));
        assert_eq!(s.clips, 1);
    }

    #[test]
    fn click_sets_cursor() {
        let mut v = view(10);
        v.sync(0, Some(99));
        v.apply(Nav::Column(3), 0, Some(99));
        assert!(!v.follow);
        assert_eq!(v.cursor, 93);
        assert_eq!(v.cursor_column(), Some(3));
        // Clicking an empty column (before the data starts) does nothing.
        let mut v = view(10);
        v.sync(0, Some(2));
        v.apply(Nav::Column(0), 0, Some(2));
        assert!(v.follow);
    }

    #[test]
    fn no_data_is_noop() {
        let mut v = view(10);
        v.apply(Nav::Left(1), 0, None);
        assert!(v.follow);
    }

    #[test]
    fn fps_meter() {
        use std::time::Duration;
        let mut m = FpsMeter::new();
        let t = Instant::now();
        for i in 0..90 {
            m.tick(t + Duration::from_millis(i * 1000 / 60), Duration::from_micros(800));
        }
        // 90 draws at 60 Hz: only the last second's worth count.
        assert_eq!(m.fps, 60);
        assert!((m.draw_ms - 0.8).abs() < 1e-9);
        m.tick(t + Duration::from_secs(10), Duration::ZERO);
        assert_eq!(m.fps, 1);
    }

}
