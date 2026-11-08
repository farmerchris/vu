//! Command-line interface.

use crate::dsp::scale::FreqScale;
use crate::dsp::weighting::Weighting;
use crate::dsp::window::WindowKind;
use crate::recorder::RecordFormat;
use crate::ui::colormap::{ColorChoice, ColorMap};
use anyhow::{bail, Result};
use clap::{Parser, ValueEnum};
use std::path::PathBuf;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Panel {
    /// Level meters (peak, RMS, loudness)
    Meter,
    /// Graphic equalizer bands
    Eq,
    /// Scrolling spectrogram
    Spectrogram,
    /// Level timeline
    Timeline,
}

pub const ALL_PANELS: [Panel; 4] = [Panel::Meter, Panel::Eq, Panel::Spectrogram, Panel::Timeline];

#[derive(Parser, Debug, Clone)]
#[command(
    name = "vu",
    version,
    about = "Terminal sound meter: levels, loudness, timeline, spectrogram and graphic equalizer",
    after_help = "KEYS: \u{2190}/\u{2192} scroll (shift = x10), PgUp/PgDn page, Home/End oldest/live, \
space hold, p pause, r record, +/- zoom, 1-4 toggle panels, c colormap, s freq scale, v VU/RMS, \
[ ] floor, ? help, q quit\n\n\
EXAMPLES:\n  vu                              meter the default input\n  \
vu -f song.wav                  play a file through the meters in real time\n  \
vu -f song.wav --fast           analyze the whole file instantly, then browse it\n  \
vu -r take.wav                  meter and record the default input\n  \
vu -g sweep:20:20000:10 -p spectrogram,eq\n  \
vu -f song.wav --report         print peak/RMS/LUFS statistics without the UI"
)]
pub struct Cli {
    /// Input device: index or case-insensitive name substring (see --list-devices)
    #[arg(short = 'd', long)]
    pub device: Option<String>,

    /// List audio input devices and exit
    #[arg(short = 'l', long)]
    pub list_devices: bool,

    /// Read audio from a WAV file instead of an input device
    #[arg(short = 'f', long, conflicts_with = "generate")]
    pub file: Option<PathBuf>,

    /// Use a test signal: sine:FREQ[:DB], square:FREQ[:DB], noise[:DB], pink[:DB],
    /// sweep:F0:F1:SECS[:DB[:END_DB]], silence
    #[arg(short = 'g', long, value_name = "SPEC")]
    pub generate: Option<String>,

    /// Process file/generator input as fast as possible instead of in real time
    #[arg(long)]
    pub fast: bool,

    /// Loop the input file
    #[arg(long = "loop")]
    pub looping: bool,

    /// Record the input to a WAV file (press r in the UI to toggle)
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

    /// Bottom of the spectrogram/equalizer/timeline dB range
    #[arg(long, default_value_t = -70.0, allow_hyphen_values = true)]
    pub db_min: f32,

    /// Top of the dB range
    #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
    pub db_max: f32,

    /// Bottom of the level meter scale (dBFS)
    #[arg(long, default_value_t = -70.0, allow_hyphen_values = true)]
    pub meter_floor: f32,

    /// RMS meter sliding-window length (ms)
    #[arg(long, default_value_t = 300.0)]
    pub rms_time: f32,

    /// Peak meter fall-back rate (dB/s; default 20 dB per 1.7 s, IEC 60268-18)
    #[arg(long, default_value_t = 11.8)]
    pub peak_decay: f32,

    /// Start the level meters as VU needles (v toggles VU and RMS/peak)
    #[arg(long)]
    pub vu: bool,

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
        if self.db_min >= self.db_max {
            bail!("--db-min must be below --db-max");
        }
        if self.meter_floor >= 0.0 {
            bail!("--meter-floor must be negative");
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
        Ok(())
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
    fn panel_list_and_options() {
        let c = parse(&["-p", "spectrogram,eq", "--db-min", "-120", "--fft-size", "8192", "--hop", "256"]);
        assert_eq!(c.panels, vec![Panel::Spectrogram, Panel::Eq]);
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
        assert!(Cli::try_parse_from(["vu", "-p", "bogus"]).is_err());
    }
}
