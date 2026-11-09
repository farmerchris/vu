//! The window and Dock icon: a segmented LED level meter on a dark rounded
//! square, laid out on the macOS icon grid (the body is about 80% of the
//! canvas, leaving room for its shadow). The static icon shows a still
//! meter; on macOS the Dock icon is redrawn from the live levels.

use eframe::egui::IconData;

/// Size of the static icon, pixels.
const SIZE: usize = 512;
/// Geometry on a 512-pixel canvas, scaled to other sizes: the body's inset
/// and corner radius, and the meter area.
const INSET: f32 = 50.0;
const RADIUS: f32 = 92.0;
const METER: (f32, f32, f32, f32) = (120.0, 122.0, 392.0, 390.0);
const COL_GAP: f32 = 14.0;
const SEG_GAP: f32 = 6.0;
/// Widest a column gets, so one or two columns stay meter-shaped.
const MAX_COL_W: f32 = 84.0;
/// LED segments per column.
pub const SEGMENTS: usize = 12;
/// Segments from this index up are yellow, and from the next up red.
const YELLOW_FROM: usize = 7;
const RED_FROM: usize = 10;
/// The still meter of the static icon: lit segments and peak cap.
const STILL: [(usize, usize); 5] = [(7, 9), (10, 11), (8, 10), (11, 11), (6, 8)];

type Rgba = [f32; 4];

const GREEN: Rgba = [0.24, 0.81, 0.43, 1.0];
const YELLOW: Rgba = [0.95, 0.77, 0.24, 1.0];
const RED: Rgba = [0.94, 0.31, 0.24, 1.0];
const CAP: Rgba = [0.95, 0.95, 0.97, 1.0];
const BODY_TOP: Rgba = [0.17, 0.19, 0.26, 1.0];
const BODY_BOTTOM: Rgba = [0.07, 0.08, 0.12, 1.0];

/// One meter column, like a meter bar: lit segments from the bottom, a dim
/// envelope above them up to `envelope` (the peak envelope of an RMS bar),
/// the peak cap's segment when it stands above both, and a marker beside
/// the column (integrated loudness). Colours are sRGB, per segment from the
/// bottom, bright and dim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Column {
    pub lit: usize,
    pub envelope: usize,
    pub peak: Option<usize>,
    pub colors: [[u8; 3]; SEGMENTS],
    pub dim: [[u8; 3]; SEGMENTS],
    pub cap: [u8; 3],
    pub marker: Option<(usize, [u8; 3])>,
}

impl Column {
    /// A column in the icon's own green, yellow and red zones.
    pub fn zoned(lit: usize, peak: Option<usize>) -> Self {
        let byte = |c: Rgba| [0, 1, 2].map(|i| (c[i] * 255.0).round() as u8);
        let colors: [[u8; 3]; SEGMENTS] = std::array::from_fn(|s| {
            byte(if s >= RED_FROM {
                RED
            } else if s >= YELLOW_FROM {
                YELLOW
            } else {
                GREEN
            })
        });
        Column {
            lit,
            envelope: lit,
            peak,
            colors,
            dim: colors.map(|c| c.map(|v| v / 2)),
            cap: byte(CAP),
            marker: None,
        }
    }
}

/// The static icon.
pub fn icon() -> IconData {
    let still = STILL.map(|(lit, peak)| Column::zoned(lit, Some(peak)));
    Canvas::new(SIZE).render(&still)
}

/// An icon canvas with its background drawn once, for drawing meters on.
pub struct Canvas {
    size: usize,
    bg: Vec<Rgba>,
}

impl Canvas {
    pub fn new(size: usize) -> Self {
        let k = size as f32 / 512.0;
        let mut bg = vec![[0.0f32; 4]; size * size];
        let (lo, hi) = (INSET * k, size as f32 - INSET * k);
        let centre = (size as f32 / 2.0, size as f32 / 2.0);
        let half = ((hi - lo) / 2.0, (hi - lo) / 2.0);
        // Soft drop shadow, then the body with a vertical gradient and a faint rim.
        for y in 0..size {
            for x in 0..size {
                let p = (x as f32 + 0.5, y as f32 + 0.5);
                let px = &mut bg[y * size + x];
                let d = round_rect(p, (centre.0, centre.1 + 8.0 * k), half, RADIUS * k) / k;
                over(px, [0.0, 0.0, 0.0, 1.0], 0.45 * (-d.max(0.0) / 14.0).exp());
                let d = round_rect(p, centre, half, RADIUS * k);
                let t = ((p.1 - lo) / (hi - lo)).clamp(0.0, 1.0);
                over(px, mix(BODY_TOP, BODY_BOTTOM, t), coverage(d));
                if d < 0.0 && d > -2.5 * k {
                    over(px, [1.0; 4], 0.10);
                }
            }
        }
        Canvas { size, bg }
    }

    /// The icon with `cols` as LED columns: lit segments bright, the
    /// envelope in the dim colours, a cap at each peak, unlit segments faint,
    /// and markers as pointers beside their column.
    pub fn render(&self, cols: &[Column]) -> IconData {
        let size = self.size;
        let k = size as f32 / 512.0;
        let mut px = self.bg.clone();
        let (x0, y0, x1, y1) = (METER.0 * k, METER.1 * k, METER.2 * k, METER.3 * k);
        let (col_gap, seg_gap) = (COL_GAP * k, SEG_GAP * k);
        let n = cols.len().max(1) as f32;
        let col_w = ((x1 - x0 - col_gap * (n - 1.0)) / n).min(MAX_COL_W * k);
        let left0 = (x0 + x1) / 2.0 - (n * col_w + (n - 1.0) * col_gap) / 2.0;
        let seg_h = (y1 - y0 - seg_gap * (SEGMENTS - 1) as f32) / SEGMENTS as f32;
        for (c, col) in cols.iter().enumerate() {
            let left = left0 + c as f32 * (col_w + col_gap);
            for s in 0..SEGMENTS {
                let bottom = y1 - s as f32 * (seg_h + seg_gap);
                let rgba = |c: [u8; 3]| [c[0], c[1], c[2], 255].map(|v| v as f32 / 255.0);
                let zone = rgba(col.colors[s]);
                let (color, alpha) = if s < col.lit {
                    (zone, 1.0)
                } else if s < col.envelope {
                    (rgba(col.dim[s]), 1.0)
                } else if col.peak == Some(s) {
                    (rgba(col.cap), 0.95)
                } else {
                    (zone, 0.13)
                };
                let rect = (left, bottom - seg_h, left + col_w, bottom);
                fill_round_rect(&mut px, size, rect, 4.0 * k, color, alpha);
            }
            if let Some((s, c)) = col.marker.filter(|&(s, _)| s < SEGMENTS) {
                // A pointer just right of the column, level with segment `s`.
                let mid = y1 - s as f32 * (seg_h + seg_gap) - seg_h / 2.0;
                let x = left + col_w + 3.0 * k;
                let color = [c[0], c[1], c[2], 255].map(|v| v as f32 / 255.0);
                fill_triangle(
                    &mut px,
                    size,
                    [
                        (x, mid),
                        (x + 12.0 * k, mid - 8.0 * k),
                        (x + 12.0 * k, mid + 8.0 * k),
                    ],
                    color,
                );
            }
        }
        IconData {
            rgba: px.iter().flat_map(|p| unpremultiply(*p)).collect(),
            width: size as u32,
            height: size as u32,
        }
    }
}

/// Signed distance from `p` to a rounded rectangle (negative inside).
fn round_rect(p: (f32, f32), c: (f32, f32), half: (f32, f32), r: f32) -> f32 {
    let qx = (p.0 - c.0).abs() - half.0 + r;
    let qy = (p.1 - c.1).abs() - half.1 + r;
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

/// Antialiased coverage of a pixel at signed distance `d`.
fn coverage(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

fn fill_round_rect(
    px: &mut [Rgba],
    size: usize,
    (l, t, r, b): (f32, f32, f32, f32),
    radius: f32,
    color: Rgba,
    alpha: f32,
) {
    let c = ((l + r) / 2.0, (t + b) / 2.0);
    let half = ((r - l) / 2.0, (b - t) / 2.0);
    for y in (t.max(0.0).floor() as usize)..(b.ceil() as usize).min(size) {
        for x in (l.max(0.0).floor() as usize)..(r.ceil() as usize).min(size) {
            let d = round_rect((x as f32 + 0.5, y as f32 + 0.5), c, half, radius);
            over(&mut px[y * size + x], color, alpha * coverage(d));
        }
    }
}

/// Fill a triangle, antialiased by its edge distance.
fn fill_triangle(px: &mut [Rgba], size: usize, t: [(f32, f32); 3], color: Rgba) {
    let (lx, hx) = (
        t.iter().map(|p| p.0).fold(f32::MAX, f32::min),
        t.iter().map(|p| p.0).fold(f32::MIN, f32::max),
    );
    let (ly, hy) = (
        t.iter().map(|p| p.1).fold(f32::MAX, f32::min),
        t.iter().map(|p| p.1).fold(f32::MIN, f32::max),
    );
    // Signed distance to each edge, positive inside for a clockwise-or-not
    // triangle (normalized by orientation).
    let area = (t[1].0 - t[0].0) * (t[2].1 - t[0].1) - (t[2].0 - t[0].0) * (t[1].1 - t[0].1);
    let sign = area.signum();
    for y in (ly.max(0.0) as usize)..(hy.ceil() as usize + 1).min(size) {
        for x in (lx.max(0.0) as usize)..(hx.ceil() as usize + 1).min(size) {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let d = (0..3)
                .map(|i| {
                    let (a, b) = (t[i], t[(i + 1) % 3]);
                    let (ex, ey) = (b.0 - a.0, b.1 - a.1);
                    sign * (ex * (p.1 - a.1) - ey * (p.0 - a.0)) / (ex * ex + ey * ey).sqrt()
                })
                .fold(f32::MAX, f32::min);
            over(&mut px[y * size + x], color, (d + 0.5).clamp(0.0, 1.0));
        }
    }
}

fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
}

/// Composite `color` at opacity `a` over a premultiplied pixel.
fn over(dst: &mut Rgba, color: Rgba, a: f32) {
    if a <= 0.0 {
        return;
    }
    for i in 0..3 {
        dst[i] = color[i] * a + dst[i] * (1.0 - a);
    }
    dst[3] = a + dst[3] * (1.0 - a);
}

fn unpremultiply(p: Rgba) -> [u8; 4] {
    let a = p[3];
    let ch = |v: f32| {
        if a > 0.0 {
            (v / a * 255.0).round().clamp(0.0, 255.0) as u8
        } else {
            0
        }
    };
    [ch(p[0]), ch(p[1]), ch(p[2]), (a * 255.0).round() as u8]
}

/// The icon as a PNG file (stored, uncompressed), for handing to the system.
pub fn png(icon: &IconData) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }
    let (w, h) = (icon.width as usize, icon.height as usize);
    // Each row starts with filter type 0 (none).
    let mut raw = Vec::with_capacity(h * (w * 4 + 1));
    for row in icon.rgba.chunks(w * 4) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    // zlib stream of stored deflate blocks (at most 65535 bytes each).
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, b) in blocks.iter().enumerate() {
        z.push((i + 1 == blocks.len()) as u8);
        let len = b.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(b);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &x in data {
        c ^= x as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape() {
        let icon = icon();
        assert_eq!((icon.width, icon.height), (SIZE as u32, SIZE as u32));
        assert_eq!(icon.rgba.len(), SIZE * SIZE * 4);
        let alpha = |x: usize, y: usize| icon.rgba[(y * SIZE + x) * 4 + 3];
        assert_eq!(alpha(0, 0), 0, "corner outside the body is clear");
        assert_eq!(alpha(SIZE / 2, SIZE / 2), 255, "body is opaque");
        // The bottom-left segment is lit green.
        let i = ((380 * SIZE) + 130) * 4;
        let [r, g, b] = [icon.rgba[i], icon.rgba[i + 1], icon.rgba[i + 2]];
        assert!(g > 180 && r < 100 && b < 140, "{r} {g} {b}");
    }

    #[test]
    fn live_columns_and_png() {
        let canvas = Canvas::new(64);
        let quiet = canvas.render(&[Column::zoned(0, None); 2]);
        let loud = canvas.render(&[Column::zoned(12, None); 2]);
        assert_ne!(quiet.rgba, loud.rgba);
        let png = png(&loud);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
