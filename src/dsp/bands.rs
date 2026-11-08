//! Graphic-equalizer band layout: standard 1/3-octave or log-spaced bands
//! summed from an FFT power spectrum.

pub struct BandLayout {
    pub centers: Vec<f32>,
    pub edges: Vec<f32>,
    /// Label for each band (nominal frequency, e.g. "31.5", "1k", "12.5k").
    pub labels: Vec<String>,
    /// Bands worth labelling when space is short (octave centres for 1/3 octaves).
    pub major: Vec<bool>,
    /// Whether these are the standard 1/3-octave bands.
    pub third_octave: bool,
    ranges: Vec<(usize, usize)>,
}

/// Nominal centre frequencies of the 1/3-octave bands (IEC 61260 / ISO 266),
/// 20 Hz to 20 kHz. Band `i` is exactly 1000 · 2^((i − 17)/3) Hz.
pub const THIRD_OCTAVE_NOMINAL: [&str; 31] = [
    "20", "25", "31.5", "40", "50", "63", "80", "100", "125", "160", "200", "250", "315", "400",
    "500", "630", "800", "1k", "1.25k", "1.6k", "2k", "2.5k", "3.15k", "4k", "5k", "6.3k", "8k",
    "10k", "12.5k", "16k", "20k",
];

/// Short frequency label: "22", "850", "1k", "1.5k", "12k".
fn short_label(f: f32) -> String {
    if f >= 1000.0 {
        let k = f / 1000.0;
        if k >= 10.0 || (k - k.round()).abs() < 0.05 {
            format!("{:.0}k", k)
        } else {
            format!("{:.1}k", k)
        }
    } else {
        format!("{f:.0}")
    }
}

impl BandLayout {
    /// The standard 1/3-octave bands whose centres lie in `[min, max]` Hz.
    pub fn third_octave(min: f32, max: f32, bin_hz: f32, nbins: usize) -> Self {
        let mut centers = Vec::new();
        let mut labels = Vec::new();
        let mut major = Vec::new();
        for (i, name) in THIRD_OCTAVE_NOMINAL.iter().enumerate() {
            let k = i as i32 - 17;
            let c = 1000.0 * 2f64.powf(k as f64 / 3.0);
            // Nominal 20 Hz is 19.7 Hz exactly; allow for that at the ends.
            if c < min as f64 * 0.97 || c > max as f64 * 1.03 {
                continue;
            }
            centers.push(c as f32);
            labels.push(name.to_string());
            major.push(k.rem_euclid(3) == 0);
        }
        if centers.is_empty() {
            return Self::new(1, min, max, bin_hz, nbins);
        }
        let half = 2f32.powf(1.0 / 6.0);
        let mut edges: Vec<f32> = centers.iter().map(|c| c / half).collect();
        edges.push(centers[centers.len() - 1] * half);
        Self::build(centers, edges, labels, major, true, bin_hz, nbins)
    }

    /// `n` log-spaced bands from `min` to `max` Hz over `nbins` FFT bins of width `bin_hz`.
    pub fn new(n: usize, min: f32, max: f32, bin_hz: f32, nbins: usize) -> Self {
        let n = n.max(1);
        let ratio = (max / min) as f64;
        let edges: Vec<f32> = (0..=n)
            .map(|i| (min as f64 * ratio.powf(i as f64 / n as f64)) as f32)
            .collect();
        let centers = edges
            .windows(2)
            .map(|e| (e[0] * e[1]).sqrt())
            .collect::<Vec<_>>();
        let labels = centers.iter().map(|&c| short_label(c)).collect();
        let major = vec![true; n];
        Self::build(centers, edges, labels, major, false, bin_hz, nbins)
    }

    fn build(
        centers: Vec<f32>,
        edges: Vec<f32>,
        labels: Vec<String>,
        major: Vec<bool>,
        third_octave: bool,
        bin_hz: f32,
        nbins: usize,
    ) -> Self {
        let last = nbins.saturating_sub(1);
        let ranges = edges
            .windows(2)
            .zip(&centers)
            .map(|(e, &c)| {
                let lo = ((e[0] / bin_hz).ceil() as usize).min(last);
                let hi = ((e[1] / bin_hz).ceil() as usize).min(nbins);
                if hi > lo {
                    (lo, hi)
                } else {
                    let k = ((c / bin_hz).round() as usize).min(last);
                    (k, k + 1)
                }
            })
            .collect();
        BandLayout {
            centers,
            edges,
            labels,
            major,
            third_octave,
            ranges,
        }
    }

    pub fn len(&self) -> usize {
        self.centers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.centers.is_empty()
    }

    pub fn bin_range(&self, band: usize) -> (usize, usize) {
        self.ranges[band]
    }

    /// Band power from a power spectrum calibrated so a tone of amplitude A reads A².
    /// Summed power is divided by the window ENBW so a tone wholly inside a band
    /// reads its own level, while the max bin guards narrow bands that only catch
    /// part of a spectral lobe.
    pub fn compute(&self, power: &[f32], enbw: f32, out_db: &mut [f32]) {
        let inv = 1.0 / enbw.max(1e-6);
        for (o, &(lo, hi)) in out_db.iter_mut().zip(&self.ranges) {
            let slice = &power[lo..hi.min(power.len())];
            let mut sum = 0.0f32;
            let mut mx = 0.0f32;
            for &p in slice {
                sum += p;
                mx = mx.max(p);
            }
            *o = super::level::power_to_db((sum * inv).max(mx));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::spectrum::Spectrum;
    use crate::dsp::window::WindowKind;

    #[test]
    fn edges_are_log_spaced() {
        let b = BandLayout::new(10, 20.0, 20000.0, 10.0, 2049);
        assert_eq!(b.len(), 10);
        assert_eq!(b.edges.len(), 11);
        assert!((b.edges[0] - 20.0).abs() < 1e-3);
        assert!((b.edges[10] - 20000.0).abs() < 0.1);
        let r = b.edges[1] / b.edges[0];
        for e in b.edges.windows(2) {
            assert!(((e[1] / e[0]) - r).abs() < 1e-3);
        }
        for (i, c) in b.centers.iter().enumerate() {
            assert!(*c > b.edges[i] && *c < b.edges[i + 1]);
        }
    }

    #[test]
    fn ranges_non_empty_and_bounded() {
        let b = BandLayout::new(64, 20.0, 24000.0, 48000.0 / 1024.0, 513);
        for i in 0..b.len() {
            let (lo, hi) = b.bin_range(i);
            assert!(hi > lo && hi <= 513, "band {i}: {lo}..{hi}");
        }
    }

    #[test]
    fn tone_lands_in_its_band() {
        let sr = 48000.0;
        let n = 4096;
        let mut sp = Spectrum::new(n, WindowKind::Hann);
        let a = 0.5f32;
        let x: Vec<f32> = (0..n)
            .map(|i| a * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sr).sin())
            .collect();
        let mut p = vec![0.0; sp.bins()];
        sp.power(&x, &mut p);
        let b = BandLayout::new(10, 20.0, 20000.0, sr / n as f32, sp.bins());
        let mut out = vec![0.0; b.len()];
        b.compute(&p, sp.enbw(), &mut out);
        let idx = (0..b.len())
            .find(|&i| b.edges[i] <= 1000.0 && 1000.0 < b.edges[i + 1])
            .unwrap();
        let expected = 20.0 * a.log10();
        assert!(
            (out[idx] - expected).abs() < 1.0,
            "{} vs {expected}",
            out[idx]
        );
        for (i, &v) in out.iter().enumerate() {
            if (i as isize - idx as isize).abs() > 1 {
                assert!(v < expected - 40.0, "band {i} leaked: {v}");
            }
        }
    }

    #[test]
    fn third_octave_bands_are_standard() {
        let b = BandLayout::third_octave(20.0, 20000.0, 48000.0 / 2048.0, 1025);
        assert!(b.third_octave);
        assert_eq!(b.len(), 31);
        assert_eq!(b.labels[0], "20");
        assert_eq!(b.labels[17], "1k");
        assert_eq!(b.labels[30], "20k");
        assert!((b.centers[17] - 1000.0).abs() < 1e-3);
        // Adjacent centres a third of an octave apart; edges touch.
        for w in b.centers.windows(2) {
            assert!((w[1] / w[0] - 2f32.powf(1.0 / 3.0)).abs() < 1e-4);
        }
        assert_eq!(b.edges.len(), 32);
        assert!((b.edges[18] / b.centers[17] - 2f32.powf(1.0 / 6.0)).abs() < 1e-4);
        // Octave centres are the majors: 31.5, 63, 125 ... 16k.
        let majors: Vec<&str> = (0..b.len())
            .filter(|&i| b.major[i])
            .map(|i| b.labels[i].as_str())
            .collect();
        assert_eq!(
            majors,
            ["31.5", "63", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"]
        );
        // A 24 kHz input (12 kHz top) keeps the bands up to 10 kHz.
        let b = BandLayout::third_octave(20.0, 12000.0, 24000.0 / 2048.0, 1025);
        assert_eq!(b.labels.last().unwrap(), "10k");
        assert_eq!(b.len(), 28);
        // A narrow range still yields its bands.
        let b = BandLayout::third_octave(900.0, 1100.0, 10.0, 2400);
        assert_eq!(b.labels, ["1k"]);
    }

    #[test]
    fn log_band_labels() {
        assert_eq!(short_label(22.4), "22");
        assert_eq!(short_label(1000.0), "1k");
        assert_eq!(short_label(1500.0), "1.5k");
        assert_eq!(short_label(12500.0), "12k");
        let b = BandLayout::new(4, 100.0, 1600.0, 10.0, 1000);
        assert_eq!(b.labels, ["141", "283", "566", "1.1k"]);
        assert!(b.major.iter().all(|&m| m) && !b.third_octave);
    }

    #[test]
    fn white_power_sums() {
        // Flat power spectrum of 1e-4 per bin: a band of k bins reads k*1e-4/enbw.
        let b = BandLayout::new(1, 1000.0, 2000.0, 10.0, 1000);
        let p = vec![1e-4f32; 1000];
        let mut out = [0.0f32];
        b.compute(&p, 1.0, &mut out);
        let (lo, hi) = b.bin_range(0);
        let expected = 10.0 * ((hi - lo) as f32 * 1e-4).log10();
        assert!((out[0] - expected).abs() < 1e-3);
    }
}
