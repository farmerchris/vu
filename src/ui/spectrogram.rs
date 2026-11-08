//! Scrolling spectrogram. Each cell holds two pixels (upper-half block with
//! fg = top, bg = bottom). Pixel-to-bin ranges, the colour LUT and each visible
//! frame's folded pixel column are cached, so a draw only folds new frames.

use super::colormap::{build_lut, to_color, ColorMap, ColorMode};
use super::{fmt_freq, split_axis, title};
use crate::analyzer::Frame;
use crate::app::App;
use crate::dsp::scale::{pixel_bin_ranges, to_unit, FreqScale};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Widget};

#[derive(Default)]
pub struct SpecCache {
    range_key: Option<(usize, FreqScale, u32, u32, usize)>,
    ranges: Vec<(u32, u32)>,
    lut_key: Option<(ColorMap, ColorMode, u32, u32)>,
    pub lut: Vec<Color>,
    col: Vec<u8>,
    pixels: PixelCache,
}

/// Folded pixel columns of recent frames in a ring addressed by frame index.
#[derive(Default)]
struct PixelCache {
    key: Option<(usize, FreqScale, u32, u32, usize)>,
    pixels: usize,
    /// Frame index and time bits held by each slot.
    tags: Vec<(usize, u64)>,
    data: Vec<u8>,
}

impl PixelCache {
    /// Make room for `slots` frames of `pixels` each, dropping everything on a change.
    fn prepare(&mut self, key: (usize, FreqScale, u32, u32, usize), pixels: usize, slots: usize) {
        if self.key != Some(key) || self.pixels != pixels || self.tags.len() < slots {
            self.key = Some(key);
            self.pixels = pixels;
            self.tags = vec![(usize::MAX, 0); slots.max(1)];
            self.data = vec![0; self.tags.len() * pixels];
        }
    }

    /// Pixel column of frame `i`, folding it on a miss.
    fn column(&mut self, i: usize, f: &Frame, ranges: &[(u32, u32)]) -> &[u8] {
        let slot = i % self.tags.len();
        let tag = (i, f.time.to_bits());
        let px = &mut self.data[slot * self.pixels..(slot + 1) * self.pixels];
        if self.tags[slot] != tag {
            px.fill(0);
            fold_column(&f.spectrum, ranges, px);
            self.tags[slot] = tag;
        }
        px
    }
}

impl SpecCache {
    pub fn ranges(
        &mut self,
        pixels: usize,
        scale: FreqScale,
        min: f32,
        max: f32,
        bin_hz: f32,
        bins: usize,
    ) -> &[(u32, u32)] {
        let key = (pixels, scale, min.to_bits(), max.to_bits(), bins);
        if self.range_key != Some(key) {
            self.ranges =
                pixel_bin_ranges(scale, pixels, min as f64, max as f64, bin_hz as f64, bins);
            self.range_key = Some(key);
            self.pixels.key = None;
        }
        &self.ranges
    }

    pub fn ensure_lut(&mut self, map: ColorMap, mode: ColorMode, lo: f32, hi: f32) {
        let key = (map, mode, lo.to_bits(), hi.to_bits());
        if self.lut_key != Some(key) {
            self.lut = build_lut(map, mode, lo, hi);
            self.lut_key = Some(key);
        }
    }
}

/// Per-pixel maximum of `spectrum` over each bin range, folded into `out`.
#[inline]
pub fn fold_column(spectrum: &[u8], ranges: &[(u32, u32)], out: &mut [u8]) {
    for (o, &(lo, hi)) in out.iter_mut().zip(ranges) {
        let hi = (hi as usize).min(spectrum.len());
        let lo = (lo as usize).min(hi);
        let m = spectrum[lo..hi].iter().copied().max().unwrap_or(0);
        if m > *o {
            *o = m;
        }
    }
}

/// Candidate axis labels for a frequency range, low to high.
pub fn freq_labels(min: f32, max: f32, scale: FreqScale) -> Vec<f32> {
    let mut out = Vec::new();
    let mut decade = 10.0f32;
    while decade <= max * 10.0 {
        let mults: &[f32] = match scale {
            FreqScale::Log => &[1.0, 2.0, 5.0],
            _ => &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0],
        };
        for &m in mults {
            let f = decade * m;
            if f >= min && f <= max {
                out.push(f);
            }
        }
        decade *= 10.0;
    }
    out
}

/// Colour key on the top border, right-aligned: ` -70 ▆▆▆▆▆▆▆▆▆▆▆▆ 0 dB `.
fn draw_legend(
    buf: &mut Buffer,
    area: Rect,
    heading_w: u16,
    map: ColorMap,
    mode: ColorMode,
    lo: f32,
    hi: f32,
) {
    const STRIP: u16 = 12;
    let (l, h) = (format!(" {lo:.0} "), format!(" {hi:.0} dB "));
    let w = l.len() as u16 + STRIP + h.len() as u16;
    if area.height == 0 || area.width < heading_w + w + 4 {
        return;
    }
    let x = area.x + area.width - 1 - w;
    let style = Style::default().fg(Color::Gray);
    buf.set_string(x, area.y, &l, style);
    for i in 0..STRIP {
        let c = to_color(mode, map.sample((i as f32 + 0.5) / STRIP as f32));
        buf[(x + l.len() as u16 + i, area.y)]
            .set_char('█')
            .set_fg(c);
    }
    buf.set_string(x + l.len() as u16 + STRIP, area.y, &h, style);
}

/// Draw the spectrogram; returns the chart rectangle (for mouse mapping).
pub fn render(app: &mut App, area: Rect, buf: &mut Buffer) -> Rect {
    let a = &app.analysis;
    let d = &app.display;
    let v = &app.view;
    let heading = format!(
        " Spectrogram  {} pt {} · {:?} · {}-weighted · {} ",
        a.fft_size,
        a.window,
        d.freq_scale,
        a.weighting,
        format!("{:?}", d.colormap).to_lowercase()
    );
    let heading_w = heading.chars().count() as u16;
    let block = Block::bordered().title(title(heading));
    let inner = block.inner(area);
    block.render(area, buf);
    draw_legend(
        buf,
        area,
        heading_w,
        d.colormap,
        d.color_mode,
        d.spec_floor,
        d.db_max,
    );
    let (axis, chart) = split_axis(inner);
    if chart.width == 0 || chart.height == 0 {
        return chart;
    }
    let pixels = chart.height as usize * 2;
    let cache = &mut app.spec_cache;
    cache.ensure_lut(d.colormap, d.color_mode, d.spec_floor, d.db_max);
    cache.ranges(
        pixels,
        d.freq_scale,
        a.min_freq,
        a.max_freq,
        a.bin_hz,
        a.bins,
    );
    let SpecCache {
        ranges,
        lut,
        col,
        pixels: pcache,
        range_key,
        ..
    } = cache;
    col.resize(pixels, 0);
    if let Some(key) = *range_key {
        pcache.prepare(key, pixels, v.span());
    }

    let first = app.history.first();
    let cursor_col = v.cursor_column();
    for c in 0..chart.width as usize {
        let Some((fs, fe)) = v.column_frames(c).filter(|&(_, hi)| hi >= first) else {
            continue;
        };
        let fs = fs.max(first);
        col.fill(0);
        let mut any = false;
        for (i, fr) in (fs..).zip(app.history.range(fs, fe)) {
            for (o, &p) in col.iter_mut().zip(pcache.column(i, fr, ranges)) {
                *o = (*o).max(p);
            }
            any = true;
        }
        if !any {
            continue;
        }
        let x = chart.x + c as u16;
        let is_cursor = cursor_col == Some(c);
        for row in 0..chart.height as usize {
            let top = col[pixels - 1 - 2 * row] as usize;
            let bot = col[pixels - 2 - 2 * row] as usize;
            let cell = &mut buf[(x, chart.y + row as u16)];
            if is_cursor {
                cell.set_char('│').set_fg(Color::White).set_bg(lut[bot]);
            } else {
                cell.set_char('▀').set_fg(lut[top]).set_bg(lut[bot]);
            }
        }
    }
    if let Some(cc) = cursor_col {
        // Mark the cursor even over columns with no data yet.
        let x = chart.x + cc as u16;
        buf[(x, chart.y)].set_char('▼').set_fg(Color::White);
    }

    // Frequency axis.
    let style = Style::default().fg(Color::DarkGray);
    let mut last_row: Option<u16> = None;
    for f in freq_labels(a.min_freq, a.max_freq, d.freq_scale)
        .into_iter()
        .rev()
    {
        let u = to_unit(d.freq_scale, f as f64, a.min_freq as f64, a.max_freq as f64) as f32;
        let row =
            chart.height - 1 - ((u * chart.height as f32).floor() as u16).min(chart.height - 1);
        if last_row.is_some_and(|r| row < r + 2) {
            continue;
        }
        let label = format!(
            "{:>w$}",
            fmt_freq(f),
            w = axis.width.saturating_sub(1) as usize
        );
        buf.set_string(axis.x, axis.y + row, label, style);
        last_row = Some(row);
    }
    // The timeline carries the time axis when it is shown.
    if !app.display.shown(crate::cli::Panel::Timeline) {
        super::draw_time_axis(buf, app, area, chart);
    }
    chart
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_takes_max_per_range() {
        let spec = [1u8, 5, 3, 9, 2, 7];
        let ranges = [(0, 2), (2, 4), (4, 6), (5, 6)];
        let mut out = [0u8; 4];
        fold_column(&spec, &ranges, &mut out);
        assert_eq!(out, [5, 9, 7, 7]);
        // Folding a second frame keeps the maximum.
        fold_column(&[10, 0, 0, 0, 0, 0], &ranges, &mut out);
        assert_eq!(out, [10, 9, 7, 7]);
        // Out-of-range bins are clipped rather than panicking.
        let mut o = [0u8; 1];
        fold_column(&spec, &[(4, 100)], &mut o);
        assert_eq!(o, [7]);
    }

    #[test]
    fn labels() {
        assert_eq!(
            freq_labels(20.0, 20000.0, FreqScale::Log),
            vec![20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0]
        );
        let lin = freq_labels(1000.0, 5000.0, FreqScale::Linear);
        assert_eq!(lin, vec![1000.0, 2000.0, 3000.0, 4000.0, 5000.0]);
    }

    #[test]
    fn cache_reuses() {
        let mut c = SpecCache::default();
        let a = c
            .ranges(20, FreqScale::Log, 20.0, 20000.0, 23.4375, 1025)
            .to_vec();
        assert_eq!(a.len(), 20);
        let b = c
            .ranges(20, FreqScale::Log, 20.0, 20000.0, 23.4375, 1025)
            .to_vec();
        assert_eq!(a, b);
        let l = c
            .ranges(40, FreqScale::Linear, 20.0, 20000.0, 23.4375, 1025)
            .len();
        assert_eq!(l, 40);
        c.ensure_lut(ColorMap::Gray, ColorMode::TrueColor, -96.0, 0.0);
        assert_eq!(c.lut.len(), 256);
    }
}
