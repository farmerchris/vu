//! Level timeline: RMS bars with the peak envelope behind them.

use super::colormap::{zone_color, zone_color_dim};
use super::{cell_fill, db_to_height, draw_db_axis, eighth, split_axis, title};
use crate::app::App;
use crate::dsp::level::FLOOR_DB;
use crate::history::History;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::{Block, Widget};

/// Max peak and max RMS (over channels and frames) for frames `[a, b]`.
pub fn bucket_levels(h: &History, a: usize, b: usize) -> Option<(f32, f32)> {
    let mut it = h.range(a, b).peekable();
    it.peek()?;
    Some(it.fold((FLOOR_DB, FLOOR_DB), |(p, r), f| (p.max(f.max_peak()), r.max(f.max_rms()))))
}

pub fn render(app: &mut App, area: Rect, buf: &mut Buffer) -> Rect {
    let v = &app.view;
    let d = &app.display;
    let ms_per_col = app.analysis.frame_dt * 1000.0 * v.zoom as f64;
    let block = Block::bordered().title(title(format!(
        " Timeline  peak / rms · {:.1} ms/col · {} visible ",
        ms_per_col,
        super::fmt_time(ms_per_col / 1000.0 * v.width as f64)
    )));
    let inner = block.inner(area);
    block.render(area, buf);
    let (axis, chart) = split_axis(inner);
    if chart.width == 0 || chart.height == 0 {
        return chart;
    }
    let (lo, hi) = (d.db_min, d.db_max);
    draw_db_axis(buf, axis, lo, hi);
    let rows = chart.height;
    let row_db = |r: usize| lo + (hi - lo) * (r as f32 + 0.5) / rows as f32;
    let first = app.history.first();
    let cursor_col = v.cursor_column();
    for c in 0..chart.width as usize {
        let x = chart.x + c as u16;
        let is_cursor = cursor_col == Some(c);
        let levels = v
            .column_frame(c)
            .filter(|&f| f >= first)
            .and_then(|fe| bucket_levels(&app.history, (fe + 1).saturating_sub(v.zoom), fe));
        let (hp, hr) = match levels {
            Some((p, r)) => (db_to_height(p, lo, hi, rows), db_to_height(r, lo, hi, rows)),
            None => (0.0, 0.0),
        };
        for rb in 0..rows as usize {
            let y = chart.y + rows - 1 - rb as u16;
            let db = row_db(rb);
            let (fr, fp) = (cell_fill(hr, rb), cell_fill(hp, rb));
            let cell = &mut buf[(x, y)];
            if fr >= 1.0 {
                cell.set_char('█').set_fg(zone_color(db));
            } else if fr > 0.0 {
                cell.set_char(eighth(fr)).set_fg(zone_color(db));
                if fp >= 1.0 {
                    cell.set_bg(zone_color_dim(db));
                }
            } else if fp > 0.0 {
                cell.set_char(eighth(fp)).set_fg(zone_color_dim(db));
            } else if is_cursor {
                cell.set_char('│');
            }
            if is_cursor {
                cell.set_fg(Color::White);
            }
        }
    }
    chart
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::tests::frame;

    #[test]
    fn bucket_maxima() {
        let mut h = History::new(10);
        assert_eq!(bucket_levels(&h, 0, 5), None);
        h.push(frame(-10.0, &[]));
        h.push(frame(-4.0, &[]));
        h.push(frame(-30.0, &[]));
        assert_eq!(bucket_levels(&h, 0, 2), Some((-4.0, -4.0)));
        assert_eq!(bucket_levels(&h, 2, 2), Some((-30.0, -30.0)));
        assert_eq!(bucket_levels(&h, 5, 9), None);
    }
}
