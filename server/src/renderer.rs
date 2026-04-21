use sha2::{Sha256, Digest};
use crate::font::draw_text;
use crate::image::{E6Canvas, E6Color, SCREEN_H};
use crate::modules::{Module, Rect};

pub use crate::image::SCREEN_W;

pub struct RenderedImage {
    pub packed: Vec<u8>,   // 192,000 bytes, 4bpp
    pub etag:   String,    // hex SHA-256 of packed
}

pub fn render(
    modules:    &[(&dyn Module, Rect)],
    server_ver: &str,
    fw_ver:     &str,
) -> RenderedImage {
    let mut canvas = E6Canvas::new(E6Color::White);

    for (module, region) in modules {
        module.render(&mut canvas, *region);
    }

    // Version bar — two lines at the bottom, ~42 px em (≈ 9% of 480)
    const SIZE_PX: f32 = 42.0;
    const LINE_H:  i32 = 42;
    const MARGIN:  i32 = 8;
    const GAP:     i32 = 4;
    let line2_y = SCREEN_H - MARGIN - LINE_H;       // 430
    let line1_y = line2_y - GAP - LINE_H;           // 384

    draw_text(&mut canvas, MARGIN, line1_y, &format!("SV: {server_ver}"), SIZE_PX, E6Color::Black);
    draw_text(&mut canvas, MARGIN, line2_y, &format!("FW: {fw_ver}"),     SIZE_PX, E6Color::Black);

    let packed = canvas.pack();
    let etag   = hex::encode(Sha256::digest(&packed));
    RenderedImage { packed, etag }
}

/// Full-screen region covering the entire display.
pub fn full_screen() -> Rect {
    use crate::image::SCREEN_W;
    Rect { x: 0, y: 0, width: SCREEN_W, height: SCREEN_H }
}
