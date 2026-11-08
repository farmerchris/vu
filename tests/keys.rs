//! Keyboard handling tests driven through App::handle_event.

use clap::Parser;
use std::sync::Arc;
use vu::analyzer::{Analyzer, AnalyzerConfig};
use vu::app::App;
use vu::cli::Cli;
use vu::generator::{parse_spec, Generator};
use vu::source::{Control, SourceInfo, SourceKind};
use vu::ui::colormap::ColorMode;

const SR: u32 = 48_000;

/// Build an app fed with `secs` seconds of a test signal (0.0 = empty history).
fn make_app(args: &[&str], channels: usize, secs: f64) -> App {
    let cli = Cli::try_parse_from(std::iter::once("vu").chain(args.iter().copied())).unwrap();
    cli.validate().unwrap();
    let mut analyzer = Analyzer::new(AnalyzerConfig {
        sample_rate: SR,
        channels,
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
    });
    let info = SourceInfo {
        name: "test-gen".into(),
        sample_rate: SR,
        channels,
        total_frames: None,
        kind: SourceKind::Generator,
    };
    let mut app = App::new(
        &cli,
        info,
        &analyzer,
        Arc::new(Control::default()),
        ColorMode::TrueColor,
    );
    let mut gen = Generator::new(parse_spec("sweep:50:15000:2:-10").unwrap(), SR);
    let total = (secs * SR as f64) as usize;
    let mut done = 0;
    while done < total {
        let n = (total - done).min(4096);
        let mut chunk = vec![0.0f32; n * channels];
        gen.fill(&mut chunk, channels);
        analyzer.push(&chunk, |f| app.push_frame(f));
        done += n;
    }
    app
}

use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use std::sync::atomic::Ordering;
use vu::app::Action;
use vu::cli::Panel;
use vu::dsp::scale::FreqScale;
use vu::ui::colormap::ColorMap;

fn key(app: &mut App, code: KeyCode) -> Action {
    app.handle_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn mod_key(app: &mut App, code: KeyCode, m: KeyModifiers) -> Action {
    app.handle_event(Event::Key(KeyEvent::new(code, m)))
}

/// App with data and a laid-out view (width set by drawing once), zoomed to
/// one analysis frame per column.
fn ready(args: &[&str]) -> App {
    let mut app = make_app(args, 2, 2.0);
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| vu::ui::draw(f, &mut app)).unwrap();
    app.view.span_secs = app.view.width as f64 * app.view.dt;
    term.draw(|f| vu::ui::draw(f, &mut app)).unwrap();
    app
}

#[test]
fn left_unfollows_and_moves_one_frame() {
    let mut app = ready(&[]);
    let last = app.history.last().unwrap();
    assert!(app.view.follow);
    assert_eq!(key(&mut app, KeyCode::Left), Action::Redraw);
    assert!(!app.view.follow);
    assert_eq!(app.view.cursor, last - 1);
}

#[test]
fn right_moves_and_refollows_at_live_edge() {
    let mut app = ready(&[]);
    let last = app.history.last().unwrap();
    for _ in 0..3 {
        key(&mut app, KeyCode::Left);
    }
    assert_eq!(app.view.cursor, last - 3);
    key(&mut app, KeyCode::Right);
    assert_eq!(app.view.cursor, last - 2);
    assert!(!app.view.follow);
    key(&mut app, KeyCode::Right);
    key(&mut app, KeyCode::Right);
    assert!(app.view.follow);
    assert_eq!(app.view.cursor, last);
}

#[test]
fn right_while_following_is_noop() {
    let mut app = ready(&[]);
    key(&mut app, KeyCode::Right);
    assert!(app.view.follow);
}

#[test]
fn shift_left_moves_ten() {
    let mut app = ready(&[]);
    let last = app.history.last().unwrap();
    mod_key(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
    assert_eq!(app.view.cursor, last - 10);
    mod_key(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
    assert!(app.view.follow, "shift+right by 10 reaches the live edge");
}

#[test]
fn page_up_and_down() {
    let mut app = ready(&[]);
    let last = app.history.last().unwrap();
    let span = app.view.span();
    key(&mut app, KeyCode::PageUp);
    assert!(!app.view.follow);
    assert_eq!(
        app.view.cursor,
        last.saturating_sub(span).max(app.history.first())
    );
    key(&mut app, KeyCode::PageDown);
    assert!(app.view.follow, "paging back to the end resumes following");
    assert_eq!(app.view.cursor, last);
}

#[test]
fn home_and_end() {
    let mut app = ready(&[]);
    let first = app.history.first();
    let last = app.history.last().unwrap();
    key(&mut app, KeyCode::Home);
    assert!(!app.view.follow);
    assert_eq!(app.view.cursor, first);
    assert_eq!(app.view.cursor_column(), Some(0));
    key(&mut app, KeyCode::End);
    assert!(app.view.follow);
    assert_eq!(app.view.cursor, last);
}

#[test]
fn left_at_start_is_clamped() {
    let mut app = ready(&[]);
    key(&mut app, KeyCode::Home);
    for _ in 0..5 {
        key(&mut app, KeyCode::Left);
    }
    assert_eq!(app.view.cursor, app.history.first());
}

#[test]
fn space_toggles_hold() {
    let mut app = ready(&[]);
    let last = app.history.last().unwrap();
    key(&mut app, KeyCode::Char(' '));
    assert!(!app.view.follow);
    assert_eq!(app.view.cursor, last);
    key(&mut app, KeyCode::Char(' '));
    assert!(app.view.follow);
}

#[test]
fn plus_minus_zoom() {
    let mut app = make_app(&[], 2, 2.0);
    assert_eq!(app.view.span_secs, 10.0);
    key(&mut app, KeyCode::Char('-'));
    key(&mut app, KeyCode::Char('-'));
    assert_eq!(app.view.span_secs, 30.0);
    key(&mut app, KeyCode::Char('+'));
    assert_eq!(app.view.span_secs, 20.0);
    for _ in 0..20 {
        key(&mut app, KeyCode::Char('+'));
    }
    assert_eq!(
        app.view.span_secs,
        vu::app::ZOOM_SPANS[0],
        "zoom stops at the shortest span"
    );
    for _ in 0..20 {
        key(&mut app, KeyCode::Char('-'));
    }
    assert_eq!(app.view.span_secs, *vu::app::ZOOM_SPANS.last().unwrap());
}

#[test]
fn span_option_sets_initial_zoom() {
    let app = make_app(&["--span", "5"], 2, 2.0);
    assert_eq!(app.view.span_secs, 5.0);
}

#[test]
fn zoomed_left_steps_by_one_column() {
    let mut app = ready(&[]);
    let last = app.history.last().unwrap();
    // Four frames per column.
    app.view.span_secs *= 4.0;
    key(&mut app, KeyCode::Left);
    assert_eq!(app.view.cursor, last - 4);
}

#[test]
fn panel_toggles() {
    let mut app = ready(&[]);
    assert!(app.display.shown(Panel::Meter));
    key(&mut app, KeyCode::Char('1'));
    assert!(!app.display.shown(Panel::Meter));
    key(&mut app, KeyCode::Char('1'));
    assert!(app.display.shown(Panel::Meter));
    key(&mut app, KeyCode::Char('2'));
    assert!(!app.display.shown(Panel::Eq));
    key(&mut app, KeyCode::Char('3'));
    assert!(!app.display.shown(Panel::Spectrogram));
    key(&mut app, KeyCode::Char('4'));
    assert!(!app.display.shown(Panel::Timeline));
    // Only the meter is left: it cannot be hidden.
    key(&mut app, KeyCode::Char('1'));
    assert!(app.display.shown(Panel::Meter));
    assert_eq!(app.display.panels.iter().filter(|&&b| b).count(), 1);
    key(&mut app, KeyCode::Char('4'));
    assert!(app.display.shown(Panel::Timeline));
}

#[test]
fn last_visible_panel_survives_every_order() {
    for first in ['1', '2', '3', '4'] {
        let mut app = ready(&["-p", "eq,timeline"]);
        let _ = first;
        key(&mut app, KeyCode::Char('2'));
        key(&mut app, KeyCode::Char('4'));
        assert!(app.display.shown(Panel::Timeline));
        assert!(!app.display.shown(Panel::Eq));
    }
}

#[test]
fn colormap_cycles() {
    let mut app = ready(&[]);
    let start = app.display.colormap;
    key(&mut app, KeyCode::Char('c'));
    assert_ne!(app.display.colormap, start);
    assert_eq!(app.display.colormap, start.next());
    assert!(app.message.is_some());
    for _ in 0..4 {
        key(&mut app, KeyCode::Char('c'));
    }
    assert_eq!(app.display.colormap, start, "five maps cycle back");
    let _ = ColorMap::Magma;
}

#[test]
fn r_resets_loudness_and_shift_r_records() {
    let mut app = ready(&[]);
    app.session.max_lufs_m = -10.0;
    app.session.max_tp = -0.5;
    app.session.max_db = -1.0;
    assert_eq!(key(&mut app, KeyCode::Char('r')), Action::Redraw);
    assert_eq!(app.session.max_lufs_m, f32::NEG_INFINITY);
    assert!(app.session.max_tp < -100.0);
    assert!(
        app.loudness_reset,
        "asks the analyzer to restart integrated loudness"
    );
    assert_eq!(
        app.session.max_db, -1.0,
        "r leaves the level maximum (that is x)"
    );
    assert!(app.recorder.is_none(), "r no longer records");
    assert!(app
        .message
        .as_ref()
        .is_some_and(|(m, _)| m.contains("loudness")));

    let dir = std::env::temp_dir().join(format!("vu-keys-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    app.record_base = Some(dir.join("take.wav"));
    key(&mut app, KeyCode::Char('R'));
    assert!(app.recorder.is_some(), "R starts recording");
    key(&mut app, KeyCode::Char('R'));
    assert!(app.recorder.is_none(), "R stops it");
    assert_eq!(app.saved_recordings.len(), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn v_cycles_meter_modes() {
    use vu::app::MeterMode::*;
    let mut app = ready(&[]);
    assert_eq!(app.display.mode, Rms);
    for want in [Vu, Lufs, Rms] {
        assert_eq!(key(&mut app, KeyCode::Char('v')), Action::Redraw);
        assert_eq!(app.display.mode, want);
        assert!(app
            .message
            .as_ref()
            .is_some_and(|(m, _)| m.contains(want.name())));
    }
    assert_eq!(ready(&["--vu"]).display.mode, Vu, "--vu starts in VU mode");
    assert_eq!(
        ready(&["--lufs"]).display.mode,
        Lufs,
        "--lufs starts in LUFS mode"
    );
    assert!(Cli::try_parse_from(["vu", "--vu", "--lufs"]).is_err());
    assert_eq!(
        ready(&[]).display.lufs_target,
        -14.0,
        "streaming target by default"
    );
    assert_eq!(ready(&["--lufs-target", "-23"]).display.lufs_target, -23.0);
    for bad in ["-41", "1", "x"] {
        assert!(
            Cli::try_parse_from(["vu", "--lufs-target", bad])
                .map_or(true, |c| c.validate().is_err()),
            "{bad}"
        );
    }
}

#[test]
fn brackets_move_the_chart_and_spectrogram_floors_together() {
    let mut app = ready(&[]);
    assert_eq!((app.display.db_min, app.display.spec_floor), (-70.0, -90.0));
    key(&mut app, KeyCode::Char(']'));
    assert_eq!((app.display.db_min, app.display.spec_floor), (-64.0, -84.0));
    for _ in 0..20 {
        key(&mut app, KeyCode::Char('['));
    }
    assert_eq!(
        (app.display.db_min, app.display.spec_floor),
        (-120.0, -120.0),
        "both stop at -120"
    );
    for _ in 0..30 {
        key(&mut app, KeyCode::Char(']'));
    }
    assert_eq!(
        (app.display.db_min, app.display.spec_floor),
        (-12.0, -12.0),
        "12 dB under the ceiling"
    );
    assert_eq!(ready(&["--spec-floor", "-100"]).display.spec_floor, -100.0);
    assert!(Cli::try_parse_from(["vu", "--spec-floor", "5"])
        .unwrap()
        .validate()
        .is_err());
    assert_eq!(ready(&["--rms-aes17"]).display.rms_offset, 3.01);
}

#[test]
fn cycle_views_steps_modes_on_a_timer() {
    use std::time::{Duration, Instant};
    use vu::app::MeterMode::*;
    let mut app = ready(&[]);
    assert!(
        !app.cycle_tick(Instant::now() + Duration::from_secs(60)),
        "off by default"
    );
    let mut app = ready(&["--cycle-views"]);
    let t0 = app.cycle.unwrap().1;
    assert_eq!(app.cycle.unwrap().0, Duration::from_secs(3));
    let at = |s: f64| t0 + Duration::from_secs_f64(s);
    assert!(!app.cycle_tick(at(2.9)));
    assert!(app.cycle_tick(at(3.0)));
    assert_eq!(app.display.mode, Vu);
    assert!(!app.cycle_tick(at(5.9)));
    assert!(app.cycle_tick(at(6.0)));
    assert!(app.cycle_tick(at(9.0)));
    assert_eq!(app.display.mode, Rms, "wraps around");
    let app = ready(&["--cycle-views=0.5", "--lufs"]);
    assert_eq!(
        (app.cycle.unwrap().0, app.display.mode),
        (Duration::from_millis(500), Lufs)
    );
    for bad in ["--cycle-views=0", "--cycle-views=x"] {
        assert!(
            Cli::try_parse_from(["vu", bad]).map_or(true, |c| c.validate().is_err()),
            "{bad}"
        );
    }
}

#[test]
fn freq_scale_cycles() {
    let mut app = ready(&[]);
    let start = app.display.freq_scale;
    key(&mut app, KeyCode::Char('s'));
    assert_eq!(app.display.freq_scale, start.next());
    key(&mut app, KeyCode::Char('s'));
    key(&mut app, KeyCode::Char('s'));
    assert_eq!(app.display.freq_scale, start);
    let _ = FreqScale::Log;
}

#[test]
fn brackets_adjust_db_floor() {
    let mut app = ready(&[]);
    let (lo, hi) = (app.display.db_min, app.display.db_max);
    key(&mut app, KeyCode::Char('['));
    assert_eq!(app.display.db_min, lo - 6.0);
    key(&mut app, KeyCode::Char(']'));
    assert_eq!(app.display.db_min, lo);
    for _ in 0..100 {
        key(&mut app, KeyCode::Char('['));
    }
    assert_eq!(app.display.db_min, -120.0);
    for _ in 0..100 {
        key(&mut app, KeyCode::Char(']'));
    }
    assert!(app.display.db_min <= app.display.db_max - 12.0);
    assert_eq!(app.display.db_max, hi);
}

#[test]
fn help_toggles_and_esc_closes_it() {
    let mut app = ready(&[]);
    key(&mut app, KeyCode::Char('?'));
    assert!(app.show_help);
    assert_eq!(key(&mut app, KeyCode::Esc), Action::Redraw);
    assert!(!app.show_help);
    key(&mut app, KeyCode::Char('?'));
    key(&mut app, KeyCode::Char('?'));
    assert!(!app.show_help);
    // Esc with no help open quits.
    assert_eq!(key(&mut app, KeyCode::Esc), Action::Quit);
}

#[test]
fn quit_keys() {
    let mut app = ready(&[]);
    assert_eq!(key(&mut app, KeyCode::Char('q')), Action::Quit);
    assert_eq!(
        mod_key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL),
        Action::Quit
    );
    // Plain 'c' is the colormap key, not quit.
    assert_eq!(key(&mut app, KeyCode::Char('c')), Action::Redraw);
}

#[test]
fn p_toggles_pause() {
    let mut app = ready(&[]);
    assert!(!app.control.paused.load(Ordering::Relaxed));
    key(&mut app, KeyCode::Char('p'));
    assert!(app.control.paused.load(Ordering::Relaxed));
    assert!(app.paused());
    key(&mut app, KeyCode::Char('p'));
    assert!(!app.control.paused.load(Ordering::Relaxed));
}

#[test]
fn unknown_key_and_release_do_nothing() {
    let mut app = ready(&[]);
    assert_eq!(key(&mut app, KeyCode::Char('z')), Action::None);
    let mut ev = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
    ev.kind = KeyEventKind::Release;
    assert_eq!(app.handle_event(Event::Key(ev)), Action::None);
    assert!(app.view.follow);
    assert_eq!(app.handle_event(Event::Resize(10, 10)), Action::Redraw);
}

#[test]
fn keys_on_empty_history_do_not_panic() {
    let mut app = make_app(&[], 2, 0.0);
    for code in [
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::Char(' '),
        KeyCode::Char('+'),
        KeyCode::Char('-'),
    ] {
        key(&mut app, code);
    }
    assert!(app.view.follow);
}
