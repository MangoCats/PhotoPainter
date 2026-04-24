use sha2::{Sha256, Digest};
use crate::font::{draw_text, measure_text};
use crate::image::{E6Canvas, E6Color, SCREEN_W, SCREEN_H};
use crate::modules::{Module, Rect};
use crate::modules::stock::{StockModule, STRIP_H};

pub struct RenderedImage {
    pub packed: Vec<u8>,   // 192,000 bytes, 4bpp
    pub etag:   String,    // hex SHA-256 of packed
}

pub fn render(
    modules:      &[(&dyn Module, Rect)],
    server_ver:   &str,
    fw_ver:       &str,
    show_version: bool,
    stock:        &StockModule,
) -> RenderedImage {
    let mut canvas = E6Canvas::new(E6Color::White);

    for (module, region) in modules {
        module.render(&mut canvas, *region);
    }

    if show_version {
        // Version bar — single right-justified line at the bottom, 4% of screen height
        const SIZE_PX: f32 = 19.2;
        const LINE_H:  i32 = 20;
        const MARGIN:  i32 = 8;
        let line_y   = SCREEN_H - MARGIN - LINE_H;
        let ver_text = format!("SV: {server_ver}   FW: {fw_ver}");
        let (ver_w, _) = measure_text(&ver_text, SIZE_PX, false);
        draw_text(&mut canvas, SCREEN_W - MARGIN - ver_w, line_y, &ver_text, SIZE_PX, E6Color::Black, false);
    } else {
        stock.render_strip(&mut canvas);
    }

    let packed = canvas.pack();
    let etag   = hex::encode(Sha256::digest(&packed));
    RenderedImage { packed, etag }
}

/// Full-screen region.
pub fn full_screen() -> Rect {
    Rect { x: 0, y: 0, width: SCREEN_W, height: SCREEN_H }
}

/// GCal region: full width, but height excludes the stock strip at the bottom.
/// GCalModule derives its max_y from region.height so it never draws over the strip.
pub fn gcal_region() -> Rect {
    Rect { x: 0, y: 0, width: SCREEN_W, height: SCREEN_H - STRIP_H }
}
