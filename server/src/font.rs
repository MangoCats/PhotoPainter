// Font file: download JetBrains Mono Regular and place at server/assets/JetBrainsMono-Regular.ttf
// https://github.com/JetBrains/JetBrainsMono/releases  →  JetBrainsMono-*.zip  →  fonts/ttf/
use std::sync::OnceLock;
use crate::image::{E6Canvas, E6Color};

static FONT_BYTES: &[u8] = include_bytes!("../assets/JetBrainsMono-Regular.ttf");
static FONT: OnceLock<fontdue::Font> = OnceLock::new();

fn font() -> &'static fontdue::Font {
    FONT.get_or_init(|| {
        fontdue::Font::from_bytes(FONT_BYTES, fontdue::FontSettings::default())
            .expect("failed to load JetBrainsMono-Regular.ttf")
    })
}

/// Render `text` starting at pixel `(x, y)` (top of the line).
/// `size_px` is the font em-size in pixels.  Pixels with coverage > 50% are drawn.
pub fn draw_text(canvas: &mut E6Canvas, x: i32, y: i32, text: &str, size_px: f32, color: E6Color) {
    let f = font();
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
