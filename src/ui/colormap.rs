//! Colour maps, terminal colour-depth handling and level-zone colours.

use crate::dsp::quant::q_to_db;
use clap::ValueEnum;
use ratatui::style::Color;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum ColorMap {
    Magma,
    Inferno,
    Viridis,
    Fire,
    Gray,
}

impl ColorMap {
    pub fn next(self) -> Self {
        match self {
            ColorMap::Magma => ColorMap::Inferno,
            ColorMap::Inferno => ColorMap::Viridis,
            ColorMap::Viridis => ColorMap::Fire,
            ColorMap::Fire => ColorMap::Gray,
            ColorMap::Gray => ColorMap::Magma,
        }
    }

    fn stops(self) -> &'static [(f32, [u8; 3])] {
        match self {
            ColorMap::Magma => &[
                (0.0, [0, 0, 4]),
                (0.13, [28, 16, 68]),
                (0.25, [79, 18, 123]),
                (0.38, [129, 37, 129]),
                (0.5, [181, 54, 122]),
                (0.63, [229, 80, 100]),
                (0.75, [251, 135, 97]),
                (0.88, [254, 194, 135]),
                (1.0, [252, 253, 191]),
            ],
            ColorMap::Inferno => &[
                (0.0, [0, 0, 4]),
                (0.14, [40, 11, 84]),
                (0.29, [101, 21, 110]),
                (0.43, [159, 42, 99]),
                (0.57, [212, 72, 66]),
                (0.71, [245, 125, 21]),
                (0.86, [250, 193, 39]),
                (1.0, [252, 255, 164]),
            ],
            ColorMap::Viridis => &[
                (0.0, [68, 1, 84]),
                (0.25, [59, 82, 139]),
                (0.5, [33, 145, 140]),
                (0.75, [94, 201, 98]),
                (1.0, [253, 231, 37]),
            ],
            ColorMap::Fire => &[
                (0.0, [0, 0, 0]),
                (0.33, [180, 0, 0]),
                (0.66, [255, 170, 0]),
                (1.0, [255, 255, 255]),
            ],
            ColorMap::Gray => &[(0.0, [0, 0, 0]), (1.0, [255, 255, 255])],
        }
    }

    /// Colour at position `t` in [0, 1] (clamped), linearly interpolated.
    pub fn sample(self, t: f32) -> [u8; 3] {
        let s = self.stops();
        let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
        let i = s.iter().position(|&(p, _)| p >= t).unwrap_or(s.len() - 1);
        if i == 0 {
            return s[0].1;
        }
        let (p0, c0) = s[i - 1];
        let (p1, c1) = s[i];
        let f = (t - p0) / (p1 - p0);
        let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * f).round() as u8;
        [mix(c0[0], c1[0]), mix(c0[1], c1[1]), mix(c0[2], c1[2])]
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    Auto,
    Truecolor,
    #[value(name = "256")]
    Ansi256,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ColorMode {
    TrueColor,
    Ansi256,
}

impl ColorChoice {
    pub fn resolve(self, colorterm: Option<&str>) -> ColorMode {
        match self {
            ColorChoice::Truecolor => ColorMode::TrueColor,
            ColorChoice::Ansi256 => ColorMode::Ansi256,
            ColorChoice::Auto => match colorterm.map(str::to_ascii_lowercase).as_deref() {
                Some("truecolor") | Some("24bit") => ColorMode::TrueColor,
                _ => ColorMode::Ansi256,
            },
        }
    }
}

const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

fn cube_index(v: u8) -> usize {
    let mut best = 0;
    for (i, &c) in CUBE.iter().enumerate() {
        if (c as i32 - v as i32).abs() < (CUBE[best] as i32 - v as i32).abs() {
            best = i;
        }
    }
    best
}

/// Nearest xterm-256 palette entry (6x6x6 cube or grey ramp).
pub fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    let (ri, gi, bi) = (cube_index(r), cube_index(g), cube_index(b));
    let cube = [CUBE[ri], CUBE[gi], CUBE[bi]];
    let dist = |c: [u8; 3]| {
        let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
        d(c[0], r) + d(c[1], g) + d(c[2], b)
    };
    let avg = (r as u32 + g as u32 + b as u32) / 3;
    let gi_ = ((avg as i32 - 8 + 5) / 10).clamp(0, 23) as u8;
    let gv = 8 + 10 * gi_;
    if dist([gv, gv, gv]) < dist(cube) {
        232 + gi_
    } else {
        16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8
    }
}

pub fn to_color(mode: ColorMode, c: [u8; 3]) -> Color {
    match mode {
        ColorMode::TrueColor => Color::Rgb(c[0], c[1], c[2]),
        ColorMode::Ansi256 => Color::Indexed(rgb_to_ansi256(c[0], c[1], c[2])),
    }
}

/// Map every quantized level to a colour for the given dB range.
pub fn build_lut(map: ColorMap, mode: ColorMode, db_min: f32, db_max: f32) -> Vec<Color> {
    (0..=255u8)
        .map(|q| to_color(mode, map.sample((q_to_db(q) - db_min) / (db_max - db_min))))
        .collect()
}

/// Bright level colour for a meter segment at `db`.
pub fn zone_color(db: f32) -> Color {
    if db >= -6.0 {
        Color::Red
    } else if db >= -18.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

/// Traditional VU scale colours for a reading `vu` (VU, not dBFS): green
/// below 0 VU, red from 0 VU up; the second colour is the dim companion.
pub fn vu_zone_colors(vu: f32) -> (Color, Color) {
    if vu >= 0.0 {
        (Color::Red, Color::Indexed(88))
    } else {
        (Color::Green, Color::Indexed(22))
    }
}

/// Loudness colours for a reading `lu` relative to the target (LU): cyan
/// below the target, yellow at or above it; the second colour is the dim one.
pub fn lu_zone_colors(lu: f32) -> (Color, Color) {
    if lu >= 0.0 {
        (LU_OVER_COLOR, Color::Indexed(100))
    } else {
        (Color::Cyan, Color::Indexed(23))
    }
}

/// Loudness colours: momentary in soft blue (#5fafd7) and short-term in sage
/// green (#87af87), of equal weight.
pub const LU_M_COLOR: Color = Color::Indexed(74);
pub const LU_S_COLOR: Color = Color::Indexed(108);
/// The faint line marking the loudness target across empty cells.
pub const LU_TARGET_LINE: Color = Color::Indexed(100);
/// Loudness above the target.
pub const LU_OVER_COLOR: Color = Color::Yellow;

/// Dim companion of [`zone_color`] used for the peak envelope.
pub fn zone_color_dim(db: f32) -> Color {
    if db >= -6.0 {
        Color::Indexed(88)
    } else if db >= -18.0 {
        Color::Indexed(100)
    } else {
        Color::Indexed(22)
    }
}

/// Green-yellow-red gradient for equalizer bars at height fraction `t`.
pub fn eq_gradient(t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    if t < 0.6 {
        let f = t / 0.6;
        [(40.0 + 190.0 * f) as u8, (200.0 + 30.0 * f) as u8, 40]
    } else {
        let f = (t - 0.6) / 0.4;
        [(230.0 + 20.0 * f) as u8, (230.0 * (1.0 - f)) as u8, 40]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_endpoints_and_interpolation() {
        assert_eq!(ColorMap::Gray.sample(0.0), [0, 0, 0]);
        assert_eq!(ColorMap::Gray.sample(1.0), [255, 255, 255]);
        assert_eq!(ColorMap::Gray.sample(0.5), [128, 128, 128]);
        assert_eq!(ColorMap::Gray.sample(-3.0), [0, 0, 0]);
        assert_eq!(ColorMap::Gray.sample(7.0), [255, 255, 255]);
        assert_eq!(ColorMap::Gray.sample(f32::NAN), [0, 0, 0]);
        assert_eq!(ColorMap::Magma.sample(0.5), [181, 54, 122]);
        assert_eq!(ColorMap::Fire.sample(1.0), [255, 255, 255]);
    }

    #[test]
    fn maps_cycle() {
        let mut m = ColorMap::Magma;
        for _ in 0..5 {
            m = m.next();
        }
        assert_eq!(m, ColorMap::Magma);
    }

    #[test]
    fn ansi256_mapping() {
        assert_eq!(rgb_to_ansi256(0, 0, 0), 16);
        assert_eq!(rgb_to_ansi256(255, 255, 255), 231);
        assert_eq!(rgb_to_ansi256(255, 0, 0), 196);
        assert_eq!(rgb_to_ansi256(0, 255, 0), 46);
        assert_eq!(rgb_to_ansi256(128, 128, 128), 244);
        assert_eq!(rgb_to_ansi256(95, 135, 175), 16 + 36 + 12 + 3);
    }

    #[test]
    fn color_choice() {
        assert_eq!(
            ColorChoice::Auto.resolve(Some("truecolor")),
            ColorMode::TrueColor
        );
        assert_eq!(
            ColorChoice::Auto.resolve(Some("24BIT")),
            ColorMode::TrueColor
        );
        assert_eq!(ColorChoice::Auto.resolve(None), ColorMode::Ansi256);
        assert_eq!(ColorChoice::Truecolor.resolve(None), ColorMode::TrueColor);
        assert_eq!(
            ColorChoice::Ansi256.resolve(Some("truecolor")),
            ColorMode::Ansi256
        );
        assert_eq!(
            to_color(ColorMode::TrueColor, [1, 2, 3]),
            Color::Rgb(1, 2, 3)
        );
        assert_eq!(
            to_color(ColorMode::Ansi256, [255, 0, 0]),
            Color::Indexed(196)
        );
    }

    #[test]
    fn lut_spans_range() {
        let lut = build_lut(ColorMap::Gray, ColorMode::TrueColor, -100.0, 0.0);
        assert_eq!(lut.len(), 256);
        assert_eq!(lut[0], Color::Rgb(0, 0, 0));
        // q=240 is 0 dB, q=40 is -100 dB.
        assert_eq!(lut[240], Color::Rgb(255, 255, 255));
        assert_eq!(lut[40], Color::Rgb(0, 0, 0));
        assert_eq!(lut[140], Color::Rgb(128, 128, 128));
    }

    #[test]
    fn zones_and_gradient() {
        assert_eq!(zone_color(-30.0), Color::Green);
        assert_eq!(zone_color(-10.0), Color::Yellow);
        assert_eq!(zone_color(-1.0), Color::Red);
        assert_ne!(zone_color_dim(-1.0), zone_color(-1.0));
        assert_eq!(eq_gradient(0.0), [40, 200, 40]);
        assert_eq!(eq_gradient(1.0), [250, 0, 40]);
        let mid = eq_gradient(0.6);
        assert_eq!(mid, [230, 230, 40]);
    }
}
