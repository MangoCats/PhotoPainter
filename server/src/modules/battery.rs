/// Battery status received from the device via the `X-Battery` request header.
#[derive(Clone, Debug)]
pub struct BatteryInfo {
    pub pct:      i32,         // state of charge 0–100
    pub mv:       u32,         // terminal voltage in millivolts
    pub hrs:      Option<f32>, // estimated hours remaining (discharging only)
    pub charging: bool,        // true for status=charging or status=standby
}

/// Parse the `X-Battery` header value into a `BatteryInfo`.
/// Returns `None` if the header is malformed or reports no battery (`pct < 0`).
pub fn parse_battery_header(s: &str) -> Option<BatteryInfo> {
    let mut pct    = None::<i32>;
    let mut mv     = None::<u32>;
    let mut hrs    = None::<f32>;
    let mut status = "";

    for part in s.split(',') {
        if let Some((k, v)) = part.trim().split_once('=') {
            match k.trim() {
                "pct"    => pct    = v.trim().parse().ok(),
                "mv"     => mv     = v.trim().parse().ok(),
                "hrs"    => hrs    = v.trim().parse().ok(),
                "status" => status = v.trim(),
                _        => {}
            }
        }
    }

    let pct = pct?;
    if pct < 0 { return None; }   // no battery connected — nothing to display
    let mv       = mv.unwrap_or(0);
    let charging = matches!(status, "charging" | "standby");
    Some(BatteryInfo { pct, mv, hrs, charging })
}
