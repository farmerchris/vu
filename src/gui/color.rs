//! Terminal colours (as the shared code names them) to window colours.

use eframe::egui::Color32;
use ratatui::style::Color;

pub const BG: Color32 = Color32::from_rgb(16, 16, 20);
pub const PANEL: Color32 = Color32::from_rgb(26, 26, 32);
pub const TRACK: Color32 = Color32::from_rgb(36, 36, 44);
pub const GRID: Color32 = Color32::from_rgb(50, 50, 60);
pub const LABEL: Color32 = Color32::from_rgb(120, 120, 132);
pub const TEXT: Color32 = Color32::from_rgb(220, 220, 226);

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// The 16 ANSI colours, tuned for a dark window.
const ANSI: [Color32; 16] = [
    rgb(0, 0, 0),
    rgb(235, 70, 60),
    rgb(70, 200, 110),
    rgb(235, 205, 60),
    rgb(80, 130, 240),
    rgb(210, 100, 210),
    rgb(60, 190, 210),
    rgb(180, 180, 186),
    rgb(105, 105, 115),
    rgb(255, 120, 110),
    rgb(130, 230, 150),
    rgb(250, 235, 130),
    rgb(120, 170, 255),
    rgb(240, 150, 240),
    rgb(130, 230, 240),
    rgb(255, 255, 255),
];

/// The xterm 256-colour cube levels.
const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

pub fn c32(c: Color) -> Color32 {
    match c {
        Color::Rgb(r, g, b) => rgb(r, g, b),
        Color::Indexed(i) => indexed(i),
        Color::Reset => ANSI[15],
        Color::Black => ANSI[0],
        Color::Red => ANSI[1],
        Color::Green => ANSI[2],
        Color::Yellow => ANSI[3],
        Color::Blue => ANSI[4],
        Color::Magenta => ANSI[5],
        Color::Cyan => ANSI[6],
        Color::Gray => ANSI[7],
        Color::DarkGray => ANSI[8],
        Color::LightRed => ANSI[9],
        Color::LightGreen => ANSI[10],
        Color::LightYellow => ANSI[11],
        Color::LightBlue => ANSI[12],
        Color::LightMagenta => ANSI[13],
        Color::LightCyan => ANSI[14],
        Color::White => ANSI[15],
    }
}

fn indexed(i: u8) -> Color32 {
    match i {
        0..=15 => ANSI[i as usize],
        16..=231 => {
            let n = i as usize - 16;
            rgb(CUBE[n / 36], CUBE[n / 6 % 6], CUBE[n % 6])
        }
        _ => {
            let v = 8 + 10 * (i - 232);
            rgb(v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xterm_palette() {
        assert_eq!(c32(Color::Indexed(74)), rgb(95, 175, 215));
        assert_eq!(c32(Color::Indexed(22)), rgb(0, 95, 0));
        assert_eq!(c32(Color::Indexed(232)), rgb(8, 8, 8));
        assert_eq!(c32(Color::Indexed(255)), rgb(238, 238, 238));
        assert_eq!(c32(Color::Indexed(9)), c32(Color::LightRed));
    }
}
