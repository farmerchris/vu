//! Piano keyboard (`--piano`): the 88 keys A0–C8 with the key nearest the
//! strongest tone lit. When the panel is too narrow for all of them, a window
//! of keys follows the tone.

use super::title;
use crate::app::App;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Widget};

/// Panel height: border, two rows with black keys, one without, and labels.
pub const HEIGHT: u16 = 6;
/// MIDI numbers of the lowest and highest piano keys (A0, C8).
const LOWEST: i32 = 21;
const HIGHEST: i32 = 108;
/// Shown when there is no tone, so the window sits in the middle.
const MIDDLE_C: i32 = 60;
const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
const LIT: Color = Color::Cyan;

pub fn is_black(midi: i32) -> bool {
    matches!(midi.rem_euclid(12), 1 | 3 | 6 | 8 | 10)
}

/// Index among the white keys (A0 = 0) of `midi`, or of the white key just
/// left of it for a black key.
pub fn white_index(midi: i32) -> i32 {
    (LOWEST..=midi).filter(|&m| !is_black(m)).count() as i32 - 1
}

/// Nearest piano key to `hz`, if on the keyboard.
pub fn key_of(hz: f32) -> Option<i32> {
    if hz.is_nan() || hz <= 0.0 {
        return None;
    }
    let midi = (69.0 + 12.0 * (hz / 440.0).log2()).round() as i32;
    (LOWEST..=HIGHEST).contains(&midi).then_some(midi)
}

/// Note name with octave, e.g. "C4", "F#2".
pub fn key_name(midi: i32) -> String {
    format!("{}{}", NAMES[midi.rem_euclid(12) as usize], midi.div_euclid(12) - 1)
}

/// Columns per white key and the first visible white key, for `width`
/// columns centred (as far as the keyboard allows) on `focus`.
pub fn window(width: u16, focus: i32) -> (u16, i32, i32) {
    let whites = white_index(HIGHEST) + 1;
    let kw: u16 = if width as i32 >= whites * 3 { 3 } else { 2 };
    let shown = (width / kw) as i32;
    let shown = shown.min(whites);
    let first = (white_index(focus) - shown / 2).clamp(0, whites - shown);
    (kw, first, shown)
}

pub fn render(app: &mut App, area: Rect, buf: &mut Buffer) {
    let history = &app.history;
    let shown = app.readout.pick(app.view.cursor, app.view.follow, std::time::Instant::now(), |i| history.get(i).is_some());
    let tone = app.history.get(shown).and_then(|f| f.peak_freq).filter(|&(_, db)| db > -100.0);
    let lit = tone.and_then(|(hz, _)| key_of(hz));
    let heading = match (tone, lit) {
        (Some((hz, _)), Some(k)) => format!(" Piano  {} · {hz:.1} Hz ", key_name(k)),
        _ => " Piano ".to_string(),
    };
    let block = Block::bordered().title(title(heading));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height < 3 || inner.width < 14 {
        return;
    }
    let (kw, first, count) = window(inner.width, lit.unwrap_or(MIDDLE_C));
    let x0 = inner.x + (inner.width - kw * count as u16) / 2;
    let key_rows = inner.height - 1;
    let white_bg = Color::Indexed(254);
    let col = |wi: i32| x0 + (wi - first) as u16 * kw;
    let visible = |wi: i32| (first..first + count).contains(&wi);

    // White keys, with a thin divider down their right edge.
    for m in (LOWEST..=HIGHEST).filter(|&m| !is_black(m)) {
        let wi = white_index(m);
        if !visible(wi) {
            continue;
        }
        let bg = if lit == Some(m) { LIT } else { white_bg };
        for r in 0..key_rows {
            for dx in 0..kw {
                let cell = &mut buf[(col(wi) + dx, inner.y + r)];
                cell.set_char(if dx == kw - 1 { '▕' } else { ' ' }).set_fg(Color::Indexed(245)).set_bg(bg);
            }
        }
    }
    // Black keys over the top rows, straddling the gap between their white
    // neighbours (only the left one's last column when keys are narrow).
    for m in (LOWEST..=HIGHEST).filter(|&m| is_black(m)) {
        let left = white_index(m);
        let bg = if lit == Some(m) { LIT } else { Color::Indexed(16) };
        let cols: &[u16] = if kw == 3 { &[2, 3] } else { &[1] };
        for &dx in cols {
            let wi = left + (dx / kw) as i32;
            if !visible(wi) {
                continue;
            }
            for r in 0..key_rows - 1 {
                buf[(col(wi) + dx % kw, inner.y + r)].set_char(' ').set_bg(bg);
            }
        }
    }

    // Labels: the lit key's name under it, then every C that still fits.
    let y = inner.y + key_rows;
    let end = inner.x + inner.width;
    let mut taken: Vec<(u16, u16)> = Vec::new();
    let mut label = |buf: &mut Buffer, m: i32, style: Style| {
        let wi = white_index(m);
        if !visible(wi) {
            return;
        }
        let name = key_name(m);
        let x = col(wi) + if is_black(m) { kw - 1 } else { 0 };
        let w = name.len() as u16;
        if x + w > end || taken.iter().any(|&(a, b)| x < b + 1 && a < x + w + 1) {
            return;
        }
        taken.push((x, x + w));
        buf.set_string(x, y, name, style);
    };
    if let Some(k) = lit {
        label(buf, k, Style::default().fg(LIT).add_modifier(Modifier::BOLD));
    }
    for m in (LOWEST..=HIGHEST).filter(|m| m.rem_euclid(12) == 0) {
        label(buf, m, Style::default().fg(Color::DarkGray));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(white_index(LOWEST), 0);
        assert_eq!(white_index(HIGHEST), 51, "52 white keys");
        assert_eq!((LOWEST..=HIGHEST).filter(|&m| is_black(m)).count(), 36);
        assert!(is_black(61) && !is_black(60) && !is_black(64) && !is_black(65));
        assert_eq!(white_index(61), white_index(60), "C#4 sits right of C4");
        assert_eq!(key_of(440.0), Some(69));
        assert_eq!(key_of(452.0), Some(69), "within half a semitone");
        assert_eq!(key_of(27.5), Some(21));
        assert_eq!(key_of(4186.0), Some(108));
        assert_eq!((key_of(20.0), key_of(5000.0), key_of(0.0), key_of(f32::NAN)), (None, None, None, None));
        assert_eq!((key_name(60), key_name(66), key_name(21)), ("C4".into(), "F#4".into(), "A0".into()));
    }

    #[test]
    fn window_fits_and_follows() {
        // Wide: every key, three columns each.
        assert_eq!(window(160, 60), (3, 0, 52));
        // Narrow: a window of two-column keys around the focus, kept on the keyboard.
        let (kw, first, n) = window(78, 69);
        assert_eq!((kw, n), (2, 39));
        assert!((first..first + n).contains(&white_index(69)));
        assert_eq!(window(78, 21).1, 0);
        assert_eq!(window(78, 108).1, 52 - 39);
    }
}
