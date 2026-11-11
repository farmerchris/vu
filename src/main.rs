//! vu-meter — terminal sound meter (`vu-meter --gui` for the window).

use anyhow::Result;
use clap::{CommandFactory, FromArgMatches};
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, EndSynchronizedUpdate,
    EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::CrosstermBackend;
use ratatui::Terminal;
use std::io::{stdout, Stdout};
use std::time::{Duration, Instant};
use vu::analyzer::Analyzer;
use vu::app::{Action, App};
use vu::cli::Cli;
use vu::pump::{self, Pump};
use vu::source::Source;
use vu::{report, source, ui};

#[cfg(feature = "gui")]
fn gui(cli: Cli, matches: &clap::ArgMatches) -> Result<()> {
    if !cli.foreground {
        return vu::gui::detach();
    }
    vu::gui::run(cli, vu::gui::explicit_args(matches))
}

#[cfg(not(feature = "gui"))]
fn gui(_cli: Cli, _matches: &clap::ArgMatches) -> Result<()> {
    anyhow::bail!("this vu-meter was built without the window (cargo feature \"gui\")")
}

/// Exit status when `--check` fails (1 is an error, 2 a command line that
/// could not be understood).
const CHECK_FAILED: i32 = 3;

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("vu-meter: {e:#}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<i32> {
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    if let Some(shell) = cli.completions {
        clap_complete::generate(shell, &mut Cli::command(), "vu-meter", &mut stdout());
        return Ok(0);
    }
    if cli.list_devices {
        return source::list_devices().map(|()| 0);
    }
    cli.validate()?;
    if cli.gui {
        return gui(cli, &matches).map(|()| 0);
    }
    if cli.report {
        return Ok(match report::run(&cli)? {
            report::Outcome::Ok => 0,
            report::Outcome::CheckFailed => CHECK_FAILED,
            report::Outcome::InputFailed => 1,
        });
    }
    let source = source::open(&cli, cli.fast, cli.looping)?;
    let acfg = pump::analyzer_config(&cli, &source.info)?;
    let analyzer = Analyzer::new(acfg);
    run_tui(&cli, source, analyzer).map(|()| 0)
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
    let mode = cli
        .color
        .resolve(std::env::var("COLORTERM").ok().as_deref());
    let mut app = App::new(
        cli,
        source.info.clone(),
        &analyzer,
        source.control.clone(),
        mode,
    );
    if cli.record.is_some() {
        app.start_recording()?;
    }
    if let Some(path) = &cli.log {
        app.start_log(path, cli.log_interval)?;
    }
    let mouse = !cli.no_mouse;
    let res = setup_terminal(mouse).and_then(|mut term| {
        let r = event_loop(&mut term, &mut app, &mut analyzer, &mut source, cli);
        restore_terminal(mouse);
        r
    });
    let finished = pump::finish(&mut app, &source);
    res.and(finished)
}

/// Open the live input again (its rate or the Mic Mode changed), keeping
/// the view. A recording in progress ends.
fn restart(
    app: &mut App,
    analyzer: &mut Analyzer,
    source: &mut Source,
    pump: &mut Pump,
    cli: &Cli,
) {
    let why = app.input_warning.take().unwrap_or_default();
    let opened = source::open(cli, false, false)
        .and_then(|s| Ok((Analyzer::new(pump::analyzer_config(cli, &s.info)?), s)));
    let (new_analyzer, new_source) = match opened {
        Ok(o) => o,
        Err(e) => {
            app.input_warning = Some(format!("{why}; could not restart the input: {e:#}"));
            return;
        }
    };
    let mut new = App::new(
        cli,
        new_source.info.clone(),
        &new_analyzer,
        new_source.control.clone(),
        app.display.color_mode,
    );
    new.keep_view(app);
    if let Err(e) = app.stop_recording() {
        new.flash(format!("record error: {e:#}"));
    } else {
        new.flash(format!("{why}; input restarted"));
    }
    new.saved_recordings = std::mem::take(&mut app.saved_recordings);
    // The log carries on; markers belong to the history that is going.
    new.log = app.log.take();
    new.reopens = app.reopens;
    // The old source stops when dropped here.
    *pump = Pump::new(cli, &new_source.info);
    (*app, *analyzer, *source) = (new, new_analyzer, new_source);
}

fn event_loop(
    term: &mut Term,
    app: &mut App,
    analyzer: &mut Analyzer,
    source: &mut Source,
    cli: &Cli,
) -> Result<()> {
    let frame_dt = Duration::from_secs_f64(1.0 / cli.fps as f64);
    let mut pump = Pump::new(cli, &source.info);
    let mut next_frame = Instant::now();
    let mut dirty = true;
    loop {
        let budget = Instant::now() + frame_dt.mul_f32(0.7);
        dirty |= pump.run(app, analyzer, source, budget);
        if std::mem::take(&mut app.restart_input) {
            restart(app, analyzer, source, &mut pump, cli);
            dirty = true;
        }

        let now = Instant::now();
        if app.cycle_tick(now) {
            dirty = true;
        }
        if now >= next_frame {
            if dirty || app.message.is_some() {
                let t = Instant::now();
                // The terminal shows the frame only once it is complete
                // (ignored by terminals without synchronized output).
                execute!(term.backend_mut(), BeginSynchronizedUpdate)?;
                term.draw(|f| ui::draw(f, app))?;
                execute!(term.backend_mut(), EndSynchronizedUpdate)?;
                app.fps.tick(t, t.elapsed());
                dirty = false;
                if app
                    .message
                    .as_ref()
                    .is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(5))
                {
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
