//! Window functions for spectral analysis.

use clap::ValueEnum;
use std::f64::consts::PI;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum WindowKind {
    Hann,
    Hamming,
    Blackman,
    BlackmanHarris,
    Rect,
}

/// Build a periodic (DFT-even) window of length `n`.
pub fn make_window(kind: WindowKind, n: usize) -> Vec<f32> {
    let nf = n as f64;
    (0..n)
        .map(|i| {
            let x = 2.0 * PI * i as f64 / nf;
            let w = match kind {
                WindowKind::Rect => 1.0,
                WindowKind::Hann => 0.5 - 0.5 * x.cos(),
                WindowKind::Hamming => 0.54 - 0.46 * x.cos(),
                WindowKind::Blackman => 0.42 - 0.5 * x.cos() + 0.08 * (2.0 * x).cos(),
                WindowKind::BlackmanHarris => {
                    0.35875 - 0.48829 * x.cos() + 0.14128 * (2.0 * x).cos()
                        - 0.01168 * (3.0 * x).cos()
                }
            };
            w as f32
        })
        .collect()
}

/// Coherent gain: the mean of the window (amplitude scaling of a tone).
pub fn coherent_gain(w: &[f32]) -> f64 {
    if w.is_empty() {
        return 0.0;
    }
    w.iter().map(|&x| x as f64).sum::<f64>() / w.len() as f64
}

/// Equivalent noise bandwidth in bins.
pub fn enbw_bins(w: &[f32]) -> f64 {
    let s1: f64 = w.iter().map(|&x| x as f64).sum();
    let s2: f64 = w.iter().map(|&x| (x as f64) * (x as f64)).sum();
    if s1 == 0.0 {
        return 0.0;
    }
    w.len() as f64 * s2 / (s1 * s1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn hann_shape() {
        let w = make_window(WindowKind::Hann, 1024);
        assert_eq!(w.len(), 1024);
        assert!(w[0].abs() < 1e-7);
        assert!((w[512] - 1.0).abs() < 1e-6);
        for i in 1..512 {
            assert!((w[i] - w[1024 - i]).abs() < 1e-6, "periodic symmetry at {i}");
        }
    }

    #[test]
    fn known_gains_and_enbw() {
        let n = 4096;
        let cases = [
            (WindowKind::Rect, 1.0, 1.0),
            (WindowKind::Hann, 0.5, 1.5),
            (WindowKind::Hamming, 0.54, 1.3628),
            (WindowKind::Blackman, 0.42, 1.7268),
            (WindowKind::BlackmanHarris, 0.35875, 2.0044),
        ];
        for (kind, cg, enbw) in cases {
            let w = make_window(kind, n);
            assert!(close(coherent_gain(&w), cg, 1e-4), "{kind:?} cg {}", coherent_gain(&w));
            assert!(close(enbw_bins(&w), enbw, 1e-3), "{kind:?} enbw {}", enbw_bins(&w));
        }
    }

    #[test]
    fn empty_window() {
        assert_eq!(coherent_gain(&[]), 0.0);
        assert_eq!(enbw_bins(&[]), 0.0);
        assert!(make_window(WindowKind::Hann, 0).is_empty());
    }
}
