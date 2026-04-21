use std::time::Duration;
use chrono::Timelike;
use crate::font::{draw_text, measure_text};
use crate::image::{E6Canvas, E6Color};
use super::{Module, Rect};

const CLOCK_SIZE_PX: f32 = 220.0;

pub struct ClockModule;

impl Module for ClockModule {
    fn render(&self, canvas: &mut E6Canvas, region: Rect) {
        let now  = chrono::Local::now();
        let text = format!("{:02}:{:02}", now.hour(), now.minute());

        let (text_w, ascent) = measure_text(&text, CLOCK_SIZE_PX);
        let x = region.x + (region.width  - text_w) / 2;
        let y = region.y + (region.height - ascent)  / 2;

        draw_text(canvas, x, y, &text, CLOCK_SIZE_PX, E6Color::Black);
    }

    fn data_refresh_interval(&self) -> Duration {
        Duration::from_secs(60)
    }

    fn suggested_poll_interval(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }
}
