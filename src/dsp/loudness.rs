//! ITU-R BS.1770-4 / EBU R128 loudness: K-weighting, momentary (400 ms),
//! short-term (3 s) and gated integrated loudness.

use std::collections::VecDeque;
use std::f64::consts::PI;

#[derive(Clone, Copy, Debug, Default)]
pub struct Biquad {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    pub fn new(b0: f64, b1: f64, b2: f64, a1: f64, a2: f64) -> Self {
        Biquad { b0, b1, b2, a1, a2, z1: 0.0, z2: 0.0 }
    }

    /// Transposed direct form II.
    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    /// Magnitude response at `f` Hz for sample rate `fs`.
    pub fn magnitude(&self, f: f64, fs: f64) -> f64 {
        let w = 2.0 * PI * f / fs;
        let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
    }
}

/// K-weighting stage 1 (high-shelf, head effects) for any sample rate.
pub fn k_shelf(fs: f64) -> Biquad {
    let f0 = 1681.974_450_955_533;
    let g = 3.999_843_853_973_347;
    let q = 0.707_175_236_955_419_6;
    let k = (PI * f0 / fs).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let a0 = 1.0 + k / q + k * k;
    Biquad::new(
        (vh + vb * k / q + k * k) / a0,
        2.0 * (k * k - vh) / a0,
        (vh - vb * k / q + k * k) / a0,
        2.0 * (k * k - 1.0) / a0,
        (1.0 - k / q + k * k) / a0,
    )
}

/// K-weighting stage 2 (RLB high-pass) for any sample rate.
pub fn k_highpass(fs: f64) -> Biquad {
    let f0 = 38.135_470_876_024_44;
    let q = 0.500_327_037_323_877_3;
    let k = (PI * f0 / fs).tan();
    let a0 = 1.0 + k / q + k * k;
    Biquad::new(1.0, -2.0, 1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0)
}

pub fn energy_to_lufs(e: f64) -> f32 {
    if e <= 0.0 {
        f32::NEG_INFINITY
    } else {
        (-0.691 + 10.0 * e.log10()) as f32
    }
}

/// Gated integrated loudness over 400 ms block energies (BS.1770-4 §2.8).
pub fn gated_integrated(blocks: &[f64]) -> f32 {
    let abs_gate = 10f64.powf((-70.0 + 0.691) / 10.0);
    let (sum, n) = blocks
        .iter()
        .filter(|&&e| e > abs_gate)
        .fold((0.0, 0usize), |(s, n), &e| (s + e, n + 1));
    if n == 0 {
        return f32::NEG_INFINITY;
    }
    // Relative gate sits 10 LU below the absolute-gated level.
    let rel_gate = (sum / n as f64) * 0.1;
    let gate = rel_gate.max(abs_gate);
    let (sum, n) = blocks
        .iter()
        .filter(|&&e| e > gate)
        .fold((0.0, 0usize), |(s, n), &e| (s + e, n + 1));
    if n == 0 {
        f32::NEG_INFINITY
    } else {
        energy_to_lufs(sum / n as f64)
    }
}

pub struct LoudnessMeter {
    filters: Vec<(Biquad, Biquad)>,
    sub_len: usize,
    sub_count: usize,
    sub_acc: f64,
    /// Most recent 100 ms sub-block energies (up to 30 = 3 s).
    subs: VecDeque<f64>,
    /// Every 400 ms block (75% overlap) for integrated loudness.
    blocks: Vec<f64>,
    integrated: f32,
    max_momentary: f32,
    max_short_term: f32,
}

impl LoudnessMeter {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        let fs = sample_rate as f64;
        LoudnessMeter {
            filters: (0..channels.max(1)).map(|_| (k_shelf(fs), k_highpass(fs))).collect(),
            sub_len: (sample_rate as usize / 10).max(1),
            sub_count: 0,
            sub_acc: 0.0,
            subs: VecDeque::with_capacity(31),
            blocks: Vec::new(),
            integrated: f32::NEG_INFINITY,
            max_momentary: f32::NEG_INFINITY,
            max_short_term: f32::NEG_INFINITY,
        }
    }

    /// Feed one interleaved sample frame (extra channels beyond the meter's are ignored).
    #[inline]
    pub fn process_frame(&mut self, frame: &[f32]) {
        for (x, (s1, s2)) in frame.iter().zip(self.filters.iter_mut()) {
            let y = s2.process(s1.process(*x as f64));
            self.sub_acc += y * y;
        }
        self.sub_count += 1;
        if self.sub_count == self.sub_len {
            self.finish_sub_block();
        }
    }

    fn finish_sub_block(&mut self) {
        let e = self.sub_acc / self.sub_len as f64;
        self.sub_acc = 0.0;
        self.sub_count = 0;
        if self.subs.len() == 30 {
            self.subs.pop_front();
        }
        self.subs.push_back(e);
        if self.subs.len() >= 4 {
            let block = self.subs.iter().rev().take(4).sum::<f64>() / 4.0;
            self.blocks.push(block);
            self.integrated = gated_integrated(&self.blocks);
            self.max_momentary = self.max_momentary.max(energy_to_lufs(block));
            if self.subs.len() == 30 {
                self.max_short_term = self.max_short_term.max(self.short_term());
            }
        }
    }

    fn mean_last(&self, n: usize) -> f32 {
        let k = self.subs.len().min(n);
        if k == 0 {
            return f32::NEG_INFINITY;
        }
        energy_to_lufs(self.subs.iter().rev().take(k).sum::<f64>() / k as f64)
    }

    /// Momentary loudness (400 ms window).
    pub fn momentary(&self) -> f32 {
        self.mean_last(4)
    }

    /// Short-term loudness (3 s window).
    pub fn short_term(&self) -> f32 {
        self.mean_last(30)
    }

    pub fn integrated(&self) -> f32 {
        self.integrated
    }

    pub fn max_momentary(&self) -> f32 {
        self.max_momentary
    }

    pub fn max_short_term(&self) -> f32 {
        self.max_short_term
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_sine(m: &mut LoudnessMeter, freq: f64, amp: f64, sr: u32, secs: f64, ch: usize) {
        let n = (secs * sr as f64) as usize;
        let mut frame = vec![0.0f32; ch];
        for i in 0..n {
            let s = (amp * (2.0 * PI * freq * i as f64 / sr as f64).sin()) as f32;
            frame.iter_mut().for_each(|x| *x = s);
            m.process_frame(&frame);
        }
    }

    #[test]
    fn coefficients_match_bs1770_at_48k() {
        let s = k_shelf(48000.0);
        assert!((s.b0 - 1.535_124_859_586_97).abs() < 1e-9);
        assert!((s.b1 + 2.691_696_189_406_38).abs() < 1e-9);
        assert!((s.b2 - 1.198_392_810_852_85).abs() < 1e-9);
        assert!((s.a1 + 1.690_659_293_182_41).abs() < 1e-9);
        assert!((s.a2 - 0.732_480_774_215_85).abs() < 1e-9);
        let h = k_highpass(48000.0);
        assert!((h.a1 + 1.990_047_454_833_98).abs() < 1e-9);
        assert!((h.a2 - 0.990_072_250_366_21).abs() < 1e-9);
    }

    #[test]
    fn k_weighting_response_shape() {
        for fs in [44100.0, 48000.0, 96000.0] {
            let (s, h) = (k_shelf(fs), k_highpass(fs));
            let g = |f: f64| 20.0 * (s.magnitude(f, fs) * h.magnitude(f, fs)).log10();
            assert!(g(1000.0).abs() < 0.8, "1k at {fs}: {}", g(1000.0));
            assert!((g(10000.0) - 4.0).abs() < 0.5, "shelf at {fs}");
            assert!(g(20.0) < -10.0, "high-pass at {fs}");
        }
    }

    #[test]
    fn full_scale_997hz_mono_reads_minus_3() {
        let mut m = LoudnessMeter::new(48000, 1);
        feed_sine(&mut m, 997.0, 1.0, 48000, 3.0, 1);
        assert!((m.momentary() + 3.01).abs() < 0.1, "M {}", m.momentary());
        assert!((m.short_term() + 3.01).abs() < 0.1, "S {}", m.short_term());
        assert!((m.integrated() + 3.01).abs() < 0.1, "I {}", m.integrated());
    }

    #[test]
    fn stereo_adds_3db_and_minus_23_target() {
        let mut m = LoudnessMeter::new(44100, 2);
        // A stereo 997 Hz tone reads its peak amplitude in dBFS as LUFS
        // (+3.01 dB for two channels cancels the -3.01 dB sine RMS).
        let amp = 10f64.powf(-23.0 / 20.0);
        feed_sine(&mut m, 997.0, amp, 44100, 4.0, 2);
        assert!((m.integrated() + 23.0).abs() < 0.15, "I {}", m.integrated());
        assert!((m.max_momentary() - m.integrated()).abs() < 0.3);
        assert!(m.max_short_term().is_finite());
    }

    #[test]
    fn silence_is_negative_infinity() {
        let mut m = LoudnessMeter::new(48000, 2);
        assert_eq!(m.momentary(), f32::NEG_INFINITY);
        for _ in 0..48000 {
            m.process_frame(&[0.0, 0.0]);
        }
        assert_eq!(m.integrated(), f32::NEG_INFINITY);
        assert_eq!(m.momentary(), f32::NEG_INFINITY);
    }

    #[test]
    fn gating() {
        assert_eq!(gated_integrated(&[]), f32::NEG_INFINITY);
        // Blocks below -70 LUFS are discarded by the absolute gate.
        let quiet = 10f64.powf((-80.0 + 0.691) / 10.0);
        assert_eq!(gated_integrated(&[quiet; 10]), f32::NEG_INFINITY);
        // One loud block at -20 and many at -40: -40 blocks fall below the relative gate.
        let loud = 10f64.powf((-20.0 + 0.691) / 10.0);
        let soft = 10f64.powf((-40.0 + 0.691) / 10.0);
        let mut blocks = vec![soft; 5];
        blocks.push(loud);
        let i = gated_integrated(&blocks);
        assert!((i + 20.0).abs() < 1e-3, "{i}");
        assert_eq!(energy_to_lufs(0.0), f32::NEG_INFINITY);
    }

    #[test]
    fn biquad_identity() {
        let mut b = Biquad::new(1.0, 0.0, 0.0, 0.0, 0.0);
        assert_eq!(b.process(0.3), 0.3);
        assert!((b.magnitude(1000.0, 48000.0) - 1.0).abs() < 1e-12);
    }
}
