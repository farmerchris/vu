//! Drawing. Charts write straight into the frame buffer for speed.

pub mod colormap;
pub mod eq;
pub mod meter;
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
    rows.saturating_sub(1).saturating_sub((h.floor() as u16).min(rows.saturating_sub(1)))
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
        Rect { x: inner.x + aw, width: inner.width - aw, ..inner },
    )
}

fn title(s: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(s.into(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let [status, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let d = &app.display;
    let top = d.shown(Panel::Meter) || d.shown(Panel::Eq);
    let (spec, tl) = (d.shown(Panel::Spectrogram), d.shown(Panel::Timeline));
    let mut cons = Vec::new();
    if top {
        cons.push(Constraint::Fill(if spec || tl { 3 } else { 1 }));
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
    let spec_rect = if spec { it.next() } else { None };
    let tl_rect = if tl { it.next() } else { None };

    // Establish chart width first so the view and every panel agree on the cursor.
    let chart_w = spec_rect.or(tl_rect).map(|r| split_axis(Block::bordered().inner(r)).1.width).unwrap_or(1);
    app.view.width = chart_w.max(1) as usize;
    app.sync_view();
    app.chart_rects.clear();

    let buf = f.buffer_mut();
    if let Some(r) = top_rect {
        let (m, e) = (app.display.shown(Panel::Meter), app.display.shown(Panel::Eq));
        let (mr, er) = match (m, e) {
            (true, true) => {
                let mw = meter::preferred_width(app.analysis.channels).min(r.width / 2);
                let [a, b] = Layout::horizontal([Constraint::Length(mw), Constraint::Fill(1)]).areas(r);
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
        Span::styled(" vu ", Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(app.info.name.clone(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {} Hz {}ch ", a.sample_rate, a.channels), Style::default().fg(Color::Gray)),
    ];
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
    spans.push(Span::styled(format!(" {mode} "), Style::default().fg(color).add_modifier(Modifier::BOLD)));
    if let Some(fr) = app.history.get(app.view.cursor) {
        let latest = app.history.last().and_then(|l| app.history.get(l)).map(|f| f.time).unwrap_or(0.0);
        spans.push(Span::styled(format!(" t {}", fmt_time(fr.time)), Style::default().fg(Color::White)));
        if !app.view.follow {
            spans.push(Span::styled(format!(" (-{:.2}s)", latest - fr.time), Style::default().fg(Color::Yellow)));
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
            spans.push(Span::styled(format!("  {msg}"), Style::default().fg(Color::LightBlue)));
        }
    }
    let dropped = app.control.dropped.load(std::sync::atomic::Ordering::Relaxed);
    if dropped > 0 {
        spans.push(Span::styled(format!("  {dropped} buffers dropped"), Style::default().fg(Color::LightRed)));
    }
    if let Some(e) = app.control.error.lock().ok().and_then(|g| g.clone()) {
        spans.push(Span::styled(format!("  {e}"), Style::default().fg(Color::Red)));
    }
    let right = format!("{} fps {:.1} ms  ? help ", app.fps.fps, app.fps.draw_ms);
    Paragraph::new(Line::from(spans)).render(area, buf);
    let rw = right.chars().count() as u16;
    if area.width > rw {
        buf.set_string(area.x + area.width - rw, area.y, right, Style::default().fg(Color::DarkGray));
    }
}

const HELP: &[(&str, &str)] = &[
    ("← / →  h / l", "move cursor one column (Shift/Ctrl or H/L: ×10)"),
    ("PgUp / PgDn  b / f", "page backward / forward"),
    ("Home / End  g / G", "oldest data / back to live"),
    ("space", "hold / release the display"),
    ("click / wheel", "place cursor / scroll"),
    ("+ / -  ↑ / ↓", "zoom time axis in / out"),
    ("p", "pause / resume input"),
    ("r", "start / stop recording"),
    ("1 2 3 4", "toggle meter, equalizer, spectrogram, timeline"),
    ("c", "cycle spectrogram colour map"),
    ("s", "cycle frequency scale (log, mel, linear)"),
    ("[ / ]", "lower / raise display floor"),
    ("{ / }", "lower / raise display ceiling"),
    ("? / F1", "toggle this help"),
    ("q / Esc", "quit"),
];

fn render_help(area: Rect, buf: &mut Buffer) {
    let w = 66.min(area.width);
    let h = (HELP.len() as u16 + 4).min(area.height);
    let r = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    Clear.render(r, buf);
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!(" {k:<20}"), Style::default().fg(Color::Yellow)),
                Span::raw(*v),
            ])
        })
        .collect();
    Paragraph::new(lines)
        .block(Block::bordered().title(title(" Keys ")).title_bottom(" the cursor selects the frame shown by the meters "))
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
    fn label_steps() {
        assert_eq!(db_label_step(96.0, 40, 2), 6.0);
        assert_eq!(db_label_step(96.0, 10, 2), 20.0);
        assert_eq!(db_label_step(60.0, 1, 2), 120.0);
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
