//! Turns a stream of interleaved samples into analysis frames, one per hop.

use crate::dsp::bands::BandLayout;
use crate::dsp::level::{amp_to_db, peak_decay, power_to_db, FLOOR_DB};
use crate::dsp::loudness::LoudnessMeter;
use crate::dsp::quant::db_to_q;
use crate::dsp::spectrum::{peak_frequency, prominence_db, Spectrum};
use crate::dsp::truepeak::TruePeak;
use crate::dsp::weighting::{power_gain_table, Weighting};
use crate::dsp::window::WindowKind;

/// Channels tracked individually by the level meters.
pub const MAX_CHANNELS: usize = 8;
/// Samples at or above this magnitude count as clipped.
pub const CLIP_LEVEL: f32 = 0.9999;
/// A dominant frequency must stand this far above the median of nearby bins.
const PEAK_PROMINENCE_DB: f32 = 15.0;
/// Bins either side of the peak excluded as its own main lobe, and the
/// neighbourhood the median is taken over.
const PEAK_SKIP_BINS: usize = 4;
const PEAK_REACH_BINS: usize = 24;
/// The dominant frequency stays on the previous tone while that tone is within
/// this many dB of the strongest one, so it does not flip between harmonics.
const TONE_STICK_DB: f32 = 3.0;
/// Peak-hold marker fall rate once its hold time is over, dB/s.
pub const HOLD_FALL_DB_S: f32 = 20.0;
/// Equalizer band ballistics: the share of the gap to a new reading closed per
/// 1/30 s when rising and when falling (about 50 ms and 260 ms).
const BAND_ATTACK: f32 = 0.5;
const BAND_RELEASE: f32 = 0.12;
/// A smoothed band this close to its target snaps to it, dB.
const BAND_SNAP_DB: f32 = 0.1;
/// Equalizer peak caps hold this long, then fall at `BAND_CAP_FALL_DB_S`.
const BAND_CAP_HOLD_S: f32 = 1.0;
const BAND_CAP_FALL_DB_S: f32 = 30.0;
/// VU needle (IEC 60268-17): a damped spring that reaches 99% of a step in
/// 300 ms with about 1% overshoot, driven by the full-wave rectified signal.
const VU_RISE_S: f64 = 0.3;
const VU_OVERSHOOT: f64 = 0.01;
/// Scales the rectified average so a sine reads its level in dBFS as AES17
/// defines it (its peak), so a -18 dBFS alignment tone reads -18 (π / 2).
const VU_CAL: f32 = std::f32::consts::FRAC_PI_2;

#[derive(Clone, Debug)]
pub struct AnalyzerConfig {
    pub sample_rate: u32,
    pub channels: usize,
    pub fft_size: usize,
    pub hop: usize,
    pub window: WindowKind,
    pub weighting: Weighting,
    /// Number of log-spaced equalizer bands, or `None` for the standard 1/3-octave bands.
    pub bands: Option<usize>,
    pub min_freq: f32,
    pub max_freq: f32,
    /// Length of the sliding RMS window, seconds.
    pub rms_tau: f32,
    /// Peak meter fall-back rate, dB/s.
    pub peak_decay_db_s: f32,
    /// How long the peak-hold marker holds before falling, seconds.
    pub peak_hold_s: f32,
    /// Equalizer bands are clamped to this floor (the display floor, dB)
    /// before smoothing, so a band rising from silence is not held back.
    pub band_floor_db: f32,
    /// Channel feeding the spectrum (0-based); `None` mixes all channels.
    pub spectrum_channel: Option<usize>,
}

impl AnalyzerConfig {
    pub fn bin_hz(&self) -> f32 {
        self.sample_rate as f32 / self.fft_size as f32
    }

    pub fn frame_dt(&self) -> f64 {
        self.hop as f64 / self.sample_rate as f64
    }

    /// Upper display frequency, clamped to Nyquist.
    pub fn top_freq(&self) -> f32 {
        self.max_freq.min(self.sample_rate as f32 / 2.0)
    }
}

#[derive(Clone, Debug)]
pub struct Frame {
    /// Seconds since the start of the stream, at the end of this hop.
    pub time: f64,
    pub channels: u8,
    /// Sample peak over the hop, dBFS.
    pub peak: [f32; MAX_CHANNELS],
    /// True peak over the hop (BS.1770-4, 4× oversampled), dBTP.
    pub true_peak: [f32; MAX_CHANNELS],
    /// Peak with meter fall-back applied, dBFS.
    pub peak_decay: [f32; MAX_CHANNELS],
    /// RMS over the hop, dBFS.
    pub rms: [f32; MAX_CHANNELS],
    /// RMS over the sliding `rms_tau` window, dBFS.
    pub rms_avg: [f32; MAX_CHANNELS],
    /// Highest peak, held for `peak_hold_s` and then falling, dBFS.
    pub hold: [f32; MAX_CHANNELS],
    /// Highest point the VU needle reached during the hop, dBFS of a sine
    /// with the same reading (AES17: a sine's peak).
    pub vu: [f32; MAX_CHANNELS],
    /// Highest VU reading, held for `peak_hold_s` and then falling.
    pub vu_hold: [f32; MAX_CHANNELS],
    pub lufs_m: f32,
    pub lufs_s: f32,
    pub lufs_i: f32,
    /// Loudness range so far (EBU Tech 3342), LU; NaN before 3 s of signal.
    pub lufs_lra: f32,
    pub peak_freq: Option<(f32, f32)>,
    pub clipped: bool,
    /// Quantized power spectrum (see [`crate::dsp::quant`]).
    pub spectrum: Box<[u8]>,
    /// Quantized equalizer band levels, with rise/fall smoothing.
    pub bands: Box<[u8]>,
    /// Quantized equalizer peak caps (held, then falling).
    pub band_caps: Box<[u8]>,
}

impl Frame {
    pub fn n(&self) -> usize {
        self.channels as usize
    }

    pub fn max_peak(&self) -> f32 {
        self.peak[..self.n()]
            .iter()
            .copied()
            .fold(FLOOR_DB, f32::max)
    }

    pub fn max_rms(&self) -> f32 {
        self.rms[..self.n()]
            .iter()
            .copied()
            .fold(FLOOR_DB, f32::max)
    }
}

/// Whole-stream statistics for reports.
#[derive(Clone, Debug)]
pub struct Stats {
    pub frames: u64,
    pub peak: Vec<f32>,
    /// True peak (linear) of each metered channel.
    pub true_peak: Vec<f32>,
    pub sumsq: Vec<f64>,
    pub clipped: Vec<u64>,
    pub spectrum_sum: Vec<f64>,
    pub spectra: u64,
}

impl Stats {
    fn new(channels: usize, bins: usize) -> Self {
        Stats {
            frames: 0,
            peak: vec![0.0; channels],
            true_peak: vec![0.0; channels],
            sumsq: vec![0.0; channels],
            clipped: vec![0; channels],
            spectrum_sum: vec![0.0; bins],
            spectra: 0,
        }
    }

    pub fn rms(&self, ch: usize) -> f32 {
        if self.frames == 0 {
            return 0.0;
        }
        (self.sumsq[ch] / self.frames as f64).sqrt() as f32
    }

    pub fn mean_spectrum(&self) -> Vec<f32> {
        let n = self.spectra.max(1) as f64;
        self.spectrum_sum.iter().map(|&s| (s / n) as f32).collect()
    }
}

pub struct Analyzer {
    cfg: AnalyzerConfig,
    spectrum: Spectrum,
    weights: Vec<f32>,
    pub bands: BandLayout,
    ring: Vec<f32>,
    ring_pos: usize,
    lin: Vec<f32>,
    power: Vec<f32>,
    band_db: Vec<f32>,
    hop_count: usize,
    hop_peak: [f32; MAX_CHANNELS],
    hop_tp: [f32; MAX_CHANNELS],
    true_peak: Vec<TruePeak>,
    hop_sumsq: [f64; MAX_CHANNELS],
    hop_clip: bool,
    /// Per-hop mean squares of the last `rms_win.len()` hops (a ring).
    rms_win: Vec<[f32; MAX_CHANNELS]>,
    rms_pos: usize,
    peak_hold: [f32; MAX_CHANNELS],
    hold: [f32; MAX_CHANNELS],
    hold_age: [f32; MAX_CHANNELS],
    band_smooth: Vec<f32>,
    band_cap: Vec<f32>,
    band_cap_age: Vec<f32>,
    band_coef: (f32, f32),
    tone: Option<f32>,
    /// VU needle position and velocity per channel, and its hop maximum.
    needle: [f32; MAX_CHANNELS],
    needle_v: [f32; MAX_CHANNELS],
    hop_needle: [f32; MAX_CHANNELS],
    vu_hold: [f32; MAX_CHANNELS],
    vu_hold_age: [f32; MAX_CHANNELS],
    /// Per-sample spring coefficients: ω²·dt, 2ζω·dt and dt.
    vu_coef: (f32, f32, f32),
    pub loudness: LoudnessMeter,
    samples: u64,
    peak_bins: (usize, usize),
    quant: PowerQuant,
    pub stats: Stats,
}

/// Table-driven `db_to_q(power_to_db(p))`, bit-identical to the direct form.
/// Positive f32 bit patterns order like their values, so each quantization step
/// starts at a fixed bit threshold. A coarse table indexed by exponent and top
/// mantissa bits (1/16 octave, well under one 0.5 dB step, so at most one
/// threshold per cell) gives the step at the cell start; one compare finishes.
struct PowerQuant {
    /// `thresholds[q]`: smallest bit pattern quantizing to at least `q`.
    thresholds: [u32; 257],
    coarse: Box<[u8]>,
}

impl PowerQuant {
    const SHIFT: u32 = 19;

    fn reference(bits: u32) -> u8 {
        db_to_q(power_to_db(f32::from_bits(bits)))
    }

    fn new() -> Self {
        let mut thresholds = [u32::MAX; 257];
        thresholds[0] = 0;
        let top = f32::INFINITY.to_bits();
        for (q, t) in thresholds.iter_mut().enumerate().take(256).skip(1) {
            if Self::reference(top) < q as u8 {
                continue;
            }
            let (mut lo, mut hi) = (0u32, top);
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                if Self::reference(mid) >= q as u8 {
                    hi = mid;
                } else {
                    lo = mid + 1;
                }
            }
            *t = lo;
        }
        let coarse = (0..=top >> Self::SHIFT)
            .map(|i| Self::reference(i << Self::SHIFT))
            .collect();
        PowerQuant { thresholds, coarse }
    }

    #[inline]
    fn quantize(&self, p: f32) -> u8 {
        let bits = p.to_bits();
        match self.coarse.get((bits >> Self::SHIFT) as usize) {
            Some(&q) if p > 0.0 => q + (bits >= self.thresholds[q as usize + 1]) as u8,
            _ => Self::reference(bits),
        }
    }
}

impl Analyzer {
    pub fn new(cfg: AnalyzerConfig) -> Self {
        let spectrum = Spectrum::new(cfg.fft_size, cfg.window);
        let bins = spectrum.bins();
        let bin_hz = cfg.bin_hz();
        let top = cfg.top_freq();
        let bands = match cfg.bands {
            Some(n) => BandLayout::new(n, cfg.min_freq, top, bin_hz, bins),
            None => BandLayout::third_octave(cfg.min_freq, top, bin_hz, bins),
        };
        let peak_bins = (
            ((cfg.min_freq / bin_hz).floor() as usize).max(1),
            ((top / bin_hz).ceil() as usize).min(bins - 1),
        );
        Analyzer {
            weights: power_gain_table(cfg.weighting, bins, bin_hz as f64),
            ring: vec![0.0; cfg.fft_size],
            ring_pos: 0,
            lin: vec![0.0; cfg.fft_size],
            power: vec![0.0; bins],
            band_db: vec![0.0; bands.len()],
            hop_count: 0,
            hop_peak: [0.0; MAX_CHANNELS],
            hop_tp: [0.0; MAX_CHANNELS],
            true_peak: vec![TruePeak::new(); cfg.channels.clamp(1, MAX_CHANNELS)],
            hop_sumsq: [0.0; MAX_CHANNELS],
            hop_clip: false,
            rms_win: vec![
                [0.0; MAX_CHANNELS];
                ((cfg.rms_tau as f64 / cfg.frame_dt()).round() as usize).max(1)
            ],
            rms_pos: 0,
            peak_hold: [FLOOR_DB; MAX_CHANNELS],
            hold: [FLOOR_DB; MAX_CHANNELS],
            hold_age: [0.0; MAX_CHANNELS],
            band_smooth: vec![cfg.band_floor_db; bands.len()],
            band_cap: vec![cfg.band_floor_db; bands.len()],
            band_cap_age: vec![0.0; bands.len()],
            band_coef: {
                let steps = cfg.frame_dt() as f32 * 30.0;
                (
                    1.0 - (1.0 - BAND_ATTACK).powf(steps),
                    1.0 - (1.0 - BAND_RELEASE).powf(steps),
                )
            },
            tone: None,
            needle: [0.0; MAX_CHANNELS],
            needle_v: [0.0; MAX_CHANNELS],
            hop_needle: [f32::NEG_INFINITY; MAX_CHANNELS],
            vu_hold: [FLOOR_DB; MAX_CHANNELS],
            vu_hold_age: [0.0; MAX_CHANNELS],
            vu_coef: {
                let (zeta, omega) = vu_spring();
                let dt = 1.0 / cfg.sample_rate as f64;
                (
                    (omega * omega * dt) as f32,
                    (2.0 * zeta * omega * dt) as f32,
                    dt as f32,
                )
            },
            loudness: LoudnessMeter::new(cfg.sample_rate, cfg.channels),
            samples: 0,
            peak_bins,
            quant: PowerQuant::new(),
            stats: Stats::new(cfg.channels, bins),
            bands,
            spectrum,
            cfg,
        }
    }

    pub fn config(&self) -> &AnalyzerConfig {
        &self.cfg
    }

    pub fn bins(&self) -> usize {
        self.spectrum.bins()
    }

    /// Follow a change of the display floor (see `band_floor_db`).
    pub fn set_band_floor(&mut self, db: f32) {
        self.cfg.band_floor_db = db;
    }

    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// Feed interleaved samples; `emit` is called once per completed hop.
    pub fn push(&mut self, data: &[f32], mut emit: impl FnMut(Frame)) {
        let nch = self.cfg.channels.max(1);
        let metered = nch.min(MAX_CHANNELS);
        let inv = 1.0 / nch as f32;
        for frame in data.chunks_exact(nch) {
            let mut mix = 0.0f32;
            for (c, &s) in frame.iter().enumerate() {
                let a = s.abs();
                if c < metered {
                    if a > self.hop_peak[c] {
                        self.hop_peak[c] = a;
                    }
                    self.hop_tp[c] = self.hop_tp[c].max(self.true_peak[c].push(s));
                    self.hop_sumsq[c] += (s as f64) * (s as f64);
                    let (k, d, dt) = self.vu_coef;
                    self.needle_v[c] += k * (a * VU_CAL - self.needle[c]) - d * self.needle_v[c];
                    self.needle[c] += self.needle_v[c] * dt;
                    self.hop_needle[c] = self.hop_needle[c].max(self.needle[c]);
                }
                if a >= CLIP_LEVEL {
                    self.hop_clip = true;
                    self.stats.clipped[c] += 1;
                }
                if a > self.stats.peak[c] {
                    self.stats.peak[c] = a;
                }
                self.stats.sumsq[c] += (s as f64) * (s as f64);
                mix += s;
            }
            let mono = match self.cfg.spectrum_channel {
                Some(c) => frame[c],
                None => mix * inv,
            };
            self.ring[self.ring_pos] = mono;
            self.ring_pos += 1;
            if self.ring_pos == self.ring.len() {
                self.ring_pos = 0;
            }
            self.loudness.process_frame(frame);
            self.samples += 1;
            self.stats.frames += 1;
            self.hop_count += 1;
            if self.hop_count == self.cfg.hop {
                emit(self.finish_hop());
            }
        }
    }

    fn finish_hop(&mut self) -> Frame {
        let (newer, older) = self.ring.split_at(self.ring_pos);
        self.lin[..older.len()].copy_from_slice(older);
        self.lin[older.len()..].copy_from_slice(newer);
        self.spectrum.power(&self.lin, &mut self.power);
        for (p, &w) in self.power.iter_mut().zip(&self.weights) {
            *p *= w;
        }
        for (acc, &p) in self.stats.spectrum_sum.iter_mut().zip(&self.power) {
            *acc += p as f64;
        }
        self.stats.spectra += 1;
        let spectrum: Box<[u8]> = self.power.iter().map(|&p| self.quant.quantize(p)).collect();
        self.bands
            .compute(&self.power, self.spectrum.enbw(), &mut self.band_db);
        let dt = self.cfg.frame_dt() as f32;
        // Raw bands are jittery: each band rises fast and falls slowly, from
        // the display floor so silence does not leave it climbing up through
        // invisible levels, and the caps follow the smoothed level.
        let (attack, release) = self.band_coef;
        for (i, &raw) in self.band_db.iter().enumerate() {
            let target = raw.max(self.cfg.band_floor_db);
            let s = &mut self.band_smooth[i];
            let gap = target - *s;
            *s = if gap.abs() < BAND_SNAP_DB {
                target
            } else {
                *s + gap * if gap > 0.0 { attack } else { release }
            };
            let (cap, age) = (&mut self.band_cap[i], &mut self.band_cap_age[i]);
            hold_then_fall(cap, age, *s, BAND_CAP_HOLD_S, BAND_CAP_FALL_DB_S, dt);
        }
        let bands: Box<[u8]> = self.band_smooth.iter().map(|&d| db_to_q(d)).collect();
        let band_caps: Box<[u8]> = self.band_cap.iter().map(|&d| db_to_q(d)).collect();
        // Only report a dominant frequency that stands clearly above its
        // surroundings; in a quiet room the loudest bin is just hiss.
        let bin_hz = self.cfg.bin_hz();
        let prominent = |hz: f32, power: &[f32]| {
            prominence_db(
                power,
                (hz / bin_hz).round() as usize,
                PEAK_SKIP_BINS,
                PEAK_REACH_BINS,
            ) >= PEAK_PROMINENCE_DB
        };
        let (lo, hi) = self.peak_bins;
        let mut peak_freq = peak_frequency(&self.power, bin_hz, lo, hi)
            .filter(|&(hz, _)| prominent(hz, &self.power));
        // Stay on the previous tone while it is still nearly as strong.
        if let (Some((_, db)), Some(prev)) = (peak_freq, self.tone) {
            let b = (prev / bin_hz).round() as usize;
            let near = peak_frequency(
                &self.power,
                bin_hz,
                b.saturating_sub(PEAK_SKIP_BINS).max(lo),
                (b + PEAK_SKIP_BINS).min(hi),
            );
            if let Some(n) =
                near.filter(|&(hz, ndb)| ndb >= db - TONE_STICK_DB && prominent(hz, &self.power))
            {
                peak_freq = Some(n);
            }
        }
        self.tone = peak_freq.map(|(hz, _)| hz);

        let metered = self.cfg.channels.clamp(1, MAX_CHANNELS);
        let mut peak = [FLOOR_DB; MAX_CHANNELS];
        let mut true_peak = [FLOOR_DB; MAX_CHANNELS];
        let mut rms = [FLOOR_DB; MAX_CHANNELS];
        let mut rms_avg = [FLOOR_DB; MAX_CHANNELS];
        let mut vu = [FLOOR_DB; MAX_CHANNELS];
        let n = self.hop_count.max(1) as f64;
        let slot = self.rms_pos;
        self.rms_pos = (self.rms_pos + 1) % self.rms_win.len();
        for c in 0..metered {
            let ms = (self.hop_sumsq[c] / n) as f32;
            rms[c] = power_to_db(ms);
            self.rms_win[slot][c] = ms;
            let sum: f32 = self.rms_win.iter().map(|w| w[c]).sum();
            rms_avg[c] = power_to_db(sum / self.rms_win.len() as f32);
            peak[c] = amp_to_db(self.hop_peak[c]);
            // Never below the sample peak, whatever the interpolator's ripple.
            let tp = self.hop_tp[c].max(self.hop_peak[c]);
            true_peak[c] = amp_to_db(tp);
            self.stats.true_peak[c] = self.stats.true_peak[c].max(tp);
            self.peak_hold[c] =
                peak_decay(self.peak_hold[c], peak[c], self.cfg.peak_decay_db_s, dt);
            hold_then_fall(
                &mut self.hold[c],
                &mut self.hold_age[c],
                peak[c],
                self.cfg.peak_hold_s,
                HOLD_FALL_DB_S,
                dt,
            );
            self.hold[c] = self.hold[c].max(self.peak_hold[c]);
            vu[c] = amp_to_db(self.hop_needle[c]);
            hold_then_fall(
                &mut self.vu_hold[c],
                &mut self.vu_hold_age[c],
                vu[c],
                self.cfg.peak_hold_s,
                HOLD_FALL_DB_S,
                dt,
            );
        }
        let frame = Frame {
            time: self.samples as f64 / self.cfg.sample_rate as f64,
            channels: metered as u8,
            peak,
            true_peak,
            peak_decay: self.peak_hold,
            rms,
            rms_avg,
            hold: self.hold,
            vu,
            vu_hold: self.vu_hold,
            lufs_m: self.loudness.momentary(),
            lufs_s: self.loudness.short_term(),
            lufs_i: self.loudness.integrated(),
            lufs_lra: self.loudness.loudness_range(),
            peak_freq,
            clipped: self.hop_clip,
            spectrum,
            bands,
            band_caps,
        };
        self.hop_count = 0;
        self.hop_peak = [0.0; MAX_CHANNELS];
        self.hop_tp = [0.0; MAX_CHANNELS];
        self.hop_sumsq = [0.0; MAX_CHANNELS];
        self.hop_needle = [f32::NEG_INFINITY; MAX_CHANNELS];
        self.hop_clip = false;
        frame
    }
}

/// Damping ratio and natural frequency (rad/s) of the VU needle: ζ gives the
/// overshoot, and ω is set so the step response first reaches 99% at
/// `VU_RISE_S`.
fn vu_spring() -> (f64, f64) {
    let l = VU_OVERSHOOT.ln();
    let zeta = -l / (std::f64::consts::PI.powi(2) + l * l).sqrt();
    let wd = (1.0 - zeta * zeta).sqrt();
    // Unit step response at ω = 1, rising until its peak at t = π / ωd.
    let step = |t: f64| 1.0 - (-zeta * t).exp() * ((wd * t).cos() + zeta / wd * (wd * t).sin());
    let (mut lo, mut hi) = (0.0, std::f64::consts::PI / wd);
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if step(mid) < 0.99 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (zeta, lo / VU_RISE_S)
}

/// Peak hold: jump up to `level` at once, hold for `hold_s`, then fall at
/// `fall_db_s`, never below `level`.
fn hold_then_fall(v: &mut f32, age: &mut f32, level: f32, hold_s: f32, fall_db_s: f32, dt: f32) {
    if level >= *v {
        *v = level;
        *age = 0.0;
    } else {
        *age += dt;
        if *age > hold_s {
            *v = (*v - fall_db_s * dt).max(level);
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::dsp::quant::q_to_db;

    pub fn config(channels: usize) -> AnalyzerConfig {
        AnalyzerConfig {
            sample_rate: 48000,
            channels,
            fft_size: 2048,
            hop: 512,
            window: WindowKind::Hann,
            weighting: Weighting::Z,
            bands: Some(16),
            min_freq: 20.0,
            max_freq: 20000.0,
            rms_tau: 0.3,
            peak_decay_db_s: 20.0,
            peak_hold_s: 1.5,
            band_floor_db: -70.0,
            spectrum_channel: None,
        }
    }

    fn stereo_sine(freq: f32, amp_l: f32, amp_r: f32, n: usize) -> Vec<f32> {
        let mut v = Vec::with_capacity(n * 2);
        for i in 0..n {
            let s = (2.0 * std::f32::consts::PI * freq * i as f32 / 48000.0).sin();
            v.push(amp_l * s);
            v.push(amp_r * s);
        }
        v
    }

    fn run(cfg: AnalyzerConfig, data: &[f32]) -> (Analyzer, Vec<Frame>) {
        let mut a = Analyzer::new(cfg);
        let mut frames = Vec::new();
        a.push(data, |f| frames.push(f));
        (a, frames)
    }

    #[test]
    fn frame_count_and_timing() {
        let (a, frames) = run(config(2), &stereo_sine(1000.0, 0.5, 0.5, 48000));
        assert_eq!(frames.len(), 48000 / 512);
        assert!((frames[0].time - 512.0 / 48000.0).abs() < 1e-9);
        assert_eq!(a.samples(), 48000);
        assert_eq!(frames[0].spectrum.len(), 1025);
        assert_eq!(frames[0].bands.len(), 16);
    }

    #[test]
    fn chunking_does_not_change_results() {
        let data = stereo_sine(440.0, 0.3, 0.6, 20000);
        let (_, whole) = run(config(2), &data);
        let mut a = Analyzer::new(config(2));
        let mut pieces = Vec::new();
        for chunk in data.chunks(2 * 37) {
            a.push(chunk, |f| pieces.push(f));
        }
        assert_eq!(whole.len(), pieces.len());
        for (x, y) in whole.iter().zip(&pieces) {
            assert_eq!(x.spectrum, y.spectrum);
            assert_eq!(x.peak, y.peak);
        }
    }

    #[test]
    fn per_channel_levels() {
        let (a, frames) = run(config(2), &stereo_sine(1000.0, 0.5, 0.25, 48000));
        let f = frames.last().unwrap();
        assert!((f.peak[0] - amp_to_db(0.5)).abs() < 0.05);
        assert!((f.peak[1] - amp_to_db(0.25)).abs() < 0.05);
        assert!((f.rms[0] - amp_to_db(0.5 / 2f32.sqrt())).abs() < 0.2);
        // The 300 ms RMS window is full well before one second.
        assert!((f.rms_avg[1] - amp_to_db(0.25 / 2f32.sqrt())).abs() < 0.2);
        assert!((f.max_peak() - f.peak[0]).abs() < 1e-6);
        assert!((f.max_rms() - f.rms[0]).abs() < 1e-6);
        assert!(!f.clipped);
        assert!((a.stats.rms(0) - 0.5 / 2f32.sqrt()).abs() < 1e-3);
        assert!((a.stats.peak[1] - 0.25).abs() < 1e-3);
    }

    #[test]
    fn spectrum_peak_and_band() {
        let (a, frames) = run(config(2), &stereo_sine(1000.0, 0.5, 0.5, 48000));
        let f = frames.last().unwrap();
        let (freq, _) = f.peak_freq.unwrap();
        assert!((freq - 1000.0).abs() < 3.0, "{freq}");
        let max_band = (0..f.bands.len()).max_by_key(|&i| f.bands[i]).unwrap();
        let e = &a.bands.edges;
        assert!(e[max_band] <= 1000.0 && 1000.0 < e[max_band + 1]);
        assert!((q_to_db(f.bands[max_band]) - amp_to_db(0.5)).abs() < 1.5);
        let mean = a.stats.mean_spectrum();
        assert_eq!(mean.len(), 1025);
    }

    /// A tone burst of `on` samples followed by `off` samples of silence.
    fn burst(freq: f32, on: usize, off: usize) -> Vec<f32> {
        let mut v = stereo_sine(freq, 0.5, 0.5, on);
        v.resize(2 * (on + off), 0.0);
        v
    }

    #[test]
    fn rms_window_reaches_full_level_after_its_length() {
        // 300 ms is 28 hops of 512 samples (28.1, rounded).
        let (_, frames) = run(config(2), &stereo_sine(1000.0, 0.5, 0.5, 48000));
        let full = amp_to_db(0.5 / 2f32.sqrt());
        assert!(frames[13].rms_avg[0] < full - 2.5, "half full is 3 dB down");
        assert!((frames[27].rms_avg[0] - full).abs() < 0.1);
        assert!((frames[60].rms_avg[0] - full).abs() < 0.1);
    }

    #[test]
    fn peak_hold_holds_then_falls() {
        let (_, frames) = run(config(2), &burst(1000.0, 4800, 48000 * 3));
        let dt = 512.0 / 48000.0;
        let at = |s: f64| &frames[(s / dt) as usize];
        let top = at(0.05).hold[0];
        assert!((top - amp_to_db(0.5)).abs() < 0.1);
        // Still held at 1.5 s, then falling at 20 dB/s.
        assert_eq!(at(1.5).hold[0], top);
        assert!(
            (at(2.0).hold[0] - (top - 8.0)).abs() < 0.5,
            "{}",
            at(2.0).hold[0]
        );
        // The decaying peak envelope falls at its own rate.
        assert!((at(1.1).peak_decay[0] - (top - 20.0)).abs() < 0.5);
    }

    #[test]
    fn vu_needle_ballistics() {
        let (_, frames) = run(config(2), &burst(1000.0, 48000, 48000));
        let dt = 512.0 / 48000.0;
        let at = |s: f64| frames[(s / dt) as usize - 1].vu[0];
        // A sine reads its AES17 level (its peak) once settled.
        let level = amp_to_db(0.5);
        assert!((at(0.9) - level).abs() < 0.02, "{}", at(0.9));
        // 99% (-0.09 dB) after 300 ms, not before 270 ms.
        assert!(
            at(0.27) < level - 0.09 && at(0.31) > level - 0.09,
            "{} {}",
            at(0.27),
            at(0.31)
        );
        // About 1% overshoot (+0.09 dB), never much more.
        let top = frames[..90]
            .iter()
            .map(|f| f.vu[0])
            .fold(FLOOR_DB, f32::max);
        assert!(top > level + 0.05 && top < level + 0.15, "{top} vs {level}");
        // The hold keeps the overshoot for 1.5 s; the needle falls the same way.
        assert!((frames[100].vu_hold[0] - top).abs() < 1e-6);
        assert!(at(1.3) < level - 20.0, "{}", at(1.3));
        let (zeta, omega) = vu_spring();
        assert!(
            (zeta - 0.826).abs() < 0.001 && omega > 10.0 && omega < 30.0,
            "{zeta} {omega}"
        );
    }

    #[test]
    fn bands_rise_fast_and_fall_slowly() {
        let (_, frames) = run(config(2), &burst(1000.0, 9600, 48000 * 2));
        let dt = 512.0 / 48000.0;
        let band = (0..frames[0].bands.len())
            .max_by_key(|&i| frames[18].bands[i])
            .unwrap();
        let level = |s: f64| q_to_db(frames[(s / dt) as usize].bands[band]);
        let cap = |s: f64| q_to_db(frames[(s / dt) as usize].band_caps[band]);
        let full = level(0.19);
        assert!((full - amp_to_db(0.5)).abs() < 1.5, "{full}");
        // Rising from the -70 dB floor: within 3 dB 0.2 s in (0.04 s of it
        // filling the FFT window).
        assert!(level(0.2) > full - 3.0, "{}", level(0.2));
        // Falling: 0.1 s after the tone (and its FFT window) has gone the band
        // has dropped only part of the way, and its cap still holds the top.
        assert!(
            level(0.34) > -55.0 && level(0.34) < full - 6.0,
            "{}",
            level(0.34)
        );
        assert!(cap(0.34) >= full - 0.5);
        // The cap falls once its 1 s hold is over; the band settles exactly
        // at the floor rather than hovering above it.
        assert!(cap(1.4) < full - 1.0);
        assert_eq!(level(2.1), -70.0);
    }

    #[test]
    fn dominant_frequency_sticks_to_a_near_equal_tone() {
        // Two tones 1 dB apart: the weaker, heard first, keeps the readout.
        let n = 48000;
        let tone = |f: f32, a: f32, i: usize| {
            a * (2.0 * std::f32::consts::PI * f * i as f32 / 48000.0).sin()
        };
        let mut data: Vec<f32> = (0..n / 2).map(|i| tone(440.0, 0.3, i)).collect();
        data.extend((n / 2..n).map(|i| tone(440.0, 0.3, i) + tone(660.0, 0.3 * 1.12, i)));
        let (_, frames) = run(config(1), &data);
        let (hz, _) = frames.last().unwrap().peak_freq.unwrap();
        assert!((hz - 440.0).abs() < 3.0, "{hz}");
        // Without the earlier tone the stronger one wins.
        let fresh: Vec<f32> = (n / 2..n)
            .map(|i| tone(440.0, 0.3, i) + tone(660.0, 0.3 * 1.12, i))
            .collect();
        let (_, frames) = run(config(1), &fresh);
        let (hz, _) = frames.last().unwrap().peak_freq.unwrap();
        assert!((hz - 660.0).abs() < 3.0, "{hz}");
    }

    #[test]
    fn spectrum_channel_selection() {
        let mut cfg = config(2);
        cfg.spectrum_channel = Some(1);
        // Left carries the tone, right is silent: selecting right gives no peak.
        let (_, frames) = run(cfg, &stereo_sine(1000.0, 0.5, 0.0, 8192));
        assert!(frames.last().unwrap().peak_freq.is_none());
    }

    #[test]
    fn no_dominant_frequency_in_noise() {
        let mut x = 99u32;
        let data: Vec<f32> = (0..16384)
            .map(|_| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                0.01 * ((x >> 8) as f32 / (1u32 << 24) as f32 - 0.5)
            })
            .collect();
        let (_, frames) = run(config(1), &data);
        assert!(frames.iter().all(|f| f.peak_freq.is_none()));
    }

    #[test]
    fn peak_decay_falls_back() {
        let mut data = stereo_sine(1000.0, 1.0, 1.0, 4800);
        data.extend(std::iter::repeat_n(0.0, 2 * 48000));
        let (_, frames) = run(config(2), &data);
        let last = frames.last().unwrap();
        assert_eq!(last.peak[0], FLOOR_DB);
        // After ~1 s of silence at 20 dB/s the decaying peak is ~20 dB down.
        assert!(
            last.peak_decay[0] < -18.0 && last.peak_decay[0] > -22.0,
            "{}",
            last.peak_decay[0]
        );
    }

    #[test]
    fn clipping_detected() {
        let data = stereo_sine(1000.0, 1.2, 0.1, 4800);
        let (a, frames) = run(config(2), &data);
        assert!(frames.iter().any(|f| f.clipped));
        assert!(a.stats.clipped[0] > 0);
        assert_eq!(a.stats.clipped[1], 0);
    }

    #[test]
    fn loudness_in_frames() {
        let data = stereo_sine(997.0, 0.1, 0.1, 48000 * 2);
        let (_, frames) = run(config(2), &data);
        let f = frames.last().unwrap();
        assert!((f.lufs_m + 20.0).abs() < 0.2, "{}", f.lufs_m);
        assert!((f.lufs_i + 20.0).abs() < 0.2);
    }

    #[test]
    fn a_weighting_attenuates_lows() {
        let mut cfg = config(1);
        cfg.weighting = Weighting::A;
        let data: Vec<f32> = (0..8192)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * 100.0 * i as f32 / 48000.0).sin())
            .collect();
        let (_, frames) = run(cfg, &data);
        let (_, db) = frames.last().unwrap().peak_freq.unwrap();
        // -6 dBFS tone, A(100 Hz) = -19.1 dB.
        assert!((db - (-6.02 - 19.1)).abs() < 1.5, "{db}");
    }

    #[test]
    fn many_channels_mix_and_meter_limit() {
        let ch = 10;
        let data: Vec<f32> = (0..4096 * ch)
            .map(|i| if i % ch == 9 { 0.5 } else { 0.0 })
            .collect();
        let (a, frames) = run(config(ch), &data);
        assert_eq!(frames[0].channels as usize, MAX_CHANNELS);
        assert!((a.stats.peak[9] - 0.5).abs() < 1e-6);
        // Clipping is counted on unmetered channels too.
        let data: Vec<f32> = (0..4096 * ch)
            .map(|i| if i % ch == 9 { 1.0 } else { 0.0 })
            .collect();
        let (a, _) = run(config(ch), &data);
        assert_eq!(a.stats.clipped[9], 4096);
    }

    #[test]
    fn power_quant_matches_direct() {
        let pq = PowerQuant::new();
        let check = |b: u32| {
            assert_eq!(
                pq.quantize(f32::from_bits(b)),
                PowerQuant::reference(b),
                "bits {b:#x}"
            )
        };
        for i in 0..pq.coarse.len() as u32 - 1 {
            let (a, b) = (i << PowerQuant::SHIFT, ((i + 1) << PowerQuant::SHIFT) - 1);
            assert!(
                PowerQuant::reference(b) - PowerQuant::reference(a) <= 1,
                "cell {i}"
            );
        }
        for b in (0..=f32::INFINITY.to_bits()).step_by(4099) {
            check(b);
        }
        for &t in &pq.thresholds[1..256] {
            for b in t.saturating_sub(2)..=t.saturating_add(2).min(f32::INFINITY.to_bits()) {
                check(b);
            }
        }
        for p in [
            0.0f32,
            -0.0,
            -1.0,
            f32::NAN,
            f32::INFINITY,
            f32::MIN_POSITIVE,
            1e-12,
            1.0,
            1e3,
        ] {
            assert_eq!(pq.quantize(p), db_to_q(power_to_db(p)), "{p}");
        }
    }

    /// Every positive f32; run with `cargo test --release -- --ignored`.
    #[test]
    #[ignore]
    fn power_quant_exhaustive() {
        let pq = PowerQuant::new();
        for b in 0..=f32::INFINITY.to_bits() {
            assert_eq!(
                pq.quantize(f32::from_bits(b)),
                PowerQuant::reference(b),
                "bits {b:#x}"
            );
        }
    }

    #[test]
    fn config_helpers() {
        let mut c = config(2);
        assert!((c.bin_hz() - 23.4375).abs() < 1e-4);
        assert!((c.frame_dt() - 512.0 / 48000.0).abs() < 1e-12);
        c.max_freq = 30000.0;
        assert_eq!(c.top_freq(), 24000.0);
    }
}
