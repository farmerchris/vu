//! Setup, audio intake and shutdown shared by the terminal and window front
//! ends: both open a source, analyze it into an [`App`] and report on exit
//! the same way, and differ only in drawing and input.

use crate::analyzer::{Analyzer, AnalyzerConfig};
use crate::app::App;
use crate::cli::Cli;
use crate::report;
use crate::source::{RateChanged, SilenceWatch, Source, SourceInfo, SourceKind};
use anyhow::{bail, Result};
use std::sync::atomic::Ordering;
use std::sync::mpsc::TryRecvError;
use std::time::Instant;

/// Seconds of exact-zero input before a live device is considered stalled.
const SILENCE_SECS: f64 = 1.5;
/// Reopen attempts per stall before reporting the input as silent.
const MAX_REOPENS: u32 = 3;
/// How often a waiting input is checked to see if it can be shared now.
const CLAIM_EVERY: std::time::Duration = std::time::Duration::from_secs(1);
/// How often a live input checks the user's Mic Mode (macOS).
const MIC_MODE_EVERY: std::time::Duration = std::time::Duration::from_secs(1);
/// A live input may run this much faster than real time (bursts, clock
/// drift) before the excess is dropped...
const FAST_RATIO: f64 = 1.25;
/// ...plus this many seconds of slack.
const FAST_SLACK_SECS: f64 = 1.0;

/// Analyzer settings from the command line for a source.
pub fn analyzer_config(cli: &Cli, info: &SourceInfo) -> Result<AnalyzerConfig> {
    if let Some(c) = cli.channel {
        if c > info.channels {
            bail!("--channel {c} but the input has {} channels", info.channels);
        }
    }
    let cfg = AnalyzerConfig {
        sample_rate: info.sample_rate,
        channels: info.channels,
        fft_size: cli.fft_size,
        hop: cli.hop(),
        window: cli.window,
        weighting: cli.weighting,
        bands: cli.bands,
        min_freq: cli.min_freq,
        max_freq: cli.max_freq,
        rms_tau: cli.rms_time / 1000.0,
        peak_decay_db_s: cli.peak_decay,
        peak_hold_s: cli.peak_hold,
        band_floor_db: cli.db_min,
        spectrum_channel: cli.channel.map(|c| c - 1),
    };
    if cfg.min_freq >= cfg.top_freq() {
        bail!(
            "--min-freq must be below Nyquist ({} Hz)",
            info.sample_rate / 2
        );
    }
    Ok(cfg)
}

/// Moves audio from the source through the analyzer into the app, watching a
/// live input for stalls and honouring `--duration` and recording.
pub struct Pump {
    watch: SilenceWatch,
    attempts: u32,
    limit: Option<u64>,
    live: bool,
    /// Live input: when the stream (re)started and frames received since.
    clock: Option<(Instant, u64)>,
    /// The input is shown as running fast.
    fast: bool,
    /// When a waiting input was last checked.
    last_claim: Option<Instant>,
    /// When the Mic Mode was last checked.
    mic_mode_checked: Instant,
    sample_rate: f64,
    /// The options the input was opened with, to open it again.
    cli: Cli,
}

impl Pump {
    pub fn new(cli: &Cli, info: &SourceInfo) -> Self {
        let ch = info.channels;
        Pump {
            watch: SilenceWatch::new(
                (SILENCE_SECS * (info.sample_rate as usize * ch) as f64) as u64,
            ),
            attempts: 0,
            limit: cli
                .duration
                .map(|d| (d * info.sample_rate as f64).round() as u64),
            live: info.kind == SourceKind::Device,
            clock: None,
            fast: false,
            last_claim: None,
            mic_mode_checked: Instant::now(),
            sample_rate: info.sample_rate as f64,
            cli: cli.clone(),
        }
    }

    /// Ingest queued audio until the queue is empty or `deadline` passes, so
    /// a fast file scan never starves the UI. Returns whether anything changed.
    pub fn run(
        &mut self,
        app: &mut App,
        analyzer: &mut Analyzer,
        source: &mut Source,
        deadline: Instant,
    ) -> bool {
        analyzer.set_band_floor(app.display.db_min);
        if std::mem::take(&mut app.loudness_reset) {
            analyzer.loudness.reset();
        }
        if self.live && self.mic_mode_checked.elapsed() >= MIC_MODE_EVERY {
            self.mic_mode_checked = Instant::now();
            if source.mic_mode_changed() {
                self.follow_mic_mode(app, source);
            } else if source.name_mic_mode() {
                app.info.name = source.info.name.clone();
                app.flash(format!("input: {}", source.info.name));
            }
        }
        if source.waiting.is_some() && !self.wait_for_input(app, source) {
            return false;
        }
        let ch = source.info.channels;
        let mut dirty = false;
        loop {
            match source.try_recv() {
                Ok(mut chunk) => {
                    if self.live && self.too_fast(app, source, chunk.len() / ch.max(1)) {
                        continue;
                    }
                    if app.ingest_done || (self.live && app.paused()) {
                        continue;
                    }
                    chunk.truncate(report::truncate_len(
                        chunk.len(),
                        ch,
                        analyzer.samples(),
                        self.limit,
                    ));
                    if let Some(r) = app.recorder.as_mut() {
                        if let Err(e) = r.write(&chunk) {
                            app.recorder = None;
                            app.flash(format!("record error: {e:#}"));
                        }
                    }
                    analyzer.push(&chunk, |f| app.push_frame(f));
                    dirty = true;
                    if source.can_reopen() {
                        self.watch_input(app, source, &chunk);
                    }
                    if self.limit.is_some_and(|l| analyzer.samples() >= l) {
                        app.ingest_done = true;
                        source.control.stop.store(true, Ordering::Relaxed);
                        if let Err(e) = app.stop_recording() {
                            app.flash(format!("record error: {e:#}"));
                        }
                    }
                    if Instant::now() >= deadline {
                        break;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !app.source_done {
                        app.source_done = true;
                        dirty = true;
                    }
                    break;
                }
            }
        }
        dirty
    }

    /// Open the live input again through voice processing or without it, as
    /// the user's Mic Mode now asks, carrying on the same analysis (and any
    /// recording) when the new capture can be shown in the same format.
    /// Otherwise the app is asked to start the input afresh.
    fn follow_mic_mode(&mut self, app: &mut App, source: &mut Source) {
        let mut new = match crate::source::open(&self.cli, false, false) {
            Ok(s) => s,
            Err(e) => {
                app.input_warning = Some(format!("Mic Mode changed; could not reopen: {e:#}"));
                return;
            }
        };
        if !new.keep_format(&source.info) {
            app.input_warning = Some("Mic Mode changed".into());
            app.restart_input = true;
            return;
        }
        new.control.paused.store(app.paused(), Ordering::Relaxed);
        app.control = new.control.clone();
        app.info.name = new.info.name.clone();
        app.input_warning = None;
        app.flash(format!("input: {}", new.info.name));
        // The old capture stops when dropped here.
        *source = new;
        self.clock = None;
        self.watch.reset();
        self.attempts = 0;
    }

    /// Try now and then to claim an input that is waiting to be shared,
    /// showing why it waits; returns whether it is running.
    fn wait_for_input(&mut self, app: &mut App, source: &mut Source) -> bool {
        if self.last_claim.is_some_and(|t| t.elapsed() < CLAIM_EVERY) {
            return false;
        }
        self.last_claim = Some(Instant::now());
        match source.claim() {
            Ok(true) => {
                app.input_warning = None;
                app.flash(format!("{}: started", source.info.name));
                self.clock = None;
                self.watch.reset();
                true
            }
            Ok(false) => {
                app.input_warning = Some(format!(
                    "waiting for {}: {}",
                    source.info.name,
                    source.waiting.as_deref().unwrap_or("")
                ));
                false
            }
            Err(e) => {
                app.restart_input |= e.is::<RateChanged>();
                app.input_warning = Some(format!("input lost: {e:#}"));
                false
            }
        }
    }

    /// Whether a live input has delivered more audio than real time allows
    /// (counting `frames` just received): a device whose clock or rate
    /// changed under it, which would otherwise race the timeline ahead. The
    /// excess is dropped; if the device's rate did change, the app is asked to
    /// restart the input.
    fn too_fast(&mut self, app: &mut App, source: &Source, frames: usize) -> bool {
        let (start, received) = self.clock.get_or_insert((Instant::now(), 0));
        let allowed =
            (start.elapsed().as_secs_f64() * FAST_RATIO + FAST_SLACK_SECS) * self.sample_rate;
        if (*received + frames as u64) as f64 > allowed {
            if !self.fast {
                self.fast = true;
                let from = self.sample_rate as u32;
                app.input_warning = Some(match source.device_rate().filter(|&r| r != from) {
                    Some(to) => {
                        app.restart_input = true;
                        RateChanged { from, to }.to_string()
                    }
                    None => "input running faster than real time; dropping the excess".into(),
                });
            }
            return true;
        }
        *received += frames as u64;
        // Clear the warning once comfortably back within real time.
        if self.fast && (*received as f64) < allowed - FAST_SLACK_SECS / 2.0 * self.sample_rate {
            self.fast = false;
            app.input_warning = None;
        }
        false
    }

    /// Reopen a live input that has gone digitally silent, a few times.
    fn watch_input(&mut self, app: &mut App, source: &mut Source, chunk: &[f32]) {
        let stalled = self.watch.feed(chunk);
        if app.input_warning.is_some() && chunk.iter().any(|&x| x != 0.0) {
            app.input_warning = None;
            app.flash("input restored");
            self.attempts = 0;
        }
        if !stalled {
            return;
        }
        if source.bluetooth {
            // Reopening would pull it back from the phone or computer that
            // took it; wait for it instead.
            app.input_warning = Some(
                "input is silent: the Bluetooth device may be in use by another device \
                 (choose the input again to take it back)"
                    .into(),
            );
            return;
        }
        if self.attempts < MAX_REOPENS {
            self.attempts += 1;
            app.reopens += 1;
            app.input_warning = Some(format!(
                "input silent (all zeros), reopening ({}/{MAX_REOPENS})",
                self.attempts
            ));
            match source.reopen() {
                Ok(()) if source.waiting.is_some() => self.last_claim = Some(Instant::now()),
                Ok(()) => self.clock = None,
                Err(e) if e.is::<RateChanged>() => {
                    app.restart_input = true;
                    app.input_warning = Some(e.to_string());
                }
                Err(e) => app.input_warning = Some(format!("input lost: {e:#}")),
            }
            self.watch.reset();
        } else {
            app.input_warning = Some(
                "input is sending pure silence: mic muted, or taken by another app/device?".into(),
            );
        }
    }
}

/// Finish recording and report what happened on stderr, after the UI is gone.
pub fn finish(app: &mut App, source: &Source) -> Result<()> {
    let finished = app.stop_recording();
    for p in &app.saved_recordings {
        eprintln!("vu-meter: recorded {}", p.display());
    }
    if let Some(e) = source.control.take_error() {
        eprintln!("vu-meter: {e}");
    }
    if app.reopens > 0 {
        eprintln!(
            "vu-meter: the input went digitally silent; reopened it {} time(s)",
            app.reopens
        );
    }
    if let Some(n) = source.dropped_chunks() {
        eprintln!("vu-meter: {n} audio buffers were dropped because the UI fell behind");
    }
    finished
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Control;
    use crate::ui::colormap::ColorMode;
    use clap::Parser;
    use std::sync::Arc;
    use std::time::Duration;

    /// A live input delivering audio far faster than real time (here an
    /// unpaced generator posing as one) cannot run the timeline ahead.
    #[test]
    fn a_fast_live_input_is_held_to_real_time() {
        let cli = Cli::try_parse_from(["vu-meter", "-g", "sine"]).unwrap();
        let mut source = crate::source::open(&cli, true, false).unwrap();
        let mut analyzer = Analyzer::new(analyzer_config(&cli, &source.info).unwrap());
        let mut app = App::new(
            &cli,
            source.info.clone(),
            &analyzer,
            Arc::new(Control::default()),
            ColorMode::TrueColor,
        );
        let mut pump = Pump::new(&cli, &source.info);
        pump.live = true;
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(300) {
            pump.run(
                &mut app,
                &mut analyzer,
                &mut source,
                Instant::now() + Duration::from_millis(5),
            );
        }
        let secs = analyzer.samples() as f64 / source.info.sample_rate as f64;
        let limit = start.elapsed().as_secs_f64() * FAST_RATIO + FAST_SLACK_SECS;
        assert!(
            secs <= limit + 0.2,
            "{secs} s of audio in {:?}",
            start.elapsed()
        );
        assert!(app
            .input_warning
            .as_deref()
            .unwrap_or("")
            .contains("faster than real time"));
        assert!(!app.restart_input, "the rate did not change");
    }
}
