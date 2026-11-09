//! Keyboard and menu input. Keys are translated into the terminal app's key
//! events so both front ends share one set of bindings. The menus are
//! described once ([`menus`]) as commands on the shared state, and shown
//! either as the native macOS menu bar or as a menu bar in the window.

use crate::app::{Action, App, MeterMode};
use crate::cli::{Panel, LUFS_TARGETS};
use crate::dsp::scale::FreqScale;
use crate::ui::colormap::ColorMap;
use eframe::egui::{self, Button, Event, Key, Modifiers, Ui};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};

/// Interval used when demo cycling is switched on from the menu.
const CYCLE_SECS: f64 = 3.0;
/// The VU references offered in the menu, dBFS.
const VU_REFS: [f32; 4] = [-20.0, -18.0, -14.0, -12.0];

/// Key presses this frame, as terminal key events.
pub fn key_events(ctx: &egui::Context) -> Vec<KeyEvent> {
    ctx.input(|i| i.events.iter().filter_map(translate).collect())
}

fn translate(e: &Event) -> Option<KeyEvent> {
    match e {
        // Printable keys arrive as text, already shifted for the layout ('R', '?', '{').
        Event::Text(t) => {
            let c = t.chars().next()?;
            Some(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
        }
        Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => {
            let code = match key {
                Key::ArrowLeft => KeyCode::Left,
                Key::ArrowRight => KeyCode::Right,
                Key::ArrowUp => KeyCode::Up,
                Key::ArrowDown => KeyCode::Down,
                Key::PageUp => KeyCode::PageUp,
                Key::PageDown => KeyCode::PageDown,
                Key::Home => KeyCode::Home,
                Key::End => KeyCode::End,
                Key::Escape => KeyCode::Esc,
                Key::F1 => KeyCode::F(1),
                Key::C if modifiers.ctrl => KeyCode::Char('c'),
                _ => return None,
            };
            Some(KeyEvent::new(code, mods(*modifiers)))
        }
        _ => None,
    }
}

fn mods(m: Modifiers) -> KeyModifiers {
    let mut k = KeyModifiers::NONE;
    if m.shift {
        k |= KeyModifiers::SHIFT;
    }
    if m.ctrl {
        k |= KeyModifiers::CONTROL;
    }
    k
}

/// What a menu item does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmd {
    /// The same as pressing the key.
    Key(KeyCode),
    Mode(MeterMode),
    ColorMap(ColorMap),
    FreqScale(FreqScale),
    /// Index into `LUFS_TARGETS`.
    Target(usize),
    VuRef(f32),
    /// Index into `MenuState::list`.
    Source(usize),
    Aes17,
    Cycle,
    DockMeter,
    Help,
    Quit,
}

/// An input the menu can switch to.
#[derive(Clone, Debug, PartialEq)]
pub enum Choice {
    Default,
    Device(String),
    /// A `--generate` spec.
    Signal(String),
}

/// Test signals offered in the menu: label and `--generate` spec.
const SIGNALS: [(&str, &str); 4] = [
    ("1 kHz Sine, −18 dBFS", "sine:1000:-18"),
    ("Pink Noise, −20 dBFS", "pink:-20"),
    ("Sweep 20 Hz – 20 kHz", "sweep:20:20000:10:-20"),
    ("Silence", "silence"),
];

/// Window state the menus show beyond the app's: the inputs to choose from,
/// the one in use (none for a file), and the live Dock icon.
pub struct MenuState {
    pub list: Vec<Choice>,
    pub current: Option<Choice>,
    default_name: Option<String>,
    pub dock_live: bool,
}

impl MenuState {
    pub fn new(current: Option<Choice>, dock_live: bool) -> Self {
        let mut s = MenuState {
            list: Vec::new(),
            current,
            default_name: None,
            dock_live,
        };
        s.set_devices(Vec::new(), None);
        s
    }

    /// The input matching a command line: a device, a test signal, the
    /// default input, or none (a file).
    pub fn from_cli(cli: &crate::cli::Cli) -> Option<Choice> {
        match (&cli.file, &cli.generate, &cli.device) {
            (Some(_), _, _) => None,
            (_, Some(g), _) => Some(Choice::Signal(g.clone())),
            (_, _, Some(d)) => Some(Choice::Device(d.clone())),
            _ => Some(Choice::Default),
        }
    }

    /// Replace the device list. Returns whether it changed.
    pub fn set_devices(&mut self, devices: Vec<String>, default: Option<String>) -> bool {
        let mut list = vec![Choice::Default];
        list.extend(devices.into_iter().map(Choice::Device));
        list.extend(
            SIGNALS
                .iter()
                .map(|(_, spec)| Choice::Signal(spec.to_string())),
        );
        let changed = list != self.list || default != self.default_name;
        self.list = list;
        self.default_name = default;
        changed
    }

    pub fn label(&self, c: &Choice) -> String {
        match c {
            Choice::Default => match &self.default_name {
                Some(n) => format!("Default Input ({n})"),
                None => "Default Input".into(),
            },
            Choice::Device(n) => n.clone(),
            Choice::Signal(spec) => SIGNALS.iter().find(|(_, s)| s == spec).map_or_else(
                || format!("Test Signal {spec}"),
                |(l, _)| format!("Test: {l}"),
            ),
        }
    }
}

/// What the front end must do after a command, beyond changing the state.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Quit,
    Open(Choice),
    /// Turn the live Dock icon on or off.
    ToggleDockMeter,
}

/// One entry of a menu.
pub enum Entry {
    /// An action, with the key that does the same.
    Item(String, Cmd, &'static str),
    /// A setting shown with a check mark while on (see [`is_on`]).
    Check(String, Cmd, &'static str),
    Sub(String, Vec<Entry>),
    Separator,
}

fn item(label: &str, cmd: Cmd, key: &'static str) -> Entry {
    Entry::Item(label.into(), cmd, key)
}

fn check(label: &str, cmd: Cmd, key: &'static str) -> Entry {
    Entry::Check(label.into(), cmd, key)
}

fn key(c: char) -> Cmd {
    Cmd::Key(KeyCode::Char(c))
}

/// The menus, in menu-bar order. The native macOS bar puts Quit in the
/// application menu and closes the window from File instead.
pub fn menus(native: bool, sources: &MenuState) -> Vec<(&'static str, Vec<Entry>)> {
    use Entry::Separator as Sep;
    let mut file = vec![item("Start Recording", key('R'), "R"), Sep];
    if !native {
        file.push(item("Quit", Cmd::Quit, "q"));
    }
    let panels = [
        ("Levels", '1'),
        ("Equalizer", '2'),
        ("Spectrogram", '3'),
        ("Timeline", '4'),
        ("Piano", '5'),
    ];
    let mut view: Vec<Entry> = panels
        .iter()
        .map(|&(name, k)| {
            check(
                name,
                key(k),
                ["1", "2", "3", "4", "5"][k as usize - '1' as usize],
            )
        })
        .collect();
    view.extend([
        Sep,
        Entry::Sub(
            "Colour Map".into(),
            [
                ColorMap::Magma,
                ColorMap::Inferno,
                ColorMap::Viridis,
                ColorMap::Fire,
                ColorMap::Gray,
            ]
            .into_iter()
            .map(|m| check(&format!("{m:?}"), Cmd::ColorMap(m), ""))
            .collect(),
        ),
        Entry::Sub(
            "Frequency Scale".into(),
            [FreqScale::Log, FreqScale::Linear, FreqScale::Mel]
                .into_iter()
                .map(|s| check(&format!("{s:?}"), Cmd::FreqScale(s), ""))
                .collect(),
        ),
        Sep,
        item("Zoom In", key('+'), "+"),
        item("Zoom Out", key('-'), "-"),
        Sep,
        item("Lower Floor", key('['), "["),
        item("Raise Floor", key(']'), "]"),
        item("Lower Ceiling", key('{'), "{"),
        item("Raise Ceiling", key('}'), "}"),
    ]);
    // The Dock exists only on macOS, where the menu bar is native.
    if native {
        view.extend([Sep, check("Live Dock Icon", Cmd::DockMeter, "")]);
    }
    // Devices, then test signals, in one list.
    let mut choose = Vec::new();
    for (i, c) in sources.list.iter().enumerate() {
        let first_signal = matches!(c, Choice::Signal(_))
            && !matches!(sources.list.get(i.wrapping_sub(1)), Some(Choice::Signal(_)));
        if first_signal || i == 1 {
            choose.push(Sep);
        }
        choose.push(check(&sources.label(c), Cmd::Source(i), ""));
    }
    let input = vec![
        Entry::Sub("Source".into(), choose),
        Sep,
        check("Pause Input", key('p'), "p"),
        check("Hold Display", key(' '), "space"),
        Sep,
        item("Back to Live", Cmd::Key(KeyCode::End), "End"),
        item("Oldest Data", Cmd::Key(KeyCode::Home), "Home"),
    ];
    let mut meter: Vec<Entry> = [MeterMode::Rms, MeterMode::Vu, MeterMode::Lufs]
        .into_iter()
        .map(|m| check(m.name(), Cmd::Mode(m), "v"))
        .collect();
    meter.extend([
        check("Cycle Modes (Demo)", Cmd::Cycle, ""),
        Sep,
        Entry::Sub(
            "Loudness Target".into(),
            LUFS_TARGETS
                .iter()
                .enumerate()
                .map(|(i, (name, lufs))| check(&format!("{name}  {lufs} LUFS"), Cmd::Target(i), ""))
                .collect(),
        ),
        Entry::Sub(
            "VU Reference".into(),
            VU_REFS
                .into_iter()
                .map(|r| check(&format!("0 VU = {r} dBFS"), Cmd::VuRef(r), ""))
                .collect(),
        ),
        check("RMS as AES17 (+3 dB)", Cmd::Aes17, ""),
        Sep,
        item("Reset Loudness", key('r'), "r"),
        item("Reset Max and Clips", key('x'), "x"),
    ]);
    let help = vec![item("Keys and Readouts", Cmd::Help, "?")];
    vec![
        ("File", file),
        ("View", view),
        ("Input", input),
        ("Meter", meter),
        ("Help", help),
    ]
}

/// Carry out a menu command, returning what the front end must do next.
pub fn apply(app: &mut App, cmd: Cmd, sources: &MenuState) -> Option<Request> {
    let d = &mut app.display;
    match cmd {
        Cmd::Key(code) => {
            let quit = app.handle_key(KeyEvent::new(code, KeyModifiers::NONE)) == Action::Quit;
            return quit.then_some(Request::Quit);
        }
        Cmd::Source(i) => return sources.list.get(i).cloned().map(Request::Open),
        Cmd::Mode(m) => {
            d.mode = m;
            app.flash(format!("meter mode: {}", m.name()));
        }
        Cmd::ColorMap(m) => d.colormap = m,
        Cmd::FreqScale(s) => d.freq_scale = s,
        Cmd::Target(i) => {
            let (name, lufs) = LUFS_TARGETS[i];
            d.lufs_target = lufs;
            app.flash(format!("loudness target: {lufs} LUFS ({name})"));
        }
        Cmd::VuRef(r) => d.vu_ref = r,
        Cmd::Aes17 => d.rms_offset = if d.rms_offset == 0.0 { 3.01 } else { 0.0 },
        Cmd::Cycle => {
            app.cycle = match app.cycle {
                Some(_) => None,
                None => Some((Duration::from_secs_f64(CYCLE_SECS), Instant::now())),
            }
        }
        Cmd::Help => app.show_help = !app.show_help,
        Cmd::DockMeter => return Some(Request::ToggleDockMeter),
        Cmd::Quit => return Some(Request::Quit),
    }
    None
}

/// Whether a setting is on, for its check mark.
pub fn is_on(app: &App, cmd: Cmd, sources: &MenuState) -> bool {
    let d = &app.display;
    match cmd {
        Cmd::Source(i) => {
            sources.current.is_some() && sources.list.get(i) == sources.current.as_ref()
        }
        Cmd::Key(KeyCode::Char(c @ '1'..='5')) => {
            let p = [
                Panel::Meter,
                Panel::Eq,
                Panel::Spectrogram,
                Panel::Timeline,
                Panel::Piano,
            ];
            d.shown(p[c as usize - '1' as usize])
        }
        Cmd::Key(KeyCode::Char('p')) => app.paused(),
        Cmd::Key(KeyCode::Char(' ')) => !app.view.follow,
        Cmd::Mode(m) => d.mode == m,
        Cmd::ColorMap(m) => d.colormap == m,
        Cmd::FreqScale(s) => d.freq_scale == s,
        Cmd::Target(i) => d.lufs_target == LUFS_TARGETS[i].1,
        Cmd::VuRef(r) => d.vu_ref == r,
        Cmd::Aes17 => d.rms_offset != 0.0,
        Cmd::Cycle => app.cycle.is_some(),
        Cmd::DockMeter => sources.dock_live,
        _ => false,
    }
}

/// An item's label in the current state (recording starts or stops).
pub fn label(app: &App, cmd: Cmd, base: &str) -> String {
    match cmd {
        Cmd::Key(KeyCode::Char('R')) if app.recorder.is_some() => "Stop Recording".into(),
        _ => base.into(),
    }
}

/// The menu bar inside the window, for systems without a native one.
pub fn menu_bar(ui: &mut Ui, app: &mut App, sources: &MenuState) -> Option<Request> {
    let mut req = None;
    egui::menu::bar(ui, |ui| {
        for (title, entries) in menus(false, sources) {
            ui.menu_button(title, |ui| {
                if let Some(r) = show(ui, app, &entries, sources) {
                    req = Some(r);
                }
            });
        }
    });
    req
}

fn show(ui: &mut Ui, app: &mut App, entries: &[Entry], sources: &MenuState) -> Option<Request> {
    let mut req = None;
    for e in entries {
        let (text, cmd, key, on) = match e {
            Entry::Separator => {
                ui.separator();
                continue;
            }
            Entry::Sub(title, sub) => {
                ui.menu_button(title, |ui| {
                    if let Some(r) = show(ui, app, sub, sources) {
                        req = Some(r);
                    }
                });
                continue;
            }
            Entry::Item(t, cmd, key) => (label(app, *cmd, t), *cmd, *key, false),
            Entry::Check(t, cmd, key) => (t.clone(), *cmd, *key, is_on(app, *cmd, sources)),
        };
        let mark = if matches!(e, Entry::Check(..)) {
            if on {
                "✔ "
            } else {
                "    "
            }
        } else {
            ""
        };
        let b = Button::new(format!("{mark}{text}")).shortcut_text(key);
        if ui.add(b).clicked() {
            if let Some(r) = apply(app, cmd, sources) {
                req = Some(r);
            }
            ui.close_menu();
        }
    }
    req
}
