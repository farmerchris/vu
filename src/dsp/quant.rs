//! Compact storage of dB values in a byte (0.5 dB steps).

pub const Q_MIN_DB: f32 = -120.0;
pub const Q_STEP_DB: f32 = 0.5;
pub const Q_MAX_DB: f32 = Q_MIN_DB + 255.0 * Q_STEP_DB;

#[inline]
pub fn db_to_q(db: f32) -> u8 {
    let v = ((db - Q_MIN_DB) / Q_STEP_DB).round();
    if v.is_nan() {
        0
    } else {
        v.clamp(0.0, 255.0) as u8
    }
}

#[inline]
pub fn q_to_db(q: u8) -> f32 {
    Q_MIN_DB + q as f32 * Q_STEP_DB
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_within_half_step() {
        let mut db = -119.0f32;
        while db < 7.0 {
            assert!(
                (q_to_db(db_to_q(db)) - db).abs() <= Q_STEP_DB / 2.0 + 1e-4,
                "{db}"
            );
            db += 0.37;
        }
    }

    #[test]
    fn clamps() {
        assert_eq!(db_to_q(-500.0), 0);
        assert_eq!(db_to_q(100.0), 255);
        assert_eq!(db_to_q(f32::NAN), 0);
        assert_eq!(q_to_db(0), Q_MIN_DB);
        assert_eq!(q_to_db(255), Q_MAX_DB);
        assert_eq!(db_to_q(0.0), 240);
    }
}
