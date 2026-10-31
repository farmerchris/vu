//! Level meters for the frame under the cursor: per-channel averaged RMS bars
//! with decaying peak envelope and peak-hold marker, a momentary-loudness bar,
//! and numeric readouts.

use super::colormap::{zone_color, zone_color_dim};
use super::{cell_fill, db_row, db_to_height, draw_db_axis, eighth, fmt_db, title};
use crate::analyzer::Frame;
use crate::app::App;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Widget};

const BAR_W: u16 = 3;
const MAX_BAR_W: u16 = 8;
const SCALE_W: u16 = 5;
const TEXT_W: u16 = 34;

/// Smallest width that fits every bar and the readouts.
pub fn preferred_width(channels: usize) -> u16 {
    2 + SCALE_W + bar_count(channels) * (BAR_W + 1) + TEXT_W
}

/// Panel width when sharing a row of `total` columns with the equalizer: about
/// two fifths of the row, never less than fits nor more than half.
pub fn width(channels: usize, total: u16) -> u16 {
    (total * 2 / 5).max(preferred_width(channels)).min(total / 2)
}

/// Channel bars plus the loudness bar.
fn bar_count(channels: usize) -> u16 {
    channels.clamp(1, crate::analyzer::MAX_CHANNELS) as u16 + 1
}

/// Bar width for `bars` bars in an inner width of `inner`: the space left after
/// the scale and readouts, shared out, between `BAR_W` and `MAX_BAR_W`.
pub fn bar_width(bars: u16, inner: u16) -> u16 {
    let spare = inner.saturating_sub(SCALE_W + TEXT_W + 1);
    (spare / bars.max(1)).saturating_sub(1).clamp(BAR_W, MAX_BAR_W)
}

pub fn channel_label(i: usize, n: usize) -> String {
    match (n, i) {
        (1, _) => "M".into(),
        (2, 0) => "L".into(),
        (2, 1) => "R".into(),
        _ => format!("{}", i + 1),
    }
}

struct Bar {
    label: String,
    fill: f32,
    envelope: f32,
    hold: Option<f32>,
    solid: Option<Color>,
}

fn draw_bar(buf: &mut Buffer, x: u16, bw: u16, area: Rect, lo: f32, hi: f32, bar: &Bar) {
    let rows = area.height;
    let hf = db_to_height(bar.fill, lo, hi, rows);
    let he = db_to_height(bar.envelope, lo, hi, rows);
    for rb in 0..rows as usize {
        let y = area.y + rows - 1 - rb as u16;
        let db = lo + (hi - lo) * (rb as f32 + 0.5) / rows as f32;
        let (fg, dim) = match bar.solid {
            Some(c) => (c, Color::Indexed(23)),
            None => (zone_color(db), zone_color_dim(db)),
        };
        let (ff, fe) = (cell_fill(hf, rb), cell_fill(he, rb));
        for dx in 0..bw {
            let cell = &mut buf[(x + dx, y)];
            if ff > 0.0 {
                cell.set_char(eighth(ff)).set_fg(fg);
                if ff < 1.0 && fe >= 1.0 {
                    cell.set_bg(dim);
                }
            } else if fe > 0.0 {
                cell.set_char(eighth(fe)).set_fg(dim);
            } else {
                cell.set_char('·').set_fg(Color::Indexed(236));
            }
        }
    }
    if let Some(h) = bar.hold.filter(|h| *h > lo) {
        let y = area.y + db_row(h, lo, hi, rows);
        for dx in 0..bw {
            buf[(x + dx, y)].set_char('▔').set_fg(Color::White).set_bg(Color::Reset);
        }
    }
    let label_y = area.y + rows;
    let lx = x + bw.saturating_sub(bar.label.len() as u16) / 2;
    buf.set_string(lx, label_y, &bar.label, Style::default().fg(Color::Gray));
}

pub fn render(app: &mut App, area: Rect, buf: &mut Buffer) {
    let block = Block::bordered().title(title(" Levels "));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height < 3 || inner.width < SCALE_W + BAR_W {
        return;
    }
    let Some(f) = app.history.get(app.view.cursor) else {
        buf.set_string(inner.x + 1, inner.y, "waiting for audio…", Style::default().fg(Color::DarkGray));
        return;
    };
    let lo = app.display.meter_floor;
    let hi = 0.0;
    let n = f.n();
    let hold = app.history.peak_hold(app.view.cursor, app.display.hold_frames);
    let clipped = app.history.clipped(app.view.cursor, app.display.hold_frames);

    let bars_area = Rect { height: inner.height - 1, ..inner };
    let scale = Rect { width: SCALE_W, ..bars_area };
    draw_db_axis(buf, scale, lo, hi);
    let mut bars: Vec<Bar> = (0..n)
        .map(|c| Bar {
            label: channel_label(c, n),
            fill: f.rms_avg[c],
            envelope: f.peak_decay[c],
            hold: Some(hold[c]),
            solid: None,
        })
        .collect();
    bars.push(Bar { label: "LU".into(), fill: f.lufs_m, envelope: f.lufs_s, hold: None, solid: Some(Color::Cyan) });
    let bw = bar_width(bars.len() as u16, inner.width);
    let mut x = inner.x + SCALE_W;
    let bars_end = inner.x + inner.width;
    for b in &bars {
        if x + bw > bars_end {
            break;
        }
        draw_bar(buf, x, bw, bars_area, lo, hi, b);
        x += bw + 1;
    }

    // Numeric readouts to the right of the bars.
    let tx = x + 1;
    if tx + 20 > bars_end {
        return;
    }
    let tw = (bars_end - tx) as usize;
    let mut y = inner.y;
    let mut line = |buf: &mut Buffer, s: String, st: Style| {
        if y < inner.y + inner.height {
            let s: String = s.chars().take(tw).collect();
            buf.set_string(tx, y, s, st);
            y += 1;
        }
    };
    let head = Style::default().fg(Color::DarkGray);
    let val = Style::default().fg(Color::White);
    line(buf, "     peak  hold   rms  avg".into(), head);
    for c in 0..n {
        let st = Style::default().fg(zone_color(f.peak[c].max(hold[c])));
        line(
            buf,
            format!("{:>2} {} {} {} {}", channel_label(c, n), fmt_db(f.peak[c]), fmt_db(hold[c]), fmt_db(f.rms[c]), fmt_db(f.rms_avg[c])),
            st,
        );
    }
    line(buf, String::new(), val);
    line(buf, "LUFS  mom.  short  integ.".into(), head);
    line(buf, format!("     {} {}  {}", fmt_db(f.lufs_m), fmt_db(f.lufs_s), fmt_db(f.lufs_i)), Style::default().fg(Color::Cyan));
    line(buf, String::new(), val);
    line(buf, peak_freq_text(f), val);
    if clipped {
        line(buf, " CLIP ".into(), Style::default().fg(Color::White).bg(Color::Red).add_modifier(Modifier::BOLD));
    }
}

fn peak_freq_text(f: &Frame) -> String {
    match f.peak_freq {
        Some((hz, db)) if db > -100.0 => format!("peak {hz:.1} Hz {} dB {}", fmt_db(db).trim(), note_name(hz)),
        _ => "peak —".into(),
    }
}

/// Nearest musical note with cents offset, e.g. "A4 +0c".
pub fn note_name(hz: f32) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    if hz <= 0.0 || !hz.is_finite() {
        return "—".into();
    }
    let midi = 69.0 + 12.0 * (hz / 440.0).log2();
    let n = midi.round();
    let cents = ((midi - n) * 100.0).round() as i32;
    let ni = n as i32;
    format!("{}{} {:+}c", NAMES[ni.rem_euclid(12) as usize], ni.div_euclid(12) - 1, cents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(channel_label(0, 1), "M");
        assert_eq!(channel_label(0, 2), "L");
        assert_eq!(channel_label(1, 2), "R");
        assert_eq!(channel_label(3, 6), "4");
        assert!(preferred_width(2) > preferred_width(1));
    }

    #[test]
    fn widths() {
        // Narrow screens get the minimum, wide ones two fifths, never over half.
        assert_eq!(width(1, 100), 49);
        assert_eq!(width(1, 145), 58);
        assert_eq!(width(1, 300), 120);
        assert_eq!(width(8, 145), 72);
        // Minimum layout keeps 3-column bars; spare room widens them up to 8.
        assert_eq!(bar_width(2, preferred_width(1) - 2), BAR_W);
        assert_eq!(bar_width(2, 56), 7);
        assert_eq!(bar_width(2, 200), MAX_BAR_W);
        assert_eq!(bar_width(9, 20), BAR_W);
    }

    #[test]
    fn notes() {
        assert_eq!(note_name(440.0), "A4 +0c");
        assert_eq!(note_name(261.63), "C4 +0c");
        assert_eq!(note_name(27.5), "A0 +0c");
        assert_eq!(note_name(446.0), "A4 +23c");
        assert_eq!(note_name(0.0), "—");
    }
}
