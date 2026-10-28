//! Turns a stream of interleaved samples into analysis frames, one per hop.

use crate::dsp::bands::BandLayout;
use crate::dsp::level::{amp_to_db, peak_decay, power_to_db, smoothing_coeff, FLOOR_DB};
use crate::dsp::loudness::LoudnessMeter;
use crate::dsp::quant::db_to_q;
use crate::dsp::spectrum::{peak_frequency, Spectrum};
use crate::dsp::weighting::{power_gain_table, Weighting};
use crate::dsp::window::WindowKind;

/// Channels tracked individually by the level meters.
pub const MAX_CHANNELS: usize = 8;
/// Samples at or above this magnitude count as clipped.
pub const CLIP_LEVEL: f32 = 0.9999;

#[derive(Clone, Debug)]
pub struct AnalyzerConfig {
    pub sample_rate: u32,
    pub channels: usize,
    pub fft_size: usize,
    pub hop: usize,
    pub window: WindowKind,
    pub weighting: Weighting,
    pub bands: usize,
    pub min_freq: f32,
    pub max_freq: f32,
    /// Time constant of the averaged RMS meter, seconds.
    pub rms_tau: f32,
    /// Peak meter fall-back rate, dB/s.
    pub peak_decay_db_s: f32,
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
    /// Peak with meter fall-back applied, dBFS.
    pub peak_decay: [f32; MAX_CHANNELS],
    /// RMS over the hop, dBFS.
    pub rms: [f32; MAX_CHANNELS],
    /// Exponentially averaged RMS, dBFS.
    pub rms_avg: [f32; MAX_CHANNELS],
    pub lufs_m: f32,
    pub lufs_s: f32,
    pub lufs_i: f32,
    pub peak_freq: Option<(f32, f32)>,
    pub clipped: bool,
    /// Quantized power spectrum (see [`crate::dsp::quant`]).
    pub spectrum: Box<[u8]>,
    /// Quantized equalizer band levels.
    pub bands: Box<[u8]>,
}

impl Frame {
    pub fn n(&self) -> usize {
        self.channels as usize
    }

    pub fn max_peak(&self) -> f32 {
        self.peak[..self.n()].iter().copied().fold(FLOOR_DB, f32::max)
    }

    pub fn max_rms(&self) -> f32 {
        self.rms[..self.n()].iter().copied().fold(FLOOR_DB, f32::max)
    }
}

/// Whole-stream statistics for reports.
#[derive(Clone, Debug)]
pub struct Stats {
    pub frames: u64,
    pub peak: Vec<f32>,
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
    hop_sumsq: [f64; MAX_CHANNELS],
    hop_clip: bool,
    ms_avg: [f32; MAX_CHANNELS],
    peak_hold: [f32; MAX_CHANNELS],
    rms_coef: f32,
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
        let coarse = (0..=top >> Self::SHIFT).map(|i| Self::reference(i << Self::SHIFT)).collect();
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
        let bands = BandLayout::new(cfg.bands, cfg.min_freq, top, bin_hz, bins);
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
            hop_sumsq: [0.0; MAX_CHANNELS],
            hop_clip: false,
            ms_avg: [0.0; MAX_CHANNELS],
            peak_hold: [FLOOR_DB; MAX_CHANNELS],
            rms_coef: smoothing_coeff(cfg.rms_tau, cfg.frame_dt() as f32),
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
                    self.hop_sumsq[c] += (s as f64) * (s as f64);
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
        self.bands.compute(&self.power, self.spectrum.enbw(), &mut self.band_db);
        let bands: Box<[u8]> = self.band_db.iter().map(|&d| db_to_q(d)).collect();
        let peak_freq = peak_frequency(&self.power, self.cfg.bin_hz(), self.peak_bins.0, self.peak_bins.1);

        let dt = self.cfg.frame_dt() as f32;
        let metered = self.cfg.channels.clamp(1, MAX_CHANNELS);
        let mut peak = [FLOOR_DB; MAX_CHANNELS];
        let mut rms = [FLOOR_DB; MAX_CHANNELS];
        let mut rms_avg = [FLOOR_DB; MAX_CHANNELS];
        let n = self.hop_count.max(1) as f64;
        for c in 0..metered {
            let ms = (self.hop_sumsq[c] / n) as f32;
            rms[c] = power_to_db(ms);
            self.ms_avg[c] += self.rms_coef * (ms - self.ms_avg[c]);
            rms_avg[c] = power_to_db(self.ms_avg[c]);
            peak[c] = amp_to_db(self.hop_peak[c]);
            self.peak_hold[c] = peak_decay(self.peak_hold[c], peak[c], self.cfg.peak_decay_db_s, dt);
        }
        let frame = Frame {
            time: self.samples as f64 / self.cfg.sample_rate as f64,
            channels: metered as u8,
            peak,
            peak_decay: self.peak_hold,
            rms,
            rms_avg,
            lufs_m: self.loudness.momentary(),
            lufs_s: self.loudness.short_term(),
            lufs_i: self.loudness.integrated(),
            peak_freq,
            clipped: self.hop_clip,
            spectrum,
            bands,
        };
        self.hop_count = 0;
        self.hop_peak = [0.0; MAX_CHANNELS];
        self.hop_sumsq = [0.0; MAX_CHANNELS];
        self.hop_clip = false;
        frame
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
            bands: 16,
            min_freq: 20.0,
            max_freq: 20000.0,
            rms_tau: 0.3,
            peak_decay_db_s: 20.0,
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
        // Averaged RMS has settled after one second (> 3 time constants).
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

    #[test]
    fn spectrum_channel_selection() {
        let mut cfg = config(2);
        cfg.spectrum_channel = Some(1);
        // Left carries the tone, right is silent: selecting right gives no peak.
        let (_, frames) = run(cfg, &stereo_sine(1000.0, 0.5, 0.0, 8192));
        assert!(frames.last().unwrap().peak_freq.is_none());
    }

    #[test]
    fn peak_decay_falls_back() {
        let mut data = stereo_sine(1000.0, 1.0, 1.0, 4800);
        data.extend(std::iter::repeat_n(0.0, 2 * 48000));
        let (_, frames) = run(config(2), &data);
        let last = frames.last().unwrap();
        assert_eq!(last.peak[0], FLOOR_DB);
        // After ~1 s of silence at 20 dB/s the decaying peak is ~20 dB down.
        assert!(last.peak_decay[0] < -18.0 && last.peak_decay[0] > -22.0, "{}", last.peak_decay[0]);
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
        let data: Vec<f32> = (0..4096 * ch).map(|i| if i % ch == 9 { 0.5 } else { 0.0 }).collect();
        let (a, frames) = run(config(ch), &data);
        assert_eq!(frames[0].channels as usize, MAX_CHANNELS);
        assert!((a.stats.peak[9] - 0.5).abs() < 1e-6);
        // Clipping is counted on unmetered channels too.
        let data: Vec<f32> = (0..4096 * ch).map(|i| if i % ch == 9 { 1.0 } else { 0.0 }).collect();
        let (a, _) = run(config(ch), &data);
        assert_eq!(a.stats.clipped[9], 4096);
    }

    #[test]
    fn power_quant_matches_direct() {
        let pq = PowerQuant::new();
        let check = |b: u32| assert_eq!(pq.quantize(f32::from_bits(b)), PowerQuant::reference(b), "bits {b:#x}");
        for i in 0..pq.coarse.len() as u32 - 1 {
            let (a, b) = (i << PowerQuant::SHIFT, ((i + 1) << PowerQuant::SHIFT) - 1);
            assert!(PowerQuant::reference(b) - PowerQuant::reference(a) <= 1, "cell {i}");
        }
        for b in (0..=f32::INFINITY.to_bits()).step_by(4099) {
            check(b);
        }
        for &t in &pq.thresholds[1..256] {
            for b in t.saturating_sub(2)..=t.saturating_add(2).min(f32::INFINITY.to_bits()) {
                check(b);
            }
        }
        for p in [0.0f32, -0.0, -1.0, f32::NAN, f32::INFINITY, f32::MIN_POSITIVE, 1e-12, 1.0, 1e3] {
            assert_eq!(pq.quantize(p), db_to_q(power_to_db(p)), "{p}");
        }
    }

    /// Every positive f32; run with `cargo test --release -- --ignored`.
    #[test]
    #[ignore]
    fn power_quant_exhaustive() {
        let pq = PowerQuant::new();
        for b in 0..=f32::INFINITY.to_bits() {
            assert_eq!(pq.quantize(f32::from_bits(b)), PowerQuant::reference(b), "bits {b:#x}");
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
