//! Drawing. Charts write straight into the frame buffer for speed.

pub mod colormap;
pub mod eq;
pub mod meter;
pub mod piano;
pub mod spectrogram;
pub mod timeline;

use crate::app::App;
use crate::cli::Panel;
use crate::source::SourceKind;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Widget};
use ratatui::Frame;
use std::time::Duration;

pub use spectrogram::SpecCache;

/// Width of the left axis on the spectrogram and timeline (kept equal so columns align).
pub const AXIS_W: u16 = 6;

/// Eighth-block glyphs, index = filled eighths.
pub const EIGHTHS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Glyph for a cell whose fill fraction (0..=1) is `f`.
pub fn eighth(f: f32) -> char {
    EIGHTHS[((f.clamp(0.0, 1.0) * 8.0).round() as usize).min(8)]
}

/// Fraction of a cell `row_from_bottom` filled by a bar of height `h` cells.
pub fn cell_fill(h: f32, row_from_bottom: usize) -> f32 {
    (h - row_from_bottom as f32).clamp(0.0, 1.0)
}

/// Bar height in cells for `db` on a `[lo, hi]` scale of `rows` cells.
pub fn db_to_height(db: f32, lo: f32, hi: f32, rows: u16) -> f32 {
    if !db.is_finite() {
        return 0.0;
    }
    ((db - lo) / (hi - lo)).clamp(0.0, 1.0) * rows as f32
}

/// Smallest "round" tick interval (s) that leaves at least `min_cols` columns
/// between ticks when each column spans `secs_per_col` seconds.
pub fn tick_step(secs_per_col: f64, min_cols: usize) -> f64 {
    const STEPS: [f64; 19] = [
        0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0,
        1800.0, 3600.0, 7200.0, 14400.0,
    ];
    let need = secs_per_col * min_cols as f64;
    STEPS
        .into_iter()
        .find(|&s| s >= need)
        .unwrap_or(STEPS[STEPS.len() - 1])
}

/// Tick label: `5s`, `2.5s`, `0.25s` under a minute, `1:30` / `1:30.5` past it,
/// `1:02:00` past an hour. Decimals follow the tick step.
pub fn fmt_tick(t: f64, step: f64) -> String {
    let dec = if step >= 1.0 {
        0
    } else if step >= 0.1 {
        1
    } else {
        2
    };
    let scale = 10f64.powi(dec);
    let t = (t * scale).round() / scale;
    if t < 60.0 {
        return format!("{t:.dec$}s", dec = dec as usize);
    }
    let whole = t.floor() as u64;
    let frac = t - whole as f64;
    let (h, m, s) = (whole / 3600, whole / 60 % 60, whole % 60);
    let frac = if dec > 0 {
        format!("{:.dec$}", frac, dec = dec as usize)[1..].to_string()
    } else {
        String::new()
    };
    if h > 0 {
        format!("{h}:{m:02}:{s:02}{frac}")
    } else {
        format!("{m}:{s:02}{frac}")
    }
}

/// Seconds of stream time at each frame, anchored on one known frame.
#[derive(Clone, Copy)]
pub struct Clock {
    pub frame: usize,
    pub time: f64,
    pub dt: f64,
}

impl Clock {
    pub fn of(app: &App) -> Option<Clock> {
        let first = app.history.first();
        let f = app.history.get(first)?;
        Some(Clock {
            frame: first,
            time: f.time,
            dt: app.analysis.frame_dt,
        })
    }

    pub fn time(&self, f: usize) -> f64 {
        self.time + (f as f64 - self.frame as f64) * self.dt
    }

    /// Nearest frame to time `t`, if not before frame 0.
    pub fn frame(&self, t: f64) -> Option<usize> {
        let f = self.frame as f64 + ((t - self.time) / self.dt).round();
        (f >= 0.0).then_some(f as usize)
    }
}

/// Tick positions `(column, seconds)` at round times across the view.
pub fn time_ticks(
    view: &crate::app::View,
    clock: Clock,
    min_cols: usize,
) -> (f64, Vec<(usize, f64)>) {
    let step = tick_step(view.span_secs / view.width.max(1) as f64, min_cols);
    let Some(start) = view
        .column_frames(0)
        .map(|(lo, _)| lo)
        .or_else(|| view.column_frame(view.width - 1).map(|_| 0))
    else {
        return (step, Vec::new());
    };
    // The first frame ends one hop in; let the stream start count as 0 s.
    let (t0, t1) = (
        (clock.time(start) - clock.dt).max(0.0),
        clock.time(view.view_end),
    );
    let mut ticks = Vec::new();
    let mut k = (t0 / step).ceil() as i64;
    while (k as f64) * step <= t1 + 1e-9 {
        let t = k as f64 * step;
        let frame = clock.frame(t).or_else(|| (t >= 0.0).then_some(0));
        if let Some(c) = frame.and_then(|f| view.frame_column(f)) {
            if ticks.last().is_none_or(|&(lc, _)| c > lc) {
                ticks.push((c, t));
            }
        }
        k += 1;
    }
    (step, ticks)
}

/// Columns kept between time ticks, enough for a label like `1:02:30`.
const TICK_SPACING: usize = 10;

/// Draw time ticks into the bottom border of a chart panel: `┴` marks the
/// column, with its label to the right.
pub fn draw_time_axis(buf: &mut Buffer, app: &App, area: Rect, chart: Rect) {
    let Some(clock) = Clock::of(app) else { return };
    if area.height < 2 {
        return;
    }
    let y = area.y + area.height - 1;
    let (step, ticks) = time_ticks(&app.view, clock, TICK_SPACING);
    let end = area.x + area.width - 1;
    let style = Style::default().fg(Color::Gray);
    let mut free_x = chart.x;
    for (c, t) in ticks {
        let x = chart.x + c as u16;
        if x < free_x || x >= end {
            continue;
        }
        buf[(x, y)].set_char('┴');
        let label = fmt_tick(t, step);
        let lw = label.len() as u16;
        if x + 1 + lw < end {
            buf.set_string(x + 1, y, &label, style);
            free_x = x + 2 + lw;
        } else {
            free_x = x + 1;
        }
    }
}

/// Visible time range `start – end (span)` for chart titles.
pub fn view_range_text(app: &App) -> String {
    let span = app.view.span_secs;
    let span_txt = if span < 60.0 {
        format!("{span} s")
    } else {
        format!("{} min", span / 60.0)
    };
    let Some(clock) = Clock::of(app) else {
        return span_txt;
    };
    let v = &app.view;
    let last = app.history.last().unwrap_or(v.view_end).min(v.view_end);
    let start = v.column_frames(0).map_or(0, |(lo, _)| lo);
    format!(
        "{} – {} ({span_txt})",
        fmt_time(clock.time(start).max(0.0)),
        fmt_time(clock.time(last).max(0.0))
    )
}

/// `m:ss.mmm`, or `h:mm:ss.mmm` past an hour.
pub fn fmt_time(secs: f64) -> String {
    let ms_total = (secs.max(0.0) * 1000.0).round() as u64;
    let (h, rem) = (ms_total / 3_600_000, ms_total % 3_600_000);
    let (m, rem) = (rem / 60_000, rem % 60_000);
    let (s, ms) = (rem / 1000, rem % 1000);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{ms:03}")
    } else {
        format!("{m}:{s:02}.{ms:03}")
    }
}

/// Compact frequency label: 20, 500, 1k, 1.5k, 20k.
pub fn fmt_freq(f: f32) -> String {
    if f >= 1000.0 {
        let k = f / 1000.0;
        if (k - k.round()).abs() < 0.05 || k >= 10.0 {
            format!("{}k", k.round() as u32)
        } else {
            format!("{k:.1}k")
        }
    } else {
        format!("{}", f.round() as u32)
    }
}

/// dB value for text: "-12.3" or "-inf".
pub fn fmt_db(db: f32) -> String {
    if !db.is_finite() || db <= -140.0 {
        " -inf".into()
    } else {
        format!("{db:5.1}")
    }
}

/// Grid step (dB) giving at least `min_rows` rows between labels.
pub fn db_label_step(range: f32, rows: u16, min_rows: u16) -> f32 {
    for step in [3.0, 6.0, 10.0, 12.0, 20.0, 24.0, 30.0, 40.0, 60.0] {
        if (step / range) * rows as f32 >= min_rows as f32 {
            return step;
        }
    }
    120.0
}

/// Row (from the top of an area of `rows`) where `db` sits on `[lo, hi]`.
pub fn db_row(db: f32, lo: f32, hi: f32, rows: u16) -> u16 {
    let h = ((db - lo) / (hi - lo)).clamp(0.0, 1.0) * rows as f32;
    rows.saturating_sub(1)
        .saturating_sub((h.floor() as u16).min(rows.saturating_sub(1)))
}

/// Where to draw a thin level marker (peak hold, EQ cap) for `db` on `[lo, hi]`
/// over `rows` rows: `(row from the top, glyph)`. The glyph is the cell's top or
/// bottom edge, whichever is nearer the level, so the marker is never more than
/// half a row off. `None` at or below the floor.
pub fn edge_marker(db: f32, lo: f32, hi: f32, rows: u16) -> Option<(u16, char)> {
    if !db.is_finite() || db <= lo {
        return None;
    }
    edge_marker_at(((db - lo) / (hi - lo)).clamp(0.0, 1.0) * rows as f32, rows)
}

/// [`edge_marker`] for a level already converted to a height of `h` rows.
pub fn edge_marker_at(h: f32, rows: u16) -> Option<(u16, char)> {
    if rows == 0 || h.is_nan() || h <= 0.0 {
        return None;
    }
    let rb = (h.floor() as u16).min(rows - 1);
    let frac = h - rb as f32;
    let glyph = if frac >= 0.5 { '▔' } else { '▁' };
    Some((rows - 1 - rb, glyph))
}

/// Draw dB labels down a left axis.
pub fn draw_db_axis(buf: &mut Buffer, axis: Rect, lo: f32, hi: f32) {
    if axis.width < 3 || axis.height == 0 {
        return;
    }
    let step = db_label_step(hi - lo, axis.height, 2);
    let mut v = (hi / step).floor() * step;
    let style = Style::default().fg(Color::DarkGray);
    while v >= lo {
        let row = db_row(v, lo, hi, axis.height);
        let s = format!("{:>w$}", v as i32, w = (axis.width - 1) as usize);
        buf.set_string(axis.x, axis.y + row, s, style);
        v -= step;
    }
}

fn split_axis(inner: Rect) -> (Rect, Rect) {
    let aw = AXIS_W.min(inner.width);
    (
        Rect { width: aw, ..inner },
        Rect {
            x: inner.x + aw,
            width: inner.width - aw,
            ..inner
        },
    )
}

fn title(s: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(
        s.into(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))
}

/// Rows the top panels need: enough for every level readout, including the
/// CLIP badge, when the meter is shown.
pub fn top_min_height(meter: bool, channels: usize) -> u16 {
    if meter {
        2 + 6 + channels.clamp(1, crate::analyzer::MAX_CHANNELS) as u16
    } else {
        6
    }
}

/// Height of the top row: a third of the body (all of it when alone), at least
/// `min`, while leaving each of the `charts` below 4 rows.
pub fn top_height(body: u16, min: u16, charts: u16) -> u16 {
    if charts == 0 {
        return body;
    }
    (body / 3)
        .max(min)
        .min(body.saturating_sub(4 * charts))
        .max(body / 3)
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let [status, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let d = &app.display;
    let top = d.shown(Panel::Meter) || d.shown(Panel::Eq);
    let (spec, tl) = (d.shown(Panel::Spectrogram), d.shown(Panel::Timeline));
    let piano = d.shown(Panel::Piano);
    let mut cons = Vec::new();
    if top {
        let min = top_min_height(d.shown(Panel::Meter), app.analysis.channels);
        let rest = body
            .height
            .saturating_sub(if piano { piano::HEIGHT } else { 0 });
        cons.push(Constraint::Length(top_height(
            rest,
            min,
            spec as u16 + tl as u16,
        )));
    }
    if piano {
        cons.push(Constraint::Length(piano::HEIGHT));
    }
    if spec {
        cons.push(Constraint::Fill(4));
    }
    if tl {
        cons.push(Constraint::Fill(2));
    }
    let rows = Layout::vertical(cons).split(body);
    let mut it = rows.iter().copied();
    let top_rect = if top { it.next() } else { None };
    let piano_rect = if piano { it.next() } else { None };
    let spec_rect = if spec { it.next() } else { None };
    let tl_rect = if tl { it.next() } else { None };

    // Establish chart width first so the view and every panel agree on the cursor.
    let chart_w = spec_rect
        .or(tl_rect)
        .map(|r| split_axis(Block::bordered().inner(r)).1.width)
        .unwrap_or(1);
    app.view.width = chart_w.max(1) as usize;
    app.sync_view();
    app.chart_rects.clear();

    let buf = f.buffer_mut();
    if let Some(r) = top_rect {
        let (m, e) = (
            app.display.shown(Panel::Meter),
            app.display.shown(Panel::Eq),
        );
        let (mr, er) = match (m, e) {
            (true, true) => {
                let mw = meter::width(app.analysis.channels, r.width);
                let [a, b] =
                    Layout::horizontal([Constraint::Length(mw), Constraint::Fill(1)]).areas(r);
                (Some(a), Some(b))
            }
            (true, false) => (Some(r), None),
            _ => (None, Some(r)),
        };
        if let Some(mr) = mr {
            meter::render(app, mr, buf);
        }
        if let Some(er) = er {
            eq::render(app, er, buf);
        }
    }
    if let Some(r) = piano_rect {
        piano::render(app, r, buf);
    }
    if let Some(r) = spec_rect {
        let c = spectrogram::render(app, r, buf);
        app.chart_rects.push(c);
    }
    if let Some(r) = tl_rect {
        let c = timeline::render(app, r, buf);
        app.chart_rects.push(c);
    }
    render_status(app, status, buf);
    if app.show_help {
        render_help(area, buf);
    }
}

fn render_status(app: &mut App, area: Rect, buf: &mut Buffer) {
    let a = &app.analysis;
    let mut spans = vec![
        Span::styled(
            " vu ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            app.info.name.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {} Hz {}ch ", a.sample_rate, a.channels),
            Style::default().fg(Color::Gray),
        ),
    ];
    // Indices of the spans that give way first when the bar is too narrow.
    let (name_i, info_i) = (2, 3);
    let (mode, color) = if app.ingest_done {
        ("■ DONE", Color::Blue)
    } else if app.source_done {
        ("■ END", Color::Blue)
    } else if app.paused() {
        ("❚❚ PAUSED", Color::Magenta)
    } else if !app.view.follow {
        ("◆ HOLD", Color::Yellow)
    } else if app.info.kind == SourceKind::Device {
        ("● LIVE", Color::Green)
    } else {
        ("▶ PLAY", Color::Green)
    };
    spans.push(Span::styled(
        format!(" {mode} "),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ));
    if let Some(fr) = app.history.get(app.view.cursor) {
        let latest = app
            .history
            .last()
            .and_then(|l| app.history.get(l))
            .map(|f| f.time)
            .unwrap_or(0.0);
        spans.push(Span::styled(
            format!(" t {}", fmt_time(fr.time)),
            Style::default().fg(Color::White),
        ));
        if !app.view.follow {
            spans.push(Span::styled(
                format!(" (-{:.2}s)", latest - fr.time),
                Style::default().fg(Color::Yellow),
            ));
        }
    }
    if let Some(r) = &app.recorder {
        let secs = r.samples as f64 / (a.channels.max(1) as f64 * a.sample_rate as f64);
        spans.push(Span::styled(
            format!("  ● REC {:.1}s", secs),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    }
    if let Some((msg, at)) = &app.message {
        if at.elapsed() < Duration::from_secs(4) {
            spans.push(Span::styled(
                format!("  {msg}"),
                Style::default().fg(Color::LightBlue),
            ));
        }
    }
    if let Some(w) = &app.input_warning {
        spans.push(Span::styled(
            format!("  ⚠ {w}"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let dropped = app
        .control
        .dropped
        .load(std::sync::atomic::Ordering::Relaxed);
    if dropped > 0 {
        spans.push(Span::styled(
            format!("  {dropped} buffers dropped"),
            Style::default().fg(Color::LightRed),
        ));
    }
    if let Some(e) = app.control.error.lock().ok().and_then(|g| g.clone()) {
        spans.push(Span::styled(
            format!("  {e}"),
            Style::default().fg(Color::Red),
        ));
    }
    // Nothing redraws while idle (e.g. a finished file), so hide a meaningless 0 fps.
    let right = if app.fps.fps > 0 {
        format!("{} fps {:.1} ms  ? help ", app.fps.fps, app.fps.draw_ms)
    } else {
        "? help ".to_string()
    };
    let rw = right.chars().count();
    let width = area.width as usize;
    let used = |spans: &[Span]| {
        spans
            .iter()
            .map(|s| s.content.chars().count())
            .sum::<usize>()
    };
    // Give way in order: the fps/help corner, the format, then the device name.
    let show_right = used(&spans) + rw < width;
    if used(&spans) > width {
        spans[info_i].content = "".into();
    }
    let over = used(&spans).saturating_sub(width);
    if over > 0 {
        let name: Vec<char> = spans[name_i].content.chars().collect();
        let keep = name.len().saturating_sub(over + 1).max(4).min(name.len());
        spans[name_i].content = format!("{}…", name[..keep].iter().collect::<String>()).into();
    }
    Paragraph::new(Line::from(spans)).render(area, buf);
    if show_right {
        buf.set_string(
            area.x + (width - rw) as u16,
            area.y,
            right,
            Style::default().fg(Color::DarkGray),
        );
    }
}

pub const HELP: &[(&str, &str)] = &[
    (
        "← / →  h / l",
        "move the cursor (Shift, Ctrl or H / L: 10 columns)",
    ),
    ("PgUp / PgDn  b / f", "page backward / forward"),
    ("Home / End  g / G", "oldest data / back to live"),
    ("space", "hold / release the display"),
    ("click / wheel", "place the cursor / scroll"),
    ("+ / -  ↑ / ↓", "zoom the time axis in / out"),
    ("p", "pause / resume the input"),
    ("R", "start / stop recording"),
    ("x", "reset the max level and clip count"),
    (
        "r",
        "reset the loudness measurement (integrated, LRA, max M, max TP)",
    ),
    (
        "1 2 3 4 5",
        "show / hide levels, equalizer, spectrogram, timeline, piano",
    ),
    ("c / s", "cycle colour map / frequency scale"),
    ("v", "meter mode: RMS, VU, LUFS (meters and timeline)"),
    ("[ / ]  { / }", "lower / raise the chart floor / ceiling"),
    ("? / F1", "toggle this help"),
    ("q / Esc", "quit"),
];

/// What the level readouts mean.
pub const LEGEND: &[(&str, &str)] = &[
    (
        "peak  hold",
        "RMS: sample peak now; highest, held 1.5 s, then falling",
    ),
    (
        "rms  avg",
        "RMS: RMS of the latest hop; over 300 ms (the bar)",
    ),
    (
        "vu  hold",
        "VU: needle and its held highest; 0 VU = --vu-ref dBFS",
    ),
    (
        "LUFS",
        "LUFS: 0.4 s (M bar), 3 s (S bar), integrated (◀); highest M (the mark)",
    ),
    (
        "tgt  LRA",
        "LUFS: integrated minus --lufs-target; loudness range (r resets all)",
    ),
    (
        "max/TP  clips",
        "highest sample peak (true peak, dBTP, in LUFS) and clip events (x resets)",
    ),
    (
        "tone",
        "strongest tone that stands out (above -50 dB), with its note",
    ),
];

/// Width of the key column in the help.
const HELP_KEY_W: usize = 20;

/// Help rows `(key, text, is_legend)` for an inner width of `w`, wrapping long
/// descriptions under their own column (or below the key when very narrow).
pub fn help_rows(w: usize) -> Vec<(String, String, bool)> {
    let wide = w >= HELP_KEY_W + 1 + 20;
    let text_w = if wide {
        w - HELP_KEY_W - 1
    } else {
        w.saturating_sub(3).max(8)
    };
    let mut rows = Vec::new();
    for (entries, legend) in [(HELP, false), (LEGEND, true)] {
        for (k, v) in entries {
            let mut chunks = wrap(v, text_w).into_iter();
            if wide {
                rows.push((k.to_string(), chunks.next().unwrap_or_default(), legend));
            } else {
                rows.push((k.to_string(), String::new(), legend));
            }
            rows.extend(chunks.map(|c| (String::new(), c, legend)));
        }
    }
    rows
}

/// Greedy word wrap to `width` characters.
fn wrap(s: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in s.split(' ') {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    out.push(cur);
    out
}

fn render_help(area: Rect, buf: &mut Buffer) {
    let longest = HELP
        .iter()
        .chain(LEGEND)
        .map(|(_, v)| v.chars().count())
        .max()
        .unwrap_or(0);
    let w = ((HELP_KEY_W + 1 + longest + 3) as u16).min(area.width);
    let rows = help_rows(w.saturating_sub(3) as usize);
    let h = (rows.len() as u16 + 3).min(area.height);
    let r = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    Clear.render(r, buf);
    let wide = w as usize >= HELP_KEY_W + 1 + 20 + 3;
    let mut lines: Vec<Line> = Vec::new();
    let mut in_legend = false;
    for (k, v, legend) in rows {
        if legend && !in_legend {
            in_legend = true;
            lines.push(Line::from(""));
        }
        let ks = Style::default().fg(if legend { Color::Cyan } else { Color::Yellow });
        let (k, v) = if wide {
            (format!(" {k:<HELP_KEY_W$}"), v)
        } else if k.is_empty() {
            (String::new(), format!("   {v}"))
        } else {
            (format!(" {k}"), v)
        };
        lines.push(Line::from(vec![Span::styled(k, ks), Span::raw(v)]));
    }
    Paragraph::new(lines)
        .block(
            Block::bordered()
                .title(title(" Keys "))
                .title_bottom(" the cursor picks the moment the levels show "),
        )
        .render(r, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eighths() {
        assert_eq!(eighth(0.0), ' ');
        assert_eq!(eighth(1.0), '█');
        assert_eq!(eighth(0.5), '▄');
        assert_eq!(eighth(2.0), '█');
        assert_eq!(eighth(-1.0), ' ');
        assert_eq!(cell_fill(2.5, 0), 1.0);
        assert_eq!(cell_fill(2.5, 2), 0.5);
        assert_eq!(cell_fill(2.5, 3), 0.0);
    }

    #[test]
    fn heights_and_rows() {
        assert_eq!(db_to_height(-30.0, -60.0, 0.0, 10), 5.0);
        assert_eq!(db_to_height(-90.0, -60.0, 0.0, 10), 0.0);
        assert_eq!(db_to_height(10.0, -60.0, 0.0, 10), 10.0);
        assert_eq!(db_to_height(f32::NEG_INFINITY, -60.0, 0.0, 10), 0.0);
        assert_eq!(db_row(0.0, -60.0, 0.0, 10), 0);
        assert_eq!(db_row(-60.0, -60.0, 0.0, 10), 9);
        assert_eq!(db_row(-30.0, -60.0, 0.0, 10), 4);
        assert_eq!(db_row(-30.0, -60.0, 0.0, 0), 0);
    }

    #[test]
    fn markers_sit_on_the_nearest_edge() {
        // 5 rows over -60..0: 12 dB per row; -12 dB is the bottom edge of the top row.
        assert_eq!(edge_marker(-12.0, -60.0, 0.0, 5), Some((0, '▁')));
        assert_eq!(edge_marker(0.0, -60.0, 0.0, 5), Some((0, '▔')));
        assert_eq!(
            edge_marker(-14.0, -60.0, 0.0, 5),
            Some((1, '▔')),
            "near the top of row 1"
        );
        assert_eq!(
            edge_marker(-22.0, -60.0, 0.0, 5),
            Some((1, '▁')),
            "near the bottom of row 1"
        );
        assert_eq!(edge_marker(-59.0, -60.0, 0.0, 5), Some((4, '▁')));
        assert_eq!(edge_marker(-60.0, -60.0, 0.0, 5), None);
        assert_eq!(edge_marker(f32::NEG_INFINITY, -60.0, 0.0, 5), None);
        assert_eq!(edge_marker(-10.0, -60.0, 0.0, 0), None);
        // Error never exceeds half a row.
        for i in 1..600 {
            let db = -60.0 + i as f32 * 0.1;
            let (row, g) = edge_marker(db, -60.0, 0.0, 5).unwrap();
            let edge = (4 - row) as f32 + if g == '▔' { 1.0 } else { 0.0 };
            let h = (db + 60.0) / 12.0;
            assert!((edge - h).abs() <= 0.5 + 1e-4, "{db}: edge {edge} vs {h}");
        }
    }

    #[test]
    fn label_steps() {
        assert_eq!(db_label_step(96.0, 40, 2), 6.0);
        assert_eq!(db_label_step(96.0, 10, 2), 20.0);
        assert_eq!(db_label_step(60.0, 1, 2), 120.0);
    }

    #[test]
    fn ticks_are_round() {
        assert_eq!(tick_step(0.0853, 10), 1.0);
        assert_eq!(tick_step(0.0213, 10), 0.5);
        assert_eq!(tick_step(0.6, 10), 10.0);
        assert_eq!(tick_step(1.6, 10), 30.0);
        assert_eq!(tick_step(1e6, 10), 14400.0);
        assert_eq!(fmt_tick(10.0, 1.0), "10s");
        assert_eq!(fmt_tick(2.5, 0.5), "2.5s");
        assert_eq!(fmt_tick(0.25, 0.05), "0.25s");
        assert_eq!(fmt_tick(90.0, 10.0), "1:30");
        assert_eq!(fmt_tick(90.5, 0.5), "1:30.5");
        assert_eq!(fmt_tick(3720.0, 60.0), "1:02:00");
        assert_eq!(fmt_tick(59.99, 1.0), "1:00");
    }

    #[test]
    fn tick_columns() {
        use crate::app::View;
        // 100 columns, 4 frames per column, 0.01 s per frame: 0.04 s/col, 4 s visible.
        let v = View {
            follow: true,
            cursor: 999,
            view_end: 999,
            span_secs: 4.0,
            dt: 0.01,
            width: 100,
        };
        let clock = Clock {
            frame: 0,
            time: 0.01,
            dt: 0.01,
        };
        let (step, ticks) = time_ticks(&v, clock, 10);
        assert_eq!(step, 0.5);
        let times: Vec<f64> = ticks.iter().map(|t| t.1).collect();
        assert_eq!(times, vec![6.5, 7.0, 7.5, 8.0, 8.5, 9.0, 9.5, 10.0]);
        // 10 s is frame 999, the last column; ticks 0.5 s apart are 12-13 columns apart.
        assert_eq!(ticks.last().unwrap().0, 99);
        for w in ticks.windows(2) {
            assert!((12..=13).contains(&(w[1].0 - w[0].0)));
        }
        // A view reaching back before the stream start begins ticking at 0 s.
        let v = View {
            follow: true,
            cursor: 99,
            view_end: 99,
            span_secs: 4.0,
            dt: 0.01,
            width: 100,
        };
        let (_, ticks) = time_ticks(&v, clock, 10);
        assert_eq!(
            ticks.first().unwrap().1,
            0.0,
            "the stream start gets a 0 s tick"
        );
        assert_eq!(ticks.last().unwrap(), &(99, 1.0));
    }

    #[test]
    fn help_fits_its_width() {
        for w in [20usize, 40, 60, 75, 120] {
            let rows = help_rows(w);
            assert!(rows.iter().any(|r| r.0 == "x"), "x listed");
            for (k, v, _) in &rows {
                let len = if w >= HELP_KEY_W + 21 {
                    HELP_KEY_W + 1 + v.chars().count()
                } else {
                    (k.chars().count() + 1).max(v.chars().count() + 3)
                };
                assert!(len <= w.max(HELP_KEY_W), "{w}: {k:?} {v:?}");
            }
            // Nothing is lost: all words appear.
            let text: String = rows
                .iter()
                .map(|r| r.1.clone())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(text.contains("columns)") && text.contains("note"), "{w}");
        }
        assert_eq!(wrap("a bb ccc", 4), ["a bb", "ccc"]);
        assert_eq!(wrap("", 4), [""]);
    }

    #[test]
    fn top_row_fits_readouts() {
        // 80x24: body 23 rows, stereo needs 10.
        assert_eq!(top_min_height(true, 2), 10);
        assert_eq!(top_height(23, 10, 2), 10);
        // Tall screens keep a third.
        assert_eq!(top_height(60, 9, 2), 20);
        // Never squeezes the charts below 4 rows each...
        assert_eq!(top_height(15, 9, 2), 7);
        // ...unless even a third would.
        assert_eq!(top_height(6, 9, 2), 2);
        assert_eq!(top_height(30, 9, 0), 30);
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt_time(0.0), "0:00.000");
        assert_eq!(fmt_time(61.5), "1:01.500");
        assert_eq!(fmt_time(3725.25), "1:02:05.250");
        assert_eq!(fmt_time(-3.0), "0:00.000");
        assert_eq!(fmt_freq(20.0), "20");
        assert_eq!(fmt_freq(999.6), "1000");
        assert_eq!(fmt_freq(1000.0), "1k");
        assert_eq!(fmt_freq(1500.0), "1.5k");
        assert_eq!(fmt_freq(12500.0), "13k");
        assert_eq!(fmt_db(-12.34), "-12.3");
        assert_eq!(fmt_db(f32::NEG_INFINITY), " -inf");
        assert_eq!(fmt_db(-150.0), " -inf");
    }
}
