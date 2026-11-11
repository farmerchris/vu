//! Command-line interface.

use crate::dsp::scale::FreqScale;
use crate::dsp::weighting::Weighting;
use crate::dsp::window::WindowKind;
use crate::recorder::RecordFormat;
use crate::ui::colormap::{ColorChoice, ColorMap};
use anyhow::{bail, Result};
use clap::{Parser, ValueEnum};
use std::path::PathBuf;

/// How the live Dock icon shows the levels (window mode, macOS).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum DockStyle {
    /// LED columns, like the meter bars
    #[default]
    Bars,
    /// An analogue meter with a needle
    Needle,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Panel {
    /// Level meters (RMS, VU or LUFS mode; v cycles)
    Meter,
    /// Graphic equalizer bands
    Eq,
    /// Scrolling spectrogram
    Spectrogram,
    /// Level timeline
    Timeline,
    /// Piano keyboard lighting the strongest tone (not shown by default)
    Piano,
    /// Stereo vectorscope with phase correlation, or an oscilloscope for a
    /// mono input (not shown by default)
    Scope,
    /// Every panel, the piano and scope included
    All,
}

/// How `--report` prints its statistics.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum ReportFormat {
    /// Aligned text, one block per input
    #[default]
    Text,
    /// A JSON array with one object per input
    Json,
    /// A header line and one row per input
    Csv,
}

/// Whether loudness is measured with the BS.1770 surround channel weights.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum Surround {
    /// For files with five or six channels
    #[default]
    Auto,
    /// For any input with five or six channels
    On,
    /// Never: every channel counts the same
    Off,
}

/// Named loudness targets for `--lufs-target`, in LUFS.
pub const LUFS_TARGETS: [(&str, f32); 10] = [
    ("streaming", -14.0),
    ("spotify", -14.0),
    ("youtube", -14.0),
    ("apple", -16.0),
    ("podcast", -16.0),
    ("podcast-mono", -19.0),
    ("broadcast", -23.0), // EBU R128
    ("ebu", -23.0),
    ("atsc", -24.0),    // ATSC A/85
    ("netflix", -27.0), // dialogue-gated
];

/// A `--lufs-target`: a number of LUFS or one of the `LUFS_TARGETS` names.
fn parse_lufs_target(s: &str) -> std::result::Result<f32, String> {
    if let Some(&(_, v)) = LUFS_TARGETS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(s.trim()))
    {
        return Ok(v);
    }
    let names: Vec<&str> = LUFS_TARGETS.iter().map(|t| t.0).collect();
    s.trim()
        .parse()
        .map_err(|_| format!("expected a LUFS value or one of: {}", names.join(", ")))
}

/// Panels shown by default.
pub const ALL_PANELS: [Panel; 4] = [Panel::Meter, Panel::Eq, Panel::Spectrogram, Panel::Timeline];
/// Number of real panels (everything but `All`).
pub const PANEL_COUNT: usize = 6;
/// The real panels, indexed by `Panel as usize`; keys 1 to 6 toggle them in
/// this order.
pub const PANELS: [Panel; PANEL_COUNT] = [
    Panel::Meter,
    Panel::Eq,
    Panel::Spectrogram,
    Panel::Timeline,
    Panel::Piano,
    Panel::Scope,
];

/// Which panels `list` turns on, indexed by `Panel as usize`.
pub fn panel_set(list: &[Panel], piano: bool, scope: bool) -> [bool; PANEL_COUNT] {
    let mut on = [false; PANEL_COUNT];
    for &p in list {
        match p {
            Panel::All => on = [true; PANEL_COUNT],
            p => on[p as usize] = true,
        }
    }
    on[Panel::Piano as usize] |= piano;
    on[Panel::Scope as usize] |= scope;
    on
}

#[derive(Parser, Debug, Clone)]
#[command(
    name = "vu-meter",
    version,
    about = "Sound meter: levels, loudness, timeline, spectrogram, equalizer and scope, in the terminal or a window",
    after_help = "KEYS: \u{2190}/\u{2192} scroll (shift = x10), PgUp/PgDn page, Home/End oldest/live, \
space hold, p pause, o monitor, R record, r reset loudness, +/- zoom, 1-6 toggle panels, c colormap, \
s freq scale, v meter mode, m marker, n/N next/previous marker, [ ] floor, ? help, q quit\n\n\
EXAMPLES:\n  vu-meter                        meter the default input\n  \
vu-meter song.flac --monitor    play a file through the meters, and listen to it\n  \
vu-meter song.mp3 --fast        analyze the whole file instantly, then browse it\n  \
vu-meter -r take.wav            meter and record the default input\n  \
vu-meter -g sweep:20:20000:10 -p spectrogram,eq\n  \
vu-meter --report *.wav         print peak/RMS/LUFS statistics without the UI\n  \
vu-meter --report --check --lufs-target podcast mix.wav   exit 3 unless it meets the target\n  \
ffmpeg -i in.mkv -f wav - | vu-meter -f -                 meter another program's output"
)]
pub struct Cli {
    /// Input device: index or case-insensitive name substring (see --list-devices)
    #[arg(short = 'd', long)]
    pub device: Option<String>,

    /// List audio input devices and exit
    #[arg(short = 'l', long)]
    pub list_devices: bool,

    /// Read audio from a file instead of an input device: WAV, AIFF, FLAC,
    /// MP3, Ogg Vorbis, AAC / ALAC (.m4a); `-` reads standard input
    #[arg(short = 'f', long, conflicts_with = "generate")]
    pub file: Option<PathBuf>,

    /// Audio files, the same as --file; --report takes several
    #[arg(value_name = "FILE", conflicts_with_all = ["generate", "file"])]
    pub files: Vec<PathBuf>,

    /// Use a test signal: sine[:FREQ[:DB]], square[:FREQ[:DB]], noise[:DB], pink[:DB],
    /// sweep[:F0[:F1[:SECS[:DB[:END_DB]]]]], silence. Fields are optional (empty ones
    /// too, e.g. sine::-20): 1000 Hz, sweep 20-20000 Hz over 10 s, -6 dBFS (noise -12).
    /// -g alone plays a 1 kHz sine
    #[arg(short = 'g', long, value_name = "SPEC", num_args = 0..=1, default_missing_value = crate::generator::DEFAULT_SPEC)]
    pub generate: Option<String>,

    /// Process file/generator input as fast as possible instead of in real time
    #[arg(long)]
    pub fast: bool,

    /// Loop the input file
    #[arg(long = "loop")]
    pub looping: bool,

    /// Record the input to a WAV file (press R in the UI to stop and start)
    #[arg(short = 'r', long, value_name = "WAV")]
    pub record: Option<PathBuf>,

    /// Sample format for recordings
    #[arg(long, value_enum, default_value = "int16")]
    pub record_format: RecordFormat,

    /// Stop after this many seconds of audio (input stops; recording is finalized)
    #[arg(short = 't', long, value_name = "SECS")]
    pub duration: Option<f64>,

    /// Print a statistics report (peak, RMS, LUFS, dominant frequency) and exit; no UI
    #[arg(long)]
    pub report: bool,

    /// How --report prints
    #[arg(long, value_enum, default_value = "text", requires = "report")]
    pub format: ReportFormat,

    /// With --report: check each input against --lufs-target (within
    /// --tolerance) and --tp-limit, and exit with status 3 if any fails
    #[arg(long, requires = "report")]
    pub check: bool,

    /// Highest true peak --check allows, dBTP
    #[arg(long, default_value_t = -1.0, allow_hyphen_values = true, value_name = "DBTP")]
    pub tp_limit: f32,

    /// How far integrated loudness may be from the target for --check, LU
    #[arg(long, default_value_t = 1.0, value_name = "LU")]
    pub tolerance: f32,

    /// Write levels and loudness over time to a CSV file, one row per --log-interval
    #[arg(long, value_name = "CSV")]
    pub log: Option<PathBuf>,

    /// Seconds between --log rows
    #[arg(long, default_value_t = 1.0, value_name = "SECS")]
    pub log_interval: f64,

    /// Play a file or test signal through the default output while metering (o toggles)
    #[arg(short = 'm', long, conflicts_with_all = ["fast", "report"])]
    pub monitor: bool,

    /// Print a shell completion script and exit
    #[arg(long, value_name = "SHELL", value_enum)]
    pub completions: Option<clap_complete::Shell>,

    /// Open the meters in a window instead of the terminal (also: vu-meter-gui)
    #[arg(long, conflicts_with = "report")]
    pub gui: bool,

    /// Window mode: stay attached to the terminal instead of returning to the prompt
    #[arg(long)]
    pub foreground: bool,

    /// Window mode on macOS: keep the Dock icon still instead of showing live levels
    #[arg(long)]
    pub no_dock_meter: bool,

    /// Window mode on macOS: how the live Dock icon shows the levels
    #[arg(long, value_enum, default_value = "bars")]
    pub dock_style: DockStyle,

    /// Panels to show, comma separated
    #[arg(short = 'p', long, value_enum, value_delimiter = ',', default_values_t = ALL_PANELS.to_vec())]
    pub panels: Vec<Panel>,

    /// Requested sample rate (device or generator)
    #[arg(long)]
    pub sample_rate: Option<u32>,

    /// Requested channel count (device or generator; generator defaults to 2)
    #[arg(long)]
    pub channels: Option<usize>,

    /// Device buffer size in frames
    #[arg(long)]
    pub buffer_size: Option<u32>,

    /// Channel analysed by the spectrogram/equalizer (1-based); default mixes all channels
    #[arg(long, value_name = "N")]
    pub channel: Option<usize>,

    /// FFT size (power of two, 256..=32768)
    #[arg(long, default_value_t = 4096)]
    pub fft_size: usize,

    /// Analysis hop in samples (default: fft-size / 4)
    #[arg(long)]
    pub hop: Option<usize>,

    /// FFT window function
    #[arg(long, value_enum, default_value = "hann")]
    pub window: WindowKind,

    /// Frequency weighting applied to the spectrum and equalizer
    #[arg(short = 'w', long, value_enum, default_value = "z")]
    pub weighting: Weighting,

    /// Number of log-spaced equalizer bands (default: the standard 1/3-octave bands)
    #[arg(short = 'b', long)]
    pub bands: Option<usize>,

    /// Lowest displayed frequency (Hz)
    #[arg(long, default_value_t = 20.0)]
    pub min_freq: f32,

    /// Highest displayed frequency (Hz, clamped to Nyquist)
    #[arg(long, default_value_t = 20000.0)]
    pub max_freq: f32,

    /// Spectrum frequency axis
    #[arg(long, value_enum, default_value = "log")]
    pub freq_scale: FreqScale,

    /// Bottom of the equalizer/timeline dB range
    #[arg(long, default_value_t = -70.0, allow_hyphen_values = true)]
    pub db_min: f32,

    /// Bottom of the spectrogram's colour range (dB); lower than --db-min, as
    /// each FFT bin holds only a small share of broadband energy
    #[arg(long, default_value_t = -90.0, allow_hyphen_values = true)]
    pub spec_floor: f32,

    /// Top of the dB range
    #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
    pub db_max: f32,

    /// Bottom of the level meter scale (dBFS)
    #[arg(long, default_value_t = -70.0, allow_hyphen_values = true)]
    pub meter_floor: f32,

    /// Show RMS levels 3 dB higher (AES17), so a sine reads the same RMS as peak
    #[arg(long)]
    pub rms_aes17: bool,

    /// RMS meter sliding-window length (ms)
    #[arg(long, default_value_t = 300.0)]
    pub rms_time: f32,

    /// Peak meter fall-back rate (dB/s; default 20 dB per 1.7 s, IEC 60268-18)
    #[arg(long, default_value_t = 11.8)]
    pub peak_decay: f32,

    /// Start in VU meter mode (v cycles RMS, VU, LUFS)
    #[arg(long, conflicts_with = "lufs")]
    pub vu: bool,

    /// Loudness that reads 0 LU in LUFS mode: a LUFS value or a preset name.
    ///
    /// Presets: streaming, spotify, youtube (-14); apple, podcast (-16); podcast-mono (-19);
    /// broadcast, ebu (-23, EBU R128); atsc (-24, ATSC A/85); netflix (-27).
    #[arg(long, default_value = "streaming", allow_hyphen_values = true, value_name = "LUFS|NAME", value_parser = parse_lufs_target)]
    pub lufs_target: f32,

    /// Also show the piano keyboard panel (same as adding `piano` to -p)
    #[arg(long)]
    pub piano: bool,

    /// Also show the scope panel (same as adding `scope` to -p)
    #[arg(long)]
    pub scope: bool,

    /// Start in loudness (LUFS) meter mode
    #[arg(long)]
    pub lufs: bool,

    /// Measure loudness with the BS.1770 surround weights (L R C LFE Ls Rs:
    /// surrounds +1.5 dB, LFE left out) when there are five or six channels
    #[arg(long, value_enum, default_value = "auto")]
    pub surround: Surround,

    /// Step through the meter modes every SECS seconds (default 3), for demos
    #[arg(long, value_name = "SECS", num_args = 0..=1, default_missing_value = "3", require_equals = true)]
    pub cycle_views: Option<f64>,

    /// Level (dBFS) of 0 VU on the VU scale (-18: EBU R68; -20: SMPTE)
    #[arg(long, default_value_t = -18.0, allow_hyphen_values = true, value_name = "DBFS")]
    pub vu_ref: f32,

    /// Peak hold time (s), after which the marker falls at 20 dB/s
    #[arg(long, default_value_t = 1.5)]
    pub peak_hold: f32,

    /// Spectrogram colour map
    #[arg(long, value_enum, default_value = "magma")]
    pub colormap: ColorMap,

    /// Terminal colour support
    #[arg(long, value_enum, default_value = "auto")]
    pub color: ColorChoice,

    /// Target UI frame rate
    #[arg(long, default_value_t = 30)]
    pub fps: u32,

    /// Seconds shown across the charts (+/- step through 0.5, 1, 2, 5, 10, 20, 30, 60, 120, 300, 600)
    #[arg(short = 'z', long, default_value_t = crate::app::DEFAULT_SPAN)]
    pub span: f64,

    /// Seconds of history kept for scrolling back (file inputs keep the whole file)
    #[arg(long, default_value_t = 600.0)]
    pub history: f64,

    /// Disable mouse support (click to place the cursor, wheel to scroll)
    #[arg(long)]
    pub no_mouse: bool,
}

impl Cli {
    pub fn validate(&self) -> Result<()> {
        if !self.fft_size.is_power_of_two() || !(256..=32768).contains(&self.fft_size) {
            bail!("--fft-size must be a power of two between 256 and 32768");
        }
        let hop = self.hop();
        if hop == 0 || hop > self.fft_size {
            bail!("--hop must be between 1 and --fft-size");
        }
        if self.bands.is_some_and(|b| b == 0 || b > 256) {
            bail!("--bands must be between 1 and 256");
        }
        if !(self.min_freq > 0.0 && self.min_freq < self.max_freq) {
            bail!("need 0 < --min-freq < --max-freq");
        }
        if self.spec_floor >= self.db_max {
            bail!("--spec-floor must be below --db-max");
        }
        if self.db_min >= self.db_max {
            bail!("--db-min must be below --db-max");
        }
        if self.meter_floor >= 0.0 {
            bail!("--meter-floor must be negative");
        }
        if self
            .cycle_views
            .is_some_and(|s| !(0.5..=3600.0).contains(&s))
        {
            bail!("--cycle-views must be between 0.5 and 3600 seconds");
        }
        if !(-40.0..=0.0).contains(&self.lufs_target) {
            bail!("--lufs-target must be between -40 and 0 LUFS");
        }
        if !(-40.0..=0.0).contains(&self.vu_ref) {
            bail!("--vu-ref must be between -40 and 0 dBFS");
        }
        if !(1..=480).contains(&self.fps) {
            bail!("--fps must be between 1 and 480");
        }
        if !(0.1..=3600.0).contains(&self.span) {
            bail!("--span must be between 0.1 and 3600 seconds");
        }
        if self.history <= 0.0 {
            bail!("--history must be positive");
        }
        if matches!(self.duration, Some(d) if d <= 0.0) {
            bail!("--duration must be positive");
        }
        if self.channel == Some(0) {
            bail!("--channel is 1-based");
        }
        if matches!(self.channels, Some(0)) {
            bail!("--channels must be at least 1");
        }
        if matches!(self.sample_rate, Some(sr) if !(1000..=768_000).contains(&sr)) {
            bail!("--sample-rate must be between 1000 and 768000");
        }
        if self.panels.is_empty() {
            bail!("--panels needs at least one panel");
        }
        if self.files.len() > 1 && !self.report {
            bail!("several files need --report (the meters show one input at a time)");
        }
        if self.files.len() > 1 && (self.record.is_some() || self.log.is_some()) {
            bail!("--record and --log take one input");
        }
        if !(0.0..=20.0).contains(&self.tolerance) {
            bail!("--tolerance must be between 0 and 20 LU");
        }
        if !(-40.0..=6.0).contains(&self.tp_limit) {
            bail!("--tp-limit must be between -40 and 6 dBTP");
        }
        if !(0.05..=3600.0).contains(&self.log_interval) {
            bail!("--log-interval must be between 0.05 and 3600 seconds");
        }
        Ok(())
    }

    /// The files to read: `--file` or the positional ones.
    pub fn inputs(&self) -> Vec<PathBuf> {
        self.file.iter().chain(&self.files).cloned().collect()
    }

    /// This command line for one of its inputs, as `--file`.
    pub fn for_input(&self, path: PathBuf) -> Cli {
        Cli {
            file: Some(path),
            files: Vec::new(),
            ..self.clone()
        }
    }

    /// Whether `file` is standard input (`-f -`).
    pub fn is_stdin(path: &std::path::Path) -> bool {
        path.as_os_str() == "-"
    }

    pub fn hop(&self) -> usize {
        self.hop.unwrap_or(self.fft_size / 4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("vu").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn defaults_validate() {
        let c = parse(&[]);
        c.validate().unwrap();
        assert_eq!(c.panels, ALL_PANELS.to_vec());
        assert_eq!(c.hop(), 1024);
        assert_eq!(c.fft_size, 4096);
    }

    #[test]
    fn lufs_target_names_and_numbers() {
        assert_eq!(parse(&[]).lufs_target, -14.0);
        for (args, want) in [
            ("netflix", -27.0),
            ("EBU", -23.0),
            ("broadcast", -23.0),
            ("atsc", -24.0),
            ("-18.5", -18.5),
            ("podcast-mono", -19.0),
        ] {
            assert_eq!(parse(&["--lufs-target", args]).lufs_target, want, "{args}");
        }
        for (name, v) in LUFS_TARGETS {
            assert_eq!(parse_lufs_target(name), Ok(v));
        }
        let err = Cli::try_parse_from(["vu", "--lufs-target", "loud"])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("streaming") && err.contains("netflix"),
            "{err}"
        );
    }

    #[test]
    fn panel_list_and_options() {
        let c = parse(&[
            "-p",
            "spectrogram,eq",
            "--db-min",
            "-120",
            "--fft-size",
            "8192",
            "--hop",
            "256",
        ]);
        assert_eq!(c.panels, vec![Panel::Spectrogram, Panel::Eq]);
        assert_eq!(
            panel_set(&c.panels, false, false),
            [false, true, true, false, false, false]
        );
        assert_eq!(
            panel_set(&ALL_PANELS, false, false),
            [true, true, true, true, false, false],
            "no piano or scope by default"
        );
        assert_eq!(panel_set(&ALL_PANELS, true, true), [true; PANEL_COUNT]);
        assert_eq!(panel_set(&[Panel::All], false, false), [true; PANEL_COUNT]);
        for (i, p) in PANELS.into_iter().enumerate() {
            assert_eq!(p as usize, i);
        }
        assert_eq!(c.db_min, -120.0);
        assert_eq!(c.hop(), 256);
        c.validate().unwrap();
    }

    #[test]
    fn rejects_bad_values() {
        for args in [
            &["--fft-size", "1000"][..],
            &["--fft-size", "128"],
            &["--hop", "0"],
            &["--hop", "5000"],
            &["--bands", "0"],
            &["--min-freq", "500", "--max-freq", "100"],
            &["--db-min", "0", "--db-max", "-10"],
            &["--fps", "0"],
            &["--channel", "0"],
            &["--duration", "0"],
            &["--meter-floor", "3"],
            &["--span", "0"],
            &["--span", "5000"],
        ] {
            assert!(parse(args).validate().is_err(), "{args:?}");
        }
        assert!(Cli::try_parse_from(["vu", "-f", "a.wav", "-g", "noise"]).is_err());
        assert!(Cli::try_parse_from(["vu", "a.wav", "-g", "noise"]).is_err());
        assert!(Cli::try_parse_from(["vu", "a.wav", "-f", "b.wav"]).is_err());
        assert!(Cli::try_parse_from(["vu", "a.wav", "--check"]).is_err());
        assert!(Cli::try_parse_from(["vu", "a.wav", "--format", "json"]).is_err());
        assert!(Cli::try_parse_from(["vu", "a.wav", "--fast", "--monitor"]).is_err());
        // Several files are for reports only.
        assert!(parse(&["a.wav", "b.wav"]).validate().is_err());
        assert!(parse(&["a.wav", "b.wav", "--report"]).validate().is_ok());
        assert!(parse(&["--tolerance", "30"]).validate().is_err());
        assert!(parse(&["--log-interval", "0"]).validate().is_err());
        assert!(Cli::try_parse_from(["vu", "-p", "bogus"]).is_err());
    }

    #[test]
    fn files_by_position_or_option() {
        let c = parse(&["song.flac"]);
        assert_eq!(c.inputs(), [PathBuf::from("song.flac")]);
        assert_eq!(parse(&["-f", "a.wav"]).inputs(), [PathBuf::from("a.wav")]);
        assert!(parse(&[]).inputs().is_empty());
        let c = parse(&["--report", "a.wav", "b.mp3", "--format", "csv", "--check"]);
        assert_eq!(c.inputs().len(), 2);
        assert_eq!(c.format, ReportFormat::Csv);
        let one = c.for_input("b.mp3".into());
        assert_eq!(one.inputs(), [PathBuf::from("b.mp3")]);
        assert!(Cli::is_stdin(&parse(&["-f", "-"]).inputs()[0]));
        assert!(!Cli::is_stdin(std::path::Path::new("-.wav")));
        assert_eq!(parse(&["--tp-limit", "-2"]).tp_limit, -2.0);
    }
}
