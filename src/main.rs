//! vu — terminal sound meter.

use anyhow::{bail, Result};
use clap::Parser;
use vu::analyzer::{Analyzer, AnalyzerConfig};
use vu::app::{Action, App};
use vu::cli::Cli;
use vu::{recorder, report, source, ui};
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::prelude::CrosstermBackend;
use ratatui::Terminal;
use vu::source::{SilenceWatch, Source, SourceKind};
use std::io::{stdout, Stdout};
use std::sync::atomic::Ordering;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

fn main() {
    if let Err(e) = run() {
        eprintln!("vu: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    if cli.list_devices {
        return source::list_devices();
    }
    cli.validate()?;
    let fast = cli.fast || cli.report;
    let looping = cli.looping && !cli.report;
    let source = source::open(&cli, fast, looping)?;
    let info = &source.info;
    if let Some(c) = cli.channel {
        if c > info.channels {
            bail!("--channel {c} but the input has {} channels", info.channels);
        }
    }
    let acfg = AnalyzerConfig {
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
        spectrum_channel: cli.channel.map(|c| c - 1),
    };
    if acfg.min_freq >= acfg.top_freq() {
        bail!("--min-freq must be below Nyquist ({} Hz)", info.sample_rate / 2);
    }
    let mut analyzer = Analyzer::new(acfg);
    if cli.report {
        let rec = match &cli.record {
            Some(p) => Some(recorder::Recorder::create(p, info.channels, info.sample_rate, cli.record_format)?),
            None => None,
        };
        return report::run(source, &mut analyzer, cli.duration, rec);
    }
    run_tui(&cli, source, analyzer)
}

type Term = Terminal<CrosstermBackend<Stdout>>;

fn setup_terminal(mouse: bool) -> Result<Term> {
    let res = (|| -> Result<Term> {
        enable_raw_mode()?;
        let mut out = stdout();
        execute!(out, EnterAlternateScreen)?;
        if mouse {
            execute!(out, EnableMouseCapture)?;
        }
        let mut t = Terminal::new(CrosstermBackend::new(out))?;
        t.hide_cursor()?;
        t.clear()?;
        Ok(t)
    })();
    if res.is_err() {
        restore_terminal(mouse);
        return res;
    }
    // Only a panic on the UI thread tears down the terminal; worker panics
    // surface through the source's disconnected channel instead.
    let main_id = std::thread::current().id();
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().id() == main_id {
            restore_terminal(true);
        }
        hook(info);
    }));
    res
}

fn restore_terminal(mouse: bool) {
    let _ = disable_raw_mode();
    let mut out = stdout();
    if mouse {
        let _ = execute!(out, DisableMouseCapture);
    }
    let _ = execute!(out, LeaveAlternateScreen, ratatui::crossterm::cursor::Show);
}

fn run_tui(cli: &Cli, mut source: Source, mut analyzer: Analyzer) -> Result<()> {
    let mode = cli.color.resolve(std::env::var("COLORTERM").ok().as_deref());
    let mut app = App::new(cli, source.info.clone(), &analyzer, source.control.clone(), mode);
    if cli.record.is_some() {
        app.start_recording()?;
    }
    let mouse = !cli.no_mouse;
    let res = setup_terminal(mouse).and_then(|mut term| {
        let r = event_loop(&mut term, &mut app, &mut analyzer, &mut source, cli);
        restore_terminal(mouse);
        r
    });
    let finished = app.stop_recording();
    for p in &app.saved_recordings {
        eprintln!("vu: recorded {}", p.display());
    }
    if let Some(e) = source.control.take_error() {
        eprintln!("vu: {e}");
    }
    if app.reopens > 0 {
        eprintln!("vu: the input went digitally silent; reopened it {} time(s)", app.reopens);
    }
    if let Some(n) = source.dropped_chunks() {
        eprintln!("vu: {n} audio buffers were dropped because the UI fell behind");
    }
    res.and(finished)
}

/// Seconds of exact-zero input before a live device is considered stalled.
const SILENCE_SECS: f64 = 1.5;
/// Reopen attempts per stall before reporting the input as silent.
const MAX_REOPENS: u32 = 3;

fn event_loop(term: &mut Term, app: &mut App, analyzer: &mut Analyzer, source: &mut Source, cli: &Cli) -> Result<()> {
    let frame_dt = Duration::from_secs_f64(1.0 / cli.fps as f64);
    let ch = source.info.channels;
    let mut watch = SilenceWatch::new((SILENCE_SECS * (source.info.sample_rate as usize * ch) as f64) as u64);
    let mut attempts = 0u32;
    let limit = cli.duration.map(|d| (d * source.info.sample_rate as f64).round() as u64);
    let live = source.info.kind == SourceKind::Device;
    let mut next_frame = Instant::now();
    let mut dirty = true;
    loop {
        // Ingest, bounded so a fast file scan never starves the UI.
        let budget = Instant::now() + frame_dt.mul_f32(0.7);
        loop {
            match source.rx.try_recv() {
                Ok(mut chunk) => {
                    if app.ingest_done || (live && app.paused()) {
                        continue;
                    }
                    chunk.truncate(report::truncate_len(chunk.len(), ch, analyzer.samples(), limit));
                    if let Some(r) = app.recorder.as_mut() {
                        if let Err(e) = r.write(&chunk) {
                            app.recorder = None;
                            app.flash(format!("record error: {e:#}"));
                        }
                    }
                    analyzer.push(&chunk, |f| app.push_frame(f));
                    dirty = true;
                    if source.can_reopen() {
                        let stalled = watch.feed(&chunk);
                        if app.input_warning.is_some() && chunk.iter().any(|&x| x != 0.0) {
                            app.input_warning = None;
                            app.flash("input restored");
                            attempts = 0;
                        }
                        if stalled {
                            if attempts < MAX_REOPENS {
                                attempts += 1;
                                app.reopens += 1;
                                app.input_warning = Some(format!("input silent (all zeros), reopening ({attempts}/{MAX_REOPENS})"));
                                if let Err(e) = source.reopen() {
                                    app.input_warning = Some(format!("input lost: {e:#}"));
                                }
                                watch.reset();
                            } else {
                                app.input_warning =
                                    Some("input is sending pure silence: mic muted, or taken by another app/device?".into());
                            }
                        }
                    }
                    if limit.is_some_and(|l| analyzer.samples() >= l) {
                        app.ingest_done = true;
                        source.control.stop.store(true, Ordering::Relaxed);
                        if let Err(e) = app.stop_recording() {
                            app.flash(format!("record error: {e:#}"));
                        }
                    }
                    if Instant::now() >= budget {
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

        let now = Instant::now();
        if now >= next_frame {
            if dirty || app.message.is_some() {
                let t = Instant::now();
                term.draw(|f| ui::draw(f, app))?;
                app.fps.tick(t, t.elapsed());
                dirty = false;
                if app.message.as_ref().is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(5)) {
                    app.message = None;
                    dirty = true;
                }
            }
            next_frame += frame_dt;
            if next_frame < now {
                next_frame = now + frame_dt;
            }
        }

        let timeout = next_frame.saturating_duration_since(Instant::now());
        if event::poll(timeout)? {
            loop {
                match app.handle_event(event::read()?) {
                    Action::Quit => return Ok(()),
                    Action::Redraw => dirty = true,
                    Action::None => {}
                }
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
    }
}
