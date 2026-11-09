//! A live level meter in the macOS Dock icon, as Activity Monitor does with
//! its graphs: the icon's LED columns mirror the meter bars of the current
//! mode (RMS, VU or LUFS: levels, peaks and colours), redrawn a few times a
//! second when they change.

use super::color::c32;
use super::icon::{self, Canvas, Column, SEGMENTS};
use crate::app::{App, MeterMode};
use crate::ui::colormap::{LU_M_COLOR, LU_OVER_COLOR, LU_S_COLOR};
use crate::ui::meter::{mode_scale, Scale};
use objc2::rc::Retained;
use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSImage};
use objc2_foundation::NSData;
use ratatui::style::Color;
use std::time::{Duration, Instant};

/// Dock icon size, pixels (the Dock shows at most 128 points at 2x).
const SIZE: usize = 256;
/// Fastest redraw rate; the Dock does not need the meters' frame rate.
const INTERVAL: Duration = Duration::from_millis(100);
/// Most columns drawn; more channels than this show the first ones.
const MAX_COLUMNS: usize = 8;

pub struct DockMeter {
    canvas: Canvas,
    /// Columns on the Dock icon now; `None` while it shows the still icon.
    shown: Option<Vec<Column>>,
    last: Instant,
}

impl DockMeter {
    pub fn new() -> Self {
        DockMeter {
            canvas: Canvas::new(SIZE),
            // The window icon (the still meter) is up at startup.
            shown: None,
            last: Instant::now() - INTERVAL,
        }
    }

    /// Redraw the Dock icon from the newest frame, if due and changed; when
    /// not `live`, put the still icon back.
    pub fn update(&mut self, app: &App, live: bool) {
        if !live {
            if self.shown.is_some() && set_icon(&icon::png(&icon::icon())) {
                self.shown = None;
            }
            return;
        }
        let now = Instant::now();
        if now.duration_since(self.last) < INTERVAL {
            return;
        }
        let cols = columns(app);
        if self.shown.as_ref() == Some(&cols) {
            return;
        }
        if set_icon(&icon::png(&self.canvas.render(&cols))) {
            self.shown = Some(cols);
            self.last = now;
        }
    }
}

/// Make a PNG the application's icon; false if it could not be.
fn set_icon(png: &[u8]) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let Some(image) = image(png) else {
        return false;
    };
    // SAFETY: called on the main thread with a valid image.
    unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
    true
}

fn image(png: &[u8]) -> Option<Retained<NSImage>> {
    let data = NSData::with_bytes(png);
    NSImage::initWithData(NSImage::alloc(), &data)
}

/// The meter bars of the current mode as LED columns, from the newest frame
/// (live, whatever the cursor is doing), behaving and coloured as the bars
/// do: RMS with its peak envelope and hold, the VU needle with its hold, or
/// momentary loudness with its max-M mark and short-term with the integrated
/// pointer, in the scale's zones or the loudness bars' colours (yellow above
/// the target).
fn columns(app: &App) -> Vec<Column> {
    let Some(f) = app.history.last().and_then(|i| app.history.get(i)) else {
        return Vec::new();
    };
    let scale = mode_scale(app);
    let target = app.display.lufs_target;
    let n = f.n().min(MAX_COLUMNS);
    let bars: Vec<Bar> = match app.display.mode {
        MeterMode::Rms => (0..n)
            .map(|c| Bar {
                fill: f.rms_avg[c] + app.display.rms_offset,
                envelope: f.peak_decay[c],
                hold: Some(f.hold[c]),
                ..Bar::default()
            })
            .collect(),
        MeterMode::Vu => (0..n)
            .map(|c| Bar {
                fill: f.vu[c],
                envelope: f.vu[c],
                hold: Some(f.vu_hold[c]),
                ..Bar::default()
            })
            .collect(),
        // The M bar's max-M mark is dim grey in the window; a lighter grey
        // keeps it visible on the small icon.
        MeterMode::Lufs => vec![
            Bar {
                fill: f.lufs_m,
                envelope: f.lufs_m,
                hold: Some(app.session.max_lufs_m),
                color: Some(LU_M_COLOR),
                cap: Color::Gray,
                marker: None,
            },
            Bar {
                fill: f.lufs_s,
                envelope: f.lufs_s,
                color: Some(LU_S_COLOR),
                marker: Some((
                    f.lufs_i,
                    if f.lufs_i >= target {
                        LU_OVER_COLOR
                    } else {
                        Color::White
                    },
                )),
                ..Bar::default()
            },
        ],
    };
    let segs = SEGMENTS as f32;
    let rgb = |c: Color| {
        let c = c32(c);
        [c.r(), c.g(), c.b()]
    };
    // Segments lit by a level: rounded, so a level halfway up a segment lights it.
    let lit = |db: f32| ((scale.frac(db) * segs).round() as usize).min(SEGMENTS);
    bars.into_iter()
        .map(|b| {
            let fill = lit(b.fill);
            let envelope = lit(b.envelope).max(fill);
            let peak = b
                .hold
                .map(|h| (scale.frac(h) * segs).ceil() as usize)
                .filter(|&p| p > envelope)
                .map(|p| p.min(SEGMENTS) - 1);
            let (colors, dim) = {
                let pair = |s: usize| {
                    let db = scale.db_at((s as f32 + 0.5) / segs);
                    let above_target = matches!(scale, Scale::Lu { target } if db >= target);
                    match b.color {
                        Some(c) if !above_target => (rgb(c), rgb(c)),
                        _ => {
                            let (fg, dm) = scale.colors(db);
                            (rgb(fg), rgb(dm))
                        }
                    }
                };
                (
                    std::array::from_fn(|s| pair(s).0),
                    std::array::from_fn(|s| pair(s).1),
                )
            };
            let marker = b
                .marker
                .filter(|(db, _)| scale.frac(*db) > 0.0)
                .map(|(db, c)| (((scale.frac(db) * segs) as usize).min(SEGMENTS - 1), rgb(c)));
            Column {
                lit: fill,
                envelope,
                peak,
                colors,
                dim,
                cap: rgb(b.cap),
                marker,
            }
        })
        .collect()
}

/// A meter bar's readings, as the window draws it.
struct Bar {
    fill: f32,
    envelope: f32,
    hold: Option<f32>,
    /// One colour for the bar instead of the scale's zones (below a target).
    color: Option<Color>,
    cap: Color,
    /// A pointer beside the bar: level and colour.
    marker: Option<(f32, Color)>,
}

impl Default for Bar {
    fn default() -> Self {
        Bar {
            fill: f32::NEG_INFINITY,
            envelope: f32::NEG_INFINITY,
            hold: None,
            color: None,
            cap: Color::White,
            marker: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::Analyzer;
    use crate::cli::Cli;
    use crate::generator::{parse_spec, Generator};
    use crate::source::{Control, SourceInfo, SourceKind};
    use crate::ui::colormap::ColorMode;
    use clap::Parser;
    use std::sync::Arc;

    /// An app fed 3 s of a stereo 1 kHz sine at -18 dBFS (0 VU).
    fn app(mode: &str) -> App {
        let cli = Cli::try_parse_from(["vu", mode]).unwrap();
        let info = SourceInfo {
            name: "test".into(),
            sample_rate: 48000,
            channels: 2,
            total_frames: None,
            kind: SourceKind::Generator,
        };
        let mut analyzer = Analyzer::new(crate::pump::analyzer_config(&cli, &info).unwrap());
        let mut app = App::new(
            &cli,
            info,
            &analyzer,
            Arc::new(Control::default()),
            ColorMode::TrueColor,
        );
        let mut gen = Generator::new(parse_spec("sine:1000:-18").unwrap(), 48000);
        let mut chunk = vec![0.0f32; 4800 * 2];
        for _ in 0..30 {
            gen.fill(&mut chunk, 2);
            analyzer.push(&chunk, |f| app.push_frame(f));
        }
        app
    }

    fn rgb(c: Color) -> [u8; 3] {
        let c = c32(c);
        [c.r(), c.g(), c.b()]
    }

    #[test]
    fn rms_columns_have_zones_envelope_and_hold() {
        let cols = columns(&app("--panels=meter"));
        assert_eq!(cols.len(), 2, "one per channel");
        let c = cols[0];
        assert_eq!(c.colors[0], rgb(Color::Green));
        assert_eq!(c.colors[SEGMENTS - 1], rgb(Color::Red));
        // The peak envelope (-18) stands above the RMS (-21).
        assert!(c.envelope >= c.lit && c.lit > 0, "{c:?}");
        assert_eq!(c.marker, None);
    }

    #[test]
    fn vu_columns_turn_red_at_zero_vu() {
        let cols = columns(&app("--vu"));
        assert_eq!(cols.len(), 2);
        let c = cols[0];
        // -18 dBFS reads 0 VU, about 71% up the VU scale.
        assert!((8..=9).contains(&c.lit), "{c:?}");
        assert_eq!(c.envelope, c.lit, "no envelope on a needle");
        assert_eq!(c.colors[0], rgb(Color::Green));
        assert_eq!(c.colors[SEGMENTS - 1], rgb(Color::Red));
    }

    #[test]
    fn lufs_columns_are_m_and_s_with_the_integrated_pointer() {
        let a = app("--lufs");
        let cols = columns(&a);
        assert_eq!(cols.len(), 2, "M and S");
        let (m, s) = (cols[0], cols[1]);
        assert_eq!(m.colors[0], rgb(LU_M_COLOR));
        assert_eq!(s.colors[0], rgb(LU_S_COLOR));
        // Above the -14 LUFS target, both turn yellow.
        assert_eq!(m.colors[SEGMENTS - 1], rgb(LU_OVER_COLOR));
        assert!(m.lit > 0 && s.lit > 0);
        assert!(m.marker.is_none());
        let (seg, color) = s.marker.expect("integrated pointer on S");
        assert!(
            seg < SEGMENTS && color == rgb(Color::White),
            "below target: white"
        );
    }

    #[test]
    fn modes_draw_differently() {
        let [r, v, l] = ["--panels=meter", "--vu", "--lufs"].map(|m| columns(&app(m)));
        assert!(r != v && v != l && r != l);
    }
}
