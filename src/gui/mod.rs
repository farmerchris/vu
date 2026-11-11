//! Window front end (`vu-meter --gui`, `vu-meter-gui`), drawn with egui. It shares the
//! terminal UI's state, keys and analysis ([`App`], [`Pump`]); only drawing
//! and input translation are its own.

mod color;
#[cfg(target_os = "macos")]
mod dock_mac;
mod draw;
mod icon;
mod input;
#[cfg(target_os = "macos")]
mod menu_mac;
mod settings;

use crate::analyzer::Analyzer;
use crate::app::{Action, App};
use crate::cli::{Cli, Panel};
use crate::pump::{self, Pump};
use crate::source::{self, Source};
use crate::ui::colormap::ColorMode;
use anyhow::Result;
use clap::parser::ValueSource;
use eframe::egui::{self, Rect, Sense, Vec2};
use input::{Choice, Request};
use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// Gap between panels.
const GAP: f32 = 8.0;
/// Height of the piano panel.
const PIANO_H: f32 = 96.0;
/// Scroll distance (points) per four-column step of the cursor.
const SCROLL_STEP: f32 = 40.0;

/// How often the device list is checked for inputs coming and going.
const DEVICE_POLL: Duration = Duration::from_secs(2);

/// The source, its analysis and the app state over it: everything replaced
/// when the menu switches inputs.
struct Session {
    app: App,
    analyzer: Analyzer,
    source: Source,
    pump: Pump,
}

impl Session {
    fn open(cli: &Cli) -> Result<Session> {
        let source = source::open(cli, cli.fast, cli.looping)?;
        let analyzer = Analyzer::new(pump::analyzer_config(cli, &source.info)?);
        let mut app = App::new(
            cli,
            source.info.clone(),
            &analyzer,
            source.control.clone(),
            ColorMode::TrueColor,
        );
        if cli.record.is_some() {
            app.start_recording()?;
        }
        if let Some(path) = &cli.log {
            app.start_log(path, cli.log_interval)?;
        }
        Ok(Session {
            pump: Pump::new(cli, &source.info),
            app,
            analyzer,
            source,
        })
    }
}

/// How long [`detach`] waits for the window to fail before returning.
const DETACH_GRACE: Duration = Duration::from_millis(1500);

/// Start this command again with `--foreground`, in the background and its
/// own process group (so the terminal's Ctrl-C and closing it leave the
/// window alone), and return. Its output goes to a log in the temporary
/// directory; if it fails within a moment (a device that will not open, say)
/// the error is shown here and returned.
pub fn detach() -> Result<()> {
    #[cfg(unix)]
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let log_path = std::env::temp_dir().join(format!("vu-meter-{}.log", std::process::id()));
    let log = std::fs::File::create(&log_path)?;
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args(std::env::args_os().skip(1))
        .arg("--foreground")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn()?;
    let start = Instant::now();
    while start.elapsed() < DETACH_GRACE {
        if let Some(status) = child.try_wait()? {
            let out = std::fs::read_to_string(&log_path).unwrap_or_default();
            let _ = std::fs::remove_file(&log_path);
            if status.success() {
                print!("{out}");
                return Ok(());
            }
            eprint!("{out}");
            std::process::exit(status.code().unwrap_or(1));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// The window's size on first run and after Reset to Defaults.
const WINDOW_SIZE: [f32; 2] = [1280.0, 800.0];

/// Ids of the arguments given on the command line (not defaults), which
/// take precedence over saved settings.
pub fn explicit_args(m: &clap::ArgMatches) -> HashSet<String> {
    m.ids()
        .filter(|id| m.value_source(id.as_str()) == Some(ValueSource::CommandLine))
        .map(|id| id.to_string())
        .collect()
}

/// Open the source given on the command line (the default input if none)
/// and show it in a window until it is closed. `explicit` names the
/// arguments given on the command line; saved settings fill in the rest.
pub fn run(mut cli: Cli, explicit: HashSet<String>) -> Result<()> {
    settings::apply(&mut cli, &settings::load(), &explicit);
    let session = Session::open(&cli)?;
    let title = format!("VU Meter — {}", session.source.info.name);
    let mut sources = input::MenuState::new(input::MenuState::from_cli(&cli), !cli.no_dock_meter);
    sources.dock_style = cli.dock_style;
    let gui = Gui {
        frame_dt: Duration::from_secs_f64(1.0 / cli.fps as f64),
        title: title.clone(),
        sources,
        devices: watch_devices(),
        cli,
        app: session.app,
        analyzer: session.analyzer,
        source: session.source,
        pump: session.pump,
        spec: draw::SpecView::default(),
        scroll: 0.0,
        #[cfg(target_os = "macos")]
        menu: None,
        menu_stale: true,
        #[cfg(target_os = "macos")]
        dock: dock_mac::DockMeter::new(),
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_icon(std::sync::Arc::new(icon::icon()))
            .with_inner_size(WINDOW_SIZE)
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };
    // Reporting happens when the app is dropped (see `Drop for Gui`).
    // The window's position and size are kept by eframe under the app id.
    eframe::run_native(
        settings::APP_ID,
        options,
        Box::new(move |_cc| Ok(Box::new(gui))),
    )
    .map_err(|e| anyhow::anyhow!("window: {e}"))
}

/// Lists the input devices now and whenever they change, from a thread, as
/// listing can be slow.
fn watch_devices() -> Receiver<(Vec<String>, Option<String>)> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut last = None;
        loop {
            let now = source::input_device_names();
            if last.as_ref() != Some(&now) {
                if tx.send(now.clone()).is_err() {
                    return;
                }
                last = Some(now);
            }
            std::thread::sleep(DEVICE_POLL);
        }
    });
    rx
}

struct Gui {
    /// The command line, as the template for switching inputs.
    cli: Cli,
    app: App,
    analyzer: Analyzer,
    source: Source,
    pump: Pump,
    sources: input::MenuState,
    devices: Receiver<(Vec<String>, Option<String>)>,
    frame_dt: Duration,
    /// The window title shown, which names the input.
    title: String,
    spec: draw::SpecView,
    /// Unused scroll distance, toward the next cursor step.
    scroll: f32,
    #[cfg(target_os = "macos")]
    menu: Option<menu_mac::NativeMenu>,
    /// The native menu needs building (again, when the inputs change).
    menu_stale: bool,
    #[cfg(target_os = "macos")]
    dock: dock_mac::DockMeter,
}

impl Drop for Gui {
    fn drop(&mut self) {
        if let Err(e) = pump::finish(&mut self.app, &self.source) {
            eprintln!("vu-meter: {e:#}");
        }
    }
}

/// Where each panel goes this frame.
struct Layout {
    meter: Option<Rect>,
    eq: Option<Rect>,
    scope: Option<Rect>,
    piano: Option<Rect>,
    spec: Option<Rect>,
    timeline: Option<Rect>,
}

impl Layout {
    /// Mirrors the terminal layout: levels and equalizer side by side on top,
    /// then the piano, the spectrogram and the timeline (4 : 2).
    fn new(area: Rect, app: &App, char_w: f32) -> Self {
        let d = &app.display;
        let (meter, eq) = (d.shown(Panel::Meter), d.shown(Panel::Eq));
        let scope = d.shown(Panel::Scope);
        let (spec, tl, piano) = (
            d.shown(Panel::Spectrogram),
            d.shown(Panel::Timeline),
            d.shown(Panel::Piano),
        );
        let top = meter || eq || scope;
        let charts = spec || tl;
        let gaps = (top as u8 + piano as u8 + spec as u8 + tl as u8).saturating_sub(1) as f32 * GAP;
        let piano_h = if piano { PIANO_H } else { 0.0 };
        let free = (area.height() - gaps - piano_h).max(0.0);
        let top_h = match (top, charts) {
            (false, _) => 0.0,
            (true, false) => free,
            (true, true) => (free * 0.42).max(220.0).min(free - 120.0).max(0.0),
        };
        let chart_h = free - top_h;
        let (spec_h, tl_h) = match (spec, tl) {
            (true, true) => (chart_h * 2.0 / 3.0, chart_h / 3.0),
            (true, false) => (chart_h, 0.0),
            _ => (0.0, chart_h),
        };
        let mut y = area.top();
        let mut take = |on: bool, h: f32| {
            on.then(|| {
                let r = Rect::from_min_size(egui::pos2(area.left(), y), Vec2::new(area.width(), h));
                y += h + GAP;
                r
            })
        };
        let top_r = take(top, top_h);
        let piano = take(piano, piano_h);
        let spec = take(spec, spec_h);
        let timeline = take(tl, tl_h);
        // The scope takes the right of the top row, about square (all of
        // the row when alone).
        let (top_r, scope) = match (top_r, scope) {
            (Some(r), true) if meter || eq => {
                let sw = r.height().min(r.width() * 0.3).max(160.0).min(r.width());
                let (a, b) = r.split_left_right_at_x(r.right() - sw);
                (
                    Some(Rect::from_min_max(a.min, a.max - Vec2::new(GAP, 0.0))),
                    Some(b),
                )
            }
            (Some(r), true) => (None, Some(r)),
            (r, _) => (r, None),
        };
        let (meter, eq) = match (top_r, meter, eq) {
            (Some(r), true, true) => {
                let mw = draw::meter_width(app.analysis.channels, char_w).min(r.width() * 0.6);
                let (a, b) = r.split_left_right_at_x(r.left() + mw);
                (
                    Some(a),
                    Some(Rect::from_min_max(b.min + Vec2::new(GAP, 0.0), b.max)),
                )
            }
            (Some(r), true, false) => (Some(r), None),
            (Some(r), false, true) => (None, Some(r)),
            _ => (None, None),
        };
        Layout {
            meter,
            eq,
            scope,
            piano,
            spec,
            timeline,
        }
    }
}

/// Draw every panel of `layout`.
fn paint(p: &egui::Painter, layout: &Layout, app: &mut App, spec: &mut draw::SpecView) {
    if let Some(r) = layout.meter {
        draw::meter(p, r, app);
    }
    if let Some(r) = layout.eq {
        draw::equalizer(p, r, app);
    }
    if let Some(r) = layout.scope {
        draw::scope(p, r, app);
    }
    if let Some(r) = layout.piano {
        draw::piano_panel(p, r, app);
    }
    if let Some(r) = layout.spec {
        draw::spectrogram(p, r, app, spec, layout.timeline.is_none());
    }
    if let Some(r) = layout.timeline {
        draw::timeline(p, r, app);
    }
}

impl eframe::App for Gui {
    /// Called by eframe now and then and on exit.
    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        settings::save(&settings::capture(&self.app, &self.sources));
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let t0 = Instant::now();
        let budget = t0 + self.frame_dt.mul_f32(0.7);
        self.pump
            .run(&mut self.app, &mut self.analyzer, &mut self.source, budget);
        self.app.cycle_tick(t0);
        // The input's name changes on switching inputs, and with the Mic Mode.
        let title = format!("VU Meter — {}", self.source.info.name);
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        if std::mem::take(&mut self.app.restart_input) {
            if let Some(choice) = self.sources.current.clone() {
                let why = self.app.input_warning.take().unwrap_or_default();
                self.switch(choice);
                self.app.flash(format!("{why}; input restarted"));
            }
        }
        let mut req = None;
        for k in input::key_events(ctx) {
            if self.app.handle_key(k) == Action::Quit {
                req = Some(Request::Quit);
            }
        }
        // An audio file dropped on the window is metered.
        let dropped = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()));
        if let Some(path) = dropped {
            req = Some(Request::Open(Choice::File(path)));
        }
        if let Some((names, default)) = self.devices.try_iter().last() {
            self.menu_stale |= self.sources.set_devices(names, default);
        }
        if self
            .app
            .message
            .as_ref()
            .is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(5))
        {
            self.app.message = None;
        }

        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = color::BG;
        ctx.set_visuals(visuals);

        if let Some(r) = self.menus(ctx) {
            req = Some(r);
        }
        egui::TopBottomPanel::top("status").show(ctx, |ui| draw::status(ui, &self.app));
        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(color::BG).inner_margin(GAP))
            .show(ctx, |ui| self.body(ui));
        draw::help(ctx, &mut self.app);

        match req {
            Some(Request::Quit) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Some(Request::Open(choice)) => self.switch(choice),
            Some(Request::ToggleDockMeter) => self.sources.dock_live = !self.sources.dock_live,
            Some(Request::Reset) => self.reset(ctx),
            Some(Request::SetDockStyle(s)) => self.sources.dock_style = s,
            None => {}
        }
        #[cfg(target_os = "macos")]
        self.dock
            .update(&self.app, self.sources.dock_live, self.sources.dock_style);
        self.app.fps.tick(t0, t0.elapsed());
        ctx.request_repaint_after(self.frame_dt);
    }
}

impl Gui {
    /// The native menu bar on macOS, a menu bar in the window elsewhere.
    fn menus(&mut self, ctx: &egui::Context) -> Option<Request> {
        #[cfg(target_os = "macos")]
        {
            // Built once the application is running, and again when the
            // inputs change; a failure falls back to the in-window bar.
            if std::mem::take(&mut self.menu_stale) {
                match menu_mac::NativeMenu::install(&self.sources) {
                    Ok(m) => self.menu = Some(m),
                    Err(e) => self.app.flash(format!("menu bar: {e}")),
                }
            }
            if let Some(m) = &mut self.menu {
                return m.poll(&mut self.app, &self.sources);
            }
        }
        let mut req = None;
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            req = input::menu_bar(ui, &mut self.app, &self.sources);
        });
        req
    }

    /// Put the display, zoom, Dock icon and window size back to the built-in
    /// defaults (not the command line's), and save them. The input and any
    /// recording carry on.
    fn reset(&mut self, ctx: &egui::Context) {
        use clap::Parser;
        let defaults = Cli::parse_from(["vu"]);
        let d = &self.app.display;
        self.app.display = crate::app::Display {
            clip_frames: d.clip_frames,
            ..crate::app::Display::new(&defaults, d.color_mode, self.app.view.dt)
        };
        self.app.view.span_secs = defaults.span;
        self.app.cycle = None;
        self.sources.dock_live = !defaults.no_dock_meter;
        self.sources.dock_style = defaults.dock_style;
        self.spec = draw::SpecView::default();
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(WINDOW_SIZE.into()));
        settings::save(&settings::capture(&self.app, &self.sources));
        self.app.flash("settings reset to defaults");
    }

    /// Switch to another input, starting a fresh session with the same
    /// display settings. On failure the current input carries on.
    fn switch(&mut self, choice: Choice) {
        let mut cli = self.cli.clone();
        (cli.file, cli.generate, cli.device, cli.record) = (None, None, None, None);
        // The log, if any, carries on in the new session rather than
        // starting again.
        cli.files.clear();
        cli.log = None;
        match &choice {
            Choice::Default => {}
            Choice::Device(name) => cli.device = Some(name.clone()),
            Choice::Signal(spec) => cli.generate = Some(spec.clone()),
            Choice::File(path) => cli.file = Some(path.clone()),
        }
        let label = self.sources.label(&choice);
        let mut s = match Session::open(&cli) {
            Ok(s) => s,
            Err(e) => {
                self.app.flash(format!("could not open {label}: {e:#}"));
                return;
            }
        };
        s.app.keep_view(&self.app);
        s.app.flash(format!("input: {label}"));
        s.app.log = self.app.log.take();
        if let Err(e) = pump::finish(&mut self.app, &self.source) {
            s.app.flash(format!("record error: {e:#}"));
        }
        // The old source stops when dropped here.
        self.app = s.app;
        self.analyzer = s.analyzer;
        self.source = s.source;
        self.pump = s.pump;
        self.spec = draw::SpecView::default();
        self.sources.current = Some(choice);
    }

    fn body(&mut self, ui: &mut egui::Ui) {
        let area = ui.max_rect();
        let char_w = ui.fonts(|f| f.glyph_width(&egui::FontId::monospace(13.0), '0'));
        let layout = Layout::new(area, &self.app, char_w);
        let time_on_spec = layout.timeline.is_none();

        // Establish the chart width first so the view and every panel agree
        // on the cursor: one column per point.
        let plot = layout
            .spec
            .map(|r| draw::chart_plot(r, time_on_spec))
            .or(layout.timeline.map(|r| draw::chart_plot(r, true)));
        self.app.view.width = plot.map_or(1, |p| p.width().round().max(1.0) as usize);
        self.app.sync_view();

        if let Some(plot) = plot {
            self.chart_mouse(ui, plot, &layout);
        }

        paint(ui.painter(), &layout, &mut self.app, &mut self.spec);
    }

    /// Click or drag on a chart places the cursor; the wheel scrolls.
    fn chart_mouse(&mut self, ui: &mut egui::Ui, plot: Rect, layout: &Layout) {
        let charts: Vec<Rect> = [layout.spec, layout.timeline]
            .into_iter()
            .flatten()
            .map(|r| Rect::from_x_y_ranges(plot.x_range(), r.y_range()))
            .collect();
        let Some(&first) = charts.first() else { return };
        let all = charts.iter().fold(first, |a, r| a.union(*r));
        let resp = ui.interact(all, ui.id().with("charts"), Sense::click_and_drag());
        let col_w = plot.width() / self.app.view.width.max(1) as f32;
        if resp.is_pointer_button_down_on() || resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                if charts.iter().any(|r| r.contains(pos)) {
                    let col = ((pos.x - plot.left()) / col_w).max(0.0) as usize;
                    self.app.nav(crate::app::Nav::Column(col));
                }
            }
        }
        if resp.hovered() {
            self.scroll += ui.input(|i| i.raw_scroll_delta.y + i.raw_scroll_delta.x);
            while self.scroll >= SCROLL_STEP {
                self.app.nav(crate::app::Nav::Left(4));
                self.scroll -= SCROLL_STEP;
            }
            while self.scroll <= -SCROLL_STEP {
                self.app.nav(crate::app::Nav::Right(4));
                self.scroll += SCROLL_STEP;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{Control, SourceInfo, SourceKind};
    use clap::Parser;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::sync::Arc;

    /// An app holding a second of audio: 440 Hz on the first channel and
    /// 550 Hz on the others, so two channels are unrelated.
    fn app(args: &[&str], channels: usize) -> App {
        let cli = Cli::parse_from(std::iter::once("vu").chain(args.iter().copied()));
        let info = SourceInfo {
            name: "test".into(),
            sample_rate: 48000,
            channels,
            total_frames: Some(48000),
            kind: SourceKind::File,
        };
        let mut analyzer = Analyzer::new(pump::analyzer_config(&cli, &info).unwrap());
        let mut app = App::new(
            &cli,
            info,
            &analyzer,
            Arc::new(Control::default()),
            ColorMode::TrueColor,
        );
        let data: Vec<f32> = (0..48000)
            .flat_map(|i| {
                let t = 2.0 * std::f32::consts::PI * i as f32 / 48000.0;
                (0..channels).map(move |c| 0.3 * (t * if c == 0 { 440.0 } else { 550.0 }).sin())
            })
            .collect();
        analyzer.push(&data, |f| app.push_frame(f));
        app
    }

    /// Lay out and draw `app` in a window of `size`, without a screen.
    /// Returns the layout and the number of shapes drawn.
    fn draw(app: &mut App, size: [f32; 2]) -> (Layout, usize) {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size.into())),
            ..Default::default()
        };
        let mut layout = None;
        let mut spec = draw::SpecView::default();
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let l = Layout::new(ui.max_rect(), app, 8.0);
                let plot = l
                    .spec
                    .map(|r| draw::chart_plot(r, l.timeline.is_none()))
                    .or(l.timeline.map(|r| draw::chart_plot(r, true)));
                app.view.width = plot.map_or(1, |p| p.width().round().max(1.0) as usize);
                app.sync_view();
                paint(ui.painter(), &l, app, &mut spec);
                layout = Some(l);
            });
            draw::status(
                &mut egui::Ui::new(
                    ctx.clone(),
                    "status".into(),
                    egui::UiBuilder::new().max_rect(ctx.screen_rect()),
                ),
                app,
            );
            draw::help(ctx, app);
        });
        (layout.unwrap(), out.shapes.len())
    }

    #[test]
    fn every_panel_draws_at_any_size() {
        for channels in [1, 2, 6] {
            for mode in [&[][..], &["--vu"], &["--lufs"]] {
                let mut args = vec!["-p", "all"];
                args.extend_from_slice(mode);
                let mut a = app(&args, channels);
                for size in [
                    [1280.0, 800.0],
                    [640.0, 420.0],
                    [200.0, 120.0],
                    [3000.0, 300.0],
                ] {
                    let (l, shapes) = draw(&mut a, size);
                    assert!(l.scope.is_some() && l.meter.is_some() && l.piano.is_some());
                    assert!(shapes > 50, "{channels} ch {mode:?} {size:?}: {shapes}");
                }
                // Scrolled back, with markers and the help open.
                for code in ['m', 'h', 'h', 'm', 'N', '?'] {
                    a.handle_key(KeyEvent::new(KeyCode::Char(code), KeyModifiers::NONE));
                }
                assert_eq!(a.markers.len(), 2);
                draw(&mut a, [1280.0, 800.0]);
            }
        }
        // Nothing to show yet.
        let mut empty = app(&["-p", "all"], 2);
        empty.history = crate::history::History::new(10);
        draw(&mut empty, [1280.0, 800.0]);
    }

    #[test]
    fn scope_sits_right_of_the_meters_and_draws_its_dots() {
        let mut a = app(&["--scope"], 2);
        let (l, with) = draw(&mut a, [1280.0, 800.0]);
        let (m, e, s) = (l.meter.unwrap(), l.eq.unwrap(), l.scope.unwrap());
        assert!(
            m.right() <= e.left() && e.right() <= s.left(),
            "{m:?} {e:?} {s:?}"
        );
        assert_eq!((m.top(), m.bottom()), (s.top(), s.bottom()));
        assert!(
            s.width() >= 160.0 && s.width() <= 1280.0 * 0.3 + 1.0,
            "{s:?}"
        );
        // The vectorscope's dots: the frames in the trail, a pair each.
        let (l, without) = draw(&mut app(&[], 2), [1280.0, 800.0]);
        assert!(l.scope.is_none());
        let dots = crate::ui::scope::TRAIL_FRAMES * crate::analyzer::SCOPE_POINTS;
        assert!(with >= without + dots, "{with} vs {without}");
        // Alone, it has the whole window.
        let (l, _) = draw(&mut app(&["-p", "scope"], 2), [1280.0, 800.0]);
        assert!(l.meter.is_none() && l.eq.is_none());
        assert!(l.scope.unwrap().width() > 1200.0);
    }
}
