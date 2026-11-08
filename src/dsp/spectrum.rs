//! Windowed real FFT producing a calibrated power spectrum.

use super::window::{coherent_gain, enbw_bins, make_window, WindowKind};
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use std::sync::Arc;

pub struct Spectrum {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    input: Vec<f32>,
    output: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    scale: f32,
    enbw: f32,
    size: usize,
}

impl Spectrum {
    pub fn new(size: usize, kind: WindowKind) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(size);
        let window = make_window(kind, size);
        let cg = coherent_gain(&window);
        let enbw = enbw_bins(&window) as f32;
        let input = fft.make_input_vec();
        let output = fft.make_output_vec();
        let scratch = fft.make_scratch_vec();
        let amp = 2.0 / (size as f64 * cg);
        Spectrum {
            fft,
            window,
            input,
            output,
            scratch,
            scale: (amp * amp) as f32,
            enbw,
            size,
        }
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn bins(&self) -> usize {
        self.size / 2 + 1
    }

    pub fn enbw(&self) -> f32 {
        self.enbw
    }

    /// Power spectrum of `samples` (length `size`) into `out` (length `bins`),
    /// scaled so a bin-centred sine of amplitude A reads A².
    pub fn power(&mut self, samples: &[f32], out: &mut [f32]) {
        debug_assert_eq!(samples.len(), self.size);
        for ((d, &s), &w) in self.input.iter_mut().zip(samples).zip(&self.window) {
            *d = s * w;
        }
        // Length mismatches are impossible: buffers come from the plan.
        let _ = self
            .fft
            .process_with_scratch(&mut self.input, &mut self.output, &mut self.scratch);
        let scale = self.scale;
        for (o, c) in out.iter_mut().zip(&self.output) {
            *o = c.norm_sqr() * scale;
        }
        // DC and Nyquist have no mirrored negative-frequency half.
        let n = out.len().min(self.output.len());
        if n > 0 {
            out[0] *= 0.25;
            if self.size.is_multiple_of(2) && n == self.size / 2 + 1 {
                out[n - 1] *= 0.25;
            }
        }
    }
}

/// Dominant frequency in bins `[lo, hi]`, refined with parabolic interpolation on
/// the dB magnitude. Returns `(frequency_hz, level_db)` or `None` for silence.
pub fn peak_frequency(power: &[f32], bin_hz: f32, lo: usize, hi: usize) -> Option<(f32, f32)> {
    if power.is_empty() {
        return None;
    }
    let hi = hi.min(power.len() - 1);
    if lo > hi {
        return None;
    }
    let (mut k, mut best) = (lo, power[lo]);
    for (i, &p) in power[lo..=hi].iter().enumerate() {
        if p > best {
            (k, best) = (lo + i, p);
        }
    }
    if power[k] <= 1e-14 {
        return None;
    }
    let db = |i: usize| 10.0 * power[i].max(1e-20).log10();
    let b = db(k);
    if k == 0 || k + 1 >= power.len() {
        return Some((k as f32 * bin_hz, b));
    }
    let (a, c) = (db(k - 1), db(k + 1));
    let denom = a - 2.0 * b + c;
    let p = if denom.abs() > 1e-9 {
        (0.5 * (a - c) / denom).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    Some(((k as f32 + p) * bin_hz, b - 0.25 * (a - c) * p))
}

/// How far bin `k` stands above the median of its neighbourhood, in dB: bins
/// within `reach` of `k`, excluding the `skip` bins either side that belong to
/// its own window main lobe. Tones score high; broadband hiss scores ~10 dB.
pub fn prominence_db(power: &[f32], k: usize, skip: usize, reach: usize) -> f32 {
    const MAX_REACH: usize = 32;
    let reach = reach.min(MAX_REACH);
    let mut near = [0.0f32; 2 * MAX_REACH];
    let mut n = 0;
    for d in skip + 1..=reach {
        for i in [k.checked_sub(d), k.checked_add(d)].into_iter().flatten() {
            if let Some(&p) = power.get(i) {
                near[n] = p;
                n += 1;
            }
        }
    }
    if n == 0 || k >= power.len() {
        return f32::INFINITY;
    }
    let (_, median, _) = near[..n].select_nth_unstable_by(n / 2, f32::total_cmp);
    10.0 * (power[k].max(1e-20) / median.max(1e-20)).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, amp: f32, sr: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / sr).sin())
            .collect()
    }

    #[test]
    fn calibrated_amplitude_all_windows() {
        let n = 2048;
        let sr = 48000.0;
        let bin_hz = sr / n as f32;
        for kind in [
            WindowKind::Rect,
            WindowKind::Hann,
            WindowKind::Hamming,
            WindowKind::Blackman,
            WindowKind::BlackmanHarris,
        ] {
            let mut s = Spectrum::new(n, kind);
            let mut p = vec![0.0; s.bins()];
            s.power(&sine(100.0 * bin_hz, 0.25, sr, n), &mut p);
            let db = 10.0 * p[100].log10();
            assert!((db - 20.0 * 0.25f32.log10()).abs() < 0.05, "{kind:?}: {db}");
        }
    }

    #[test]
    fn silence_is_zero() {
        let mut s = Spectrum::new(512, WindowKind::Hann);
        let mut p = vec![1.0; s.bins()];
        s.power(&vec![0.0; 512], &mut p);
        assert!(p.iter().all(|&v| v == 0.0));
        assert_eq!(peak_frequency(&p, 10.0, 0, 256), None);
        assert_eq!(peak_frequency(&[], 10.0, 0, 256), None);
        assert_eq!(peak_frequency(&p, 10.0, 300, 256), None);
    }

    #[test]
    fn dc_and_bins() {
        let mut s = Spectrum::new(1024, WindowKind::Rect);
        assert_eq!(s.bins(), 513);
        assert_eq!(s.size(), 1024);
        let mut p = vec![0.0; s.bins()];
        s.power(&vec![0.5; 1024], &mut p);
        // A DC offset reads its own power.
        assert!((p[0] - 0.25).abs() < 1e-6, "{}", p[0]);
        let (k, _) = p
            .iter()
            .enumerate()
            .fold((0, 0.0), |m, (i, &v)| if v > m.1 { (i, v) } else { m });
        assert_eq!(k, 0);
    }

    #[test]
    fn peak_frequency_interpolates() {
        let n = 4096;
        let sr = 48000.0;
        let mut s = Spectrum::new(n, WindowKind::Hann);
        let mut p = vec![0.0; s.bins()];
        for &f in &[440.0f32, 1234.5, 3001.7, 9876.0] {
            s.power(&sine(f, 0.5, sr, n), &mut p);
            let (est, db) = peak_frequency(&p, sr / n as f32, 1, s.bins() - 1).unwrap();
            assert!((est - f).abs() < 1.0, "{f} -> {est}");
            // Parabolic fit recovers level within the Hann scalloping bound.
            assert!((db - 20.0 * 0.5f32.log10()).abs() < 1.5, "level {db}");
        }
    }

    #[test]
    fn prominence_separates_tones_from_noise() {
        let n = 4096;
        let sr = 48000.0;
        let mut s = Spectrum::new(n, WindowKind::Hann);
        let mut p = vec![0.0; s.bins()];
        // Deterministic white noise (LCG), with and without a tone on top.
        let mut x = 12345u32;
        let noise: Vec<f32> = (0..n)
            .map(|_| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                (x >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            })
            .collect();
        s.power(&noise, &mut p);
        let bin_hz = sr / n as f32;
        let (f, _) = peak_frequency(&p, bin_hz, 1, s.bins() - 1).unwrap();
        let k = (f / bin_hz).round() as usize;
        assert!(
            prominence_db(&p, k, 4, 24) < 15.0,
            "noise {}",
            prominence_db(&p, k, 4, 24)
        );
        let tone: Vec<f32> = noise
            .iter()
            .zip(sine(1000.0, 0.1, sr, n))
            .map(|(a, b)| 0.1 * a + b)
            .collect();
        s.power(&tone, &mut p);
        let (f, _) = peak_frequency(&p, bin_hz, 1, s.bins() - 1).unwrap();
        let k = (f / bin_hz).round() as usize;
        assert!(prominence_db(&p, k, 4, 24) > 30.0);
        assert_eq!(prominence_db(&[1.0], 0, 4, 24), f32::INFINITY);
        assert_eq!(prominence_db(&p, p.len(), 4, 24), f32::INFINITY);
    }
}
