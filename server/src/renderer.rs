use sha2::{Sha256, Digest};
use crate::font::draw_text;
use crate::image::{E6Canvas, E6Color, SCREEN_W, SCREEN_H};
use crate::modules::{Module, Rect};

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

    // Version bar — two lines at the bottom, 4% of screen height
    const SIZE_PX: f32 = 19.2;
    const LINE_H:  i32 = 20;
    const MARGIN:  i32 = 8;
    const GAP:     i32 = 4;
    let line2_y = SCREEN_H - MARGIN - LINE_H;
    let line1_y = line2_y - GAP - LINE_H;

    draw_text(&mut canvas, MARGIN, line1_y, &format!("SV: {server_ver}"), SIZE_PX, E6Color::Black, false);
    draw_text(&mut canvas, MARGIN, line2_y, &format!("FW: {fw_ver}"),     SIZE_PX, E6Color::Black, false);

    let packed = canvas.pack();
    let etag   = hex::encode(Sha256::digest(&packed));
    RenderedImage { packed, etag }
}

/// Full-screen region.
pub fn full_screen() -> Rect {
    Rect { x: 0, y: 0, width: SCREEN_W, height: SCREEN_H }
}
