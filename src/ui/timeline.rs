//! Level timeline in the meter mode: RMS bars with the peak envelope behind
//! them (RMS), the VU needle (VU), or momentary loudness (LUFS), each on the
//! meter's scale and in its colours.

use super::colormap::{LU_M_COLOR, LU_OVER_COLOR, LU_TARGET_LINE};
use super::meter::{mode_scale, target_marker, Scale};
use super::{cell_fill, eighth, split_axis, title};
use crate::app::App;
use crate::app::MeterMode;
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

/// Highest momentary loudness for frames `[a, b]`.
pub fn bucket_momentary(h: &History, a: usize, b: usize) -> Option<f32> {
    h.range(a, b).map(|f| f.lufs_m).reduce(f32::max)
}

/// Highest VU needle reading (over channels and frames) for frames `[a, b]`.
pub fn bucket_vu(h: &History, a: usize, b: usize) -> Option<f32> {
    h.range(a, b)
        .flat_map(|f| f.vu[..f.n()].iter().copied())
        .reduce(f32::max)
}

pub fn render(app: &mut App, area: Rect, buf: &mut Buffer) -> Rect {
    let v = &app.view;
    let d = &app.display;
    let range = super::view_range_text(app);
    let heading = match d.mode {
        MeterMode::Rms => title(format!(" Timeline  peak / rms · {range} ")),
        MeterMode::Vu => title(format!(" Timeline  vu · {range} ")),
        MeterMode::Lufs => title(format!(" Timeline  momentary loudness · {range} ")),
    };
    let block = Block::bordered().title(heading);
    let inner = block.inner(area);
    block.render(area, buf);
    let (axis, chart) = split_axis(inner);
    if chart.width == 0 || chart.height == 0 {
        return chart;
    }
    // RMS keeps the charts' own dB range ([ ] { }); VU and LUFS are fixed scales.
    let scale = match d.mode {
        MeterMode::Rms => Scale::Db {
            lo: d.db_min,
            hi: d.db_max,
        },
        _ => mode_scale(app),
    };
    scale.draw_axis(buf, axis);
    let rows = chart.height;
    let height = |db: f32| scale.frac(db) * rows as f32;
    // Row colours from the bottom up: the lower layer, the band above it, and
    // the band when short-term is the higher (only LUFS mode tells them apart:
    // both, momentary alone, short-term alone; above the target, bright yellow
    // for both and yellow for either alone).
    let colors: Vec<(Color, Color)> = (0..rows)
        .map(|r| {
            let db = scale.db_at((r as f32 + 0.5) / rows as f32);
            match scale {
                Scale::Lu { target } if db >= target => (LU_OVER_COLOR, LU_OVER_COLOR),
                Scale::Lu { .. } => (LU_M_COLOR, LU_M_COLOR),
                _ => scale.colors(db),
            }
        })
        .collect();
    let target = target_marker(scale, rows);
    let first = app.history.first();
    let cursor_col = v.cursor_column();
    for c in 0..chart.width as usize {
        let x = chart.x + c as u16;
        let is_cursor = cursor_col == Some(c);
        let frames = v.column_frames(c).filter(|&(_, hi)| hi >= first);
        // RMS in front of the peak envelope. VU and LUFS draw one level alone:
        // the needle (peaks mostly sit above the top of the VU scale, so a peak
        // layer would fill it) and momentary loudness, as on the M bar.
        let (hp, hr) = match (frames, d.mode) {
            (Some((a, b)), MeterMode::Rms) => bucket_levels(&app.history, a, b)
                .map_or((0.0, 0.0), |(p, r)| (height(p), height(r + d.rms_offset))),
            (Some((a, b)), MeterMode::Vu) => {
                bucket_vu(&app.history, a, b).map_or((0.0, 0.0), |u| (height(u), height(u)))
            }
            (Some((a, b)), MeterMode::Lufs) => {
                bucket_momentary(&app.history, a, b).map_or((0.0, 0.0), |m| (height(m), height(m)))
            }
            (None, _) => (0.0, 0.0),
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
            } else if let Some((_, glyph)) =
                target.filter(|&(row, _)| row as usize == rows as usize - 1 - rb)
            {
                cell.set_char(glyph).set_fg(LU_TARGET_LINE);
            }
            if is_cursor {
                cell.set_fg(Color::White);
            }
        }
    }
    super::draw_markers(buf, app, chart);
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
        let mut f = frame(-4.0, &[]);
        f.lufs_m = -20.0;
        h.push(f);
        let mut f = frame(-4.0, &[]);
        f.lufs_m = -30.0;
        h.push(f);
        assert_eq!(bucket_momentary(&h, 3, 4), Some(-20.0));
        assert_eq!(bucket_momentary(&h, 7, 9), None);
    }
}
