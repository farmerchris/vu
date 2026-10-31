//! Graphic equalizer: one bar per band at the cursor frame, with peak caps.

use super::colormap::{eq_gradient, to_color};
use super::{cell_fill, db_to_height, draw_db_axis, edge_marker, eighth, fmt_freq, title};
use crate::app::App;
use crate::dsp::quant::q_to_db;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Widget};

/// Number of bars and the gap between them for `bands` bands in `width` columns.
pub fn bar_layout(bands: usize, width: u16) -> (usize, u16) {
    let w = width as usize;
    if bands == 0 || w == 0 {
        return (0, 0);
    }
    if w < bands {
        return (w, 0);
    }
    (bands, if w / bands >= 3 { 1 } else { 0 })
}

/// Column offset and width of bar `i` of `bars` spread across `width` columns,
/// so the bars fill the whole width (widths differ by at most one column).
pub fn bar_span(i: usize, bars: usize, width: u16, gap: u16) -> (u16, u16) {
    let edge = |j: usize| (j * width as usize / bars.max(1)) as u16;
    let (a, b) = (edge(i), edge(i + 1));
    (a, (b - a).saturating_sub(gap).max(1))
}

/// Band shown by bar `i` of `bars` when bands are resampled to fit.
pub fn bar_band(i: usize, bars: usize, bands: usize) -> usize {
    if bars == 0 {
        0
    } else {
        (i * bands / bars).min(bands.saturating_sub(1))
    }
}

pub fn render(app: &mut App, area: Rect, buf: &mut Buffer) {
    let a = &app.analysis;
    let bands = a.band_centers.len();
    let kind = if a.third_octave { "1/3 octave".to_string() } else { format!("{bands} bands") };
    let full = format!(" Equalizer  {kind} · {}-{} Hz ", fmt_freq(a.min_freq), fmt_freq(a.max_freq));
    let heading = if full.chars().count() + 2 <= area.width as usize { full } else { " Equalizer ".to_string() };
    let block = Block::bordered().title(title(heading));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height < 3 || inner.width < 8 {
        return;
    }
    let Some(f) = app.history.get(app.view.cursor) else { return };
    let (lo, hi) = (app.display.db_min, app.display.db_max);
    let axis = Rect { width: 5, height: inner.height - 1, ..inner };
    let chart = Rect { x: inner.x + 5, width: inner.width - 5, height: inner.height - 1, ..inner };
    draw_db_axis(buf, axis, lo, hi);
    app.history.band_hold(app.view.cursor, app.display.hold_frames, &mut app.band_hold_buf);
    let caps = &app.band_hold_buf;
    let (bars, gap) = bar_layout(bands, chart.width);
    let rows = chart.height;
    let mode = app.display.color_mode;
    let colors: Vec<Color> =
        (0..rows).map(|rb| to_color(mode, eq_gradient((rb as f32 + 0.5) / rows as f32))).collect();
    for i in 0..bars {
        let b = bar_band(i, bars, bands);
        let (dx0, bw) = bar_span(i, bars, chart.width, gap);
        let x = chart.x + dx0;
        let h = db_to_height(q_to_db(f.bands[b]), lo, hi, rows);
        for rb in 0..rows as usize {
            let fill = cell_fill(h, rb);
            if fill <= 0.0 {
                continue;
            }
            let y = chart.y + rows - 1 - rb as u16;
            for dx in 0..bw {
                buf[(x + dx, y)].set_char(eighth(fill)).set_fg(colors[rb]);
            }
        }
        if let Some((row, glyph)) = caps.get(b).and_then(|&cap| edge_marker(q_to_db(cap), lo, hi, rows)) {
            for dx in 0..bw {
                let cell = &mut buf[(x + dx, chart.y + row)];
                if cell.symbol() == " " {
                    cell.set_char(glyph).set_fg(Color::Gray);
                }
            }
        }
    }
    draw_band_labels(app, buf, chart, bars, gap, inner.x + inner.width);
}

/// Frequency labels centred under their bars. Every band is labelled when the
/// labels fit; otherwise only the major bands (octave centres for 1/3-octave
/// bands), still never overlapping.
fn draw_band_labels(app: &App, buf: &mut Buffer, chart: Rect, bars: usize, gap: u16, right: u16) {
    let a = &app.analysis;
    let bands = a.band_labels.len();
    let y = chart.y + chart.height;
    let place = |i: usize| {
        let b = bar_band(i, bars, bands);
        let (dx, bw) = bar_span(i, bars, chart.width, gap);
        let lw = a.band_labels[b].chars().count() as u16;
        let x = (chart.x + dx + bw / 2).saturating_sub(lw / 2).max(chart.x);
        (b, x, lw)
    };
    let all_fit = bars == bands && {
        let mut free = 0u16;
        (0..bars).all(|i| {
            let (_, x, lw) = place(i);
            let ok = x >= free && x + lw <= right;
            free = x + lw + 1;
            ok
        })
    };
    let style = Style::default().fg(Color::DarkGray);
    let mut free = 0u16;
    for i in 0..bars {
        let (b, x, lw) = place(i);
        if (all_fit || a.band_major[b]) && x >= free && x + lw <= right {
            buf.set_string(x, y, &a.band_labels[b], style);
            free = x + lw + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_fits_width() {
        assert_eq!(bar_layout(31, 124), (31, 1));
        assert_eq!(bar_layout(31, 62), (31, 0));
        assert_eq!(bar_layout(31, 20), (20, 0));
        assert_eq!(bar_layout(0, 20), (0, 0));
        assert_eq!(bar_layout(10, 0), (0, 0));
    }

    #[test]
    fn bars_fill_the_width() {
        for (bands, w) in [(31, 89), (31, 200), (8, 33), (64, 64), (100, 50), (31, 31)] {
            let (n, gap) = bar_layout(bands, w);
            let spans: Vec<_> = (0..n).map(|i| bar_span(i, n, w, gap)).collect();
            assert_eq!(spans[0].0, 0);
            let (x, bw) = spans[n - 1];
            assert_eq!(x + bw + gap, w, "{bands} bands in {w}");
            for p in spans.windows(2) {
                assert!(p[0].0 + p[0].1 + gap == p[1].0, "contiguous");
                assert!(p[0].1.abs_diff(p[1].1) <= 1);
                assert!(p[0].1 >= 1);
            }
        }
    }

    #[test]
    fn band_resampling() {
        assert_eq!(bar_band(0, 10, 31), 0);
        assert_eq!(bar_band(9, 10, 31), 27);
        assert_eq!(bar_band(5, 5, 5), 4);
        assert_eq!(bar_band(0, 0, 5), 0);
    }
}
