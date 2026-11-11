//! Scope panel for the frame under the cursor. With two channels or more it
//! is a vectorscope (goniometer) of the first two: each sample pair is a dot,
//! mid (L+R) up and side (R-L) across, so mono draws a vertical line, a wide
//! mix a round cloud and out-of-phase audio a horizontal one, over a phase
//! correlation meter. With one channel it is an oscilloscope. Both are
//! scaled to the signal, so quiet inputs still fill the panel; the title
//! gives the scale.

use super::title;
use crate::analyzer::{Frame, SCOPE_POINTS};
use crate::app::App;
use crate::dsp::level::amp_to_db;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Widget};

/// Frames drawn together, the cursor's and those just before it (about
/// 85 ms at the default hop): enough dots for the shape to read.
pub const TRAIL_FRAMES: usize = 4;
/// Samples the oscilloscope shows: half of those kept, so a trigger point
/// can be found in the first half.
pub const WAVE_POINTS: usize = SCOPE_POINTS;
/// Narrowest scope worth drawing beside the other top panels, in cells.
pub const MIN_W: u16 = 14;

const TRACE: Color = Color::Indexed(114);
const GRID: Color = Color::Indexed(238);

/// What the scope shows for a stretch of frames.
#[derive(Clone, Debug, PartialEq)]
pub enum Trace {
    /// Vectorscope dots `(side, mid)`, each within -1..=1.
    Vector(Vec<(f32, f32)>),
    /// Oscilloscope samples within -1..=1, starting at a rising zero
    /// crossing when there is one.
    Wave(Vec<f32>),
}

/// The trace for `frames` (oldest first) and the amplitude (linear) that
/// reaches the edge of the plot. `None` when there is nothing to draw.
pub fn trace<'a>(frames: impl Iterator<Item = &'a Frame>, stereo: bool) -> Option<(Trace, f32)> {
    let frames: Vec<&Frame> = frames.filter(|f| !f.scope.is_empty()).collect();
    let scale = frames.iter().map(|f| f.scope_scale).fold(0.0, f32::max);
    if scale <= 0.0 {
        return None;
    }
    // A value of `v` in frame `f`, as a fraction of the common scale.
    let unit = |f: &Frame, v: i8| v as f32 / 127.0 * f.scope_scale / scale;
    if stereo {
        let dots = frames
            .iter()
            .flat_map(|f| {
                f.scope.chunks_exact(2).map(move |p| {
                    let (l, r) = (unit(f, p[0]), unit(f, p[1]));
                    ((r - l) / 2.0, (l + r) / 2.0)
                })
            })
            .collect();
        return Some((Trace::Vector(dots), scale));
    }
    // The newest frame alone: a waveform does not overlay like dots do.
    let f = frames.last()?;
    let scale = f.scope_scale;
    if scale <= 0.0 {
        return None;
    }
    let n = WAVE_POINTS.min(f.scope.len());
    let start = (1..=f.scope.len() - n)
        .find(|&i| f.scope[i - 1] < 0 && f.scope[i] >= 0)
        .unwrap_or(0);
    let wave = f.scope[start..start + n]
        .iter()
        .map(|&v| v as f32 / 127.0)
        .collect();
    Some((Trace::Wave(wave), scale))
}

/// A grid of braille dots, 2 across and 4 down per cell.
pub struct Dots {
    pub w: usize,
    pub h: usize,
    bits: Vec<u8>,
}

impl Dots {
    pub fn new(cells_w: u16, cells_h: u16) -> Self {
        Dots {
            w: cells_w as usize * 2,
            h: cells_h as usize * 4,
            bits: vec![0; cells_w as usize * cells_h as usize],
        }
    }

    pub fn set(&mut self, x: usize, y: usize) {
        if x >= self.w || y >= self.h {
            return;
        }
        // Braille dot numbering: the left column is bits 0, 1, 2, 6 going
        // down, the right 3, 4, 5, 7.
        const BIT: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
        self.bits[(y / 4) * (self.w / 2) + x / 2] |= BIT[x % 2][y % 4];
    }

    /// A vertical run of dots from `y0` to `y1`, either way up.
    pub fn vline(&mut self, x: usize, y0: usize, y1: usize) {
        for y in y0.min(y1)..=y0.max(y1) {
            self.set(x, y);
        }
    }

    /// The glyph of cell `(cx, cy)`, if any of its dots is set.
    pub fn cell(&self, cx: usize, cy: usize) -> Option<char> {
        match self.bits[cy * (self.w / 2) + cx] {
            0 => None,
            b => char::from_u32(0x2800 + b as u32),
        }
    }

    fn draw(&self, buf: &mut Buffer, at: Rect, color: Color) {
        for cy in 0..self.h / 4 {
            for cx in 0..self.w / 2 {
                if let Some(ch) = self.cell(cx, cy) {
                    buf[(at.x + cx as u16, at.y + cy as u16)]
                        .set_char(ch)
                        .set_fg(color);
                }
            }
        }
    }
}

/// Dot position of `v` (-1..=1) along an axis of `n` dots.
fn dot(v: f32, n: usize) -> usize {
    (((v.clamp(-1.0, 1.0) + 1.0) / 2.0) * (n - 1) as f32).round() as usize
}

/// Title text for the stereo readings: correlation and balance.
pub fn stereo_summary(f: &Frame) -> String {
    let corr = if f.correlation.is_nan() {
        "—".to_string()
    } else {
        format!("{:+.2}", f.correlation)
    };
    format!("corr {corr} · {}", balance_text(f.balance()))
}

/// `bal R 1.2 dB`: which side is louder and by how much.
pub fn balance_text(balance: Option<f32>) -> String {
    match balance {
        None => "bal —".into(),
        Some(b) if b.abs() < 0.05 => "bal 0.0 dB".into(),
        Some(b) => format!("bal {} {:.1} dB", if b > 0.0 { 'R' } else { 'L' }, b.abs()),
    }
}

/// Colour of a correlation reading: red when out of phase (it would cancel
/// in mono), yellow when barely related, green otherwise.
pub fn correlation_color(c: f32) -> Color {
    if c < -0.05 {
        Color::LightRed
    } else if c < 0.3 {
        Color::Yellow
    } else {
        Color::Green
    }
}

pub fn render(app: &mut App, area: Rect, buf: &mut Buffer) {
    let cursor = app.view.cursor;
    let frame = app.history.get(cursor);
    let stereo = app.analysis.channels >= 2;
    let shown = trace(
        app.history
            .range(cursor.saturating_sub(TRAIL_FRAMES - 1), cursor),
        stereo,
    );
    let heading = match (&shown, frame) {
        (Some(_), Some(f)) if stereo => format!(" Scope  {} ", stereo_summary(f)),
        (Some((_, scale)), _) => format!(" Scope  {:.0} dB ", amp_to_db(*scale)),
        _ => " Scope ".to_string(),
    };
    let block = Block::bordered().title(title(heading));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height < 3 || inner.width < 8 {
        return;
    }
    let dim = Style::default().fg(Color::DarkGray);
    // The correlation meter takes the bottom row of a stereo scope.
    let plot = Rect {
        height: inner.height - stereo as u16,
        ..inner
    };
    let mut dots = Dots::new(plot.width, plot.height);
    match &shown {
        Some((Trace::Vector(points), _)) => {
            // Square in dots (which are about square on screen), centred.
            let side = dots.w.min(dots.h);
            let (ox, oy) = ((dots.w - side) / 2, (dots.h - side) / 2);
            draw_cross(buf, plot);
            for &(x, y) in points {
                dots.set(ox + dot(x, side), oy + dot(-y, side));
            }
            buf.set_string(plot.x, plot.y, "L", dim);
            buf.set_string(plot.x + plot.width - 1, plot.y, "R", dim);
        }
        Some((Trace::Wave(wave), _)) => {
            draw_cross(buf, plot);
            let mut prev = None;
            for x in 0..dots.w {
                let i = x * (wave.len() - 1) / (dots.w - 1).max(1);
                let y = dot(-wave[i], dots.h);
                dots.vline(x, prev.unwrap_or(y), y);
                prev = Some(y);
            }
        }
        None => {
            let msg = if frame.is_some() {
                "silence"
            } else {
                "waiting for audio…"
            };
            buf.set_string(plot.x + 1, plot.y, msg, dim);
        }
    }
    dots.draw(buf, plot, TRACE);
    if stereo {
        let corr = frame.map_or(f32::NAN, |f| f.correlation);
        draw_correlation(buf, inner, corr);
    }
}

/// Faint centre lines behind the trace.
fn draw_cross(buf: &mut Buffer, plot: Rect) {
    let (cx, cy) = (plot.x + plot.width / 2, plot.y + plot.height / 2);
    for x in plot.x..plot.x + plot.width {
        buf[(x, cy)].set_char('─').set_fg(GRID);
    }
    for y in plot.y..plot.y + plot.height {
        buf[(cx, y)]
            .set_char(if y == cy { '┼' } else { '│' })
            .set_fg(GRID);
    }
}

/// The phase correlation meter along the bottom row: `-1 ────┼──●─ +1`.
fn draw_correlation(buf: &mut Buffer, inner: Rect, corr: f32) {
    let y = inner.y + inner.height - 1;
    let dim = Style::default().fg(Color::DarkGray);
    if inner.width < 12 {
        return;
    }
    buf.set_string(inner.x, y, "-1", dim);
    buf.set_string(inner.x + inner.width - 2, y, "+1", dim);
    let (x0, w) = (inner.x + 3, inner.width - 6);
    for i in 0..w {
        buf[(x0 + i, y)]
            .set_char(if i == w / 2 { '┼' } else { '─' })
            .set_fg(GRID);
    }
    if let Some(col) = correlation_column(corr, w) {
        buf[(x0 + col, y)]
            .set_char('●')
            .set_fg(correlation_color(corr));
    }
}

/// Column of the correlation marker along a bar of `w` cells whose middle
/// (`w / 2`) is 0; `None` when there is no reading.
pub fn correlation_column(corr: f32, w: u16) -> Option<u16> {
    if corr.is_nan() || w == 0 {
        return None;
    }
    let mid = (w / 2) as f32;
    let reach = if corr < 0.0 {
        mid
    } else {
        (w - 1) as f32 - mid
    };
    Some((mid + corr.clamp(-1.0, 1.0) * reach).round() as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(scope: &[i8], scale: f32) -> Frame {
        Frame {
            channels: 2,
            scope: scope.into(),
            scope_scale: scale,
            ..Frame::default()
        }
    }

    #[test]
    fn vector_dots() {
        // In phase: on the vertical (mid) axis; left only: up and to the left.
        let f = frame(&[127, 127, -127, -127, 127, 0, 0, 0], 0.5);
        let (t, scale) = trace([&f].into_iter(), true).unwrap();
        assert_eq!(scale, 0.5);
        let Trace::Vector(d) = t else { panic!() };
        assert_eq!(d, [(0.0, 1.0), (0.0, -1.0), (-0.5, 0.5), (0.0, 0.0)]);
        // Out of phase: on the horizontal (side) axis.
        let f = frame(&[127, -127], 1.0);
        let Some((Trace::Vector(d), _)) = trace([&f].into_iter(), true) else {
            panic!()
        };
        assert_eq!(d, [(-1.0, 0.0)]);
        // Frames share the louder one's scale, so a quieter frame is smaller.
        let (a, b) = (frame(&[127, 127], 0.25), frame(&[127, 127], 0.5));
        let Some((Trace::Vector(d), scale)) = trace([&a, &b].into_iter(), true) else {
            panic!()
        };
        assert_eq!((scale, d), (0.5, vec![(0.0, 0.5), (0.0, 1.0)]));
        // Nothing to show for silence or no frames.
        assert_eq!(trace([&frame(&[0, 0], 0.0)].into_iter(), true), None);
        assert_eq!(trace(std::iter::empty(), true), None);
    }

    #[test]
    fn wave_starts_on_a_rising_crossing() {
        // Three cycles over the kept samples, starting mid-cycle.
        let n = 2 * SCOPE_POINTS;
        let scope: Vec<i8> = (0..n)
            .map(|i| {
                let p = 2.0 * std::f32::consts::PI * 3.0 * i as f32 / n as f32 + 2.0;
                (127.0 * p.sin()) as i8
            })
            .collect();
        let f = frame(&scope, 0.1);
        let Some((Trace::Wave(w), scale)) = trace([&f].into_iter(), false) else {
            panic!()
        };
        assert_eq!((w.len(), scale), (WAVE_POINTS, 0.1));
        assert!(w[0] >= 0.0 && w[0] < 0.2 && w[5] > w[0], "{:?}", &w[..6]);
        // No crossing (DC): from the start.
        let f = frame(&vec![64; n], 0.1);
        let Some((Trace::Wave(w), _)) = trace([&f].into_iter(), false) else {
            panic!()
        };
        assert!(w.iter().all(|&v| (v - 64.0 / 127.0).abs() < 1e-6));
    }

    #[test]
    fn braille_dots() {
        let mut d = Dots::new(2, 1);
        assert_eq!((d.w, d.h), (4, 4));
        assert_eq!(d.cell(0, 0), None);
        d.set(0, 0);
        assert_eq!(d.cell(0, 0), Some('⠁'));
        d.set(1, 3);
        assert_eq!(d.cell(0, 0), Some('⢁'));
        d.vline(2, 3, 0);
        assert_eq!(d.cell(1, 0), Some('⡇'));
        d.set(99, 99);
        assert_eq!(dot(-1.0, 8), 0);
        assert_eq!(dot(1.0, 8), 7);
        assert_eq!(dot(5.0, 8), 7);
    }

    #[test]
    fn correlation_marker_and_text() {
        assert_eq!(correlation_column(0.0, 21), Some(10));
        assert_eq!(correlation_column(1.0, 21), Some(20));
        assert_eq!(correlation_column(-1.0, 21), Some(0));
        assert_eq!(correlation_column(-1.0, 20), Some(0));
        assert_eq!(correlation_column(1.0, 20), Some(19));
        assert_eq!(correlation_column(f32::NAN, 20), None);
        assert_eq!(correlation_color(-0.5), Color::LightRed);
        assert_eq!(correlation_color(0.1), Color::Yellow);
        assert_eq!(correlation_color(0.9), Color::Green);
        assert_eq!(balance_text(Some(1.24)), "bal R 1.2 dB");
        assert_eq!(balance_text(Some(-6.0)), "bal L 6.0 dB");
        assert_eq!(balance_text(Some(0.01)), "bal 0.0 dB");
        assert_eq!(balance_text(None), "bal —");
        let mut f = frame(&[], 0.0);
        assert_eq!(stereo_summary(&f), "corr — · bal —");
        f.correlation = -0.25;
        (f.rms_avg[0], f.rms_avg[1]) = (-20.0, -18.0);
        assert_eq!(stereo_summary(&f), "corr -0.25 · bal R 2.0 dB");
    }
}
