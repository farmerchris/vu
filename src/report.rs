//! Headless analysis: run the analyzer over the whole input and print statistics.

use crate::analyzer::Analyzer;
use crate::dsp::level::{amp_to_db, crest_factor_db};
use crate::dsp::spectrum::peak_frequency;
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

pub fn run(source: Source, analyzer: &mut Analyzer, duration: Option<f64>, mut recorder: Option<Recorder>) -> Result<()> {
    if source.info.kind != SourceKind::File && duration.is_none() {
        bail!("--report with a live input or generator needs --duration");
    }
    let ch = source.info.channels;
    let limit = duration.map(|d| (d * source.info.sample_rate as f64).round() as u64);
    let mut frames = 0usize;
    while let Ok(mut chunk) = source.rx.recv() {
        let keep = truncate_len(chunk.len(), ch, analyzer.samples(), limit);
        chunk.truncate(keep);
        if let Some(r) = recorder.as_mut() {
            r.write(&chunk)?;
        }
        analyzer.push(&chunk, |_| frames += 1);
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
    print!("{}", format_report(&source.info.name, source.info.sample_rate, ch, analyzer, frames));
    Ok(())
}

pub fn format_report(name: &str, sample_rate: u32, ch: usize, a: &Analyzer, frames: usize) -> String {
    let s = &a.stats;
    let mut out = String::new();
    let secs = s.frames as f64 / sample_rate as f64;
    let _ = writeln!(out, "Source        {name}  ({sample_rate} Hz, {ch} ch)");
    let _ = writeln!(out, "Duration      {secs:.3} s  ({} samples, {frames} analysis frames)", s.frames);
    let per = |f: &dyn Fn(usize) -> String| -> String {
        (0..ch).map(|c| format!("{:>3} {:>9}", channel_label(c, ch), f(c))).collect::<Vec<_>>().join("  ")
    };
    let _ = writeln!(out, "Peak          {}  dBFS", per(&|c| format!("{:.2}", amp_to_db(s.peak[c]))));
    let _ = writeln!(out, "RMS           {}  dBFS", per(&|c| format!("{:.2}", amp_to_db(s.rms(c)))));
    let _ = writeln!(out, "Crest factor  {}  dB", per(&|c| format!("{:.2}", crest_factor_db(s.peak[c], s.rms(c)))));
    let _ = writeln!(out, "Clipped       {}  samples", per(&|c| s.clipped[c].to_string()));
    let l = &a.loudness;
    let lu = |v: f32| if v.is_finite() { format!("{v:.1} LUFS") } else { "-inf".into() };
    let _ = writeln!(out, "Integrated    {}", lu(l.integrated()));
    let _ = writeln!(out, "Max momentary {}", lu(l.max_momentary()));
    let _ = writeln!(out, "Max short-t.  {}", lu(l.max_short_term()));
    let cfg = a.config();
    let mean = s.mean_spectrum();
    let bin_hz = cfg.bin_hz();
    let lo = ((cfg.min_freq / bin_hz).floor() as usize).max(1);
    let hi = (cfg.top_freq() / bin_hz).ceil() as usize;
    match peak_frequency(&mean, bin_hz, lo, hi) {
        Some((f, db)) => {
            let _ = writeln!(out, "Dominant freq {f:.1} Hz  ({db:.1} dB, {})", note_name(f));
        }
        None => {
            let _ = writeln!(out, "Dominant freq —");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::tests::config;

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
        let mut a = Analyzer::new(config(2));
        let data: Vec<f32> = (0..48000 * 2)
            .flat_map(|i| {
                let s = 0.5 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin();
                [s, s]
            })
            .collect();
        let mut n = 0;
        a.push(&data, |_| n += 1);
        let r = format_report("test", 48000, 2, &a, n);
        assert!(r.contains("2.000 s"), "{r}");
        assert!(r.contains("-6.02"), "{r}");
        assert!(r.contains("-9.03"), "{r}");
        assert!(r.contains("3.01"), "{r}");
        assert!(r.contains("A 4"), "{r}");
        // K-weighting is ~0.7 dB lower at 440 Hz than at the 997 Hz reference.
        assert!(r.contains("Integrated    -6.7 LUFS"), "{r}");
        // Short-term needs 3 s of audio.
        assert!(r.contains("Max short-t.  -inf"), "{r}");
    }
}
