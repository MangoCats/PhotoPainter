pub const SCREEN_W: i32 = 800;
pub const SCREEN_H: i32 = 480;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum E6Color {
    Black  = 0x0,
    White  = 0x1,
    Yellow = 0x2,
    Red    = 0x3,
    Blue   = 0x5,
    Green  = 0x6,
}

pub struct E6Canvas {
    pixels: Vec<u8>,   // one byte per pixel, SCREEN_W * SCREEN_H entries
    width:  usize,
    height: usize,
}

impl E6Canvas {
    pub fn new(background: E6Color) -> Self {
        Self {
            pixels: vec![background as u8; (SCREEN_W * SCREEN_H) as usize],
            width:  SCREEN_W as usize,
            height: SCREEN_H as usize,
        }
    }

    /// Fill a rectangle, clamping to canvas bounds.
    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: E6Color) {
        if w <= 0 || h <= 0 { return; }
        let x0 = x.max(0) as usize;
        let y0 = y.max(0) as usize;
        let x1 = (x + w).min(self.width  as i32) as usize;
        let y1 = (y + h).min(self.height as i32) as usize;
        for row in y0..y1 {
            self.pixels[row * self.width + x0 .. row * self.width + x1]
                .fill(color as u8);
        }
    }

    /// Pack to 4bpp: high nibble = left pixel, low nibble = right pixel.
    /// Output length: SCREEN_W * SCREEN_H / 2 = 192,000 bytes.
    pub fn pack(&self) -> Vec<u8> {
        let n = self.width * self.height;
        let mut out = vec![0u8; n / 2];
        for i in 0..out.len() {
            out[i] = (self.pixels[2 * i] << 4) | self.pixels[2 * i + 1];
        }
        out
    }
}
