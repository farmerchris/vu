//! Level timeline: RMS bars with the peak envelope behind them, or in VU
//! mode the VU needle on the VU scale.

use super::meter::Scale;
use super::{cell_fill, eighth, split_axis, title};
use crate::app::App;
use crate::dsp::level::FLOOR_DB;
use crate::history::History;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::{Block, Widget};

/// Max peak and max RMS (over channels and frames) for frames `[a, b]`.
pub fn bucket_levels(h: &History, a: usize, b: usize) -> Option<(f32, f32)> {
    let (mut any, mut p, mut r) = (false, FLOOR_DB, FLOOR_DB);
    for f in h.range(a, b) {
        any = true;
        p = p.max(f.max_peak());
        r = r.max(f.max_rms());
    }
    any.then_some((p, r))
}

/// Highest VU needle reading (over channels and frames) for frames `[a, b]`.
pub fn bucket_vu(h: &History, a: usize, b: usize) -> Option<f32> {
    h.range(a, b).flat_map(|f| f.vu[..f.n()].iter().copied()).reduce(f32::max)
}

pub fn render(app: &mut App, area: Rect, buf: &mut Buffer) -> Rect {
    let v = &app.view;
    let d = &app.display;
    let what = if d.vu { "vu" } else { "peak / rms" };
    let block = Block::bordered().title(title(format!(" Timeline  {what} · {} ", super::view_range_text(app))));
    let inner = block.inner(area);
    block.render(area, buf);
    let (axis, chart) = split_axis(inner);
    if chart.width == 0 || chart.height == 0 {
        return chart;
    }
    let scale = if d.vu { Scale::Vu { reference: d.vu_ref } } else { Scale::Db { lo: d.db_min, hi: d.db_max } };
    scale.draw_axis(buf, axis);
    let rows = chart.height;
    let height = |db: f32| scale.frac(db) * rows as f32;
    // Row colours (bar, dim) from the bottom up.
    let colors: Vec<(Color, Color)> =
        (0..rows).map(|r| scale.colors(scale.db_at((r as f32 + 0.5) / rows as f32))).collect();
    let first = app.history.first();
    let cursor_col = v.cursor_column();
    for c in 0..chart.width as usize {
        let x = chart.x + c as u16;
        let is_cursor = cursor_col == Some(c);
        let frames = v.column_frames(c).filter(|&(_, hi)| hi >= first);
        // VU mode draws the needle alone: peaks mostly sit above the top of
        // the VU scale, so a peak layer would fill it.
        let (hp, hr) = match frames {
            Some((a, b)) if d.vu => bucket_vu(&app.history, a, b).map_or((0.0, 0.0), |u| (height(u), height(u))),
            Some((a, b)) => bucket_levels(&app.history, a, b).map_or((0.0, 0.0), |(p, r)| (height(p), height(r))),
            None => (0.0, 0.0),
        };
        for rb in 0..rows as usize {
            let y = chart.y + rows - 1 - rb as u16;
            let (bar, dim) = colors[rb];
            let (fr, fp) = (cell_fill(hr, rb), cell_fill(hp, rb));
            let cell = &mut buf[(x, y)];
            if fr >= 1.0 {
                cell.set_char('█').set_fg(bar);
            } else if fr > 0.0 {
                cell.set_char(eighth(fr)).set_fg(bar);
                if fp >= 1.0 {
                    cell.set_bg(dim);
                }
            } else if fp > 0.0 {
                cell.set_char(eighth(fp)).set_fg(dim);
            } else if is_cursor {
                cell.set_char('│');
            }
            if is_cursor {
                cell.set_fg(Color::White);
            }
        }
    }
    super::draw_time_axis(buf, app, area, chart);
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
        assert_eq!(bucket_vu(&h, 0, 2), Some(-4.0));
        assert_eq!(bucket_vu(&h, 5, 9), None);
    }
}
