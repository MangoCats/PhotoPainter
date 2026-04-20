use std::time::Duration;
use chrono::Timelike;
use crate::image::{E6Canvas, E6Color};
use super::{Module, Rect};

// ── Dimensions (all in pixels) ───────────────────────────────────────────────
const DIGIT_H:    i32 = 96;  // 20 % of 480
const THICKNESS:  i32 = 12;  // DIGIT_H / 8
const DIGIT_W:    i32 = 60;
const GAP:        i32 = 12;
const COLON_SLOT: i32 = 16;  // width reserved for the colon
const DOT_SIZE:   i32 = 12;

// ── Segment truth table ──────────────────────────────────────────────────────
// Segments: [a=top, b=top-right, c=bot-right, d=bottom, e=bot-left, f=top-left, g=middle]
const SEGMENTS_ON: [[bool; 7]; 10] = [
    [true,  true,  true,  true,  true,  true,  false], // 0
    [false, true,  true,  false, false, false, false], // 1
    [true,  true,  false, true,  true,  false, true],  // 2
    [true,  true,  true,  true,  false, false, true],  // 3
    [false, true,  true,  false, false, true,  true],  // 4
    [true,  false, true,  true,  false, true,  true],  // 5
    [true,  false, true,  true,  true,  true,  true],  // 6
    [true,  true,  true,  false, false, false, false], // 7
    [true,  true,  true,  true,  true,  true,  true],  // 8
    [true,  true,  true,  true,  false, true,  true],  // 9
];

fn draw_digit(canvas: &mut E6Canvas, x: i32, y: i32, digit: u8, color: E6Color) {
    let on = &SEGMENTS_ON[digit as usize];
    let (dw, dh, t) = (DIGIT_W, DIGIT_H, THICKNESS);

    // a — top horizontal
    if on[0] { canvas.fill_rect(x + t,      y,              dw - 2*t, t,        color); }
    // b — top-right vertical
    if on[1] { canvas.fill_rect(x + dw - t, y + t,          t,        dh/2 - t, color); }
    // c — bottom-right vertical
    if on[2] { canvas.fill_rect(x + dw - t, y + dh/2,       t,        dh/2 - t, color); }
    // d — bottom horizontal
    if on[3] { canvas.fill_rect(x + t,      y + dh - t,     dw - 2*t, t,        color); }
    // e — bottom-left vertical
    if on[4] { canvas.fill_rect(x,          y + dh/2,       t,        dh/2 - t, color); }
    // f — top-left vertical
    if on[5] { canvas.fill_rect(x,          y + t,          t,        dh/2 - t, color); }
    // g — middle horizontal
    if on[6] { canvas.fill_rect(x + t,      y + dh/2 - t/2, dw - 2*t, t,        color); }
}

// ── Module ────────────────────────────────────────────────────────────────────
pub struct ClockModule;

impl Module for ClockModule {
    fn render(&self, canvas: &mut E6Canvas, region: Rect) {
        let now   = chrono::Local::now();
        let hour  = now.hour();
        let minute = now.minute();

        let digits: [u8; 4] = [
            (hour   / 10) as u8,
            (hour   % 10) as u8,
            (minute / 10) as u8,
            (minute % 10) as u8,
        ];

        // Total rendered width: 4 digits + colon slot + 4 gaps
        let total_w = 4 * DIGIT_W + COLON_SLOT + 4 * GAP;
        let x0 = region.x + (region.width  - total_w) / 2;
        let y0 = region.y + (region.height - DIGIT_H) / 2;

        // X origins for each digit (colon slot sits between digit 1 and digit 2)
        let xs: [i32; 4] = [
            x0,
            x0 + DIGIT_W + GAP,
            x0 + 2 * DIGIT_W + 2 * GAP + COLON_SLOT + GAP,
            x0 + 3 * DIGIT_W + 3 * GAP + COLON_SLOT + GAP,
        ];

        for (i, &d) in digits.iter().enumerate() {
            draw_digit(canvas, xs[i], y0, d, E6Color::Black);
        }

        // Colon — two square dots centred in the COLON_SLOT
        let cx     = x0 + 2 * DIGIT_W + 2 * GAP;
        let dot_x  = cx + (COLON_SLOT - DOT_SIZE) / 2;
        let dot1_y = y0 + DIGIT_H / 3       - DOT_SIZE / 2;
        let dot2_y = y0 + 2 * DIGIT_H / 3   - DOT_SIZE / 2;
        canvas.fill_rect(dot_x, dot1_y, DOT_SIZE, DOT_SIZE, E6Color::Black);
        canvas.fill_rect(dot_x, dot2_y, DOT_SIZE, DOT_SIZE, E6Color::Black);
    }

    fn data_refresh_interval(&self) -> Duration {
        Duration::from_secs(60)
    }

    fn suggested_poll_interval(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }
}
