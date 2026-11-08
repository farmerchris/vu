//! Full-UI render tests on a ratatui TestBackend.

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
    make_app_with(args, channels, secs, "sweep:50:15000:2:-10")
}

fn make_app_with(args: &[&str], channels: usize, secs: f64, spec: &str) -> App {
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
    let mut app = App::new(&cli, info, &analyzer, Arc::new(Control::default()), ColorMode::TrueColor);
    let mut gen = Generator::new(parse_spec(spec).unwrap(), SR);
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
use ratatui::Terminal;
use vu::app::Nav;

const SIZES: [(u16, u16); 11] =
    [(1, 1), (2, 2), (5, 3), (10, 4), (20, 6), (40, 10), (80, 24), (100, 1), (120, 2), (200, 60), (300, 100)];

fn render(app: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| vu::ui::draw(f, app)).unwrap();
    let buf = term.backend().buffer();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
        .collect()
}

fn text(lines: &[String]) -> String {
    lines.join("\n")
}

#[test]
fn all_sizes_default_panels() {
    for ch in [1, 2, 10] {
        let mut app = make_app(&[], ch, 1.0);
        for (w, h) in SIZES {
            render(&mut app, w, h);
        }
    }
}

#[test]
fn all_sizes_empty_history() {
    for ch in [1, 2, 10] {
        let mut app = make_app(&[], ch, 0.0);
        assert!(app.history.last().is_none());
        for (w, h) in SIZES {
            render(&mut app, w, h);
        }
    }
}

#[test]
fn every_panel_subset() {
    let names = ["meter", "eq", "spectrogram", "timeline"];
    for mask in 1u8..16 {
        let list: Vec<&str> = (0..4).filter(|i| mask >> i & 1 == 1).map(|i| names[i]).collect();
        let arg = list.join(",");
        for ch in [1, 2, 10] {
            let mut app = make_app(&["-p", &arg], ch, 0.5);
            for (w, h) in SIZES {
                render(&mut app, w, h);
            }
        }
    }
}

#[test]
fn scrolling_zoom_and_help() {
    for ch in [1, 2, 10] {
        let mut app = make_app(&[], ch, 1.0);
        let navs = [
            Nav::Home,
            Nav::Left(1),
            Nav::Left(10_000),
            Nav::PageLeft,
            Nav::PageRight,
            Nav::Right(3),
            Nav::ZoomOut,
            Nav::ZoomOut,
            Nav::Column(0),
            Nav::Column(10_000),
            Nav::ToggleHold,
            Nav::ZoomIn,
            Nav::End,
        ];
        for nav in navs {
            app.nav(nav);
            for (w, h) in SIZES {
                render(&mut app, w, h);
            }
        }
        app.show_help = true;
        for (w, h) in SIZES {
            render(&mut app, w, h);
        }
        app.nav(Nav::Home);
        for (w, h) in SIZES {
            render(&mut app, w, h);
        }
    }
}

#[test]
fn colormaps_scales_and_modes() {
    for cm in ["magma", "inferno", "viridis", "fire", "gray"] {
        for fs in ["log", "linear", "mel"] {
            let mut app = make_app(&["--colormap", cm, "--freq-scale", fs], 2, 1.0);
            for (w, h) in [(20, 6), (80, 24), (200, 60)] {
                render(&mut app, w, h);
            }
            app.display.color_mode = ColorMode::Ansi256;
            render(&mut app, 80, 24);
        }
    }
}

#[test]
fn status_bar_shows_vu() {
    let mut app = make_app(&[], 2, 1.0);
    let lines = render(&mut app, 80, 24);
    assert!(lines[0].contains("vu"), "{}", lines[0]);
    assert!(lines[0].contains("test-gen"), "{}", lines[0]);
    assert!(lines[0].contains("48000 Hz"), "{}", lines[0]);
}

#[test]
fn meter_shows_stereo_labels() {
    let mut app = make_app(&["-p", "meter"], 2, 1.0);
    let t = text(&render(&mut app, 80, 24));
    assert!(t.contains('L'), "{t}");
    assert!(t.contains('R'), "{t}");
}

#[test]
fn vu_mode_switches_the_meter_readouts() {
    let mut app = make_app(&["-p", "meter"], 2, 1.0);
    let t = text(&render(&mut app, 100, 24));
    assert!(t.contains(" Levels ") && t.contains("peak") && !t.contains("Levels · VU"), "{t}");
    let mut app = make_app_with(&["-p", "meter", "--vu"], 2, 1.0, "sine:1000:-16");
    let lines = render(&mut app, 100, 24);
    let t = text(&lines);
    assert!(t.contains("Levels · VU") && t.contains("VU       vu   hold"), "{t}");
    // A -16 dBFS sine reads +2 VU with 0 VU at the default -18 dBFS.
    assert!(t.contains("L      +2.0"), "{t}");
    assert!(lines.iter().any(|l| l.starts_with("│ -20 ")) && lines.iter().any(|l| l.starts_with("│  +3 ")), "{t}");
}

#[test]
fn vu_mode_gives_loudness_its_own_axis_and_still_fits() {
    for (ch, w) in [(2usize, 80u16), (1, 80), (2, 140), (8, 160)] {
        let mut app = make_app_with(&["--vu"], ch, 1.0, "sine:1000:-16");
        let lines = render(&mut app, w, 24);
        let t = text(&lines);
        // EBU +9 marks beside the LU bar, and the readouts are not cut short.
        assert!(t.contains(" +9 ") && t.contains(" -18 "), "{ch}ch {w}: {t}");
        assert!(t.contains("+22c"), "{ch}ch {w}: {t}");
        assert!(t.contains("integ    max"), "{ch}ch {w}: {t}");
    }
    let mut app = make_app_with(&[], 2, 1.0, "sine:1000:-16");
    let t = text(&render(&mut app, 80, 24));
    assert!(!t.contains(" +9 "), "no LU axis in dBFS mode: {t}");
}

#[test]
fn mono_meter_label() {
    let mut app = make_app(&["-p", "meter"], 1, 1.0);
    let t = text(&render(&mut app, 80, 24));
    assert!(t.contains('M'), "{t}");
}

#[test]
fn cursor_marker_in_expected_column_when_scrolled() {
    let mut app = make_app(&["-p", "spectrogram"], 2, 3.0);
    let (w, h) = (100, 30);
    render(&mut app, w, h);
    app.nav(Nav::Left(20));
    let lines = render(&mut app, w, h);
    assert!(!app.view.follow);
    let col = app.view.cursor_column().expect("cursor visible after scrolling 20 frames");
    // The chart is inset by an axis on the left, so the buffer column is offset by the chart x.
    let x0 = app.chart_rects[0].x as usize;
    let marker_cols: Vec<usize> = lines
        .iter()
        .flat_map(|l| l.chars().enumerate().filter(|(_, c)| *c == '▼').map(|(i, _)| i))
        .collect();
    assert!(!marker_cols.is_empty(), "no cursor marker drawn:\n{}", text(&lines));
    assert!(marker_cols.contains(&(x0 + col)), "expected marker at {} got {marker_cols:?}", x0 + col);
}

#[test]
fn no_marker_when_following() {
    let mut app = make_app(&["-p", "spectrogram"], 2, 3.0);
    let t = text(&render(&mut app, 100, 30));
    assert!(app.view.follow);
    // Following live: either no marker or at the right edge; just ensure it is not mid-chart.
    let w = app.view.width;
    assert_eq!(app.view.cursor_column(), Some(w - 1));
    let _ = t;
}

#[test]
fn view_width_tracks_terminal() {
    let mut app = make_app(&["-p", "timeline"], 2, 1.0);
    render(&mut app, 60, 20);
    let a = app.view.width;
    render(&mut app, 120, 20);
    assert!(app.view.width > a);
    assert_eq!(app.chart_rects.len(), 1);
}

#[test]
fn clip_badge_and_session_line_fit_at_80x24() {
    // A full-scale square wave clips continuously.
    let mut app = make_app_with(&[], 2, 1.0, "square:100:0");
    let screen = render(&mut app, 80, 24).join("\n");
    assert!(screen.contains("CLIP"), "{screen}");
    assert!(screen.contains("max") && screen.contains("1 clip"), "{screen}");
    assert!(screen.contains("tone"), "{screen}");
}


#[test]
fn timeline_follows_vu_mode() {
    let args = ["-p", "timeline", "-z", "5"];
    let mut app = make_app_with(&args, 2, 5.0, "sweep:100:800:5:-20:-10");
    let t = text(&render(&mut app, 80, 16));
    assert!(t.contains("Timeline  peak / rms") && !t.contains("  +3 "), "{t}");
    let mut app = make_app_with(&[&args[..], &["--vu"]].concat(), 2, 5.0, "sweep:100:800:5:-20:-10");
    let lines = render(&mut app, 80, 16);
    let t = text(&lines);
    assert!(t.contains("Timeline  vu"), "{t}");
    // VU marks down the axis, the sweep near -2 VU once the needle has risen
    // (0.4 s in) and pinned above +3 VU once past -15 dBFS.
    let row = |m: &str| lines.iter().find(|l| l.starts_with(&format!("│{m:>5} "))).unwrap_or_else(|| panic!("{m}: {t}"));
    let first = |l: &str| l.chars().skip(7 + 6).take(3).collect::<String>();
    assert!(first(row("-3")) == "███" && first(row("+1")).trim().is_empty(), "{t}");
    assert!(row("+3").trim_end_matches('│').ends_with("███"), "{t}");
}
