//! IEC 61672 frequency weighting curves.

use clap::ValueEnum;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Weighting {
    /// No weighting (flat)
    Z,
    /// A-weighting
    A,
    /// C-weighting
    C,
}

const F1: f64 = 20.598_997;
const F2: f64 = 107.652_65;
const F3: f64 = 737.862_23;
const F4: f64 = 12_194.217;

/// Weighting gain in dB at frequency `f` (Hz). Returns a large negative value at DC.
pub fn weighting_db(w: Weighting, f: f64) -> f64 {
    if f <= 0.0 {
        return match w {
            Weighting::Z => 0.0,
            _ => -200.0,
        };
    }
    let f2 = f * f;
    match w {
        Weighting::Z => 0.0,
        Weighting::A => {
            let ra = F4 * F4 * f2 * f2
                / ((f2 + F1 * F1) * ((f2 + F2 * F2) * (f2 + F3 * F3)).sqrt() * (f2 + F4 * F4));
            20.0 * ra.log10() + 2.0
        }
        Weighting::C => {
            let rc = F4 * F4 * f2 / ((f2 + F1 * F1) * (f2 + F4 * F4));
            20.0 * rc.log10() + 0.062
        }
    }
}

/// Per-bin linear *power* gains for an FFT with `bins` bins spaced `bin_hz` apart.
pub fn power_gain_table(w: Weighting, bins: usize, bin_hz: f64) -> Vec<f32> {
    (0..bins)
        .map(|k| 10f64.powf(weighting_db(w, k as f64 * bin_hz) / 10.0) as f32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_weighting_reference_points() {
        // IEC 61672-1 table values.
        let pts = [
            (31.5, -39.4),
            (100.0, -19.1),
            (1000.0, 0.0),
            (4000.0, 1.0),
            (10000.0, -2.5),
        ];
        for (f, db) in pts {
            let v = weighting_db(Weighting::A, f);
            assert!((v - db).abs() < 0.15, "A({f}) = {v}, expected {db}");
        }
    }

    #[test]
    fn c_weighting_reference_points() {
        let pts = [(31.5, -3.0), (100.0, -0.3), (1000.0, 0.0), (10000.0, -4.4)];
        for (f, db) in pts {
            let v = weighting_db(Weighting::C, f);
            assert!((v - db).abs() < 0.15, "C({f}) = {v}, expected {db}");
        }
    }

    #[test]
    fn z_is_flat_and_dc_is_handled() {
        assert_eq!(weighting_db(Weighting::Z, 123.0), 0.0);
        assert_eq!(weighting_db(Weighting::Z, 0.0), 0.0);
        assert_eq!(weighting_db(Weighting::A, 0.0), -200.0);
        let t = power_gain_table(Weighting::Z, 5, 10.0);
        assert!(t.iter().all(|&g| g == 1.0));
        let t = power_gain_table(Weighting::A, 3, 1000.0);
        assert!(t[0] < 1e-10);
        assert!((t[1] - 1.0).abs() < 0.01);
    }
}
