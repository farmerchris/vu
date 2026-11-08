//! Synthetic test signals: `sine:FREQ[:DB]`, `square:FREQ[:DB]`, `noise[:DB]`,
//! `pink[:DB]`, `sweep:F0:F1:SECS[:DB[:END_DB]]`, `silence`.

use anyhow::{bail, Context, Result};
use std::f64::consts::PI;

#[derive(Clone, Debug, PartialEq)]
pub enum GenKind {
    Sine {
        freq: f64,
    },
    Square {
        freq: f64,
    },
    White,
    Pink,
    /// Exponential sweep from `f0` to `f1` over `secs`, repeating. With
    /// `end_db` the level also ramps (linearly in dB) from `level_db` to it.
    Sweep {
        f0: f64,
        f1: f64,
        secs: f64,
        end_db: Option<f64>,
    },
    Silence,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GenSpec {
    pub kind: GenKind,
    /// Peak amplitude in dBFS.
    pub level_db: f64,
}

pub fn parse_spec(s: &str) -> Result<GenSpec> {
    let parts: Vec<&str> = s.split(':').map(str::trim).collect();
    let num = |i: usize, what: &str| -> Result<f64> {
        parts
            .get(i)
            .with_context(|| format!("generator '{s}' is missing {what}"))?
            .parse::<f64>()
            .with_context(|| format!("generator '{s}': invalid {what}"))
    };
    let opt_db = |i: usize, default: f64| -> Result<f64> {
        match parts.get(i) {
            None => Ok(default),
            Some(v) => v
                .parse::<f64>()
                .with_context(|| format!("generator '{s}': invalid level")),
        }
    };
    let (kind, db_idx, max_parts) = match parts[0].to_ascii_lowercase().as_str() {
        "sine" => (
            GenKind::Sine {
                freq: num(1, "frequency")?,
            },
            2,
            3,
        ),
        "square" => (
            GenKind::Square {
                freq: num(1, "frequency")?,
            },
            2,
            3,
        ),
        "noise" | "white" => (GenKind::White, 1, 2),
        "pink" => (GenKind::Pink, 1, 2),
        "sweep" => (
            GenKind::Sweep {
                f0: num(1, "start frequency")?,
                f1: num(2, "end frequency")?,
                secs: num(3, "duration")?,
                end_db: parts.get(5).map(|_| num(5, "end level")).transpose()?,
            },
            4,
            6,
        ),
        "silence" => (GenKind::Silence, 1, 1),
        other => bail!("unknown generator '{other}' (sine, square, noise, pink, sweep, silence)"),
    };
    if parts.len() > max_parts {
        bail!("generator '{s}' has too many fields");
    }
    match kind {
        GenKind::Sine { freq } | GenKind::Square { freq } if freq <= 0.0 => {
            bail!("frequency must be positive")
        }
        GenKind::Sweep { f0, f1, secs, .. } if f0 <= 0.0 || f1 <= 0.0 || secs <= 0.0 => {
            bail!("sweep frequencies and duration must be positive")
        }
        _ => {}
    }
    let default_db = match kind {
        GenKind::White | GenKind::Pink => -12.0,
        _ => -6.0,
    };
    Ok(GenSpec {
        kind,
        level_db: opt_db(db_idx, default_db)?,
    })
}

pub struct Generator {
    spec: GenSpec,
    amp: f64,
    sr: f64,
    phase: f64,
    n: u64,
    rng: u64,
    pink: [f64; 7],
}

impl Generator {
    pub fn new(spec: GenSpec, sample_rate: u32) -> Self {
        Generator {
            amp: 10f64.powf(spec.level_db / 20.0),
            spec,
            sr: sample_rate as f64,
            phase: 0.0,
            n: 0,
            rng: 0x9E37_79B9_7F4A_7C15,
            pink: [0.0; 7],
        }
    }

    /// Uniform random in [-1, 1) (xorshift64*).
    fn white(&mut self) -> f64 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let v = self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (v >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }

    fn advance(&mut self, freq: f64) {
        self.phase += freq / self.sr;
        self.phase -= self.phase.floor();
    }

    pub fn next_sample(&mut self) -> f32 {
        let v = match self.spec.kind {
            GenKind::Sine { freq } => {
                let v = (2.0 * PI * self.phase).sin();
                self.advance(freq);
                v
            }
            GenKind::Square { freq } => {
                let v = if self.phase < 0.5 { 1.0 } else { -1.0 };
                self.advance(freq);
                v
            }
            GenKind::White => self.white(),
            GenKind::Pink => {
                // Paul Kellet's refined pink filter; output scaled to roughly ±1.
                let w = self.white();
                let b = &mut self.pink;
                b[0] = 0.99886 * b[0] + w * 0.0555179;
                b[1] = 0.99332 * b[1] + w * 0.0750759;
                b[2] = 0.96900 * b[2] + w * 0.1538520;
                b[3] = 0.86650 * b[3] + w * 0.3104856;
                b[4] = 0.55000 * b[4] + w * 0.5329522;
                b[5] = -0.7616 * b[5] - w * 0.0168980;
                let out = b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + w * 0.5362;
                b[6] = w * 0.115926;
                (out * 0.2).clamp(-1.0, 1.0)
            }
            GenKind::Sweep {
                f0,
                f1,
                secs,
                end_db,
            } => {
                let t = (self.n as f64 / self.sr) % secs;
                let f = sweep_freq(f0, f1, secs, t);
                let v = (2.0 * PI * self.phase).sin();
                self.advance(f);
                match end_db {
                    // `amp` is applied below, so scale by the ramp relative to it.
                    Some(db1) => v * 10f64.powf((db1 - self.spec.level_db) * (t / secs) / 20.0),
                    None => v,
                }
            }
            GenKind::Silence => 0.0,
        };
        self.n += 1;
        (v * self.amp) as f32
    }

    /// Fill an interleaved buffer, same signal on every channel.
    pub fn fill(&mut self, out: &mut [f32], channels: usize) {
        for frame in out.chunks_exact_mut(channels.max(1)) {
            let s = self.next_sample();
            frame.iter_mut().for_each(|x| *x = s);
        }
    }
}

/// Instantaneous frequency of an exponential sweep at time `t` within its period.
pub fn sweep_freq(f0: f64, f1: f64, secs: f64, t: f64) -> f64 {
    f0 * (f1 / f0).powf(t / secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::level::{peak_abs, rms};

    #[test]
    fn parse_variants() {
        assert_eq!(
            parse_spec("sine:440").unwrap(),
            GenSpec {
                kind: GenKind::Sine { freq: 440.0 },
                level_db: -6.0
            }
        );
        assert_eq!(parse_spec("sine:1000:-20").unwrap().level_db, -20.0);
        assert_eq!(
            parse_spec("Square:50").unwrap().kind,
            GenKind::Square { freq: 50.0 }
        );
        assert_eq!(
            parse_spec("noise").unwrap(),
            GenSpec {
                kind: GenKind::White,
                level_db: -12.0
            }
        );
        assert_eq!(parse_spec("pink:-3").unwrap().level_db, -3.0);
        assert_eq!(
            parse_spec("sweep:20:20000:5").unwrap().kind,
            GenKind::Sweep {
                f0: 20.0,
                f1: 20000.0,
                secs: 5.0,
                end_db: None
            }
        );
        let s = parse_spec("sweep:100:800:5:-20:-10").unwrap();
        assert_eq!(
            s.kind,
            GenKind::Sweep {
                f0: 100.0,
                f1: 800.0,
                secs: 5.0,
                end_db: Some(-10.0)
            }
        );
        assert_eq!(s.level_db, -20.0);
        assert_eq!(parse_spec("silence").unwrap().kind, GenKind::Silence);
    }

    #[test]
    fn parse_errors() {
        for bad in [
            "",
            "sine",
            "sine:abc",
            "sine:-5",
            "sweep:20:200",
            "bogus",
            "noise:x",
            "sine:1:2:3",
            "silence:1",
            "sweep:1:2:3:4:x",
            "sweep:1:2:3:4:5:6",
        ] {
            assert!(parse_spec(bad).is_err(), "{bad} should fail");
        }
    }

    fn render(spec: &str, n: usize) -> Vec<f32> {
        let mut g = Generator::new(parse_spec(spec).unwrap(), 48000);
        (0..n).map(|_| g.next_sample()).collect()
    }

    #[test]
    fn sine_level_and_frequency() {
        let x = render("sine:1000:-6", 48000);
        assert!((peak_abs(&x) - 10f32.powf(-6.0 / 20.0)).abs() < 1e-3);
        // Count rising zero crossings: one per cycle.
        let crossings = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!((crossings as i32 - 1000).abs() <= 1, "{crossings}");
    }

    #[test]
    fn square_rms_equals_peak() {
        let x = render("square:100:0", 48000);
        assert!((rms(&x) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn noise_statistics() {
        let x = render("noise:0", 200_000);
        assert!(peak_abs(&x) <= 1.0);
        // Uniform [-1,1) has RMS 1/sqrt(3).
        assert!((rms(&x) - 1.0 / 3f32.sqrt()).abs() < 0.01);
        let mean: f32 = x.iter().sum::<f32>() / x.len() as f32;
        assert!(mean.abs() < 0.01);
        let p = render("pink:0", 200_000);
        assert!(peak_abs(&p) <= 1.0 && rms(&p) > 0.05);
    }

    #[test]
    fn deterministic() {
        assert_eq!(render("noise", 100), render("noise", 100));
    }

    #[test]
    fn sweep_bounds() {
        assert!((sweep_freq(20.0, 20000.0, 10.0, 0.0) - 20.0).abs() < 1e-9);
        assert!((sweep_freq(20.0, 20000.0, 10.0, 5.0) - 632.455_532).abs() < 1e-3);
        assert!((sweep_freq(20.0, 20000.0, 10.0, 10.0) - 20000.0).abs() < 1e-6);
        let x = render("sweep:100:1000:0.5:-6", 48000);
        assert!(peak_abs(&x) <= 10f32.powf(-6.0 / 20.0) + 1e-6);
    }

    #[test]
    fn sweep_level_ramp() {
        // -20 dB at the start of each 1 s sweep, -15 halfway, nearly -10 at
        // the end, then back to -20 as the sweep repeats.
        let x = render("sweep:1000:1000:1:-20:-10", 96000);
        let peak_db = |a: usize| 20.0 * peak_abs(&x[a..a + 480]).log10();
        assert!((peak_db(0) - -19.95).abs() < 0.1, "{}", peak_db(0));
        assert!(
            (peak_db(24000 - 240) - -15.0).abs() < 0.1,
            "{}",
            peak_db(23760)
        );
        assert!((peak_db(47520) - -10.05).abs() < 0.1, "{}", peak_db(47520));
        assert!((peak_db(48000) - -19.95).abs() < 0.1, "{}", peak_db(48000));
    }

    #[test]
    fn fill_interleaves() {
        let mut g = Generator::new(parse_spec("sine:1000").unwrap(), 48000);
        let mut buf = vec![0.0; 20];
        g.fill(&mut buf, 2);
        for f in buf.chunks(2) {
            assert_eq!(f[0], f[1]);
        }
        assert!(render("silence", 10).iter().all(|&s| s == 0.0));
    }
}
