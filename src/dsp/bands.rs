//! Graphic-equalizer band layout: log-spaced bands summed from an FFT power spectrum.

pub struct BandLayout {
    pub centers: Vec<f32>,
    pub edges: Vec<f32>,
    ranges: Vec<(usize, usize)>,
}

impl BandLayout {
    /// `n` log-spaced bands from `min` to `max` Hz over `nbins` FFT bins of width `bin_hz`.
    pub fn new(n: usize, min: f32, max: f32, bin_hz: f32, nbins: usize) -> Self {
        let n = n.max(1);
        let ratio = (max / min) as f64;
        let edges: Vec<f32> = (0..=n)
            .map(|i| (min as f64 * ratio.powf(i as f64 / n as f64)) as f32)
            .collect();
        let centers = edges.windows(2).map(|e| (e[0] * e[1]).sqrt()).collect::<Vec<_>>();
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
        BandLayout { centers, edges, ranges }
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
        assert!((out[idx] - expected).abs() < 1.0, "{} vs {expected}", out[idx]);
        for (i, &v) in out.iter().enumerate() {
            if (i as isize - idx as isize).abs() > 1 {
                assert!(v < expected - 40.0, "band {i} leaked: {v}");
            }
        }
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
