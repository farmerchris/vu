//! `--log`: levels and loudness over time, as CSV rows at a fixed interval.

use crate::analyzer::{Frame, MAX_CHANNELS};
use crate::dsp::level::{power_to_db, FLOOR_DB};
use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

pub const HEADER: &str =
    "time_s,peak_dbfs,true_peak_dbtp,rms_dbfs,lufs_m,lufs_s,lufs_i,lra_lu,correlation,clipped";

/// What one row covers: the frames since the previous row.
struct Interval {
    frames: u32,
    peak: f32,
    true_peak: f32,
    /// Sum over the frames of each channel's mean square.
    power: [f64; MAX_CHANNELS],
    lufs_m: f32,
    clipped: bool,
}

impl Default for Interval {
    fn default() -> Self {
        Interval {
            frames: 0,
            peak: FLOOR_DB,
            true_peak: FLOOR_DB,
            power: [0.0; MAX_CHANNELS],
            lufs_m: f32::NEG_INFINITY,
            clipped: false,
        }
    }
}

/// Writes a row every `interval` seconds of stream time: the highest peak,
/// true peak and momentary loudness of the interval, the RMS of its loudest
/// channel, and the short-term, integrated, range and correlation readings
/// at its end.
pub struct LevelLog<W: Write = BufWriter<File>> {
    out: W,
    pub path: PathBuf,
    interval: f64,
    /// Stream time at which the row in progress ends.
    next: f64,
    acc: Interval,
    /// The newest frame's time and end-of-interval readings (short-term,
    /// integrated, range, correlation).
    last: (f64, [f32; 4]),
    pub rows: u64,
}

impl LevelLog {
    pub fn create(path: &Path, interval: f64) -> Result<Self> {
        let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
        let mut log = LevelLog::new(BufWriter::new(file), interval)?;
        log.path = path.to_path_buf();
        Ok(log)
    }
}

/// A CSV number: finite values to `dec` places, -inf as written, and
/// nothing at all for NaN (not measured).
fn cell(v: f32, dec: usize) -> String {
    if v.is_nan() {
        String::new()
    } else if v == f32::NEG_INFINITY || v <= FLOOR_DB {
        "-inf".into()
    } else {
        format!("{v:.dec$}")
    }
}

impl<W: Write> LevelLog<W> {
    pub fn new(mut out: W, interval: f64) -> Result<Self> {
        writeln!(out, "{HEADER}")?;
        Ok(LevelLog {
            out,
            path: PathBuf::new(),
            interval,
            next: interval,
            acc: Interval::default(),
            last: (0.0, [f32::NAN; 4]),
            rows: 0,
        })
    }

    /// Fold in a frame, writing a row once it reaches the end of the interval.
    pub fn add(&mut self, f: &Frame) -> Result<()> {
        let a = &mut self.acc;
        a.frames += 1;
        a.peak = a.peak.max(f.max_peak());
        a.true_peak = f.true_peak[..f.n()]
            .iter()
            .copied()
            .fold(a.true_peak, f32::max);
        for c in 0..f.n() {
            a.power[c] += 10f64.powf(f.rms[c] as f64 / 10.0);
        }
        a.lufs_m = a.lufs_m.max(f.lufs_m);
        a.clipped |= f.clipped;
        self.last = (f.time, [f.lufs_s, f.lufs_i, f.lufs_lra, f.correlation]);
        // Half a millisecond of slack: frame times are sums of hops.
        if f.time + 5e-4 < self.next {
            return Ok(());
        }
        // After a gap in the stream (a paused input) the row is the one the
        // frame falls in, not a late one for each interval missed.
        while self.next + self.interval <= f.time + 5e-4 {
            self.next += self.interval;
        }
        self.row(self.next)?;
        self.next += self.interval;
        Ok(())
    }

    /// Write the row for the frames gathered, stamped `time`.
    fn row(&mut self, time: f64) -> Result<()> {
        let a = std::mem::take(&mut self.acc);
        let rms = a.power.iter().copied().fold(0.0, f64::max) / a.frames.max(1) as f64;
        let [lufs_s, lufs_i, lra, correlation] = self.last.1;
        writeln!(
            self.out,
            "{time:.3},{},{},{},{},{},{},{},{},{}",
            cell(a.peak, 2),
            cell(a.true_peak, 2),
            cell(power_to_db(rms as f32), 2),
            cell(a.lufs_m, 1),
            cell(lufs_s, 1),
            cell(lufs_i, 1),
            cell(lra, 1),
            cell(correlation, 2),
            a.clipped as u8
        )?;
        // So `tail -f` follows a live log.
        self.out.flush()?;
        self.rows += 1;
        Ok(())
    }

    /// Write what is left of the last interval, stamped with the time the
    /// stream ended.
    pub fn finish(&mut self) -> Result<()> {
        if self.acc.frames > 0 {
            self.row(self.last.0)?;
        }
        Ok(self.out.flush()?)
    }

    pub fn into_inner(self) -> W {
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(time: f64, peak: f32, lufs_m: f32) -> Frame {
        let mut f = Frame {
            time,
            channels: 2,
            lufs_m,
            lufs_s: -20.0,
            lufs_i: -21.5,
            clipped: peak >= 0.0,
            ..Frame::default()
        };
        f.peak[0] = peak;
        f.true_peak[0] = if peak > FLOOR_DB { peak + 0.5 } else { peak };
        f.rms[0] = peak - 3.0;
        f
    }

    #[test]
    fn rows_cover_their_interval() {
        let mut log = LevelLog::new(Vec::new(), 1.0).unwrap();
        // Ten frames a second for 2.5 s; the loudest of the first second
        // comes early in it.
        for i in 1..=25 {
            let peak = if i == 3 { 0.0 } else { -12.0 };
            log.add(&frame(i as f64 * 0.1, peak, -30.0 + i as f32))
                .unwrap();
        }
        assert_eq!(log.rows, 2);
        // The half second left over is written at the end, once.
        log.finish().unwrap();
        log.finish().unwrap();
        assert_eq!(log.rows, 3);
        let text = String::from_utf8(log.into_inner()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], HEADER);
        assert_eq!(lines.len(), 4, "{text}");
        assert!(lines[3].starts_with("2.500,-12.00,"), "{text}");
        let cols: Vec<&str> = lines[1].split(',').collect();
        assert_eq!(cols.len(), HEADER.split(',').count());
        assert_eq!(cols[0], "1.000");
        assert_eq!((cols[1], cols[2]), ("0.00", "0.50"));
        // RMS is the mean power: nine frames at -15 and one at -3.
        let rms: f32 = cols[3].parse().unwrap();
        assert!((rms - -11.05).abs() < 0.05, "{rms}");
        assert_eq!(cols[4], "-20.0", "highest momentary of the interval");
        assert_eq!((cols[5], cols[6]), ("-20.0", "-21.5"));
        assert_eq!((cols[7], cols[8]), ("", ""), "not measured: empty");
        assert_eq!(cols[9], "1");
        let cols: Vec<&str> = lines[2].split(',').collect();
        assert_eq!((cols[0], cols[1], cols[9]), ("2.000", "-12.00", "0"));
    }

    #[test]
    fn silence_and_gaps() {
        let mut log = LevelLog::new(Vec::new(), 0.5).unwrap();
        log.add(&frame(0.5, FLOOR_DB, f32::NEG_INFINITY)).unwrap();
        // The input was paused: the next frame is 3 s on.
        log.add(&frame(3.6, -6.0, -18.0)).unwrap();
        log.add(&frame(4.0, -6.0, -18.0)).unwrap();
        let text = String::from_utf8(log.into_inner()).unwrap();
        let lines: Vec<&str> = text.lines().skip(1).collect();
        assert!(lines[0].starts_with("0.500,-inf,-inf,-inf,-inf,"), "{text}");
        assert_eq!(lines.len(), 3, "one row per interval reached: {text}");
        assert!(lines[1].starts_with("3.500,-6.00"), "{text}");
        assert!(lines[2].starts_with("4.000,-6.00"), "{text}");
        assert_eq!(cell(f32::NAN, 1), "");
        assert_eq!(cell(-3.25159, 2), "-3.25");
    }
}
