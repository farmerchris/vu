//! Headless analysis: run the analyzer over each whole input and print its
//! statistics as text, JSON or CSV, with an optional compliance check.

use crate::analyzer::Analyzer;
use crate::cli::{Cli, ReportFormat};
use crate::dsp::level::{amp_to_db, crest_factor_db, FLOOR_DB};
use crate::dsp::spectrum::peak_frequency;
use crate::levellog::LevelLog;
use crate::recorder::Recorder;
use crate::source::{Source, SourceKind};
use crate::ui::meter::{channel_label, note_name};
use anyhow::{bail, Result};
use std::fmt::Write;

/// Number of interleaved samples of `chunk` to keep so at most `limit` sample
/// frames are consumed in total, given `done` frames already consumed.
pub fn truncate_len(chunk_len: usize, channels: usize, done: u64, limit: Option<u64>) -> usize {
    match limit {
        None => chunk_len,
        Some(l) => {
            let left = l.saturating_sub(done) as usize;
            chunk_len.min(left.saturating_mul(channels.max(1)))
        }
    }
}

/// Run the analyzer over the whole of `source` (or `duration` seconds of
/// it), recording and logging on the way. Returns the number of analysis
/// frames.
pub fn analyze(
    source: &Source,
    analyzer: &mut Analyzer,
    duration: Option<f64>,
    mut recorder: Option<Recorder>,
    mut log: Option<LevelLog>,
) -> Result<usize> {
    if source.info.kind != SourceKind::File && duration.is_none() {
        bail!("--report with a live input or generator needs --duration");
    }
    let ch = source.info.channels;
    let limit = duration.map(|d| (d * source.info.sample_rate as f64).round() as u64);
    let mut frames = 0usize;
    let mut log_error = None;
    while let Ok(mut chunk) = source.rx.recv() {
        let keep = truncate_len(chunk.len(), ch, analyzer.samples(), limit);
        chunk.truncate(keep);
        if let Some(r) = recorder.as_mut() {
            r.write(&chunk)?;
        }
        analyzer.push(&chunk, |f| {
            frames += 1;
            if let Some(Err(e)) = log.as_mut().map(|l| l.add(&f)) {
                log_error = Some(e);
            }
        });
        if let Some(e) = log_error.take() {
            return Err(e.context("writing the log"));
        }
        if limit.is_some_and(|l| analyzer.samples() >= l) {
            break;
        }
    }
    if let Some(e) = source.control.take_error() {
        bail!(e);
    }
    if let Some(mut r) = recorder.take() {
        r.finish()?;
        eprintln!("recorded {}", r.path.display());
    }
    if let Some(mut l) = log.take() {
        l.finish()?;
        eprintln!("logged {} rows to {}", l.rows, l.path.display());
    }
    Ok(frames)
}

/// `--check`: whether an input meets a loudness target and true-peak limit.
#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    /// Allowed distance of integrated loudness from the target, LU.
    pub tolerance: f32,
    /// Highest true peak allowed, dBTP.
    pub tp_limit: f32,
    pub loudness_ok: bool,
    pub peak_ok: bool,
}

impl Check {
    pub fn passed(&self) -> bool {
        self.loudness_ok && self.peak_ok
    }
}

/// Whole-input statistics, as `--report` prints them. Levels are in dB;
/// per-channel vectors hold every channel of the input.
#[derive(Clone, Debug)]
pub struct Report {
    pub name: String,
    pub sample_rate: u32,
    pub channels: usize,
    /// Length in sample frames, and in analysis frames.
    pub samples: u64,
    pub frames: usize,
    pub peak: Vec<f32>,
    pub true_peak: Vec<f32>,
    pub rms: Vec<f32>,
    pub crest: Vec<f32>,
    /// Mean sample value of each channel, linear.
    pub dc: Vec<f32>,
    pub clipped: Vec<u64>,
    pub integrated: f32,
    pub max_momentary: f32,
    pub max_short_term: f32,
    /// Loudness range, LU; NaN for inputs under 3 s.
    pub lra: f32,
    /// Highest true peak of any channel, dBTP.
    pub max_true_peak: f32,
    /// Phase correlation of the first two channels over the whole input.
    pub correlation: Option<f32>,
    /// The loudness target (`--lufs-target`), LUFS.
    pub target: f32,
    /// Strongest frequency of the mean spectrum, Hz and dB.
    pub dominant: Option<(f32, f32)>,
    pub check: Option<Check>,
}

/// A level for text: two decimals, or -inf.
fn db2(v: f32) -> String {
    if v.is_finite() && v > FLOOR_DB {
        format!("{v:.2}")
    } else {
        "-inf".into()
    }
}

/// A number for JSON: `null` when not finite.
fn json_num(v: f32, dec: usize) -> String {
    if v.is_finite() && v > FLOOR_DB {
        format!("{v:.dec$}")
    } else {
        "null".into()
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A CSV field, quoted when it needs to be.
fn csv_str(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

pub const CSV_HEADER: &str = "file,sample_rate,channels,duration_s,peak_dbfs,true_peak_dbtp,\
rms_dbfs,crest_db,integrated_lufs,max_momentary_lufs,max_short_term_lufs,lra_lu,plr_db,\
correlation,dc_offset,clipped_samples,gain_to_target_db,dominant_hz,check";

impl Report {
    /// Gather the statistics of an analyzer that has seen the whole input.
    /// `check` is the `--check` tolerance (LU) and true-peak limit (dBTP).
    pub fn new(
        name: &str,
        a: &Analyzer,
        frames: usize,
        target: f32,
        check: Option<(f32, f32)>,
    ) -> Report {
        let (s, cfg, l) = (&a.stats, a.config(), &a.loudness);
        let ch = s.peak.len();
        let per = |f: &dyn Fn(usize) -> f32| (0..ch).map(f).collect::<Vec<f32>>();
        let mean = s.mean_spectrum();
        let bin_hz = cfg.bin_hz();
        let lo = ((cfg.min_freq / bin_hz).ceil() as usize).max(1);
        let hi = (cfg.top_freq() / bin_hz).ceil() as usize;
        // Never below the sample peak (channels past the metered ones have
        // no true-peak measurement of their own).
        let true_peak = per(&|c| amp_to_db(s.true_peak[c].max(s.peak[c])));
        let mut r = Report {
            name: name.to_string(),
            sample_rate: cfg.sample_rate,
            channels: ch,
            samples: s.frames,
            frames,
            peak: per(&|c| amp_to_db(s.peak[c])),
            max_true_peak: true_peak.iter().copied().fold(FLOOR_DB, f32::max),
            true_peak,
            rms: per(&|c| amp_to_db(s.rms(c))),
            crest: per(&|c| crest_factor_db(s.peak[c], s.rms(c))),
            dc: per(&|c| s.dc(c)),
            clipped: s.clipped.clone(),
            integrated: l.integrated(),
            max_momentary: l.max_momentary(),
            max_short_term: l.max_short_term(),
            lra: l.loudness_range(),
            correlation: s.correlation(),
            target,
            dominant: if s.frames > 0 {
                peak_frequency(&mean, bin_hz, lo, hi)
            } else {
                None
            },
            check: None,
        };
        r.check = check.map(|(tolerance, tp_limit)| Check {
            tolerance,
            tp_limit,
            // Rounded as printed, so a file shown on the limit passes.
            loudness_ok: r.integrated.is_finite()
                && ((r.integrated - target) * 10.0).round().abs() <= (tolerance * 10.0).round(),
            peak_ok: (r.max_true_peak * 100.0).round() <= (tp_limit * 100.0).round(),
        });
        r
    }

    pub fn duration(&self) -> f64 {
        self.samples as f64 / self.sample_rate.max(1) as f64
    }

    /// Peak to loudness ratio: the highest true peak over the integrated
    /// loudness, dB. NaN for silence.
    pub fn plr(&self) -> f32 {
        if self.integrated.is_finite() {
            self.max_true_peak - self.integrated
        } else {
            f32::NAN
        }
    }

    /// Gain that would bring the integrated loudness to the target, dB.
    pub fn gain_to_target(&self) -> f32 {
        if self.integrated.is_finite() {
            self.target - self.integrated
        } else {
            f32::NAN
        }
    }

    /// What `--check` found wrong, if anything.
    fn failures(&self) -> Vec<String> {
        let Some(c) = &self.check else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if !c.loudness_ok {
            out.push(if self.integrated.is_finite() {
                let off = self.integrated - self.target;
                format!(
                    "integrated {:.1} LUFS is {:.1} LU {} the {} LUFS target (\u{b1}{} LU)",
                    self.integrated,
                    off.abs(),
                    if off > 0.0 { "above" } else { "below" },
                    self.target,
                    c.tolerance
                )
            } else {
                "no loudness to measure (silence)".into()
            });
        }
        if !c.peak_ok {
            out.push(format!(
                "true peak {} dBTP is over the {} dBTP limit",
                db2(self.max_true_peak),
                c.tp_limit
            ));
        }
        out
    }

    pub fn to_text(&self) -> String {
        let ch = self.channels;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "Source        {}  ({} Hz, {ch} ch)",
            self.name, self.sample_rate
        );
        let _ = writeln!(
            out,
            "Duration      {:.3} s  ({} samples, {} analysis frames)",
            self.duration(),
            self.samples,
            self.frames
        );
        let per = |f: &dyn Fn(usize) -> String| -> String {
            (0..ch)
                .map(|c| format!("{:>3} {:>9}", channel_label(c, ch), f(c)))
                .collect::<Vec<_>>()
                .join("  ")
        };
        let two = |v: &[f32]| per(&|c| format!("{:.2}", v[c]));
        let _ = writeln!(out, "Peak          {}  dBFS", two(&self.peak));
        let _ = writeln!(out, "True peak     {}  dBTP", two(&self.true_peak));
        let _ = writeln!(out, "RMS           {}  dBFS", two(&self.rms));
        let _ = writeln!(out, "Crest factor  {}  dB", two(&self.crest));
        let _ = writeln!(
            out,
            "DC offset     {}  dBFS",
            per(&|c| db2(amp_to_db(self.dc[c].abs())))
        );
        let _ = writeln!(
            out,
            "Clipped       {}  samples",
            per(&|c| self.clipped[c].to_string())
        );
        let lu = |v: f32| {
            if v.is_finite() {
                format!("{v:.1} LUFS")
            } else {
                "-inf".into()
            }
        };
        let _ = writeln!(out, "Integrated    {}", lu(self.integrated));
        let _ = writeln!(out, "Max momentary {}", lu(self.max_momentary));
        let _ = writeln!(out, "Max short-t.  {}", lu(self.max_short_term));
        let _ = writeln!(
            out,
            "Loud. range   {}",
            if self.lra.is_finite() {
                format!("{:.1} LU", self.lra)
            } else {
                "\u{2014} (needs 3 s)".into()
            }
        );
        let _ = writeln!(out, "Max true peak {} dBTP", db2(self.max_true_peak));
        if self.plr().is_finite() {
            let _ = writeln!(out, "PLR           {:.1} dB", self.plr());
        }
        if let Some(c) = self.correlation {
            let _ = writeln!(out, "Correlation   {c:+.2}");
        }
        let gain = self.gain_to_target();
        if gain.is_finite() {
            let _ = writeln!(
                out,
                "To target     {gain:+.1} dB to reach {} LUFS (true peak would be {:.1} dBTP)",
                self.target,
                self.max_true_peak + gain
            );
        }
        match self.dominant {
            Some((f, db)) => {
                let _ = writeln!(
                    out,
                    "Dominant freq {f:.1} Hz  ({db:.1} dB, {})",
                    note_name(f)
                );
            }
            None => {
                let _ = writeln!(out, "Dominant freq \u{2014}");
            }
        }
        if let Some(c) = &self.check {
            if c.passed() {
                let _ = writeln!(
                    out,
                    "Check         PASS  (within {} LU of {} LUFS, true peak at most {} dBTP)",
                    c.tolerance, self.target, c.tp_limit
                );
            } else {
                let _ = writeln!(out, "Check         FAIL  {}", self.failures().join("; "));
            }
        }
        out
    }

    pub fn to_json(&self) -> String {
        let list = |v: &[f32], dec: usize| {
            let items: Vec<String> = v.iter().map(|&x| json_num(x, dec)).collect();
            format!("[{}]", items.join(", "))
        };
        let mut f: Vec<(&str, String)> = vec![
            ("file", json_str(&self.name)),
            ("sample_rate", self.sample_rate.to_string()),
            ("channels", self.channels.to_string()),
            ("duration_s", format!("{:.3}", self.duration())),
            ("samples", self.samples.to_string()),
            ("peak_dbfs", list(&self.peak, 2)),
            ("true_peak_dbtp", list(&self.true_peak, 2)),
            ("rms_dbfs", list(&self.rms, 2)),
            ("crest_db", list(&self.crest, 2)),
            (
                "dc_offset",
                format!(
                    "[{}]",
                    self.dc
                        .iter()
                        .map(|&d| format!("{:.6}", if d.abs() < 5e-7 { 0.0 } else { d }))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ),
            (
                "clipped_samples",
                format!(
                    "[{}]",
                    self.clipped
                        .iter()
                        .map(u64::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ),
            ("integrated_lufs", json_num(self.integrated, 1)),
            ("max_momentary_lufs", json_num(self.max_momentary, 1)),
            ("max_short_term_lufs", json_num(self.max_short_term, 1)),
            ("lra_lu", json_num(self.lra, 1)),
            ("max_true_peak_dbtp", json_num(self.max_true_peak, 2)),
            ("plr_db", json_num(self.plr(), 1)),
            (
                "correlation",
                json_num(self.correlation.unwrap_or(f32::NAN), 2),
            ),
            ("target_lufs", json_num(self.target, 1)),
            ("gain_to_target_db", json_num(self.gain_to_target(), 1)),
            (
                "dominant_hz",
                json_num(self.dominant.map_or(f32::NAN, |d| d.0), 1),
            ),
        ];
        if let Some(c) = &self.check {
            let failures: Vec<String> = self.failures().iter().map(|s| json_str(s)).collect();
            f.push((
                "check",
                format!(
                    "{{\"passed\": {}, \"tolerance_lu\": {}, \"tp_limit_dbtp\": {}, \"failures\": [{}]}}",
                    c.passed(),
                    c.tolerance,
                    c.tp_limit,
                    failures.join(", ")
                ),
            ));
        }
        let body: Vec<String> = f.iter().map(|(k, v)| format!("    \"{k}\": {v}")).collect();
        format!("  {{\n{}\n  }}", body.join(",\n"))
    }

    /// One CSV row (see [`CSV_HEADER`]); per-channel levels are given for
    /// the highest channel. Empty cells are values that do not apply.
    pub fn to_csv(&self) -> String {
        let max = |v: &[f32]| v.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let num = |v: f32, dec: usize| {
            if v.is_nan() {
                String::new()
            } else if v.is_finite() && v > FLOOR_DB {
                format!("{v:.dec$}")
            } else {
                "-inf".into()
            }
        };
        let dc = self
            .dc
            .iter()
            .copied()
            .fold(0.0f32, |m, d| if d.abs() > m.abs() { d } else { m });
        [
            csv_str(&self.name),
            self.sample_rate.to_string(),
            self.channels.to_string(),
            format!("{:.3}", self.duration()),
            num(max(&self.peak), 2),
            num(self.max_true_peak, 2),
            num(max(&self.rms), 2),
            num(max(&self.crest), 2),
            num(self.integrated, 1),
            num(self.max_momentary, 1),
            num(self.max_short_term, 1),
            num(self.lra, 1),
            num(self.plr(), 1),
            num(self.correlation.unwrap_or(f32::NAN), 2),
            // Never "-0.000000".
            format!("{:.6}", if dc.abs() < 5e-7 { 0.0 } else { dc }),
            self.clipped.iter().sum::<u64>().to_string(),
            num(self.gain_to_target(), 1),
            num(self.dominant.map_or(f32::NAN, |d| d.0), 1),
            match &self.check {
                Some(c) if c.passed() => "pass".into(),
                Some(_) => "fail".into(),
                None => String::new(),
            },
        ]
        .join(",")
    }
}

/// Print reports in `format`: text blocks a blank line apart, a JSON array,
/// or CSV rows under a header.
pub fn format_reports(reports: &[Report], format: ReportFormat) -> String {
    match format {
        ReportFormat::Text => reports
            .iter()
            .map(Report::to_text)
            .collect::<Vec<_>>()
            .join("\n"),
        ReportFormat::Json => {
            let items: Vec<String> = reports.iter().map(Report::to_json).collect();
            format!("[\n{}\n]\n", items.join(",\n"))
        }
        ReportFormat::Csv => {
            let mut out = format!("{CSV_HEADER}\n");
            for r in reports {
                out.push_str(&r.to_csv());
                out.push('\n');
            }
            out
        }
    }
}

/// What a `--report` run came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    /// `--check` failed for at least one input.
    CheckFailed,
    /// At least one input could not be read.
    InputFailed,
}

/// `--report`: analyze each input named on the command line (or the live
/// input or test signal, for `--duration`) and print the statistics. An
/// input that cannot be read is reported on stderr and the rest carry on.
pub fn run(cli: &Cli) -> Result<Outcome> {
    let inputs = cli.inputs();
    let runs: Vec<Cli> = if inputs.is_empty() {
        vec![cli.clone()]
    } else {
        inputs.into_iter().map(|p| cli.for_input(p)).collect()
    };
    let one = runs.len() == 1;
    let mut reports = Vec::new();
    let mut failed = false;
    for c in &runs {
        match report_one(c) {
            Ok(r) => reports.push(r),
            // A lone input's error is the command's.
            Err(e) if one => return Err(e),
            Err(e) => {
                failed = true;
                let name = c.file.as_deref().map(|p| p.display().to_string());
                eprintln!("vu-meter: {}: {e:#}", name.unwrap_or_default());
            }
        }
    }
    print!("{}", format_reports(&reports, cli.format));
    Ok(if failed {
        Outcome::InputFailed
    } else if reports
        .iter()
        .any(|r| r.check.as_ref().is_some_and(|c| !c.passed()))
    {
        Outcome::CheckFailed
    } else {
        Outcome::Ok
    })
}

fn report_one(cli: &Cli) -> Result<Report> {
    let source = crate::source::open(cli, true, false)?;
    let info = &source.info;
    let mut analyzer = Analyzer::new(crate::pump::analyzer_config(cli, info)?);
    let recorder = match &cli.record {
        Some(p) => Some(Recorder::create(
            p,
            info.channels,
            info.sample_rate,
            cli.record_format,
        )?),
        None => None,
    };
    let log = match &cli.log {
        Some(p) => Some(LevelLog::create(p, cli.log_interval)?),
        None => None,
    };
    let frames = analyze(&source, &mut analyzer, cli.duration, recorder, log)?;
    Ok(Report::new(
        &info.name,
        &analyzer,
        frames,
        cli.lufs_target,
        cli.check.then_some((cli.tolerance, cli.tp_limit)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::tests::config;

    fn tone(amp: f32, secs: usize) -> (Analyzer, usize) {
        let mut a = Analyzer::new(config(2));
        let data: Vec<f32> = (0..48000 * secs)
            .flat_map(|i| {
                let s = amp * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin();
                [s, s]
            })
            .collect();
        let mut n = 0;
        a.push(&data, |_| n += 1);
        (a, n)
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate_len(100, 2, 0, None), 100);
        assert_eq!(truncate_len(100, 2, 0, Some(10)), 20);
        assert_eq!(truncate_len(100, 2, 10, Some(10)), 0);
        assert_eq!(truncate_len(100, 2, 20, Some(10)), 0);
        assert_eq!(truncate_len(10, 2, 0, Some(1000)), 10);
    }

    #[test]
    fn report_contents() {
        let (a, n) = tone(0.5, 2);
        let rep = Report::new("test", &a, n, -14.0, None);
        let r = rep.to_text();
        assert!(r.contains("2.000 s"), "{r}");
        assert!(r.contains("-6.02"), "{r}");
        assert!(r.contains("-9.03"), "{r}");
        assert!(r.contains("3.01"), "{r}");
        assert!(r.contains("A 4"), "{r}");
        // K-weighting is ~0.7 dB lower at 440 Hz than at the 997 Hz reference.
        assert!(r.contains("Integrated    -6.7 LUFS"), "{r}");
        // Short-term needs 3 s of audio.
        assert!(r.contains("Max short-t.  -inf"), "{r}");
        // New in 2.0: the same signal on both channels is fully correlated,
        // has no DC, and needs turning down 7.3 dB for a -14 LUFS target.
        assert!(r.contains("Correlation   +1.00"), "{r}");
        assert!(
            r.contains("DC offset       L      -inf    R      -inf  dBFS"),
            "{r}"
        );
        assert!(r.contains("To target     -7.3 dB to reach -14 LUFS"), "{r}");
        assert!(r.contains("PLR           0.7 dB"), "{r}");
        assert!(!r.contains("Check"), "{r}");
        assert!((rep.plr() - 0.7).abs() < 0.1 && (rep.gain_to_target() + 7.3).abs() < 0.1);
    }

    #[test]
    fn check_passes_and_fails() {
        // -20 dBFS stereo 440 Hz: -20.7 LUFS.
        let (a, n) = tone(0.1, 2);
        let pass = Report::new("t", &a, n, -20.0, Some((1.0, -1.0)));
        assert!(pass.check.as_ref().unwrap().passed(), "{}", pass.to_text());
        assert!(pass.to_text().contains("Check         PASS"));
        assert!(pass.to_csv().ends_with(",pass"));
        // Too quiet for -14 by more than 1 LU.
        let quiet = Report::new("t", &a, n, -14.0, Some((1.0, -1.0)));
        let c = quiet.check.clone().unwrap();
        assert!(!c.loudness_ok && c.peak_ok && !c.passed());
        let text = quiet.to_text();
        assert!(
            text.contains("Check         FAIL  integrated -20.7 LUFS is 6.7 LU below"),
            "{text}"
        );
        assert!(quiet.to_csv().ends_with(",fail"));
        // A wide tolerance lets the loudness through; a strict peak limit does not.
        let peak = Report::new("t", &a, n, -14.0, Some((10.0, -30.0)));
        let c = peak.check.clone().unwrap();
        assert!(c.loudness_ok && !c.peak_ok);
        assert!(peak.to_text().contains("is over the -30 dBTP limit"));
        // Silence cannot meet a loudness target.
        let mut a = Analyzer::new(config(2));
        a.push(&[0.0; 96000], |_| {});
        let silent = Report::new("s", &a, 0, -14.0, Some((1.0, -1.0)));
        assert!(!silent.check.as_ref().unwrap().loudness_ok);
        assert!(silent.to_text().contains("silence"));
        assert!(silent.plr().is_nan() && silent.gain_to_target().is_nan());
    }

    #[test]
    fn json_and_csv() {
        let (a, n) = tone(0.5, 2);
        let r = Report::new("a \"b\",c.wav", &a, n, -14.0, Some((1.0, -1.0)));
        let json = format_reports(&[r.clone(), r.clone()], ReportFormat::Json);
        assert!(
            json.starts_with("[\n  {") && json.trim_end().ends_with(']'),
            "{json}"
        );
        assert!(json.contains(r#""file": "a \"b\",c.wav""#), "{json}");
        assert!(json.contains(r#""peak_dbfs": [-6.02, -6.02]"#), "{json}");
        assert!(json.contains(r#""integrated_lufs": -6.7"#), "{json}");
        // Not measurable in 2 s: null, not a bare NaN or -inf.
        assert!(json.contains(r#""max_short_term_lufs": null"#), "{json}");
        assert!(json.contains(r#""lra_lu": null"#), "{json}");
        assert!(json.contains(r#""check": {"passed": false"#), "{json}");
        assert!(!json.contains("NaN") && !json.contains("inf"), "{json}");
        // Balanced brackets and quotes, as a cheap well-formedness check.
        for (open, close) in [('{', '}'), ('[', ']')] {
            assert_eq!(json.matches(open).count(), json.matches(close).count());
        }
        assert_eq!(json.matches("\"file\"").count(), 2);

        let csv = format_reports(&[r], ReportFormat::Csv);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], CSV_HEADER);
        assert!(
            lines[1].starts_with(r#""a ""b"",c.wav",48000,2,2.000,-6.02,"#),
            "{csv}"
        );
        // The quoted name holds one comma; the rest line up with the header.
        assert_eq!(
            lines[1].split(',').count(),
            CSV_HEADER.split(',').count() + 1
        );
        assert_eq!(csv_str("plain.wav"), "plain.wav");
        assert_eq!(json_str("tab\there\u{1}"), "\"tab\\there\\u0001\"");
    }
}
