//! End-to-end tests of the built `vu-meter` binary (headless modes only).

use std::path::PathBuf;
use std::process::{Command, Output};

fn vu(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vu-meter"))
        .args(args)
        .output()
        .expect("run vu-meter")
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
    let line = report
        .lines()
        .find(|l| l.starts_with(label))
        .unwrap_or_else(|| panic!("no '{label}' line in:\n{report}"));
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
    assert!(out(&o).starts_with("vu-meter "));
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
    assert!(vu(&["--report", "-g", "pink", "-t", "2", "-r", wav_s])
        .status
        .success());
    let o = vu(&["--report", "-f", wav_s, "-t", "0.5"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(number_after(&out(&o), "Duration"), 0.5);
}

fn assert_fails(args: &[&str], needle: &str) {
    let o = vu(args);
    assert!(
        !o.status.success(),
        "{args:?} unexpectedly succeeded:\n{}",
        out(&o)
    );
    let e = err(&o);
    assert!(
        e.contains(needle),
        "{args:?}: stderr lacks '{needle}':\n{e}"
    );
}

#[test]
fn invalid_fft_size() {
    assert_fails(
        &["--fft-size", "1000", "--report", "-g", "noise", "-t", "1"],
        "--fft-size",
    );
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

/// A stereo WAV of a test signal, written by vu-meter itself.
fn make_wav(dir: &TempDir, name: &str, spec: &str, secs: &str) -> String {
    let path = dir.0.join(name);
    let s = path.to_str().unwrap().to_string();
    let o = vu(&["--report", "-g", spec, "-t", secs, "-r", &s]);
    assert!(o.status.success(), "{}", err(&o));
    s
}

/// The same audio as a 16-bit AIFF, a format only the decoders read.
fn wav_to_aiff(wav: &str, aiff: &std::path::Path) {
    let mut r = hound::WavReader::open(wav).unwrap();
    let spec = r.spec();
    let samples: Vec<i16> = r.samples::<i16>().map(Result::unwrap).collect();
    let frames = samples.len() as u32 / spec.channels as u32;
    let mut out = Vec::new();
    let data_len = samples.len() as u32 * 2;
    out.extend(b"FORM");
    out.extend((4 + 26 + 16 + data_len).to_be_bytes());
    out.extend(b"AIFFCOMM");
    out.extend(18u32.to_be_bytes());
    out.extend(spec.channels.to_be_bytes());
    out.extend(frames.to_be_bytes());
    out.extend(16u16.to_be_bytes());
    // The sample rate as an 80-bit extended float.
    let rate = spec.sample_rate as u64;
    let exp = 63 - rate.leading_zeros() as u16;
    out.extend((16383 + exp).to_be_bytes());
    out.extend((rate << (63 - exp)).to_be_bytes());
    out.extend(b"SSND");
    out.extend((8 + data_len).to_be_bytes());
    out.extend([0u8; 8]);
    for s in samples {
        out.extend(s.to_be_bytes());
    }
    std::fs::write(aiff, out).unwrap();
}

#[test]
fn positional_file_and_new_report_lines() {
    let d = TempDir::new("pos");
    let wav = make_wav(&d, "tone.wav", "sine:997:-20", "4");
    let o = vu(&["--report", &wav]);
    assert!(o.status.success(), "{}", err(&o));
    let s = out(&o);
    assert!((number_after(&s, "Integrated") + 20.0).abs() < 0.15, "{s}");
    assert!(
        (number_after(&s, "Max true peak") + 20.0).abs() < 0.05,
        "{s}"
    );
    assert!((number_after(&s, "Correlation") - 1.0).abs() < 0.01, "{s}");
    assert!(number_after(&s, "PLR").abs() < 0.2, "{s}");
    // -20 LUFS against the default -14 target.
    assert!((number_after(&s, "To target") - 6.0).abs() < 0.15, "{s}");
    assert!(s.contains("DC offset"), "{s}");
    assert_eq!(out(&vu(&["--report", "-f", &wav])), s, "-f is the same");
}

#[test]
fn reads_a_non_wav_format_and_standard_input() {
    let d = TempDir::new("fmt");
    let wav = make_wav(&d, "tone.wav", "sine:997:-20", "3");
    let aiff = d.0.join("tone.aiff");
    wav_to_aiff(&wav, &aiff);
    let o = vu(&["--report", aiff.to_str().unwrap()]);
    assert!(o.status.success(), "{}", err(&o));
    let s = out(&o);
    assert!(s.contains("tone.aiff  (48000 Hz, 2 ch)"), "{s}");
    assert_eq!(number_after(&s, "Duration"), 3.0, "{s}");
    assert!((number_after(&s, "Peak") + 20.0).abs() < 0.05, "{s}");
    assert!((number_after(&s, "Integrated") + 20.0).abs() < 0.15, "{s}");

    // `-f -` reads the same audio from a pipe.
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_vu-meter"))
        .args(["--report", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let bytes = std::fs::read(&wav).unwrap();
    child.stdin.take().unwrap().write_all(&bytes).unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", err(&o));
    let s = out(&o);
    assert!(s.contains("stdin  (48000 Hz, 2 ch)"), "{s}");
    assert_eq!(number_after(&s, "Duration"), 3.0, "{s}");
    assert!((number_after(&s, "Integrated") + 20.0).abs() < 0.15, "{s}");
}

#[test]
fn unreadable_files_say_why() {
    let d = TempDir::new("junk");
    let junk = d.0.join("junk.wav");
    std::fs::write(&junk, "this is not audio at all, just text\n".repeat(50)).unwrap();
    assert_fails(&["--report", junk.to_str().unwrap()], "not an audio file");
    assert_fails(&["a.wav", "b.wav"], "--report");
}

#[test]
fn several_files_as_csv_and_json() {
    let d = TempDir::new("multi");
    let a = make_wav(&d, "a.wav", "sine:997:-20", "2");
    let b = make_wav(&d, "b.wav", "sine:440:-6", "2");
    let o = vu(&["--report", "--format", "csv", &a, &b]);
    assert!(o.status.success(), "{}", err(&o));
    let s = out(&o);
    let lines: Vec<&str> = s.lines().collect();
    assert_eq!(lines.len(), 3, "{s}");
    assert!(lines[0].starts_with("file,sample_rate,channels,duration_s,peak_dbfs,"));
    assert!(lines[1].starts_with("a.wav,48000,2,2.000,-20.00,"), "{s}");
    assert!(lines[2].starts_with("b.wav,48000,2,2.000,-6.00,"), "{s}");
    for l in &lines {
        assert_eq!(l.split(',').count(), lines[0].split(',').count(), "{l}");
    }

    let o = vu(&["--report", "--format", "json", &a, &b]);
    assert!(o.status.success(), "{}", err(&o));
    let s = out(&o);
    assert!(
        s.trim_start().starts_with('[') && s.trim_end().ends_with(']'),
        "{s}"
    );
    assert_eq!(s.matches("\"file\":").count(), 2, "{s}");
    assert!(
        s.contains("\"file\": \"b.wav\"") && s.contains("\"peak_dbfs\": [-6.00, -6.00]"),
        "{s}"
    );
    // Text: one block per file, a blank line between.
    let s = out(&vu(&["--report", &a, &b]));
    assert_eq!(s.matches("Source   ").count(), 2, "{s}");
    assert!(s.contains("\n\nSource        b.wav"), "{s}");

    // One unreadable file does not stop the rest, but the run fails.
    let o = vu(&["--report", "--format", "csv", &a, "/nonexistent/x.wav", &b]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(out(&o).lines().count(), 3);
    assert!(err(&o).contains("/nonexistent/x.wav"), "{}", err(&o));
}

#[test]
fn check_sets_the_exit_status() {
    let d = TempDir::new("check");
    // A 997 Hz stereo tone at -16 dBFS measures -16 LUFS.
    let wav = make_wav(&d, "pod.wav", "sine:997:-16", "4");
    let o = vu(&["--report", "--check", "--lufs-target", "podcast", &wav]);
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    assert!(out(&o).contains("Check         PASS"), "{}", out(&o));
    // Against the streaming target it is 2 LU quiet: status 3, and why.
    let o = vu(&["--report", "--check", &wav]);
    assert_eq!(o.status.code(), Some(3));
    assert!(
        out(&o).contains("FAIL") && out(&o).contains("2.0 LU below"),
        "{}",
        out(&o)
    );
    // A wider tolerance passes; a stricter true-peak limit fails again.
    let o = vu(&["--report", "--check", "--tolerance", "2", &wav]);
    assert_eq!(o.status.code(), Some(0), "{}", out(&o));
    let o = vu(&[
        "--report",
        "--check",
        "--tolerance",
        "2",
        "--tp-limit",
        "-18",
        &wav,
    ]);
    assert_eq!(o.status.code(), Some(3));
    assert!(
        out(&o).contains("is over the -18 dBTP limit"),
        "{}",
        out(&o)
    );
    let o = vu(&[
        "--report",
        "--check",
        "--format",
        "csv",
        "--lufs-target",
        "-16",
        &wav,
    ]);
    assert!(out(&o).trim_end().ends_with(",pass"), "{}", out(&o));
    // --check and --format are for reports.
    assert_eq!(
        vu(&["--check", &wav]).status.code(),
        Some(2),
        "a usage error"
    );
}

#[test]
fn log_writes_a_level_curve() {
    let d = TempDir::new("log");
    let csv = d.0.join("levels.csv");
    let o = vu(&[
        "--report",
        "-g",
        "sweep:100:1000:4:-30:-6",
        "-t",
        "4",
        "--log",
        csv.to_str().unwrap(),
        "--log-interval",
        "0.5",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(err(&o).contains("logged 8 rows"), "{}", err(&o));
    let text = std::fs::read_to_string(&csv).unwrap();
    let rows: Vec<Vec<&str>> = text.lines().map(|l| l.split(',').collect()).collect();
    assert_eq!(rows.len(), 9, "{text}");
    assert_eq!(rows[0][0], "time_s");
    let times: Vec<f64> = rows[1..].iter().map(|r| r[0].parse().unwrap()).collect();
    assert_eq!(times[..7], [0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5], "{text}");
    // The last row is what was left, stamped with the last analysis frame.
    assert!(times[7] > 3.95 && times[7] <= 4.0, "{text}");
    // The sweep's level rises 24 dB over its 4 s: 3 dB per row.
    let peaks: Vec<f64> = rows[1..].iter().map(|r| r[1].parse().unwrap()).collect();
    assert!(
        peaks.windows(2).all(|w| (w[1] - w[0] - 3.0).abs() < 0.3),
        "{peaks:?}"
    );
    assert!((peaks[7] + 6.0).abs() < 0.3, "{peaks:?}");
}

#[test]
fn surround_files_use_the_bs1770_weights() {
    // Six channels with the tone on channel 5 (left surround) only.
    let d = TempDir::new("surround");
    let path = d.0.join("six.wav");
    let spec = hound::WavSpec {
        channels: 6,
        sample_rate: 48000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(&path, spec).unwrap();
    for i in 0..48000 * 3 {
        let s = (0.1 * (2.0 * std::f64::consts::PI * 997.0 * i as f64 / 48000.0).sin() * 32767.0)
            as i16;
        for c in 0..6 {
            w.write_sample(if c == 4 { s } else { 0 }).unwrap();
        }
    }
    w.finalize().unwrap();
    let p = path.to_str().unwrap();
    let lufs = |args: &[&str]| {
        let o = vu(&[&["--report", p][..], args].concat());
        assert!(o.status.success(), "{}", err(&o));
        number_after(&out(&o), "Integrated")
    };
    let (auto, off) = (lufs(&[]), lufs(&["--surround", "off"]));
    assert!((off + 23.0).abs() < 0.15, "{off}");
    assert!((auto - off - 1.5).abs() < 0.11, "{auto} vs {off}");
    assert_eq!(lufs(&["--surround", "on"]), auto);
}

#[test]
fn completions_for_each_shell() {
    for shell in ["bash", "zsh", "fish"] {
        let o = vu(&["--completions", shell]);
        assert!(o.status.success(), "{shell}: {}", err(&o));
        let s = out(&o);
        assert!(
            s.contains("vu-meter") && s.contains("lufs-target"),
            "{shell}"
        );
    }
    assert!(!vu(&["--completions", "nosuchshell"]).status.success());
}
