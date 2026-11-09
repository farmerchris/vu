//! Setup, audio intake and shutdown shared by the terminal and window front
//! ends: both open a source, analyze it into an [`App`] and report on exit
//! the same way, and differ only in drawing and input.

use crate::analyzer::{Analyzer, AnalyzerConfig};
use crate::app::App;
use crate::cli::Cli;
use crate::report;
use crate::source::{SilenceWatch, Source, SourceInfo, SourceKind};
use anyhow::{bail, Result};
use std::sync::atomic::Ordering;
use std::sync::mpsc::TryRecvError;
use std::time::Instant;

/// Seconds of exact-zero input before a live device is considered stalled.
const SILENCE_SECS: f64 = 1.5;
/// Reopen attempts per stall before reporting the input as silent.
const MAX_REOPENS: u32 = 3;

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
        let ch = source.info.channels;
        let mut dirty = false;
        loop {
            match source.rx.try_recv() {
                Ok(mut chunk) => {
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
        if self.attempts < MAX_REOPENS {
            self.attempts += 1;
            app.reopens += 1;
            app.input_warning = Some(format!(
                "input silent (all zeros), reopening ({}/{MAX_REOPENS})",
                self.attempts
            ));
            if let Err(e) = source.reopen() {
                app.input_warning = Some(format!("input lost: {e:#}"));
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
        eprintln!("vu: recorded {}", p.display());
    }
    if let Some(e) = source.control.take_error() {
        eprintln!("vu: {e}");
    }
    if app.reopens > 0 {
        eprintln!(
            "vu: the input went digitally silent; reopened it {} time(s)",
            app.reopens
        );
    }
    if let Some(n) = source.dropped_chunks() {
        eprintln!("vu: {n} audio buffers were dropped because the UI fell behind");
    }
    finished
}
