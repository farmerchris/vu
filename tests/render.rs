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
        surround: false,
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

const SIZES: [(u16, u16); 11] = [
    (1, 1),
    (2, 2),
    (5, 3),
    (10, 4),
    (20, 6),
    (40, 10),
    (80, 24),
    (100, 1),
    (120, 2),
    (200, 60),
    (300, 100),
];

fn render(app: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| vu::ui::draw(f, app)).unwrap();
    let buf = term.backend().buffer();
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
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
    let names = ["meter", "eq", "spectrogram", "timeline", "piano", "scope"];
    for mask in 1u8..64 {
        let list: Vec<&str> = (0..6)
            .filter(|i| mask >> i & 1 == 1)
            .map(|i| names[i])
            .collect();
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
fn each_meter_mode_shows_only_its_own_measure() {
    let tone = "sine:1000:-16";
    // RMS: dBFS bars per channel, no VU or loudness readouts.
    let mut app = make_app_with(&["-p", "meter"], 2, 1.0, tone);
    let t = text(&render(&mut app, 100, 24));
    assert!(
        t.contains("Levels · RMS") && t.contains("dBFS   peak   hold"),
        "{t}"
    );
    assert!(
        !t.contains("LUFS") && !t.contains(" LU ") && !t.contains("  vu "),
        "{t}"
    );
    // VU: needles on the VU scale. A -16 dBFS sine reads +2 VU.
    let mut app = make_app_with(&["-p", "meter", "--vu"], 2, 1.0, tone);
    let lines = render(&mut app, 100, 24);
    let t = text(&lines);
    assert!(
        t.contains("Levels · VU") && t.contains("VU       vu   hold") && t.contains("L      +2.0"),
        "{t}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("│ -20 "))
            && lines.iter().any(|l| l.starts_with("│  +3 ")),
        "{t}"
    );
    assert!(!t.contains("LUFS") && !t.contains("peak"), "{t}");
    // LUFS: M and S bars on the LU axis; the stereo tone reads -16 LUFS.
    let mut app = make_app_with(&["-p", "meter", "--lufs"], 2, 1.0, tone);
    let lines = render(&mut app, 100, 24);
    let t = text(&lines);
    assert!(
        t.contains("Levels · LUFS · 0 LU = -14") && t.contains("LUFS    mom  short  integ  max M"),
        "{t}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("│  +9 "))
            && lines.iter().any(|l| l.starts_with("│ -18 ")),
        "{t}"
    );
    assert!(
        t.contains(" M ") && t.contains(" S ") && !t.contains(" L ") && !t.contains("  vu "),
        "{t}"
    );
    assert!(t.contains("-16.0"), "{t}");
    // Integrated is 2 LU under the -14 target; one second is too short for a
    // loudness range.
    assert!(t.contains("tgt    -2.0 LU   LRA     — LU"), "{t}");
    // Peaks and clips stay in every mode: sample peak, or true peak in LUFS.
    for (args, label, unit) in [
        (&[][..], "max", "dBFS"),
        (&["--vu"], "max", "dBFS"),
        (&["--lufs"], "TP ", "dBTP"),
    ] {
        let mut app = make_app_with(&[&["-p", "meter"][..], args].concat(), 2, 1.0, tone);
        let t = text(&render(&mut app, 100, 24));
        assert!(
            t.contains(&format!("{label}   -16.0 {unit}    0 clips")),
            "{args:?}: {t}"
        );
    }
    // Integrated loudness points from beside the S bar, on the same row as
    // the bars' tops.
    let mut app = make_app_with(&["-p", "meter", "--lufs"], 2, 2.0, tone);
    let lines = render(&mut app, 100, 24);
    let marker = lines
        .iter()
        .position(|l| l.contains('◀'))
        .expect("integrated marker");
    assert!(
        lines[marker].contains("▁▁▁▁▁▁▁▁◀") || lines[marker].contains("████████◀"),
        "{}",
        lines[marker]
    );
    // --rms-aes17 shows RMS 3 dB higher: a sine's RMS reads its peak.
    let mut app = make_app_with(&["-p", "meter", "--rms-aes17"], 2, 1.0, tone);
    let t = text(&render(&mut app, 100, 24));
    assert!(t.contains("L     -16.0  -16.0  -16.0  -16.0"), "{t}");
}

#[test]
fn every_mode_fits_at_every_width() {
    for mode in [&[][..], &["--vu"], &["--lufs"]] {
        for (ch, w) in [(2usize, 80u16), (1, 80), (2, 140), (8, 160)] {
            let mut app = make_app_with(mode, ch, 1.0, "sine:1000:-16");
            let t = text(&render(&mut app, w, 24));
            // The readouts are not cut short.
            assert!(
                t.contains("+22c") && t.contains(" clips"),
                "{mode:?} {ch}ch {w}: {t}"
            );
        }
    }
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
    let col = app
        .view
        .cursor_column()
        .expect("cursor visible after scrolling 20 frames");
    // The chart is inset by an axis on the left, so the buffer column is offset by the chart x.
    let x0 = app.chart_rects[0].x as usize;
    let marker_cols: Vec<usize> = lines
        .iter()
        .flat_map(|l| {
            l.chars()
                .enumerate()
                .filter(|(_, c)| *c == '▼')
                .map(|(i, _)| i)
        })
        .collect();
    assert!(
        !marker_cols.is_empty(),
        "no cursor marker drawn:\n{}",
        text(&lines)
    );
    assert!(
        marker_cols.contains(&(x0 + col)),
        "expected marker at {} got {marker_cols:?}",
        x0 + col
    );
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
    assert!(
        screen.contains("max") && screen.contains("1 clip"),
        "{screen}"
    );
    assert!(screen.contains("tone"), "{screen}");
}

#[test]
fn timeline_follows_the_meter_mode() {
    let sweep = "sweep:100:800:5:-20:-10";
    let args = ["-p", "timeline", "-z", "5"];
    let mut app = make_app_with(&args, 2, 5.0, sweep);
    let t = text(&render(&mut app, 80, 16));
    assert!(
        t.contains("Timeline  peak / rms") && !t.contains("  +3 "),
        "{t}"
    );
    let mut app = make_app_with(&[&args[..], &["--vu"]].concat(), 2, 5.0, sweep);
    let lines = render(&mut app, 80, 16);
    let t = text(&lines);
    assert!(t.contains("Timeline  vu"), "{t}");
    // VU marks down the axis, the sweep near -2 VU once the needle has risen
    // (0.4 s in) and pinned above +3 VU once past -15 dBFS.
    let row = |lines: &[String], m: &str| {
        lines
            .iter()
            .find(|l| l.starts_with(&format!("│{m:>5} ")))
            .cloned()
            .unwrap()
    };
    let first = |l: String| l.chars().skip(7 + 6).take(3).collect::<String>();
    assert!(
        first(row(&lines, "-3")) == "███" && first(row(&lines, "+1")).trim().is_empty(),
        "{t}"
    );
    assert!(
        row(&lines, "+3").trim_end_matches('│').ends_with("███"),
        "{t}"
    );
    // LUFS, on the LU scale around the target. The sweep runs from -20 to
    // about -10 LUFS: -6 to +4 LU around the default -14 LUFS target...
    let mut app = make_app_with(&[&args[..], &["--lufs"]].concat(), 2, 5.0, sweep);
    let lines = render(&mut app, 80, 24);
    let t = text(&lines);
    assert!(t.contains("Timeline  momentary loudness ·"), "{t}");
    let last = |l: String| {
        l.trim_end_matches('│')
            .chars()
            .rev()
            .take(3)
            .collect::<String>()
    };
    assert!(
        last(row(&lines, "+9")).trim().is_empty() && !last(row(&lines, "0")).contains(' '),
        "{t}"
    );
    // At the start the 0 LU row holds only the faint target line.
    assert!(
        first(row(&lines, "-9")) == "███" && first(row(&lines, "0")) == "▔▔▔",
        "{t}"
    );
    // ...and +3 to +13 LU around the EBU -23 LUFS one, pinned at the top.
    let mut app = make_app_with(
        &[&args[..], &["--lufs", "--lufs-target", "-23"]].concat(),
        2,
        5.0,
        sweep,
    );
    let lines = render(&mut app, 80, 24);
    let t = text(&lines);
    assert!(
        last(row(&lines, "+9")) == "███" && first(row(&lines, "0")) == "███",
        "{t}"
    );
}

#[test]
fn piano_panel_lights_the_tone() {
    use ratatui::style::Color;
    // Not shown by default; `piano` in -p, --piano or -p all shows it.
    let mut app = make_app(&[], 2, 1.0);
    assert!(!text(&render(&mut app, 80, 30)).contains("Piano"));
    for args in [&["-p", "meter,piano"][..], &["--piano"], &["-p", "all"]] {
        let mut app = make_app(args, 2, 1.0);
        assert!(
            text(&render(&mut app, 80, 40)).contains("Piano"),
            "{args:?}"
        );
    }
    let mut app = make_app(&["-p", "all"], 2, 1.0);
    let t = text(&render(&mut app, 80, 40));
    assert!(
        t.contains("Levels")
            && t.contains("Equalizer")
            && t.contains("Spectrogram")
            && t.contains("Timeline"),
        "{t}"
    );

    // A 466 Hz tone lights A#4, a black key; 440 Hz lights A4, a white key.
    for (spec, name, black) in [
        ("sine:466.16:-12", "A#4", true),
        ("sine:440:-12", "A4", false),
    ] {
        let mut app = make_app_with(&["-p", "piano"], 2, 1.0, spec);
        let mut term = Terminal::new(TestBackend::new(80, 1 + vu::ui::piano::HEIGHT)).unwrap();
        term.draw(|f| vu::ui::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        let row = |y: u16| {
            (0..80)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        };
        assert!(row(1).contains(&format!("Piano  {name} ·")), "{}", row(1));
        // The lit key is cyan on the top row, and only on the bottom row for a white key.
        let lit = |y: u16| (0..80).filter(|&x| buf[(x, y)].bg == Color::Cyan).count();
        assert!(lit(2) >= 1, "{name}");
        assert_eq!(lit(4) > 0, !black, "{name}");
        // Black keys are drawn: some top-row cells are black, the bottom row has none.
        let blacks = |y: u16| {
            (0..80)
                .filter(|&x| buf[(x, y)].bg == Color::Indexed(16))
                .count()
        };
        assert!(blacks(2) > 10 && blacks(4) == 0);
        // Its name is labelled under the keys.
        assert!(row(5).contains(name), "{}", row(5));
    }
}

#[test]
fn max_momentary_mark_is_dark_gray() {
    use ratatui::style::Color;
    // Loud first, then quiet: the max-momentary mark stands above the M bar.
    let mut app = make_app_with(
        &["--lufs", "-p", "meter"],
        2,
        5.0,
        "sweep:500:500:5:-10:-40",
    );
    let mut term = Terminal::new(TestBackend::new(80, 14)).unwrap();
    term.draw(|f| vu::ui::draw(f, &mut app)).unwrap();
    let buf = term.backend().buffer();
    let marks: Vec<Color> = (0..14)
        .flat_map(|y| (6..14).map(move |x| (x, y)))
        .filter(|&p| matches!(buf[p].symbol(), "▔" | "▁"))
        .map(|p| buf[p].fg)
        .collect();
    assert!(
        !marks.is_empty() && marks.iter().all(|&c| c == Color::DarkGray),
        "{marks:?}"
    );
}

#[test]
fn loudness_bars_are_blue_and_green_then_yellow_above_target() {
    use ratatui::style::Color;
    let bar_colors = |spec: &str| {
        let mut app = make_app_with(&["--lufs", "-p", "meter"], 2, 4.0, spec);
        let mut term = Terminal::new(TestBackend::new(80, 14)).unwrap();
        term.draw(|f| vu::ui::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        // M bar right of the 5-column axis, S bar after it; colours bottom to top.
        let colors_in = move |x0: u16| -> Vec<Color> {
            let mut v: Vec<Color> = (2..12)
                .rev()
                .map(|y| (x0, y))
                .filter(|&p| buf[p].symbol() == "█")
                .map(|p| buf[p].fg)
                .collect();
            v.dedup();
            v
        };
        (colors_in(6), colors_in(15))
    };
    // -20 LUFS, under the -14 target: blue and green all the way up.
    assert_eq!(
        bar_colors("sine:1000:-20"),
        (
            vec![vu::ui::colormap::LU_M_COLOR],
            vec![vu::ui::colormap::LU_S_COLOR]
        )
    );
    // -8 LUFS, 6 LU over: yellow from the target up.
    assert_eq!(
        bar_colors("sine:1000:-8"),
        (
            vec![vu::ui::colormap::LU_M_COLOR, Color::Yellow],
            vec![vu::ui::colormap::LU_S_COLOR, Color::Yellow]
        )
    );
}

#[test]
fn loudness_target_line_and_readout_colours() {
    use ratatui::style::Color;
    use vu::ui::colormap::{LU_M_COLOR, LU_S_COLOR, LU_TARGET_LINE};
    // -30 LUFS, under the -14 target: the target line crosses the empty part
    // of both bars, and the readouts take their bars' colours.
    let mut app = make_app_with(&["--lufs", "-p", "meter"], 2, 4.0, "sine:1000:-30");
    let mut term = Terminal::new(TestBackend::new(80, 14)).unwrap();
    term.draw(|f| vu::ui::draw(f, &mut app)).unwrap();
    let buf = term.backend().buffer();
    let line_cells = (2..12)
        .flat_map(|y| (6..23).map(move |x| (x, y)))
        .filter(|&p| buf[p].fg == LU_TARGET_LINE)
        .count();
    assert_eq!(line_cells, 16, "both 8-column bars");
    let row = |y: u16| {
        (0..80)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect::<String>()
    };
    let y = (0..14).find(|&y| row(y).contains("-30.0  -30.0")).unwrap();
    let r = row(y);
    let x = r.chars().count() as u16 - r[r.find("-30.0").unwrap()..].chars().count() as u16;
    let fg_at = |dx: u16| buf[(x + dx, y)].fg;
    assert_eq!(
        (fg_at(0), fg_at(7), fg_at(14)),
        (LU_M_COLOR, LU_S_COLOR, Color::White)
    );
}

#[test]
fn loudness_timeline_shows_momentary_in_the_m_bar_colours() {
    use ratatui::style::Color;
    use vu::ui::colormap::{LU_M_COLOR, LU_OVER_COLOR, LU_S_COLOR};
    // Rising from -30 to -8 LUFS, past the -14 target.
    let mut app = make_app_with(
        &["--lufs", "-p", "timeline", "-z", "5"],
        2,
        5.0,
        "sweep:500:500:5:-30:-8",
    );
    let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
    term.draw(|f| vu::ui::draw(f, &mut app)).unwrap();
    let buf = term.backend().buffer();
    let mut seen: Vec<Color> = (2..19)
        .flat_map(|y| (7..78).map(move |x| (x, y)))
        .filter(|&p| buf[p].symbol() == "█")
        .map(|p| buf[p].fg)
        .collect();
    seen.sort_by_key(|c| format!("{c:?}"));
    seen.dedup();
    // Blue below the target, yellow above it; no short-term colour.
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(
        seen.contains(&LU_M_COLOR) && seen.contains(&LU_OVER_COLOR) && !seen.contains(&LU_S_COLOR)
    );
}

/// Whether `c` is a braille pattern with at least one dot.
fn braille(c: char) -> bool {
    ('\u{2801}'..='\u{28ff}').contains(&c)
}

#[test]
fn scope_panel_is_a_vectorscope_for_stereo() {
    // Not shown by default; `scope` in -p, --scope or -p all shows it.
    let mut app = make_app(&[], 2, 1.0);
    assert!(!text(&render(&mut app, 120, 40)).contains("Scope"));
    for args in [&["-p", "meter,scope"][..], &["--scope"], &["-p", "all"]] {
        let mut app = make_app(args, 2, 1.0);
        assert!(
            text(&render(&mut app, 150, 44)).contains("Scope"),
            "{args:?}"
        );
    }
    // The test signal is the same on both channels: fully correlated, so
    // the dots are a vertical line and the marker sits at +1.
    let mut app = make_app_with(&["-p", "scope"], 2, 1.0, "sine:440:-20");
    let lines = render(&mut app, 60, 20);
    let t = text(&lines);
    assert!(t.contains("Scope  corr +1.00 · bal 0.0 dB"), "{t}");
    let cols: std::collections::BTreeSet<usize> = lines
        .iter()
        .flat_map(|l| {
            l.chars()
                .enumerate()
                .filter(|&(_, c)| braille(c))
                .map(|(x, _)| x)
        })
        .collect();
    assert_eq!(cols.len(), 1, "one column of dots: {t}");
    let rows = lines.iter().filter(|l| l.chars().any(braille)).count();
    assert!(rows >= 8, "{rows} rows of dots\n{t}");
    let bar = lines
        .iter()
        .find(|l| l.contains("-1 "))
        .expect("correlation bar");
    assert!(bar.trim_end_matches('│').ends_with("● +1"), "{bar}");
    assert!(t.contains("│L") && t.contains("R│"), "{t}");
}

#[test]
fn scope_panel_is_an_oscilloscope_for_mono() {
    let mut app = make_app_with(&["-p", "scope"], 1, 1.0, "sine:440:-20");
    let lines = render(&mut app, 60, 20);
    let t = text(&lines);
    assert!(t.contains("Scope  -20 dB"), "{t}");
    assert!(
        !t.contains("corr") && !t.contains("+1"),
        "no correlation meter: {t}"
    );
    // A waveform crosses the whole width and most of the height.
    let cols: std::collections::BTreeSet<usize> = lines
        .iter()
        .flat_map(|l| {
            l.chars()
                .enumerate()
                .filter(|&(_, c)| braille(c))
                .map(|(x, _)| x)
        })
        .collect();
    assert_eq!(cols.len(), 58, "{t}");
    assert!(
        lines.iter().filter(|l| l.chars().any(braille)).count() >= 16,
        "{t}"
    );
    // Silence says so rather than drawing a flat line of noise.
    let mut app = make_app_with(&["-p", "scope"], 1, 1.0, "silence");
    assert!(text(&render(&mut app, 60, 20)).contains("silence"));
    let mut app = make_app_with(&["-p", "scope"], 2, 0.0, "silence");
    assert!(text(&render(&mut app, 60, 20)).contains("waiting for audio"));
}

#[test]
fn scope_shares_the_top_row() {
    let mut app = make_app(&["-p", "all"], 2, 1.0);
    let lines = render(&mut app, 150, 44);
    let top = &lines[1];
    let (m, e, s) = (
        top.find("Levels").unwrap(),
        top.find("Equalizer").unwrap(),
        top.find("Scope").unwrap(),
    );
    assert!(m < e && e < s, "{top}");
    assert_eq!(top.chars().count(), 150);
}

#[test]
fn meter_shows_correlation_and_balance_for_stereo() {
    for args in [&[][..], &["--vu"], &["--lufs"]] {
        let mut app = make_app_with(args, 2, 1.0, "sine:440:-20");
        let t = text(&render(&mut app, 100, 30));
        assert!(t.contains("corr  +1.00   bal 0.0 dB"), "{args:?}\n{t}");
        // It still fits the smallest standard terminal with everything else.
        let t = text(&render(&mut app, 80, 24));
        assert!(
            t.contains("corr  +1.00") && t.contains("tone"),
            "{args:?}\n{t}"
        );
        let mut mono = make_app_with(args, 1, 1.0, "sine:440:-20");
        assert!(!text(&render(&mut mono, 100, 30)).contains("corr"));
    }
}

#[test]
fn markers_show_on_the_charts() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = make_app(&[], 2, 3.0);
    render(&mut app, 100, 30);
    let press = |app: &mut App, c: char| {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    };
    let count = |app: &mut App| text(&render(app, 100, 30)).matches('◆').count();
    // The status bar's HOLD badge has one of its own once the cursor is placed.
    app.nav(Nav::Left(20));
    let before = count(&mut app);
    press(&mut app, 'm');
    assert_eq!(count(&mut app), before + 2, "spectrogram and timeline");
    app.nav(Nav::Left(10));
    press(&mut app, 'm');
    assert_eq!(count(&mut app), before + 4);
    // Scrolled out of view, they are not drawn.
    app.view.span_secs = 0.5;
    app.nav(Nav::End);
    assert_eq!(count(&mut app), 0);
}

#[test]
fn status_bar_shows_file_length_and_monitoring() {
    let mut app = make_app(&[], 2, 1.0);
    let t = render(&mut app, 120, 24)[0].clone();
    assert!(!t.contains(" / ") && !t.contains("MON"), "{t}");
    app.info.kind = SourceKind::File;
    app.info.total_frames = Some(SR as u64 * 90);
    app.monitor = true;
    let t = render(&mut app, 120, 24)[0].clone();
    assert!(t.contains("t 0:00.9") && t.contains(" / 1:30.000"), "{t}");
    assert!(t.contains("♪ MON"), "{t}");
}

#[test]
fn help_lists_the_new_keys() {
    let mut app = make_app(&[], 2, 0.5);
    app.show_help = true;
    let t = text(&render(&mut app, 120, 50));
    for needle in [
        "1 2 3 4 5 6",
        "scope",
        "m / M",
        "n / N",
        "monitor",
        "corr  bal",
    ] {
        assert!(t.contains(needle), "{needle}\n{t}");
    }
}
