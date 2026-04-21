use std::time::Duration;
use chrono::{Datelike, Timelike};
use crate::font::draw_text;
use crate::image::{E6Canvas, E6Color};
use super::{Module, Rect};

const SIZE_PX: f32 = 24.0;  // 5% of 480 px screen height
const MARGIN:  i32 = 8;

fn ordinal_suffix(day: u32) -> &'static str {
    match (day % 100, day % 10) {
        (11..=13, _) => "th",
        (_, 1)       => "st",
        (_, 2)       => "nd",
        (_, 3)       => "rd",
        _            => "th",
    }
}

pub struct ClockModule;

impl Module for ClockModule {
    fn render(&self, canvas: &mut E6Canvas, region: Rect) {
        let now = chrono::Local::now();
        let day = now.day();
        let (_, hour12) = now.hour12();
        let text = format!(
            "{}, {} {}{} {} {}:{:02}:{:02} {}",
            now.format("%A"),
            now.format("%B"),
            day,
            ordinal_suffix(day),
            now.year(),
            hour12,
            now.minute(),
            now.second(),
            if now.hour() < 12 { "AM" } else { "PM" },
        );

        draw_text(canvas, region.x + MARGIN, region.y + MARGIN, &text, SIZE_PX, E6Color::Black, false);
    }

    fn data_refresh_interval(&self) -> Duration {
        Duration::from_secs(60)
    }

    fn suggested_poll_interval(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }
}
