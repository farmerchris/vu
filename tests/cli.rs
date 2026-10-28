//! End-to-end tests of the built `vu` binary (headless modes only).

use std::path::PathBuf;
use std::process::{Command, Output};

fn vu(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vu")).args(args).output().expect("run vu")
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("vu-cli-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// First number following `label` on its line, in the report output.
fn number_after(report: &str, label: &str) -> f64 {
    let line = report.lines().find(|l| l.starts_with(label)).unwrap_or_else(|| panic!("no '{label}' line in:\n{report}"));
    line[label.len()..]
        .split_whitespace()
        .find_map(|t| t.parse::<f64>().ok())
        .unwrap_or_else(|| panic!("no number in '{line}'"))
}

#[test]
fn help_succeeds_and_mentions_keys() {
    let o = vu(&["--help"]);
    assert!(o.status.success());
    let s = out(&o);
    assert!(s.contains("KEYS"), "{s}");
    assert!(s.contains("PgUp/PgDn"), "{s}");
    assert!(s.contains("--report"), "{s}");
}

#[test]
fn version_succeeds() {
    let o = vu(&["--version"]);
    assert!(o.status.success());
    assert!(out(&o).starts_with("vu "));
}

#[test]
fn report_sine_levels() {
    let o = vu(&["--report", "-g", "sine:1000:-12", "-t", "2"]);
    assert!(o.status.success(), "{}", err(&o));
    let s = out(&o);
    let peak = number_after(&s, "Peak");
    assert!((peak + 12.0).abs() < 0.05, "peak {peak}\n{s}");
    let lufs = number_after(&s, "Integrated");
    // A full-scale-referenced 1 kHz sine at -12 dBFS (mono) measures about -12 LUFS.
    assert!((lufs + 12.0).abs() < 0.5, "lufs {lufs}\n{s}");
    assert!(number_after(&s, "Duration") == 2.0, "{s}");
    assert!(s.contains("Clipped"), "{s}");
}

#[test]
fn report_dominant_frequency_of_sine() {
    let o = vu(&["--report", "-g", "sine:1000:-12", "-t", "1"]);
    assert!(o.status.success(), "{}", err(&o));
    let f = number_after(&out(&o), "Dominant freq");
    assert!((f - 1000.0).abs() < 25.0, "dominant {f}");
}

#[test]
fn record_then_read_back_wav() {
    let d = TempDir::new("rec");
    let wav = d.0.join("noise.wav");
    let wav_s = wav.to_str().unwrap();
    let o = vu(&["--report", "-g", "noise", "-t", "1", "-r", wav_s]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(wav.exists(), "recording not written; stderr: {}", err(&o));
    assert!(std::fs::metadata(&wav).unwrap().len() > 44);

    let o = vu(&["--report", "-f", wav_s]);
    assert!(o.status.success(), "{}", err(&o));
    let s = out(&o);
    assert_eq!(number_after(&s, "Duration"), 1.0, "{s}");
}

#[test]
fn report_file_with_duration_limit() {
    let d = TempDir::new("lim");
    let wav = d.0.join("a.wav");
    let wav_s = wav.to_str().unwrap();
    assert!(vu(&["--report", "-g", "pink", "-t", "2", "-r", wav_s]).status.success());
    let o = vu(&["--report", "-f", wav_s, "-t", "0.5"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(number_after(&out(&o), "Duration"), 0.5);
}

fn assert_fails(args: &[&str], needle: &str) {
    let o = vu(args);
    assert!(!o.status.success(), "{args:?} unexpectedly succeeded:\n{}", out(&o));
    let e = err(&o);
    assert!(e.contains(needle), "{args:?}: stderr lacks '{needle}':\n{e}");
}

#[test]
fn invalid_fft_size() {
    assert_fails(&["--fft-size", "1000", "--report", "-g", "noise", "-t", "1"], "--fft-size");
}

#[test]
fn invalid_generator() {
    assert_fails(&["-g", "bogus", "--report", "-t", "1"], "bogus");
}

#[test]
fn report_generator_requires_duration() {
    assert_fails(&["--report", "-g", "noise"], "--duration");
}

#[test]
fn missing_file_fails() {
    let d = TempDir::new("missing");
    let p = d.0.join("nope.wav");
    let o = vu(&["--report", "-f", p.to_str().unwrap()]);
    assert!(!o.status.success());
    assert!(!err(&o).is_empty());
}

#[test]
fn conflicting_and_unknown_args_fail() {
    for args in [
        &["-f", "a.wav", "-g", "noise"][..],
        &["-p", "bogus"],
        &["--no-such-flag"],
        &["--hop", "0", "--report", "-g", "noise", "-t", "1"],
        &["--channel", "5", "--report", "-g", "noise", "-t", "1"],
    ] {
        let o = vu(args);
        assert!(!o.status.success(), "{args:?}");
        assert!(!err(&o).is_empty(), "{args:?}");
    }
}
