//! Graphic equalizer: one bar per band at the cursor frame, with peak caps.

use super::colormap::{eq_gradient, to_color};
use super::{cell_fill, db_row, db_to_height, draw_db_axis, eighth, fmt_freq, title};
use crate::app::App;
use crate::dsp::quant::q_to_db;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Widget};

/// Bar geometry: `(bars, bar_width, gap)` for `bands` bands in `width` columns.
pub fn bar_layout(bands: usize, width: u16) -> (usize, u16, u16) {
    let w = width as usize;
    if bands == 0 || w == 0 {
        return (0, 0, 0);
    }
    if w < bands {
        return (w, 1, 0);
    }
    let per = w / bands;
    let gap = if per >= 3 { 1 } else { 0 };
    (bands, (per - gap as usize) as u16, gap)
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
    let bands = app.analysis.band_centers.len();
    let block = Block::bordered().title(title(format!(
        " Equalizer  {} bands · {}-{} Hz ",
        bands,
        fmt_freq(app.analysis.min_freq),
        fmt_freq(app.analysis.max_freq)
    )));
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
    let (bars, bw, gap) = bar_layout(bands, chart.width);
    let used = bars as u16 * (bw + gap);
    let x0 = chart.x + (chart.width - used.min(chart.width)) / 2;
    let rows = chart.height;
    let mode = app.display.color_mode;
    let colors: Vec<Color> =
        (0..rows).map(|rb| to_color(mode, eq_gradient((rb as f32 + 0.5) / rows as f32))).collect();
    let mut next_label_x = 0u16;
    for i in 0..bars {
        let b = bar_band(i, bars, bands);
        let x = x0 + i as u16 * (bw + gap);
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
        if let Some(&cap) = caps.get(b) {
            let cdb = q_to_db(cap);
            if cdb > lo {
                let y = chart.y + db_row(cdb, lo, hi, rows);
                for dx in 0..bw {
                    let cell = &mut buf[(x + dx, y)];
                    if cell.symbol() == " " {
                        cell.set_char('▔').set_fg(Color::Gray);
                    }
                }
            }
        }
        // Frequency labels, spaced so they never overlap.
        let label = fmt_freq(app.analysis.band_centers[b]);
        let lw = label.len() as u16;
        if x >= next_label_x && x + lw <= inner.x + inner.width {
            buf.set_string(x, chart.y + rows, &label, Style::default().fg(Color::DarkGray));
            next_label_x = x + lw + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_fits_width() {
        assert_eq!(bar_layout(31, 124), (31, 3, 1));
        assert_eq!(bar_layout(31, 62), (31, 2, 0));
        assert_eq!(bar_layout(31, 20), (20, 1, 0));
        assert_eq!(bar_layout(0, 20), (0, 0, 0));
        assert_eq!(bar_layout(10, 0), (0, 0, 0));
        for (bands, w) in [(31, 200), (8, 33), (64, 64), (100, 50)] {
            let (n, bw, gap) = bar_layout(bands, w);
            assert!(n as u16 * (bw + gap) <= w);
            assert!(bw >= 1);
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
