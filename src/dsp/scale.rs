//! Frequency-axis scales and pixel-to-FFT-bin mapping.

use clap::ValueEnum;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum FreqScale {
    Log,
    Linear,
    Mel,
}

impl FreqScale {
    pub fn next(self) -> Self {
        match self {
            FreqScale::Log => FreqScale::Mel,
            FreqScale::Mel => FreqScale::Linear,
            FreqScale::Linear => FreqScale::Log,
        }
    }
}

pub fn hz_to_mel(f: f64) -> f64 {
    2595.0 * (1.0 + f / 700.0).log10()
}

pub fn mel_to_hz(m: f64) -> f64 {
    700.0 * (10f64.powf(m / 2595.0) - 1.0)
}

/// Map frequency `f` in `[min, max]` to `[0, 1]` on the given scale.
pub fn to_unit(scale: FreqScale, f: f64, min: f64, max: f64) -> f64 {
    match scale {
        FreqScale::Linear => (f - min) / (max - min),
        FreqScale::Log => (f.max(1e-9) / min).ln() / (max / min).ln(),
        FreqScale::Mel => (hz_to_mel(f) - hz_to_mel(min)) / (hz_to_mel(max) - hz_to_mel(min)),
    }
}

/// Inverse of [`to_unit`].
pub fn from_unit(scale: FreqScale, u: f64, min: f64, max: f64) -> f64 {
    match scale {
        FreqScale::Linear => min + u * (max - min),
        FreqScale::Log => min * (max / min).powf(u),
        FreqScale::Mel => mel_to_hz(hz_to_mel(min) + u * (hz_to_mel(max) - hz_to_mel(min))),
    }
}

/// For `pixels` vertical pixels (index 0 = bottom = `min` Hz) compute the half-open
/// FFT bin range `[lo, hi)` each pixel covers. Every range is non-empty and in bounds.
pub fn pixel_bin_ranges(
    scale: FreqScale,
    pixels: usize,
    min: f64,
    max: f64,
    bin_hz: f64,
    nbins: usize,
) -> Vec<(u32, u32)> {
    let nbins = nbins.max(1);
    let clamp = |b: f64| (b.round().max(0.0) as usize).min(nbins - 1);
    (0..pixels)
        .map(|p| {
            let p0 = p as f64 / pixels as f64;
            let p1 = (p + 1) as f64 / pixels as f64;
            let lo = clamp(from_unit(scale, p0, min, max) / bin_hz);
            let hi = clamp(from_unit(scale, p1, min, max) / bin_hz);
            if hi <= lo {
                let c = clamp(from_unit(scale, (p0 + p1) / 2.0, min, max) / bin_hz);
                (c as u32, c as u32 + 1)
            } else {
                (lo as u32, hi as u32)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_roundtrip() {
        for scale in [FreqScale::Log, FreqScale::Linear, FreqScale::Mel] {
            for &f in &[20.0, 100.0, 1000.0, 5000.0, 20000.0] {
                let u = to_unit(scale, f, 20.0, 20000.0);
                assert!((0.0..=1.0 + 1e-12).contains(&u));
                let back = from_unit(scale, u, 20.0, 20000.0);
                assert!((back - f).abs() / f < 1e-9, "{scale:?} {f} -> {back}");
            }
        }
    }

    #[test]
    fn known_points() {
        // Geometric mean sits in the middle of a log axis.
        assert!((to_unit(FreqScale::Log, 632.455_532, 20.0, 20000.0) - 0.5).abs() < 1e-6);
        assert!((to_unit(FreqScale::Linear, 10010.0, 20.0, 20000.0) - 0.5).abs() < 1e-9);
        assert!((hz_to_mel(1000.0) - 999.985).abs() < 0.01);
        assert!((mel_to_hz(hz_to_mel(440.0)) - 440.0).abs() < 1e-9);
    }

    #[test]
    fn next_cycles() {
        let s = FreqScale::Log;
        assert_eq!(s.next().next().next(), s);
    }

    #[test]
    fn pixel_ranges_cover_and_are_monotonic() {
        let bin_hz = 48000.0 / 2048.0;
        for scale in [FreqScale::Log, FreqScale::Linear, FreqScale::Mel] {
            let r = pixel_bin_ranges(scale, 80, 20.0, 20000.0, bin_hz, 1025);
            assert_eq!(r.len(), 80);
            for w in r.windows(2) {
                assert!(w[0].0 <= w[1].0, "{scale:?} not monotonic");
            }
            for &(lo, hi) in &r {
                assert!(hi > lo && hi as usize <= 1025);
            }
            // Top pixel ends near 20 kHz.
            let top = r.last().unwrap().1 as f64 * bin_hz;
            assert!((top - 20000.0).abs() < 2.0 * bin_hz, "{top}");
        }
    }

    #[test]
    fn pixel_ranges_clamp_beyond_nyquist() {
        let r = pixel_bin_ranges(FreqScale::Linear, 10, 0.0, 30000.0, 100.0, 101);
        for &(lo, hi) in &r {
            assert!(lo < 101 && hi <= 101 && hi > lo);
        }
    }
}
