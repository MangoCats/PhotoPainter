use sha2::{Sha256, Digest};
use crate::image::{E6Canvas, E6Color, SCREEN_W, SCREEN_H};
use crate::modules::{Module, Rect};

pub struct RenderedImage {
    pub packed: Vec<u8>,   // 192,000 bytes, 4bpp
    pub etag:   String,    // hex SHA-256 of packed
}

pub fn render(modules: &[(&dyn Module, Rect)]) -> RenderedImage {
    let mut canvas = E6Canvas::new(E6Color::White);

    for (module, region) in modules {
        module.render(&mut canvas, *region);
    }

    let packed = canvas.pack();
    let etag   = hex::encode(Sha256::digest(&packed));
    RenderedImage { packed, etag }
}

/// Full-screen region covering the entire display.
pub fn full_screen() -> Rect {
    Rect { x: 0, y: 0, width: SCREEN_W, height: SCREEN_H }
}
