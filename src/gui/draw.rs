//! Panels, drawn with egui's painter from the shared app state. Each mirrors
//! its terminal counterpart in `crate::ui`, reusing its scales, colours, text
//! formatting and history bucketing; only the pixels are new.

use super::color::{c32, BG, GRID, LABEL, PANEL, TEXT, TRACK};
use crate::app::{App, MeterMode};
use crate::dsp::quant::q_to_db;
use crate::dsp::scale::{pixel_bin_ranges, to_unit, FreqScale};
use crate::source::SourceKind;
use crate::ui::colormap::{
    build_lut, eq_gradient, ColorMap, ColorMode, LU_M_COLOR, LU_OVER_COLOR, LU_S_COLOR,
    LU_TARGET_LINE,
};
use crate::ui::meter::{
    channel_label, mode_scale, note_name, row, session_text, target_text, Scale,
};
use crate::ui::spectrogram::{fold_column, freq_labels};
use crate::ui::timeline::{bucket_levels, bucket_momentary, bucket_vu};
use crate::ui::{fmt_db, fmt_freq, fmt_tick, fmt_time, piano, time_ticks, view_range_text, Clock};
use eframe::egui::{
    self, Align2, Color32, ColorImage, FontId, Mesh, Painter, Pos2, Rect, RichText, Shape, Stroke,
    TextureHandle, TextureOptions, Ui, Vec2,
};
use ratatui::style::Color;
use std::time::Instant;

/// Width of the left axis on every chart, so the spectrogram and timeline align.
pub const AXIS_W: f32 = 44.0;
const PAD: f32 = 8.0;
const TITLE_H: f32 = 22.0;
/// Room under the timeline for the time axis.
const TIME_AXIS_H: f32 = 16.0;
/// Pixels kept between time-axis labels.
const TICK_SPACING_PX: f32 = 70.0;
const READOUT_FONT: f32 = 13.0;
/// Width of the meter's readout column, in characters.
const READOUT_CHARS: f32 = 36.0;

fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

/// Panel background and title; returns the area inside.
fn panel(p: &Painter, rect: Rect, title: &str) -> Rect {
    p.rect_filled(rect, 6.0, PANEL);
    p.text(
        rect.left_top() + Vec2::new(10.0, 5.0),
        Align2::LEFT_TOP,
        title,
        FontId::proportional(12.5),
        LABEL,
    );
    Rect::from_min_max(
        rect.min + Vec2::new(PAD, TITLE_H),
        rect.max - Vec2::new(PAD, PAD),
    )
}

/// Plot area of a chart panel (spectrogram, timeline) right of its axis.
pub fn chart_plot(rect: Rect, time_axis: bool) -> Rect {
    let inner = Rect::from_min_max(
        rect.min + Vec2::new(PAD, TITLE_H),
        rect.max - Vec2::new(PAD, PAD),
    );
    let bottom = inner.bottom() - if time_axis { TIME_AXIS_H } else { 0.0 };
    Rect::from_min_max(
        Pos2::new(inner.left() + AXIS_W, inner.top()),
        Pos2::new(inner.right(), bottom.max(inner.top() + 1.0)),
    )
}

/// Width the meter panel wants for `channels` channels.
pub fn meter_width(channels: usize, char_w: f32) -> f32 {
    let bars = channels.max(2) as f32;
    2.0 * PAD + AXIS_W + bars * 38.0 + 20.0 + READOUT_CHARS * char_w
}

fn y_at(plot: Rect, frac: f32) -> f32 {
    plot.bottom() - frac.clamp(0.0, 1.0) * plot.height()
}

fn hline(p: &Painter, x: (f32, f32), y: f32, width: f32, color: Color32) {
    p.line_segment(
        [Pos2::new(x.0, y), Pos2::new(x.1, y)],
        Stroke::new(width, color),
    );
}

/// Height fractions where a scale's colour zones change, low to high.
fn zone_cuts(scale: Scale) -> Vec<f32> {
    let dbs = match scale {
        Scale::Db { .. } => vec![-18.0, -6.0],
        Scale::Vu { reference } => vec![reference],
        Scale::Lu { target } => vec![target],
    };
    dbs.into_iter().map(|d| scale.frac(d)).collect()
}

/// Fill `x` between height fractions `f0..f1`, split at the zone cuts so each
/// piece takes `color` of the level at its middle.
#[allow(clippy::too_many_arguments)]
fn zone_fill(
    mesh: &mut Mesh,
    plot: Rect,
    scale: Scale,
    cuts: &[f32],
    x: (f32, f32),
    f0: f32,
    f1: f32,
    color: &dyn Fn(f32) -> Color32,
) {
    if f1 <= f0 {
        return;
    }
    let mut a = f0;
    for c in cuts
        .iter()
        .copied()
        .filter(|&c| c > f0 && c < f1)
        .chain(std::iter::once(f1))
    {
        let col = color(scale.db_at((a + c) / 2.0));
        mesh.add_colored_rect(
            Rect::from_min_max(Pos2::new(x.0, y_at(plot, c)), Pos2::new(x.1, y_at(plot, a))),
            col,
        );
        a = c;
    }
}

/// A vertical gradient quad from `cb` at `yb` to `ct` at `yt`.
fn vgrad(mesh: &mut Mesh, x: (f32, f32), yb: f32, yt: f32, cb: Color32, ct: Color32) {
    let i = mesh.vertices.len() as u32;
    mesh.colored_vertex(Pos2::new(x.0, yb), cb);
    mesh.colored_vertex(Pos2::new(x.1, yb), cb);
    mesh.colored_vertex(Pos2::new(x.1, yt), ct);
    mesh.colored_vertex(Pos2::new(x.0, yt), ct);
    mesh.add_triangle(i, i + 1, i + 2);
    mesh.add_triangle(i, i + 2, i + 3);
}

/// dB grid step giving at least `min_px` between labels.
fn db_step(range: f32, px: f32, min_px: f32) -> f32 {
    [3.0, 6.0, 10.0, 12.0, 20.0, 24.0, 30.0, 40.0, 60.0]
        .into_iter()
        .find(|s| s / range * px >= min_px)
        .unwrap_or(120.0)
}

/// Axis marks `(dBFS, label, colour)` for a scale over `px` pixels.
fn scale_marks(scale: Scale, px: f32) -> Vec<(f32, String, Color32)> {
    match scale {
        Scale::Db { lo, hi } => {
            let step = db_step(hi - lo, px, 22.0);
            let mut v = (hi / step).floor() * step;
            let mut out = Vec::new();
            while v >= lo {
                out.push((v, format!("{v:.0}"), LABEL));
                v -= step;
            }
            out
        }
        Scale::Vu { reference } => {
            // Most important first; later marks give way when they would crowd.
            let marks = [
                0.0, 3.0, -3.0, -20.0, -10.0, -5.0, -7.0, 2.0, 1.0, -1.0, -2.0f32,
            ];
            let mut kept: Vec<f32> = Vec::new();
            let mut out = Vec::new();
            for vu in marks {
                let y = scale.frac(reference + vu) * px;
                if kept.iter().any(|&k| (k - y).abs() < 13.0) {
                    continue;
                }
                kept.push(y);
                let label = if vu > 0.0 {
                    format!("+{vu}")
                } else {
                    format!("{vu}")
                };
                let color = if vu >= 0.0 { c32(Color::Red) } else { LABEL };
                out.push((reference + vu, label, color));
            }
            out
        }
        Scale::Lu { target } => {
            let span = scale.db_at(1.0) - scale.db_at(0.0);
            let step = if 3.0 / span * px >= 16.0 { 3.0 } else { 9.0 };
            let top = scale.db_at(1.0) - target;
            let mut out = Vec::new();
            let mut lu = top;
            while lu >= scale.db_at(0.0) - target - 0.01 {
                let label = if lu > 0.0 {
                    format!("+{lu}")
                } else {
                    format!("{lu}")
                };
                let color = if lu == 0.0 { c32(Color::Yellow) } else { LABEL };
                out.push((target + lu, label, color));
                lu -= step;
            }
            out
        }
    }
}

fn draw_axis(p: &Painter, plot: Rect, scale: Scale, grid: bool) {
    for (db, label, color) in scale_marks(scale, plot.height()) {
        let y = y_at(plot, scale.frac(db));
        if grid {
            hline(p, (plot.left(), plot.right()), y, 1.0, GRID);
        }
        p.text(
            Pos2::new(plot.left() - 6.0, y),
            Align2::RIGHT_CENTER,
            label,
            mono(10.5),
            color,
        );
    }
}

struct Bar {
    label: String,
    fill: f32,
    envelope: f32,
    hold: Option<f32>,
    hold_color: Color32,
    /// One colour instead of the scale's zones, except above a loudness target.
    color: Option<Color32>,
}

pub fn meter(p: &Painter, rect: Rect, app: &mut App) {
    let mode = app.display.mode;
    let title = match mode {
        MeterMode::Lufs => format!("Levels · LUFS · 0 LU = {}", app.display.lufs_target),
        _ => format!("Levels · {}", mode.name()),
    };
    let inner = panel(p, rect, &title);
    let history = &app.history;
    let shown = app
        .readout
        .pick(app.view.cursor, app.view.follow, Instant::now(), |i| {
            history.get(i).is_some()
        });
    let Some(f) = app.history.get(app.view.cursor) else {
        p.text(
            inner.left_top(),
            Align2::LEFT_TOP,
            "waiting for audio…",
            mono(12.0),
            LABEL,
        );
        return;
    };
    let scale = mode_scale(app);
    let n = f.n();
    let s = &app.session;
    let target = app.display.lufs_target;
    let bars: Vec<Bar> = match mode {
        MeterMode::Rms => (0..n)
            .map(|c| Bar {
                label: channel_label(c, n),
                fill: f.rms_avg[c] + app.display.rms_offset,
                envelope: f.peak_decay[c],
                hold: Some(f.hold[c]),
                hold_color: Color32::WHITE,
                color: None,
            })
            .collect(),
        MeterMode::Vu => (0..n)
            .map(|c| Bar {
                label: channel_label(c, n),
                fill: f.vu[c],
                envelope: f.vu[c],
                hold: Some(f.vu_hold[c]),
                hold_color: Color32::WHITE,
                color: None,
            })
            .collect(),
        MeterMode::Lufs => vec![
            Bar {
                label: "M".into(),
                fill: f.lufs_m,
                envelope: f.lufs_m,
                hold: Some(s.max_lufs_m),
                hold_color: c32(Color::DarkGray),
                color: Some(c32(LU_M_COLOR)),
            },
            Bar {
                label: "S".into(),
                fill: f.lufs_s,
                envelope: f.lufs_s,
                hold: None,
                hold_color: c32(Color::DarkGray),
                color: Some(c32(LU_S_COLOR)),
            },
        ],
    };

    let (bw, gap) = (28.0, 10.0);
    let plot = Rect::from_min_max(
        Pos2::new(inner.left() + AXIS_W, inner.top() + 6.0),
        Pos2::new(
            inner.left() + AXIS_W + bars.len() as f32 * (bw + gap),
            inner.bottom() - 16.0,
        ),
    );
    draw_axis(p, plot, scale, false);
    let cuts = zone_cuts(scale);
    let above_target = |db: f32| matches!(scale, Scale::Lu { target } if db >= target);
    let mut mesh = Mesh::default();
    let mut xs = Vec::new();
    for (i, b) in bars.iter().enumerate() {
        let x0 = plot.left() + gap / 2.0 + i as f32 * (bw + gap);
        let x = (x0, x0 + bw);
        xs.push(x);
        mesh.add_colored_rect(Rect::from_x_y_ranges(x.0..=x.1, plot.y_range()), TRACK);
        let color = |db: f32, dim: bool| match b.color {
            Some(c) if !above_target(db) => c,
            _ => {
                let (fg, d) = scale.colors(db);
                c32(if dim { d } else { fg })
            }
        };
        let (hf, he) = (scale.frac(b.fill), scale.frac(b.envelope));
        zone_fill(&mut mesh, plot, scale, &cuts, x, hf, he, &|db| {
            color(db, true)
        });
        zone_fill(&mut mesh, plot, scale, &cuts, x, 0.0, hf, &|db| {
            color(db, false)
        });
    }
    p.add(Shape::mesh(mesh));
    for (b, &x) in bars.iter().zip(&xs) {
        let hf = scale.frac(b.fill);
        if let Scale::Lu { target } = scale {
            let t = scale.frac(target);
            if t > hf {
                hline(p, (x.0, x.1), y_at(plot, t), 1.0, c32(LU_TARGET_LINE));
            }
        }
        // A hold level with the bar would only hide the bar's top.
        if let Some(h) = b
            .hold
            .map(|h| scale.frac(h))
            .filter(|&h| (h - hf) * plot.height() >= 3.0)
        {
            hline(p, (x.0, x.1), y_at(plot, h), 2.0, b.hold_color);
        }
        p.text(
            Pos2::new((x.0 + x.1) / 2.0, plot.bottom() + 3.0),
            Align2::CENTER_TOP,
            &b.label,
            mono(11.0),
            c32(Color::Gray),
        );
    }
    // Integrated loudness, the delivered number, as a pointer beside the S bar.
    if let (MeterMode::Lufs, Some(&(_, right))) = (mode, xs.last()) {
        let h = scale.frac(f.lufs_i);
        if h > 0.0 {
            let y = y_at(plot, h);
            let color = if f.lufs_i >= target {
                c32(LU_OVER_COLOR)
            } else {
                Color32::WHITE
            };
            let x = right + 2.0;
            p.add(Shape::convex_polygon(
                vec![
                    Pos2::new(x, y),
                    Pos2::new(x + 8.0, y - 5.0),
                    Pos2::new(x + 8.0, y + 5.0),
                ],
                color,
                Stroke::NONE,
            ));
        }
    }

    // Readouts beside the bars, in the terminal's fixed-width columns.
    let f = app.history.get(shown).unwrap_or(f);
    let clipped = app.history.clipped(shown, app.display.clip_frames);
    let font = mono(READOUT_FONT);
    let cw = p.fonts(|fs| fs.glyph_width(&font, '0'));
    let tx = plot.right() + 20.0;
    let lh = READOUT_FONT + 6.0;
    let y = std::cell::Cell::new(inner.top() + 4.0);
    // Writes a readout line; returns its top.
    let line = |text: &str, color: Color32| {
        let top = y.get();
        p.text(
            Pos2::new(tx, top),
            Align2::LEFT_TOP,
            text,
            font.clone(),
            color,
        );
        y.set(top + lh);
        top
    };
    let head = c32(Color::DarkGray);
    match mode {
        MeterMode::Rms => {
            line(&row("dBFS", &["peak", "hold", "rms", "avg"]), head);
            let off = app.display.rms_offset;
            for c in 0..n {
                let color = c32(scale.colors(f.peak[c].max(f.hold[c])).0);
                let vals = [f.peak[c], f.hold[c], f.rms[c] + off, f.rms_avg[c] + off].map(fmt_db);
                line(&row(&channel_label(c, n), &vals), color);
            }
        }
        MeterMode::Vu => {
            line(&row("VU", &["vu", "hold"]), head);
            for c in 0..n {
                let color = c32(scale.colors(f.vu_hold[c]).0);
                let vals = [f.vu[c], f.vu_hold[c]].map(|v| scale.fmt(v));
                line(&row(&channel_label(c, n), &vals), color);
            }
        }
        MeterMode::Lufs => {
            line(&row("LUFS", &["mom", "short", "integ", "max M"]), head);
            let vals = [f.lufs_m, f.lufs_s, f.lufs_i, s.max_lufs_m];
            let base = [c32(LU_M_COLOR), c32(LU_S_COLOR), TEXT, TEXT];
            let ry = line("", TEXT);
            for (i, (v, c)) in vals.iter().zip(base).enumerate() {
                let color = if *v >= target { c32(LU_OVER_COLOR) } else { c };
                let x = tx + (4 + i * 7 + 1) as f32 * cw;
                p.text(
                    Pos2::new(x, ry),
                    Align2::LEFT_TOP,
                    format!("{:>6}", fmt_db(*v)),
                    font.clone(),
                    color,
                );
            }
            let to_target = f.lufs_i - target;
            let color = if to_target >= 0.0 {
                c32(LU_OVER_COLOR)
            } else {
                TEXT
            };
            line(&target_text(to_target, f.lufs_lra), color);
        }
    }
    let clip_color = if s.clips > 0 {
        c32(Color::LightRed)
    } else {
        head
    };
    let (label, max, unit) = if mode == MeterMode::Lufs {
        ("TP", s.max_tp, "dBTP")
    } else {
        ("max", s.max_db, "dBFS")
    };
    line(
        &session_text(label, &fmt_db(max), unit, s.clips),
        clip_color,
    );
    y.set(y.get() + lh / 2.0);
    let tone = match f.peak_freq {
        Some((hz, db)) if db > -100.0 => {
            format!("tone {hz:>7.1} Hz {:>6} dB {}", fmt_db(db), note_name(hz))
        }
        _ => "tone —".into(),
    };
    line(&tone, TEXT);
    if clipped {
        let r = Rect::from_min_size(Pos2::new(tx, y.get() + 4.0), Vec2::new(cw * 6.0, lh));
        p.rect_filled(r, 3.0, c32(Color::Red));
        p.text(
            r.center(),
            Align2::CENTER_CENTER,
            "CLIP",
            FontId::monospace(READOUT_FONT),
            Color32::WHITE,
        );
    }
}

pub fn equalizer(p: &Painter, rect: Rect, app: &App) {
    let a = &app.analysis;
    let nb = a.band_centers.len();
    let kind = if a.third_octave {
        "1/3 octave".to_string()
    } else {
        format!("{nb} bands")
    };
    let title = format!(
        "Equalizer  {kind} · {}-{} Hz",
        fmt_freq(a.min_freq),
        fmt_freq(a.max_freq)
    );
    let inner = panel(p, rect, &title);
    let plot = Rect::from_min_max(
        Pos2::new(inner.left() + AXIS_W, inner.top() + 6.0),
        Pos2::new(inner.right(), inner.bottom() - 16.0),
    );
    let (lo, hi) = (app.display.db_min, app.display.db_max);
    let scale = Scale::Db { lo, hi };
    draw_axis(p, plot, scale, true);
    let Some(f) = app.history.get(app.view.cursor) else {
        return;
    };
    if nb == 0 {
        return;
    }
    let w = plot.width() / nb as f32;
    let gap = (w * 0.18).clamp(1.0, 6.0);
    let rgb = |t: f32| {
        let [r, g, b] = eq_gradient(t);
        Color32::from_rgb(r, g, b)
    };
    let mut mesh = Mesh::default();
    let mut caps = Vec::new();
    for i in 0..nb.min(f.bands.len()) {
        let x0 = plot.left() + i as f32 * w + gap / 2.0;
        let x = (x0, x0 + w - gap);
        let h = scale.frac(q_to_db(f.bands[i]));
        // The gradient bends at 60% of the height; split there so it stays true.
        let knee = h.min(0.6);
        vgrad(
            &mut mesh,
            x,
            y_at(plot, 0.0),
            y_at(plot, knee),
            rgb(0.0),
            rgb(knee),
        );
        if h > 0.6 {
            vgrad(
                &mut mesh,
                x,
                y_at(plot, 0.6),
                y_at(plot, h),
                rgb(0.6),
                rgb(h),
            );
        }
        let cap = f.band_caps.get(i).map(|&c| q_to_db(c));
        if let Some(c) = cap.filter(|&c| c >= lo + 6.0) {
            caps.push((x, y_at(plot, scale.frac(c))));
        }
    }
    p.add(Shape::mesh(mesh));
    for (x, y) in caps {
        hline(p, x, y, 2.0, c32(Color::Gray));
    }

    // Labels centred under their bars: every band when they fit, otherwise
    // only the major ones, never overlapping.
    let font = mono(10.0);
    let cw = p.fonts(|fs| fs.glyph_width(&font, '0'));
    let place = |i: usize| {
        let lw = a.band_labels[i].chars().count() as f32 * cw;
        let x = plot.left() + (i as f32 + 0.5) * w - lw / 2.0;
        (x, lw)
    };
    let fits = |filter: &dyn Fn(usize) -> bool| {
        let mut free = f32::MIN;
        (0..nb).filter(|&i| filter(i)).all(|i| {
            let (x, lw) = place(i);
            let ok = x >= free;
            free = x + lw + cw;
            ok
        })
    };
    let all = fits(&|_| true);
    let mut free = f32::MIN;
    for i in 0..nb {
        let (x, lw) = place(i);
        if (all || a.band_major[i]) && x >= free && x + lw <= inner.right() {
            p.text(
                Pos2::new(x, plot.bottom() + 3.0),
                Align2::LEFT_TOP,
                &a.band_labels[i],
                font.clone(),
                LABEL,
            );
            free = x + lw + cw;
        }
    }
}

pub fn piano_panel(p: &Painter, rect: Rect, app: &mut App) {
    const LOWEST: i32 = 21;
    const HIGHEST: i32 = 108;
    let history = &app.history;
    let shown = app
        .readout
        .pick(app.view.cursor, app.view.follow, Instant::now(), |i| {
            history.get(i).is_some()
        });
    let tone = app
        .history
        .get(shown)
        .and_then(|f| f.peak_freq)
        .filter(|&(_, db)| db > -100.0);
    let lit = tone.and_then(|(hz, _)| piano::key_of(hz));
    let title = match (tone, lit) {
        (Some((hz, _)), Some(k)) => format!("Piano  {} · {hz:.1} Hz", piano::key_name(k)),
        _ => "Piano".into(),
    };
    let inner = panel(p, rect, &title);
    let whites = piano::white_index(HIGHEST) + 1;
    // All 52 white keys when they fit at a usable size, else a window
    // following the tone.
    let mut kw = inner.width() / whites as f32;
    let (first, count) = if kw >= 10.0 {
        (0, whites)
    } else {
        kw = 14.0;
        let count = ((inner.width() / kw) as i32).min(whites);
        let focus = piano::white_index(lit.unwrap_or(60));
        ((focus - count / 2).clamp(0, whites - count), count)
    };
    let x0 = inner.left() + (inner.width() - kw * count as f32) / 2.0;
    let keys_h = inner.height() - 16.0;
    let visible = |wi: i32| (first..first + count).contains(&wi);
    let lit_color = c32(Color::Cyan);
    for m in (LOWEST..=HIGHEST).filter(|&m| !piano::is_black(m)) {
        let wi = piano::white_index(m);
        if !visible(wi) {
            continue;
        }
        let x = x0 + (wi - first) as f32 * kw;
        let r = Rect::from_min_size(Pos2::new(x, inner.top()), Vec2::new(kw - 1.0, keys_h));
        let fill = if lit == Some(m) {
            lit_color
        } else {
            Color32::from_gray(228)
        };
        p.rect_filled(r, 2.0, fill);
    }
    for m in (LOWEST..=HIGHEST).filter(|&m| piano::is_black(m)) {
        let left = piano::white_index(m);
        if !visible(left) || !visible(left + 1) {
            continue;
        }
        let cx = x0 + (left - first + 1) as f32 * kw - 0.5;
        let bw = kw * 0.6;
        let r = Rect::from_min_size(
            Pos2::new(cx - bw / 2.0, inner.top()),
            Vec2::new(bw, keys_h * 0.62),
        );
        let fill = if lit == Some(m) {
            lit_color
        } else {
            Color32::from_gray(22)
        };
        p.rect_filled(r, 2.0, fill);
    }
    // The lit key's name under it, then every C that still fits.
    let font = mono(10.0);
    let ly = inner.top() + keys_h + 2.0;
    let mut taken: Vec<(f32, f32)> = Vec::new();
    let mut label = |m: i32, color: Color32| {
        let wi = piano::white_index(m);
        if !visible(wi) {
            return;
        }
        let name = piano::key_name(m);
        let x = x0 + (wi - first) as f32 * kw + if piano::is_black(m) { kw * 0.7 } else { 1.0 };
        let w = name.len() as f32 * 6.5;
        if taken.iter().any(|&(a, b)| x < b + 4.0 && a < x + w + 4.0) {
            return;
        }
        taken.push((x, x + w));
        p.text(
            Pos2::new(x, ly),
            Align2::LEFT_TOP,
            name,
            font.clone(),
            color,
        );
    };
    if let Some(k) = lit {
        label(k, lit_color);
    }
    for m in (LOWEST..=HIGHEST).filter(|m| m.rem_euclid(12) == 0) {
        label(m, LABEL);
    }
}

/// Spectrogram image state: each frame's folded pixel column is cached so a
/// redraw only folds new frames.
#[derive(Default)]
pub struct SpecView {
    tex: Option<TextureHandle>,
    range_key: Option<(usize, FreqScale, u32, u32, usize)>,
    ranges: Vec<(u32, u32)>,
    lut_key: Option<(ColorMap, u32, u32)>,
    lut: Vec<Color32>,
    pixels: usize,
    tags: Vec<(usize, u64)>,
    data: Vec<u8>,
}

impl SpecView {
    fn prepare(&mut self, app: &App, pixels: usize) {
        let a = &app.analysis;
        let d = &app.display;
        let key = (
            pixels,
            d.freq_scale,
            a.min_freq.to_bits(),
            a.max_freq.to_bits(),
            a.bins,
        );
        let slots = app.view.span() + 2;
        if self.range_key != Some(key) || self.tags.len() < slots {
            self.ranges = pixel_bin_ranges(
                d.freq_scale,
                pixels,
                a.min_freq as f64,
                a.max_freq as f64,
                a.bin_hz as f64,
                a.bins,
            );
            self.range_key = Some(key);
            self.pixels = pixels;
            self.tags = vec![(usize::MAX, 0); slots];
            self.data = vec![0; slots * pixels];
        }
        let lk = (d.colormap, d.spec_floor.to_bits(), d.db_max.to_bits());
        if self.lut_key != Some(lk) {
            self.lut = build_lut(d.colormap, ColorMode::TrueColor, d.spec_floor, d.db_max)
                .into_iter()
                .map(c32)
                .collect();
            self.lut_key = Some(lk);
        }
    }

    /// Pixel column (bottom first) of frame `i`, folding it on a miss.
    fn column(&mut self, i: usize, spectrum: &[u8], time: f64) -> &[u8] {
        let slot = i % self.tags.len();
        let tag = (i, time.to_bits());
        let px = &mut self.data[slot * self.pixels..(slot + 1) * self.pixels];
        if self.tags[slot] != tag {
            px.fill(0);
            fold_column(spectrum, &self.ranges, px);
            self.tags[slot] = tag;
        }
        px
    }
}

pub fn spectrogram(p: &Painter, rect: Rect, app: &App, sv: &mut SpecView, time_axis: bool) {
    let a = &app.analysis;
    let d = &app.display;
    let title = format!(
        "Spectrogram  {} pt {} · {:?} · {}-weighted · {}",
        a.fft_size,
        a.window,
        d.freq_scale,
        a.weighting,
        format!("{:?}", d.colormap).to_lowercase()
    );
    panel(p, rect, &title);
    let plot = chart_plot(rect, time_axis);
    let ppp = p.ctx().pixels_per_point();
    let pixels = ((plot.height() * ppp).round() as usize).clamp(8, 2048);
    sv.prepare(app, pixels);

    let v = &app.view;
    let w = v.width.max(1);
    let mut img = ColorImage::new([w, pixels], BG);
    let mut col = vec![0u8; pixels];
    let first = app.history.first();
    for c in 0..w {
        let Some((fs, fe)) = v.column_frames(c).filter(|&(_, hi)| hi >= first) else {
            continue;
        };
        let fs = fs.max(first);
        col.fill(0);
        let mut any = false;
        for (i, fr) in (fs..).zip(app.history.range(fs, fe)) {
            for (o, &q) in col.iter_mut().zip(sv.column(i, &fr.spectrum, fr.time)) {
                *o = (*o).max(q);
            }
            any = true;
        }
        if any {
            for r in 0..pixels {
                img.pixels[r * w + c] = sv.lut[col[pixels - 1 - r] as usize];
            }
        }
    }
    let tex = match &mut sv.tex {
        Some(t) => {
            t.set(img, TextureOptions::LINEAR);
            t
        }
        None => sv.tex.insert(
            p.ctx()
                .load_texture("spectrogram", img, TextureOptions::LINEAR),
        ),
    };
    p.image(
        tex.id(),
        plot,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );

    // Frequency axis.
    let mut last_y = f32::INFINITY;
    for hz in freq_labels(a.min_freq, a.max_freq, d.freq_scale) {
        let u = to_unit(
            d.freq_scale,
            hz as f64,
            a.min_freq as f64,
            a.max_freq as f64,
        ) as f32;
        let y = y_at(plot, u);
        if last_y - y < 16.0 {
            continue;
        }
        last_y = y;
        p.text(
            Pos2::new(plot.left() - 6.0, y),
            Align2::RIGHT_CENTER,
            fmt_freq(hz),
            mono(10.5),
            LABEL,
        );
    }
    // Colour key in the title row, right-aligned.
    let key = Rect::from_min_size(
        Pos2::new(rect.right() - PAD - 150.0, rect.top() + 8.0),
        Vec2::new(110.0, 8.0),
    );
    let mut mesh = Mesh::default();
    let steps = 32;
    for i in 0..steps {
        let t = i as f32 / steps as f32;
        let x0 = key.left() + t * key.width();
        let q = (t * 255.0) as usize;
        let c = sv.lut.get(q).copied().unwrap_or(BG);
        mesh.add_colored_rect(
            Rect::from_x_y_ranges(x0..=x0 + key.width() / steps as f32 + 0.5, key.y_range()),
            c,
        );
    }
    p.add(Shape::mesh(mesh));
    // The LUT spans the quantizer's range; label the displayed ends.
    p.text(
        key.left_center() - Vec2::new(4.0, 0.0),
        Align2::RIGHT_CENTER,
        format!("{:.0}", d.spec_floor),
        mono(10.0),
        LABEL,
    );
    p.text(
        key.right_center() + Vec2::new(4.0, 0.0),
        Align2::LEFT_CENTER,
        format!("{:.0} dB", d.db_max),
        mono(10.0),
        LABEL,
    );

    draw_cursor(p, plot, app, true);
    if time_axis {
        draw_time_axis(p, plot, app);
    }
}

pub fn timeline(p: &Painter, rect: Rect, app: &App) {
    let d = &app.display;
    let v = &app.view;
    let range = view_range_text(app);
    let title = match d.mode {
        MeterMode::Rms => format!("Timeline  peak / rms · {range}"),
        MeterMode::Vu => format!("Timeline  vu · {range}"),
        MeterMode::Lufs => format!("Timeline  momentary loudness · {range}"),
    };
    panel(p, rect, &title);
    let plot = chart_plot(rect, true);
    // RMS keeps the charts' own dB range ([ ] { }); VU and LUFS are fixed scales.
    let scale = match d.mode {
        MeterMode::Rms => Scale::Db {
            lo: d.db_min,
            hi: d.db_max,
        },
        _ => mode_scale(app),
    };
    draw_axis(p, plot, scale, true);
    if let Scale::Lu { target } = scale {
        hline(
            p,
            (plot.left(), plot.right()),
            y_at(plot, scale.frac(target)),
            1.0,
            c32(LU_TARGET_LINE),
        );
    }
    let cuts = zone_cuts(scale);
    let color = |db: f32, dim: bool| match scale {
        Scale::Lu { target } if db >= target => c32(LU_OVER_COLOR),
        Scale::Lu { .. } => c32(LU_M_COLOR),
        _ => {
            let (fg, dm) = scale.colors(db);
            c32(if dim { dm } else { fg })
        }
    };
    let w = v.width.max(1);
    let cw = plot.width() / w as f32;
    let first = app.history.first();
    let mut mesh = Mesh::default();
    for c in 0..w {
        let Some((a, b)) = v.column_frames(c).filter(|&(_, hi)| hi >= first) else {
            continue;
        };
        let (hp, hr) = match d.mode {
            MeterMode::Rms => bucket_levels(&app.history, a, b).map_or((0.0, 0.0), |(pk, r)| {
                (scale.frac(pk), scale.frac(r + d.rms_offset))
            }),
            MeterMode::Vu => {
                bucket_vu(&app.history, a, b).map_or((0.0, 0.0), |u| (scale.frac(u), scale.frac(u)))
            }
            MeterMode::Lufs => bucket_momentary(&app.history, a, b)
                .map_or((0.0, 0.0), |m| (scale.frac(m), scale.frac(m))),
        };
        let x0 = plot.left() + c as f32 * cw;
        let x = (x0, x0 + cw);
        zone_fill(&mut mesh, plot, scale, &cuts, x, hr, hp, &|db| {
            color(db, true)
        });
        zone_fill(&mut mesh, plot, scale, &cuts, x, 0.0, hr, &|db| {
            color(db, false)
        });
    }
    p.add(Shape::mesh(mesh));
    draw_cursor(p, plot, app, false);
    draw_time_axis(p, plot, app);
}

/// The cursor column as a thin line, with a marker on top of the spectrogram.
fn draw_cursor(p: &Painter, plot: Rect, app: &App, marker: bool) {
    let Some(cc) = app.view.cursor_column() else {
        return;
    };
    let cw = plot.width() / app.view.width.max(1) as f32;
    let x = plot.left() + (cc as f32 + 0.5) * cw;
    let color = if app.view.follow {
        Color32::from_white_alpha(70)
    } else {
        Color32::WHITE
    };
    p.line_segment(
        [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
        Stroke::new(1.0, color),
    );
    if marker && !app.view.follow {
        p.add(Shape::convex_polygon(
            vec![
                Pos2::new(x - 5.0, plot.top()),
                Pos2::new(x + 5.0, plot.top()),
                Pos2::new(x, plot.top() + 7.0),
            ],
            Color32::WHITE,
            Stroke::NONE,
        ));
    }
}

/// Time ticks and labels under a chart.
fn draw_time_axis(p: &Painter, plot: Rect, app: &App) {
    let Some(clock) = Clock::of(app) else {
        return;
    };
    let cw = plot.width() / app.view.width.max(1) as f32;
    let min_cols = (TICK_SPACING_PX / cw).ceil().max(1.0) as usize;
    let (step, ticks) = time_ticks(&app.view, clock, min_cols);
    for (c, t) in ticks {
        let x = plot.left() + c as f32 * cw;
        p.line_segment(
            [
                Pos2::new(x, plot.bottom()),
                Pos2::new(x, plot.bottom() + 4.0),
            ],
            Stroke::new(1.0, LABEL),
        );
        p.text(
            Pos2::new(x + 3.0, plot.bottom() + 2.0),
            Align2::LEFT_TOP,
            fmt_tick(t, step),
            mono(10.0),
            LABEL,
        );
    }
}

/// The status line: source, state, time, recording and messages.
pub fn status(ui: &mut Ui, app: &App) {
    let a = &app.analysis;
    ui.horizontal(|ui| {
        ui.label(RichText::new(&app.info.name).strong().color(TEXT));
        ui.label(RichText::new(format!("{} Hz {} ch", a.sample_rate, a.channels)).color(LABEL));
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
        ui.label(RichText::new(mode).strong().color(c32(color)));
        if let Some(fr) = app.history.get(app.view.cursor) {
            ui.label(
                RichText::new(format!("t {}", fmt_time(fr.time)))
                    .monospace()
                    .color(TEXT),
            );
            if !app.view.follow {
                let latest = app
                    .history
                    .last()
                    .and_then(|l| app.history.get(l))
                    .map_or(0.0, |f| f.time);
                ui.label(
                    RichText::new(format!("(-{:.2}s)", latest - fr.time)).color(c32(Color::Yellow)),
                );
            }
        }
        if let Some(r) = &app.recorder {
            let secs = r.samples as f64 / (a.channels.max(1) as f64 * a.sample_rate as f64);
            ui.label(
                RichText::new(format!("● REC {secs:.1}s"))
                    .strong()
                    .color(c32(Color::Red)),
            );
        }
        if let Some((msg, at)) = &app.message {
            if at.elapsed().as_secs_f32() < 4.0 {
                ui.label(RichText::new(msg).color(c32(Color::LightBlue)));
            }
        }
        if let Some(w) = &app.input_warning {
            ui.label(
                RichText::new(format!("⚠ {w}"))
                    .strong()
                    .color(c32(Color::Yellow)),
            );
        }
        let dropped = app
            .control
            .dropped
            .load(std::sync::atomic::Ordering::Relaxed);
        if dropped > 0 {
            ui.label(
                RichText::new(format!("{dropped} buffers dropped")).color(c32(Color::LightRed)),
            );
        }
        if let Some(e) = app.control.error.lock().ok().and_then(|g| g.clone()) {
            ui.label(RichText::new(e).color(c32(Color::Red)));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(format!("{} fps  ? help", app.fps.fps)).color(LABEL));
        });
    });
}

/// Keys and readout legend, in a window over the meters.
pub fn help(ctx: &egui::Context, app: &mut App) {
    let mut open = app.show_help;
    egui::Window::new("Keys and readouts")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            egui::Grid::new("help")
                .num_columns(2)
                .spacing([16.0, 4.0])
                .show(ui, |ui| {
                    for (k, v) in crate::ui::HELP {
                        ui.label(RichText::new(*k).monospace().color(c32(Color::Yellow)));
                        ui.label(*v);
                        ui.end_row();
                    }
                    ui.end_row();
                    for (k, v) in crate::ui::LEGEND {
                        ui.label(RichText::new(*k).monospace().color(c32(Color::Cyan)));
                        ui.label(*v);
                        ui.end_row();
                    }
                });
        });
    app.show_help = open;
}
