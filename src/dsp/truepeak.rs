//! True-peak level (ITU-R BS.1770-4 Annex 2): the signal is oversampled 4×
//! so peaks between samples, which can be up to about 3 dB above the highest
//! sample, are caught.

/// Oversampling factor and taps per polyphase branch (48 taps in all, as in
/// the BS.1770 reference filter).
const PHASES: usize = 4;
const TAPS: usize = 12;

/// Polyphase 4× interpolator for one channel.
#[derive(Clone)]
pub struct TruePeak {
    coefs: [[f32; TAPS]; PHASES],
    /// The last `TAPS` samples, stored twice so a window is always contiguous.
    hist: [f32; 2 * TAPS],
    pos: usize,
}

impl Default for TruePeak {
    fn default() -> Self {
        Self::new()
    }
}

impl TruePeak {
    pub fn new() -> Self {
        // Windowed-sinc low-pass at the original Nyquist frequency, split into
        // phases; each phase is normalized to unity gain at DC.
        let n = PHASES * TAPS;
        let centre = (n - 1) as f64 / 2.0;
        let h: Vec<f64> = (0..n)
            .map(|i| {
                let x = (i as f64 - centre) / PHASES as f64;
                let sinc = if x == 0.0 {
                    1.0
                } else {
                    (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
                };
                let t = 2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64;
                let blackman = 0.42 - 0.5 * t.cos() + 0.08 * (2.0 * t).cos();
                sinc * blackman
            })
            .collect();
        let mut coefs = [[0.0f32; TAPS]; PHASES];
        for (p, phase) in coefs.iter_mut().enumerate() {
            let sum: f64 = (0..TAPS).map(|k| h[p + PHASES * k]).sum();
            for (k, c) in phase.iter_mut().enumerate() {
                *c = (h[p + PHASES * k] / sum) as f32;
            }
        }
        TruePeak {
            coefs,
            hist: [0.0; 2 * TAPS],
            pos: 0,
        }
    }

    /// Feed one sample; returns the largest magnitude among the interpolated
    /// points up to it.
    #[inline]
    pub fn push(&mut self, x: f32) -> f32 {
        self.pos = if self.pos == 0 {
            TAPS - 1
        } else {
            self.pos - 1
        };
        self.hist[self.pos] = x;
        self.hist[self.pos + TAPS] = x;
        let w = &self.hist[self.pos..self.pos + TAPS];
        let mut peak = 0.0f32;
        for phase in &self.coefs {
            let y: f32 = phase.iter().zip(w).map(|(c, s)| c * s).sum();
            peak = peak.max(y.abs());
        }
        peak
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn true_peak(freq: f64, phase: f64, amp: f64) -> f32 {
        let mut tp = TruePeak::new();
        (0..4800)
            .map(|i| {
                tp.push(
                    (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / 48000.0 + phase).sin())
                        as f32,
                )
            })
            .skip(100)
            .fold(0.0, f32::max)
    }

    #[test]
    fn catches_peaks_between_samples() {
        // 12 kHz at 48 kHz sampled 45° off its peaks: every sample is at
        // 0.707, 3 dB below the real peak of 1.0.
        let tp = true_peak(12000.0, std::f64::consts::FRAC_PI_4, 1.0);
        assert!((20.0 * tp.log10()).abs() < 0.3, "{} dB", 20.0 * tp.log10());
    }

    #[test]
    fn matches_the_sample_peak_of_low_frequencies() {
        let tp = true_peak(1000.0, 0.3, 0.5);
        assert!((20.0 * (tp / 0.5).log10()).abs() < 0.05, "{tp}");
        assert_eq!(TruePeak::new().push(0.0), 0.0);
    }
}
