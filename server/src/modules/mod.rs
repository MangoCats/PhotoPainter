use std::time::Duration;
use crate::image::E6Canvas;

pub mod clock;
pub mod gcal;
pub mod rain;
pub mod weather;

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x:      i32,
    pub y:      i32,
    pub width:  i32,
    pub height: i32,
}

pub trait Module: Send + Sync {
    /// Render this module's content into `region` on the canvas.
    fn render(&self, canvas: &mut E6Canvas, region: Rect);

    /// How long the server should wait before re-rendering this module's data.
    fn data_refresh_interval(&self) -> Duration;

    /// Suggested device poll interval. None defers to the server default.
    fn suggested_poll_interval(&self) -> Option<Duration>;
}
