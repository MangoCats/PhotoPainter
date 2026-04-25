use crate::font::{draw_text, measure_text};
use crate::image::{E6Canvas, E6Color};
use super::{Module, Rect};
use super::weather::{draw_condition_icon, WeatherCondition};

const LABEL_SIZE: f32 = 14.0;
const LABEL_GAP:  i32 = 6;   // between icon bottom and label baseline
const ROW_GAP:    i32 = 32;  // between label baseline of row N and icon top of row N+1
const COLS:       i32 = 5;
const ROWS:       i32 = 2;
const ICON_SIZE:  i32 = 64;  // must match weather::ICON_SIZE

const ALL: &[(WeatherCondition, &str)] = &[
    (WeatherCondition::ClearDay,          "Clear Day"),
    (WeatherCondition::ClearNight,        "Clear Night"),
    (WeatherCondition::PartlyCloudyDay,   "Partly Cloudy Day"),
    (WeatherCondition::PartlyCloudyNight, "Partly Cloudy Night"),
    (WeatherCondition::Cloudy,            "Cloudy"),
    (WeatherCondition::Rain,              "Rain"),
    (WeatherCondition::Thunderstorm,      "Thunderstorm"),
    (WeatherCondition::Snow,              "Snow"),
    (WeatherCondition::Fog,               "Fog"),
    (WeatherCondition::Unknown,           "(Unknown)"),
];

pub struct IconMatrixModule;

impl Module for IconMatrixModule {
    fn render(&self, canvas: &mut E6Canvas, region: Rect) {
        let label_ascent = measure_text("A", LABEL_SIZE, false).1;
        let cell_h       = ICON_SIZE + LABEL_GAP + label_ascent;
        let total_h      = ROWS * cell_h + (ROWS - 1) * ROW_GAP;
        let start_y      = region.y + (region.height - total_h) / 2;

        // Divide width into COLS equal cells; center the icon inside each cell.
        let cell_w  = region.width / COLS;
        let start_x = region.x + (region.width - cell_w * COLS) / 2;

        for (i, &(cond, label)) in ALL.iter().enumerate() {
            let col = (i as i32) % COLS;
            let row = (i as i32) / COLS;

            let cell_x = start_x + col * cell_w;
            let icon_x = cell_x + (cell_w - ICON_SIZE) / 2;
            let icon_y = start_y + row * (cell_h + ROW_GAP);

            draw_condition_icon(canvas, icon_x, icon_y, cond);

            let (lw, _) = measure_text(label, LABEL_SIZE, false);
            let label_x = cell_x + (cell_w - lw) / 2;
            let label_y = icon_y + ICON_SIZE + LABEL_GAP;
            draw_text(canvas, label_x, label_y, label, LABEL_SIZE, E6Color::Black, false);
        }
    }
}
