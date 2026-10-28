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

/// Scroll/cursor state over the history. Columns map to buckets of `zoom` frames;
/// the rightmost column ends at `view_end`.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    /// Follow live data (cursor pinned to the newest frame).
    pub follow: bool,
    pub cursor: usize,
    pub view_end: usize,
    /// Frames per column.
    pub zoom: usize,
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

pub const MAX_ZOOM: usize = 256;

impl Default for View {
    fn default() -> Self {
        View { follow: true, cursor: 0, view_end: 0, zoom: 1, width: 1 }
    }
}

impl View {
    /// Frames spanned by the visible chart.
    pub fn span(&self) -> usize {
        self.width.max(1) * self.zoom
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
            self.view_end = last;
            return;
        }
        self.cursor = self.cursor.clamp(first, last);
        self.view_end = self.view_end.clamp(first, last);
        let span = self.span();
        if self.cursor > self.view_end {
            self.view_end = self.cursor;
        } else if self.cursor + span <= self.view_end {
            self.view_end = self.cursor + span - 1;
        }
    }

    pub fn apply(&mut self, nav: Nav, first: usize, last: Option<usize>) {
        let Some(last) = last else { return };
        let zoom = self.zoom;
        let step = |n: usize| n * zoom;
        match nav {
            Nav::Left(n) => {
                self.unfollow(last);
                self.cursor = self.cursor.saturating_sub(step(n)).max(first);
            }
            Nav::Right(n) => {
                if !self.follow {
                    let c = self.cursor + step(n);
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
            Nav::ZoomIn => self.zoom = (self.zoom / 2).max(1),
            Nav::ZoomOut => self.zoom = (self.zoom * 2).min(MAX_ZOOM),
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
            self.view_end = last;
        }
    }

    /// Newest frame in column `col`'s bucket, or `None` if it lies before frame 0.
    pub fn column_frame(&self, col: usize) -> Option<usize> {
        if col >= self.width {
            return None;
        }
        self.view_end.checked_sub((self.width - 1 - col) * self.zoom)
    }

    /// Column containing the cursor, if visible.
    pub fn cursor_column(&self) -> Option<usize> {
        if self.cursor > self.view_end {
            return None;
        }
        let back = (self.view_end - self.cursor) / self.zoom;
        (back < self.width).then(|| self.width - 1 - back)
    }
}

/// Exponentially smoothed frames-per-second estimate.
pub struct FpsMeter {
    last: Option<Instant>,
    pub fps: f64,
}

impl FpsMeter {
    pub fn new() -> Self {
        FpsMeter { last: None, fps: 0.0 }
    }

    pub fn tick(&mut self, now: Instant) {
        if let Some(prev) = self.last {
            let dt = now.duration_since(prev).as_secs_f64();
            if dt > 0.0 {
                let inst = 1.0 / dt;
                self.fps = if self.fps == 0.0 { inst } else { self.fps * 0.9 + inst * 0.1 };
            }
        }
        self.last = Some(now);
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
    pub hold_frames: usize,
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
    pub recorder: Option<Recorder>,
    record_base: Option<PathBuf>,
    record_format: RecordFormat,
    record_count: usize,
    /// Chart rectangles (spectrogram, timeline) for mouse hit-testing.
    pub chart_rects: Vec<Rect>,
    pub spec_cache: SpecCache,
    pub band_hold_buf: Vec<u8>,
}

impl App {
    pub fn new(cli: &Cli, info: SourceInfo, analyzer: &Analyzer, control: Arc<Control>, color_mode: ColorMode) -> Self {
        let cfg = analyzer.config();
        let frame_dt = cfg.frame_dt();
        let mut cap = (cli.history / frame_dt).ceil() as usize;
        if let (SourceKind::File, Some(total), false) = (info.kind, info.total_frames, cli.looping) {
            cap = cap.max(total as usize / cfg.hop + 2);
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
            view: View { zoom: cli.zoom, ..View::default() },
            analysis: AnalysisInfo {
                sample_rate: cfg.sample_rate,
                channels: cfg.channels,
                fft_size: cfg.fft_size,
                hop: cfg.hop,
                bin_hz: cfg.bin_hz(),
                bins: analyzer.bins(),
                band_centers: analyzer.bands.centers.clone(),
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
                hold_frames: ((cli.peak_hold as f64 / frame_dt).round() as usize).max(1),
            },
            info,
            control,
            show_help: false,
            fps: FpsMeter::new(),
            source_done: false,
            ingest_done: false,
            message: None,
            recorder: None,
            record_base: cli.record.clone(),
            record_format: cli.record_format,
            record_count: 0,
            chart_rects: Vec::new(),
            spec_cache: SpecCache::default(),
            band_hold_buf: Vec::new(),
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

    fn view(width: usize) -> View {
        View { width, ..View::default() }
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
        v.apply(Nav::ZoomOut, 0, Some(999));
        v.apply(Nav::ZoomOut, 0, Some(999));
        assert_eq!(v.zoom, 4);
        assert_eq!(v.span(), 40);
        assert_eq!(v.column_frame(8), Some(995));
        v.apply(Nav::Left(1), 0, Some(999));
        assert_eq!(v.cursor, 995);
        assert_eq!(v.cursor_column(), Some(8));
        v.cursor = 993;
        assert_eq!(v.cursor_column(), Some(8));
        v.cursor = 991;
        assert_eq!(v.cursor_column(), Some(7));
        for _ in 0..20 {
            v.apply(Nav::ZoomOut, 0, Some(999));
        }
        assert_eq!(v.zoom, MAX_ZOOM);
        for _ in 0..20 {
            v.apply(Nav::ZoomIn, 0, Some(999));
        }
        assert_eq!(v.zoom, 1);
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
        let mut m = FpsMeter::new();
        let t = Instant::now();
        m.tick(t);
        assert_eq!(m.fps, 0.0);
        m.tick(t + std::time::Duration::from_millis(20));
        assert!((m.fps - 50.0).abs() < 1e-6);
        m.tick(t + std::time::Duration::from_millis(30));
        assert!((m.fps - (50.0 * 0.9 + 100.0 * 0.1)).abs() < 1e-6);
    }
}
