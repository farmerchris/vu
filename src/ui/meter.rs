//! Level meters for the frame under the cursor: per-channel averaged RMS bars
//! with decaying peak envelope and peak-hold marker (or, in VU mode, VU
//! needles with a hold marker), a momentary-loudness bar, and numeric readouts.

use super::colormap::{vu_zone_colors, zone_color, zone_color_dim};
use super::{cell_fill, draw_db_axis, edge_marker_at as edge_marker, eighth, fmt_db, title};
use crate::analyzer::Frame;
use crate::app::App;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Widget};

const BAR_W: u16 = 3;
/// Narrowest bars in VU mode, which also fits the LU axis in the same width.
const VU_BAR_W: u16 = 2;
/// Width of the LU bar's own axis in VU mode.
const LU_AXIS_W: u16 = 3;
const MAX_BAR_W: u16 = 8;
const SCALE_W: u16 = 5;
const TEXT_W: u16 = 35;

/// Smallest width that fits every bar and the readouts, in either mode: VU
/// mode narrows the bars to make room for the LU axis, and with few bars that
/// is not quite enough.
pub fn preferred_width(channels: usize) -> u16 {
    let bars = bar_count(channels);
    let short = LU_AXIS_W.saturating_sub(bars * (BAR_W - VU_BAR_W));
    2 + SCALE_W + bars * (BAR_W + 1) + TEXT_W + short
}

/// Narrowest equalizer worth showing beside the meter.
const MIN_EQ_W: u16 = 24;

/// Panel width when sharing a row of `total` columns with the equalizer: about
/// two fifths of the row, or whatever the readouts need when that is more, as
/// long as the equalizer keeps `MIN_EQ_W` columns.
pub fn width(channels: usize, total: u16) -> u16 {
    let want = (total * 2 / 5).max(preferred_width(channels));
    want.min(total.saturating_sub(MIN_EQ_W)).max(want.min(total / 2))
}

/// Channel bars plus the loudness bar.
fn bar_count(channels: usize) -> u16 {
    channels.clamp(1, crate::analyzer::MAX_CHANNELS) as u16 + 1
}

/// Bar width for `bars` bars in an inner width of `inner`: the space left after
/// the scale and readouts, shared out, between `BAR_W` and `MAX_BAR_W`. VU
/// mode also leaves room for the LU axis, narrowing the bars to make it fit.
pub fn bar_width(bars: u16, inner: u16, vu: bool) -> u16 {
    let (axis, min) = if vu { (LU_AXIS_W, VU_BAR_W) } else { (0, BAR_W) };
    let spare = inner.saturating_sub(SCALE_W + TEXT_W + 1 + axis);
    (spare / bars.max(1)).saturating_sub(1).clamp(min, MAX_BAR_W)
}

pub fn channel_label(i: usize, n: usize) -> String {
    match (n, i) {
        (1, _) => "M".into(),
        (2, 0) => "L".into(),
        (2, 1) => "R".into(),
        _ => format!("{}", i + 1),
    }
}

/// Vertical scale of the level bars.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scale {
    /// Linear in dB over `[lo, hi]` dBFS.
    Db { lo: f32, hi: f32 },
    /// The standard VU scale: needle deflection proportional to amplitude,
    /// full scale at +3 VU, with 0 VU at `reference` dBFS.
    Vu { reference: f32 },
    /// The EBU +9 loudness scale (EBU Tech 3341): -18 to +9 LU, linear in
    /// dB, with 0 LU at the `target` loudness (LUFS).
    Lu { target: f32 },
}

/// EBU R128 target loudness (LUFS), 0 LU on the EBU +9 scale.
const LU_TARGET: f32 = -23.0;
/// Range of the EBU +9 scale, LU.
const LU_RANGE: (f32, f32) = (-18.0, 9.0);

/// Top of the VU scale, VU.
const VU_TOP: f32 = 3.0;
/// VU scale marks, most important first; drawn while they fit.
const VU_MARKS: [f32; 11] = [0.0, 3.0, -3.0, -20.0, -10.0, -5.0, -7.0, 2.0, 1.0, -1.0, -2.0];

impl Scale {
    /// Fraction of the bar height for `db` dBFS, 0..=1.
    pub fn frac(self, db: f32) -> f32 {
        if !db.is_finite() {
            return 0.0;
        }
        match self {
            Scale::Db { lo, hi } => ((db - lo) / (hi - lo)).clamp(0.0, 1.0),
            Scale::Vu { reference } => 10f32.powf((db - reference - VU_TOP) / 20.0).clamp(0.0, 1.0),
            Scale::Lu { target } => ((db - target - LU_RANGE.0) / (LU_RANGE.1 - LU_RANGE.0)).clamp(0.0, 1.0),
        }
    }

    /// dBFS at height fraction `t` (the inverse of `frac`).
    pub fn db_at(self, t: f32) -> f32 {
        match self {
            Scale::Db { lo, hi } => lo + (hi - lo) * t,
            Scale::Vu { reference } => reference + VU_TOP + 20.0 * t.max(1e-6).log10(),
            Scale::Lu { target } => target + LU_RANGE.0 + (LU_RANGE.1 - LU_RANGE.0) * t,
        }
    }

    /// Bar and dim envelope colours of a cell at `db` dBFS.
    pub fn colors(self, db: f32) -> (Color, Color) {
        match self {
            Scale::Db { .. } => (zone_color(db), zone_color_dim(db)),
            Scale::Vu { reference } => vu_zone_colors(db - reference),
            Scale::Lu { .. } => (Color::Cyan, Color::Indexed(23)),
        }
    }

    /// A readout value: dBFS, or VU on the VU scale.
    fn fmt(self, db: f32) -> String {
        match self {
            Scale::Db { .. } => fmt_db(db),
            Scale::Vu { reference } if db.is_finite() && db > -140.0 => format!("{:+5.1}", db - reference),
            Scale::Vu { .. } => fmt_db(db),
            Scale::Lu { target } if db.is_finite() && db > -140.0 => format!("{:+5.1}", db - target),
            Scale::Lu { .. } => fmt_db(db),
        }
    }

    /// Readout colour for a level of `db` dBFS.
    fn color(self, db: f32) -> Color {
        self.colors(db).0
    }

    pub fn draw_axis(self, buf: &mut Buffer, axis: Rect) {
        let reference = match self {
            Scale::Db { lo, hi } => return draw_db_axis(buf, axis, lo, hi),
            Scale::Lu { target } => return draw_lu_axis(buf, axis, target),
            Scale::Vu { reference } => reference,
        };
        let rows = axis.height;
        if axis.width < 3 || rows == 0 {
            return;
        }
        let mut taken = vec![false; rows as usize];
        for vu in VU_MARKS {
            let h = self.frac(reference + vu) * rows as f32;
            // Full scale sits on the top row; below that, the row it falls in.
            let row = rows - 1 - (h.floor() as u16).min(rows - 1);
            // Marks squeezed into the bottom row are not worth a label.
            if taken[row as usize] || (vu < 0.0 && row == rows - 1 && vu != -20.0) {
                continue;
            }
            taken[row as usize] = true;
            let label = if vu > 0.0 { format!("+{vu}") } else { format!("{vu}") };
            let (fg, _) = vu_zone_colors(vu);
            let style = Style::default().fg(if vu >= 0.0 { fg } else { Color::DarkGray });
            buf.set_string(axis.x, axis.y + row, format!("{label:>w$}", w = (axis.width - 1) as usize), style);
        }
    }
}

/// LU labels, left-aligned to sit right of the LU bar: every 3 or 9 LU as
/// space allows, so +9 and 0 are always marked, with 0 LU (the target) in the
/// bar's colour.
fn draw_lu_axis(buf: &mut Buffer, axis: Rect, target: f32) {
    let rows = axis.height;
    if axis.width < 2 || rows == 0 {
        return;
    }
    let span = LU_RANGE.1 - LU_RANGE.0;
    let Some(step) = [3.0, 9.0].into_iter().find(|s| s / span * rows as f32 >= 2.0) else { return };
    let scale = Scale::Lu { target };
    let mut lu = LU_RANGE.1;
    while lu >= LU_RANGE.0 {
        let h = scale.frac(target + lu) * rows as f32;
        let row = rows - 1 - (h.floor() as u16).min(rows - 1);
        let label = if lu > 0.0 { format!("+{lu}") } else { format!("{lu}") };
        let color = if lu == 0.0 { Color::Cyan } else { Color::DarkGray };
        buf.set_string(axis.x, axis.y + row, format!("{label:<w$}", w = axis.width as usize), Style::default().fg(color));
        lu -= step;
    }
}

struct Bar {
    label: String,
    fill: f32,
    envelope: f32,
    hold: Option<f32>,
    solid: Option<Color>,
    scale: Scale,
}

fn draw_bar(buf: &mut Buffer, x: u16, bw: u16, area: Rect, bar: &Bar) {
    let rows = area.height;
    let scale = bar.scale;
    let hf = scale.frac(bar.fill) * rows as f32;
    let he = scale.frac(bar.envelope) * rows as f32;
    for rb in 0..rows as usize {
        let y = area.y + rows - 1 - rb as u16;
        let db = scale.db_at((rb as f32 + 0.5) / rows as f32);
        let (fg, dim) = match bar.solid {
            Some(c) => (c, Color::Indexed(23)),
            None => scale.colors(db),
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
    if let Some((row, glyph)) = bar.hold.and_then(|h| edge_marker(scale.frac(h) * rows as f32, rows)) {
        for dx in 0..bw {
            buf[(x + dx, area.y + row)].set_char(glyph).set_fg(Color::White);
        }
    }
    let label_y = area.y + rows;
    let lx = x + bw.saturating_sub(bar.label.len() as u16) / 2;
    buf.set_string(lx, label_y, &bar.label, Style::default().fg(Color::Gray));
}

pub fn render(app: &mut App, area: Rect, buf: &mut Buffer) {
    let vu = app.display.vu;
    let block = Block::bordered().title(title(if vu { " Levels · VU " } else { " Levels " }));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height < 3 || inner.width < SCALE_W + BAR_W {
        return;
    }
    let history = &app.history;
    let shown = app.readout.pick(app.view.cursor, app.view.follow, std::time::Instant::now(), |i| history.get(i).is_some());
    let Some(f) = app.history.get(app.view.cursor) else {
        buf.set_string(inner.x + 1, inner.y, "waiting for audio…", Style::default().fg(Color::DarkGray));
        return;
    };
    let scale = match vu {
        true => Scale::Vu { reference: app.display.vu_ref },
        false => Scale::Db { lo: app.display.meter_floor, hi: 0.0 },
    };
    let n = f.n();

    let bars_area = Rect { height: inner.height - 1, ..inner };
    scale.draw_axis(buf, Rect { width: SCALE_W, ..bars_area });
    let mut bars: Vec<Bar> = (0..n)
        .map(|c| {
            let label = channel_label(c, n);
            match vu {
                true => Bar { label, fill: f.vu[c], envelope: f.vu[c], hold: Some(f.vu_hold[c]), solid: None, scale },
                false => Bar { label, fill: f.rms_avg[c], envelope: f.peak_decay[c], hold: Some(f.hold[c]), solid: None, scale },
            }
        })
        .collect();
    // Loudness shares the dBFS axis, but the VU scale is no loudness scale, so
    // in VU mode the LU bar gets its own EBU +9 axis.
    let lu_scale = if vu { Scale::Lu { target: LU_TARGET } } else { scale };
    let lu = Bar { label: "LU".into(), fill: f.lufs_m, envelope: f.lufs_s, hold: None, solid: Some(Color::Cyan), scale: lu_scale };
    bars.push(lu);
    let bw = bar_width(bars.len() as u16, inner.width, vu);
    let mut x = inner.x + SCALE_W;
    let bars_end = inner.x + inner.width;
    for b in &bars {
        if x + bw > bars_end {
            break;
        }
        draw_bar(buf, x, bw, bars_area, b);
        x += bw + 1;
        if b.scale != scale && x + LU_AXIS_W <= bars_end {
            b.scale.draw_axis(buf, Rect { x, width: LU_AXIS_W, ..bars_area });
            x += LU_AXIS_W;
        }
    }

    // Numeric readouts to the right of the bars, refreshed at a readable rate
    // while live, in fixed-width columns.
    let f = app.history.get(shown).unwrap_or(f);
    let clipped = app.history.clipped(shown, app.display.clip_frames);
    let tx = x + 1;
    if tx + 20 > bars_end {
        return;
    }
    let tw = (bars_end - tx) as usize;
    // Blank separator rows only when there is room for them.
    let needed = 1 + n + 1 + 2 + 1 + clipped as usize;
    let spacious = inner.height as usize >= needed + 2;
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
    let heads = if vu { ["vu", "hold", "peak", "avg"] } else { ["peak", "hold", "rms", "avg"] };
    line(buf, row(if vu { "VU" } else { "dBFS" }, &heads), head);
    for c in 0..n {
        let (vals, loudest) = match vu {
            true => ([f.vu[c], f.vu_hold[c], f.peak[c], f.rms_avg[c]], f.vu_hold[c]),
            false => ([f.peak[c], f.hold[c], f.rms[c], f.rms_avg[c]], f.peak[c].max(f.hold[c])),
        };
        let st = Style::default().fg(scale.color(loudest));
        line(buf, row(&channel_label(c, n), &vals.map(|v| scale.fmt(v))), st);
    }
    let s = &app.session;
    let clip_style = if s.clips > 0 { Style::default().fg(Color::LightRed) } else { head };
    line(buf, session_text(&scale.fmt(s.max_db), s.clips), clip_style);
    if spacious {
        line(buf, String::new(), val);
    }
    line(buf, row("LUFS", &["mom", "short", "integ", "max"]), head);
    line(buf, row("", &[f.lufs_m, f.lufs_s, f.lufs_i, s.max_lufs_m].map(fmt_db)), Style::default().fg(Color::Cyan));
    if spacious {
        line(buf, String::new(), val);
    }
    line(buf, tone_text(f), val);
    if clipped {
        line(buf, " CLIP ".into(), Style::default().fg(Color::White).bg(Color::Red).add_modifier(Modifier::BOLD));
    }
}

/// Session line: highest peak and clip events since the start (or `x`).
pub fn session_text(max: &str, clips: u64) -> String {
    let unit = if clips == 1 { "clip" } else { "clips" };
    format!("{} {:>COL_W$} {unit}", row("max", &[max]), clips)
}

/// Width of each readout column; values are right-aligned under their headers.
const COL_W: usize = 6;

/// A readout row: a 4-character label, then fixed-width right-aligned columns.
pub fn row<S: AsRef<str>>(label: &str, cols: &[S]) -> String {
    let mut s = format!("{label:<4}");
    for c in cols {
        s.push_str(&format!(" {:>COL_W$}", c.as_ref()));
    }
    s
}

/// Strongest tone and its nearest note, or a dash when nothing stands out.
fn tone_text(f: &Frame) -> String {
    match f.peak_freq {
        Some((hz, db)) if db > -100.0 => format!("tone {hz:>7.1} Hz {:>COL_W$} dB {}", fmt_db(db), note_name(hz)),
        _ => "tone —".into(),
    }
}

/// Nearest musical note with cents offset in a fixed width, e.g. "A 4 +00c",
/// "F#2 -41c", so the readout does not shift as the note changes.
pub fn note_name(hz: f32) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    if hz <= 0.0 || !hz.is_finite() {
        return "—".into();
    }
    let midi = 69.0 + 12.0 * (hz / 440.0).log2();
    let n = midi.round();
    let cents = ((midi - n) * 100.0).round() as i32;
    let ni = n as i32;
    format!("{:<2}{} {:+03}c", NAMES[ni.rem_euclid(12) as usize], ni.div_euclid(12) - 1, cents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vu_scale_is_linear_in_amplitude() {
        let s = Scale::Vu { reference: -18.0 };
        assert!((s.frac(-15.0) - 1.0).abs() < 1e-6, "+3 VU is full scale");
        assert!((s.frac(-18.0) - 0.7079).abs() < 1e-3, "0 VU at 71%");
        assert!((s.frac(-38.0) - 0.0708).abs() < 1e-3, "-20 VU at 7%");
        assert_eq!(s.frac(0.0), 1.0);
        assert_eq!(s.frac(f32::NEG_INFINITY), 0.0);
        for t in [0.05f32, 0.5, 0.9] {
            assert!((s.frac(s.db_at(t)) - t).abs() < 1e-5);
        }
        assert_eq!(s.fmt(-21.0), " -3.0");
        assert_eq!(s.fmt(f32::NEG_INFINITY), " -inf");
        // Traditional two-zone colours: green below 0 VU, red from 0 VU up.
        assert_eq!(s.color(-18.5), Color::Green);
        assert_eq!(s.color(-18.0), Color::Red);
        assert_eq!(s.color(-15.0), Color::Red);
        let d = Scale::Db { lo: -60.0, hi: 0.0 };
        assert_eq!(d.frac(-30.0), 0.5);
        assert_eq!(d.fmt(-3.0), " -3.0");
    }

    #[test]
    fn vu_axis_marks() {
        let s = Scale::Vu { reference: -18.0 };
        let mut buf = Buffer::empty(Rect::new(0, 0, SCALE_W, 20));
        s.draw_axis(&mut buf, Rect::new(0, 0, SCALE_W, 20));
        let label = |y: u16| (0..SCALE_W).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim().to_string();
        assert_eq!(label(0), "+3", "full scale on the top row");
        let labels: Vec<String> = (0..20).map(label).filter(|l| !l.is_empty()).collect();
        for m in ["+3", "+2", "+1", "0", "-1", "-3", "-5", "-10", "-20"] {
            assert!(labels.iter().any(|l| l == m), "{m} in {labels:?}");
        }
        // Top to bottom in order, and the red zone labelled in red.
        let nums: Vec<i32> = labels.iter().map(|l| l.parse().unwrap()).collect();
        assert!(nums.windows(2).all(|w| w[0] > w[1]), "{nums:?}");
        let zero = (0..20).find(|&y| label(y) == "0").unwrap();
        assert_eq!(buf[(SCALE_W - 2, zero)].fg, Color::Red);
        assert_eq!(buf[(SCALE_W - 2, (0..20).find(|&y| label(y) == "-3").unwrap())].fg, Color::DarkGray);
    }

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
        // Wide screens get two fifths; narrow ones what the readouts need.
        assert_eq!(width(1, 100), 51, "one column for the LU axis in VU mode");
        assert_eq!(width(1, 145), 58);
        assert_eq!(width(1, 300), 120);
        assert_eq!(width(8, 145), 78);
        // 80 columns: the stereo readouts fit, the equalizer keeps 26.
        assert_eq!(width(2, 80), preferred_width(2));
        assert_eq!(80 - width(2, 80), 26);
        // Too narrow for both: the equalizer keeps its minimum, or half.
        assert_eq!(width(2, 60), 36);
        assert_eq!(width(2, 40), 20);
        // Minimum layout keeps 3-column bars; spare room widens them up to 8.
        assert_eq!(bar_width(2, preferred_width(1) - 2, false), BAR_W);
        assert_eq!(bar_width(2, 56, false), 6);
        assert_eq!(bar_width(2, 200, false), MAX_BAR_W);
        assert_eq!(bar_width(9, 20, false), BAR_W);
    }

    #[test]
    fn readout_columns_align() {
        // Column i ends at the same character position in every row.
        let ends = |r: &str| -> Vec<usize> { (1..=4).map(|i| 4 + i * (1 + COL_W)).filter(|&e| e <= r.len()).collect() };
        let head = row("dBFS", &["peak", "hold", "rms", "avg"]);
        for vals in [["-48.7", "-37.8", "-57.8", "-58.3"], [" -inf", " -inf", "-113.3", "  0.0"]] {
            let r = row("M", &vals);
            assert_eq!(r.len(), head.len());
            for (e, (h, v)) in ends(&r).into_iter().zip(["peak", "hold", "rms", "avg"].iter().zip(vals)) {
                assert!(head[..e].ends_with(h), "{head:?} col {h}");
                assert!(r[..e].ends_with(v.trim()), "{r:?} col {v}");
            }
        }
        assert_eq!(row("LUFS", &["mom", "short", "integ"]).len(), row("", &["-60.9", "-59.7", "-55.7"]).len());
        assert_eq!(session_text(&fmt_db(-3.04), 0), "max    -3.0      0 clips");
        assert_eq!(session_text(&fmt_db(0.0), 1), "max     0.0      1 clip");
        assert_eq!(session_text(&Scale::Vu { reference: -18.0 }.fmt(-16.0), 0), "max    +2.0      0 clips");
        assert!(session_text(&fmt_db(-3.0), 0).len() <= TEXT_W as usize);
        // The frequency line keeps its layout as the value changes.
        assert_eq!(format!("{:>7.1}", 92.6f32).len(), format!("{:>7.1}", 10298.8f32).len());
    }

    #[test]
    fn notes() {
        assert_eq!(note_name(440.0), "A 4 +00c");
        assert_eq!(note_name(261.63), "C 4 +00c");
        assert_eq!(note_name(27.5), "A 0 +00c");
        assert_eq!(note_name(446.0), "A 4 +23c");
        assert_eq!(note_name(92.6), "F#2 +02c");
        assert_eq!(note_name(87.0), "F 2 -06c");
        // Same width across notes and cents.
        for hz in [30.0, 92.6, 87.0, 440.0, 1234.5, 9000.0] {
            assert_eq!(note_name(hz).chars().count(), 8, "{hz}");
        }
        assert_eq!(note_name(0.0), "—");
    }
}
