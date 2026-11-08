//! Throughput benchmarks for analysis and drawing. Run with `cargo bench --bench perf`.

use clap::Parser;
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use std::sync::Arc;
use std::time::Instant;
use vu::analyzer::{Analyzer, AnalyzerConfig, Frame};
use vu::app::{App, Nav};
use vu::cli::Cli;
use vu::dsp::weighting::Weighting;
use vu::dsp::window::WindowKind;
use vu::generator::{parse_spec, Generator};
use vu::source::{Control, SourceInfo, SourceKind};
use vu::ui::colormap::ColorMode;

const SR: u32 = 48000;
const CH: usize = 2;

fn config(fft_size: usize, hop: usize) -> AnalyzerConfig {
    AnalyzerConfig {
        sample_rate: SR,
        channels: CH,
        fft_size,
        hop,
        window: WindowKind::Hann,
        weighting: Weighting::A,
        bands: None,
        min_freq: 20.0,
        max_freq: 20000.0,
        rms_tau: 0.3,
        peak_decay_db_s: 20.0,
        peak_hold_s: 1.5,
        band_floor_db: -70.0,
        spectrum_channel: None,
    }
}

fn audio(spec: &str, secs: f64) -> Vec<f32> {
    let mut g = Generator::new(parse_spec(spec).unwrap(), SR);
    let mut v = vec![0.0; (secs * SR as f64) as usize * CH];
    g.fill(&mut v, CH);
    v
}

fn bench_analysis() {
    let secs = 60.0;
    let data = audio("pink", secs);
    println!("analysis: 60 s stereo pink noise @ 48 kHz");
    for (fft, hop) in [(2048, 256), (2048, 512), (8192, 256), (8192, 512)] {
        let mut a = Analyzer::new(config(fft, hop));
        let mut n = 0usize;
        let t = Instant::now();
        for chunk in data.chunks(1024 * CH) {
            a.push(chunk, |f| {
                n += 1;
                std::hint::black_box(f);
            });
        }
        let el = t.elapsed().as_secs_f64();
        println!(
            "  fft {fft:>5} hop {hop:>3}: {:>7.1} ms  {:>6.1}x real time  {:>5.2} us/frame",
            el * 1e3,
            secs / el,
            el * 1e6 / n as f64
        );
    }
}

fn bench_fft() {
    for fft in [2048, 8192] {
        let mut sp = vu::dsp::spectrum::Spectrum::new(fft, WindowKind::Hann);
        let x = audio("pink", fft as f64 / SR as f64);
        let x: Vec<f32> = x.iter().step_by(CH).copied().collect();
        let mut out = vec![0.0; sp.bins()];
        let n = 2000;
        let t = Instant::now();
        for _ in 0..n {
            sp.power(std::hint::black_box(&x), &mut out);
        }
        println!(
            "  Spectrum::power {fft:>5}: {:>6.2} us",
            t.elapsed().as_secs_f64() * 1e6 / n as f64
        );
    }
}

fn build_app(minutes: f64, fft: usize, hop: usize) -> App {
    let (fs, hs) = (fft.to_string(), hop.to_string());
    let args = [
        "vu",
        "-g",
        "sweep:20:20000:7",
        "--history",
        "900",
        "--fft-size",
        &fs,
        "--hop",
        &hs,
    ];
    let cli = Cli::try_parse_from(args).unwrap();
    let mut analyzer = Analyzer::new(config(cli.fft_size, cli.hop()));
    let info = SourceInfo {
        name: "bench".into(),
        sample_rate: SR,
        channels: CH,
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
    // Analyze 20 s of real signal, then repeat it to fill the history.
    let mut seed: Vec<Frame> = Vec::new();
    analyzer.push(&audio("sweep:20:20000:7:-6", 20.0), |f| seed.push(f));
    let want = (minutes * 60.0 / analyzer.config().frame_dt()) as usize;
    let dt = analyzer.config().frame_dt();
    for i in 0..want {
        let mut f = seed[i % seed.len()].clone();
        f.time = (i + 1) as f64 * dt;
        app.history.push(f);
    }
    app
}

/// Mean draw time in ms; when `feed` is set, the frames of 1/60 s of audio are
/// appended before every draw, as in a live session. Also returns the first
/// (cold) draw time.
fn time_draw(
    term: &mut Terminal<TestBackend>,
    app: &mut App,
    iters: usize,
    feed: bool,
) -> (f64, f64) {
    let per_draw = if feed {
        (1.0 / 60.0 / app.analysis.frame_dt).ceil() as usize
    } else {
        0
    };
    let dt = app.analysis.frame_dt;
    let push = |app: &mut App| {
        for _ in 0..per_draw {
            let last = app.history.last().unwrap();
            let mut f = app.history.get(last - 1000).unwrap().clone();
            f.time = (last + 2) as f64 * dt;
            app.history.push(f);
        }
    };
    let t = Instant::now();
    term.draw(|f| vu::ui::draw(f, app)).unwrap();
    let cold = t.elapsed().as_secs_f64() * 1e3;
    for _ in 0..3 {
        push(app);
        term.draw(|f| vu::ui::draw(f, app)).unwrap();
    }
    let t = Instant::now();
    for _ in 0..iters {
        push(app);
        term.draw(|f| vu::ui::draw(f, app)).unwrap();
    }
    (t.elapsed().as_secs_f64() * 1e3 / iters as f64, cold)
}

fn bench_draw(minutes: f64, fft: usize, hop: usize) {
    let mut app = build_app(minutes, fft, hop);
    println!(
        "draw: {} frames of history (fft {}, hop {})",
        app.history.len(),
        app.analysis.fft_size,
        app.analysis.hop
    );
    for (w, h) in [(300u16, 100u16), (160, 48)] {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        for span in [1.0f64, 10.0, 300.0] {
            for scrolled in [false, true] {
                app.view.span_secs = span;
                app.nav(Nav::End);
                if scrolled {
                    app.nav(Nav::Home);
                }
                let (ms, cold) = time_draw(&mut term, &mut app, 50, !scrolled);
                println!(
                    "  {w:>3}x{h:<3} span {span:>3}s {:<8}: {ms:>6.3} ms/draw  (first {cold:.3} ms)",
                    if scrolled { "home" } else { "live" }
                );
            }
        }
    }
    let mut term = Terminal::new(TestBackend::new(300, 100)).unwrap();
    let ms = time_draw_with(&mut term, 50, |_| {});
    println!("  300x100 empty draw (terminal overhead): {ms:.3} ms");
    app.view.span_secs = vu::app::DEFAULT_SPAN;
    app.nav(Nav::End);
    for (i, name) in ["meter", "eq", "spectrogram", "timeline"]
        .iter()
        .enumerate()
    {
        let saved = app.display.panels;
        app.display.panels = [false; vu::cli::PANEL_COUNT];
        app.display.panels[i] = true;
        let (ms, _) = time_draw(&mut term, &mut app, 50, true);
        app.display.panels = saved;
        println!("  300x100 span 10s {name:<12} only: {ms:>6.3} ms/draw");
    }
}

fn time_draw_with(
    term: &mut Terminal<TestBackend>,
    iters: usize,
    mut f: impl FnMut(&mut ratatui::Frame),
) -> f64 {
    let t = Instant::now();
    for _ in 0..iters {
        term.draw(&mut f).unwrap();
    }
    t.elapsed().as_secs_f64() * 1e3 / iters as f64
}

fn main() {
    let filter = std::env::args().skip(1).find(|a| !a.starts_with('-'));
    let want = |n: &str| filter.as_deref().is_none_or(|f| n.contains(f));
    if want("analysis") {
        bench_analysis();
    }
    if want("analysis") {
        bench_fft();
    }
    if want("draw") {
        bench_draw(10.0, 2048, 512);
        bench_draw(3.0, 8192, 256);
    }
}
