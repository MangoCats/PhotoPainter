use crate::image::E6Canvas;

pub mod battery;
pub mod clock;
pub mod gcal;
pub mod rain;
pub mod stock;
pub mod weather;

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x:      i32,
    pub y:      i32,
    pub width:  i32,
    pub height: i32,
}

pub trait Module: Send + Sync {
    fn render(&self, canvas: &mut E6Canvas, region: Rect);
}
