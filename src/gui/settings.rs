//! Window settings kept between sessions: what is shown and how. The input
//! is not kept: each run starts on the default input unless the command line
//! names one. The settings are applied to the command line before the session opens,
//! for every option not given explicitly on it, so explicit options win for
//! that run. (eframe keeps the window's position and size itself.)
//!
//! Stored as `key=value` lines in the app's data directory (on macOS
//! `~/Library/Application Support/vu/settings.txt`); unknown or invalid
//! lines are ignored.

use super::input::MenuState;
use crate::app::{App, MeterMode};
use crate::cli::{Cli, Panel};
use crate::dsp::scale::FreqScale;
use crate::ui::colormap::ColorMap;
use clap::ValueEnum;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

/// The app id eframe stores the window geometry under, and our directory.
pub const APP_ID: &str = "vu";

pub type Map = BTreeMap<String, String>;

const PANELS: [Panel; 5] = [
    Panel::Meter,
    Panel::Eq,
    Panel::Spectrogram,
    Panel::Timeline,
    Panel::Piano,
];

fn path() -> Option<PathBuf> {
    eframe::storage_dir(APP_ID).map(|d| d.join("settings.txt"))
}

pub fn load() -> Map {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| parse(&s))
        .unwrap_or_default()
}

pub fn parse(s: &str) -> Map {
    s.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

pub fn save(map: &Map) {
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let text: String = map.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    let _ = std::fs::write(p, text);
}

fn name<T: ValueEnum>(v: &T) -> String {
    v.to_possible_value()
        .map(|p| p.get_name().to_string())
        .unwrap_or_default()
}

/// The settings to keep, from the running window.
pub fn capture(app: &App, menu: &MenuState) -> Map {
    let d = &app.display;
    let mut m = Map::new();
    let mut put = |k: &str, v: String| {
        m.insert(k.to_string(), v);
    };
    let panels: Vec<String> = PANELS.iter().filter(|&&p| d.shown(p)).map(name).collect();
    put("panels", panels.join(","));
    put("mode", d.mode.name().to_lowercase());
    put("colormap", name(&d.colormap));
    put("freq_scale", name(&d.freq_scale));
    put("db_min", d.db_min.to_string());
    put("db_max", d.db_max.to_string());
    put("spec_floor", d.spec_floor.to_string());
    put("lufs_target", d.lufs_target.to_string());
    put("vu_ref", d.vu_ref.to_string());
    put("rms_aes17", (d.rms_offset != 0.0).to_string());
    put("span", app.view.span_secs.to_string());
    put("dock_meter", menu.dock_live.to_string());
    m
}

/// Apply saved settings to `cli` for the options not in `explicit` (the
/// argument ids given on the command line). Settings that leave the command
/// line invalid are dropped.
pub fn apply(cli: &mut Cli, saved: &Map, explicit: &HashSet<String>) {
    let original = cli.clone();
    let given = |ids: &[&str]| ids.iter().any(|id| explicit.contains(*id));
    let num = |k: &str| saved.get(k).and_then(|v| v.parse::<f32>().ok());
    let flag = |k: &str| saved.get(k).and_then(|v| v.parse::<bool>().ok());

    if let (false, Some(list)) = (given(&["panels", "piano"]), saved.get("panels")) {
        let panels: Vec<Panel> = list
            .split(',')
            .filter_map(|n| Panel::from_str(n, true).ok())
            .collect();
        if !panels.is_empty() {
            cli.panels = panels;
            cli.piano = false;
        }
    }
    if let (false, Some(mode)) = (given(&["vu", "lufs"]), saved.get("mode")) {
        let m = [MeterMode::Rms, MeterMode::Vu, MeterMode::Lufs]
            .into_iter()
            .find(|m| m.name().eq_ignore_ascii_case(mode));
        if let Some(m) = m {
            cli.vu = m == MeterMode::Vu;
            cli.lufs = m == MeterMode::Lufs;
        }
    }
    if !given(&["colormap"]) {
        if let Some(c) = saved
            .get("colormap")
            .and_then(|v| ColorMap::from_str(v, true).ok())
        {
            cli.colormap = c;
        }
    }
    if !given(&["freq_scale"]) {
        if let Some(s) = saved
            .get("freq_scale")
            .and_then(|v| FreqScale::from_str(v, true).ok())
        {
            cli.freq_scale = s;
        }
    }
    for (key, field) in [
        ("db_min", &mut cli.db_min),
        ("db_max", &mut cli.db_max),
        ("spec_floor", &mut cli.spec_floor),
        ("lufs_target", &mut cli.lufs_target),
        ("vu_ref", &mut cli.vu_ref),
    ] {
        if let (false, Some(v)) = (given(&[key]), num(key)) {
            *field = v;
        }
    }
    if let (false, Some(v)) = (given(&["rms_aes17"]), flag("rms_aes17")) {
        cli.rms_aes17 = v;
    }
    if let (false, Some(v)) = (given(&["span"]), num("span")) {
        cli.span = v as f64;
    }
    if let (false, Some(v)) = (given(&["no_dock_meter"]), flag("dock_meter")) {
        cli.no_dock_meter = !v;
    }
    if cli.validate().is_err() {
        *cli = original;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn cli(args: &[&str]) -> (Cli, HashSet<String>) {
        let mut argv = vec!["vu"];
        argv.extend_from_slice(args);
        let m = <Cli as clap::CommandFactory>::command().get_matches_from(&argv);
        (Cli::parse_from(&argv), super::super::explicit_args(&m))
    }

    fn saved() -> Map {
        parse(
            "panels=meter,piano\nmode=lufs\ncolormap=viridis\nfreq_scale=mel\ndb_min=-80\n\
             lufs_target=-16\nrms_aes17=true\nspan=30\ndock_meter=false\nsource=signal:pink\n\
             junk line\nunknown=1\n",
        )
    }

    #[test]
    fn saved_settings_fill_the_command_line() {
        let (mut c, ex) = cli(&[]);
        apply(&mut c, &saved(), &ex);
        assert_eq!(c.panels, vec![Panel::Meter, Panel::Piano]);
        assert!(c.lufs && !c.vu);
        assert_eq!(
            (c.colormap, c.freq_scale),
            (ColorMap::Viridis, FreqScale::Mel)
        );
        assert_eq!((c.db_min, c.lufs_target, c.span), (-80.0, -16.0, 30.0));
        assert!(c.rms_aes17 && c.no_dock_meter);
        // An input saved by an older version is not reopened.
        assert_eq!((c.generate, c.device, c.file), (None, None, None));
    }

    #[test]
    fn explicit_options_win() {
        let (mut c, ex) = cli(&["--vu", "--colormap", "fire", "--db-min", "-60"]);
        apply(&mut c, &saved(), &ex);
        assert!(c.vu && !c.lufs);
        assert_eq!((c.colormap, c.db_min), (ColorMap::Fire, -60.0));
        // Others still come from the saved settings.
        assert_eq!(c.freq_scale, FreqScale::Mel);
    }

    #[test]
    fn invalid_settings_are_dropped() {
        let (mut c, ex) = cli(&[]);
        let before = c.clone();
        apply(&mut c, &parse("db_min=10\ndb_max=0\n"), &ex);
        assert_eq!((c.db_min, c.db_max), (before.db_min, before.db_max));
    }
}
