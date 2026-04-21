// Font files: server/assets/JetBrainsMono-{Regular,Bold}.ttf  (OFL 1.1)
// https://github.com/JetBrains/JetBrainsMono/releases
use std::sync::OnceLock;
use crate::image::{E6Canvas, E6Color};

static REGULAR_BYTES: &[u8] = include_bytes!("../assets/JetBrainsMono-Regular.ttf");
static BOLD_BYTES:    &[u8] = include_bytes!("../assets/JetBrainsMono-Bold.ttf");

static REGULAR: OnceLock<fontdue::Font> = OnceLock::new();
static BOLD:    OnceLock<fontdue::Font> = OnceLock::new();

fn get_font(bold: bool) -> &'static fontdue::Font {
    if bold {
        BOLD.get_or_init(|| fontdue::Font::from_bytes(BOLD_BYTES, fontdue::FontSettings::default())
            .expect("failed to load JetBrainsMono-Bold.ttf"))
    } else {
        REGULAR.get_or_init(|| fontdue::Font::from_bytes(REGULAR_BYTES, fontdue::FontSettings::default())
            .expect("failed to load JetBrainsMono-Regular.ttf"))
    }
}

/// Returns `(total_advance_width, ascent)` for `text` at `size_px`.
pub fn measure_text(text: &str, size_px: f32, bold: bool) -> (i32, i32) {
    let f = get_font(bold);
    let ascent = f.horizontal_line_metrics(size_px)
        .map(|m| m.ascent as i32)
        .unwrap_or((size_px * 0.8) as i32);
    let width: i32 = text.chars()
        .map(|ch| f.rasterize(ch, size_px).0.advance_width.round() as i32)
        .sum();
    (width, ascent)
}

/// Render `text` at `(x, y)` where y is the top of the line.
/// Pixels with >50% coverage are drawn at 1:1 scale.
pub fn draw_text(canvas: &mut E6Canvas, x: i32, y: i32, text: &str, size_px: f32, color: E6Color, bold: bool) {
    let f = get_font(bold);
    let baseline_y = y + f.horizontal_line_metrics(size_px)
        .map(|m| m.ascent as i32)
        .unwrap_or((size_px * 0.8) as i32);

    let mut cx = x;
    for ch in text.chars() {
        let (m, bitmap) = f.rasterize(ch, size_px);
        if m.width > 0 && m.height > 0 {
            let gx = cx + m.xmin;
            let gy = baseline_y - m.ymin - m.height as i32;
            for row in 0..m.height {
                for col in 0..m.width {
                    if bitmap[row * m.width + col] > 127 {
                        canvas.fill_rect(gx + col as i32, gy + row as i32, 1, 1, color);
                    }
                }
            }
        }
        cx += m.advance_width.round() as i32;
    }
}
