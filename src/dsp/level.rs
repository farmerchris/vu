//! Level conversions and meter ballistics.

/// Lowest dB value we ever report; stands in for "silence".
pub const FLOOR_DB: f32 = -150.0;

pub fn amp_to_db(a: f32) -> f32 {
    if a > 0.0 {
        (20.0 * a.log10()).max(FLOOR_DB)
    } else {
        FLOOR_DB
    }
}

pub fn power_to_db(p: f32) -> f32 {
    if p > 0.0 {
        (10.0 * p.log10()).max(FLOOR_DB)
    } else {
        FLOOR_DB
    }
}

pub fn db_to_amp(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn db_to_power(db: f32) -> f32 {
    10f32.powf(db / 10.0)
}

/// Maximum absolute sample value.
pub fn peak_abs(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |m, &s| m.max(s.abs()))
}

/// Root mean square of a block (0 for an empty block).
pub fn rms(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    let ms: f64 = x.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / x.len() as f64;
    ms.sqrt() as f32
}

/// One-pole smoothing coefficient for a time constant `tau` sampled every `dt`.
/// `y += coeff * (x - y)` reaches 63% of a step after `tau` seconds.
pub fn smoothing_coeff(tau: f32, dt: f32) -> f32 {
    if tau <= 0.0 {
        1.0
    } else {
        1.0 - (-dt / tau).exp()
    }
}

/// Peak-meter fall-back: instant attack, linear release in dB/s.
pub fn peak_decay(prev_db: f32, new_db: f32, rate_db_per_s: f32, dt: f32) -> f32 {
    new_db.max(prev_db - rate_db_per_s * dt)
}

/// Crest factor in dB (peak to RMS).
pub fn crest_factor_db(peak: f32, rms: f32) -> f32 {
    amp_to_db(peak) - amp_to_db(rms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_conversions() {
        assert!((amp_to_db(1.0)).abs() < 1e-6);
        assert!((amp_to_db(0.5) + 6.0206).abs() < 1e-3);
        assert!((power_to_db(0.01) + 20.0).abs() < 1e-4);
        assert_eq!(amp_to_db(0.0), FLOOR_DB);
        assert_eq!(amp_to_db(-1.0), FLOOR_DB);
        assert_eq!(power_to_db(f32::NAN), FLOOR_DB);
        assert_eq!(amp_to_db(1e-30), FLOOR_DB);
        for db in [-60.0f32, -20.0, -3.0, 0.0, 6.0] {
            assert!((amp_to_db(db_to_amp(db)) - db).abs() < 1e-4);
            assert!((power_to_db(db_to_power(db)) - db).abs() < 1e-4);
        }
    }

    #[test]
    fn peak_and_rms() {
        assert_eq!(peak_abs(&[0.1, -0.7, 0.3]), 0.7);
        assert_eq!(peak_abs(&[]), 0.0);
        assert_eq!(rms(&[]), 0.0);
        assert!((rms(&[1.0, -1.0, 1.0, -1.0]) - 1.0).abs() < 1e-6);
        // Sine RMS is amplitude / sqrt(2).
        let n = 48000;
        let s: Vec<f32> = (0..n)
            .map(|i| 0.8 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48000.0).sin())
            .collect();
        assert!((rms(&s) - 0.8 / 2f32.sqrt()).abs() < 1e-4);
        assert!((crest_factor_db(peak_abs(&s), rms(&s)) - 3.0103).abs() < 0.01);
    }

    #[test]
    fn smoothing() {
        assert_eq!(smoothing_coeff(0.0, 0.01), 1.0);
        let c = smoothing_coeff(0.3, 0.01);
        let mut y = 0.0f32;
        for _ in 0..30 {
            y += c * (1.0 - y);
        }
        // After one time constant: 1 - 1/e.
        assert!((y - (1.0 - (-1.0f32).exp())).abs() < 1e-4);
    }

    #[test]
    fn decay() {
        assert_eq!(peak_decay(-10.0, -5.0, 20.0, 0.1), -5.0);
        assert!((peak_decay(-10.0, -50.0, 20.0, 0.1) + 12.0).abs() < 1e-6);
    }
}
