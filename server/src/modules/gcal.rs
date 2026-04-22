use std::sync::Mutex;
use std::time::{Duration, Instant};
use chrono::{DateTime, FixedOffset, Local, Timelike};
use crate::font::{draw_text, measure_text};
use crate::image::{E6Canvas, E6Color, SCREEN_W};
use crate::gcal_creds::{CLIENT_ID, CLIENT_SECRET, REFRESH_TOKEN, CALENDAR_IDS};
use super::{Module, Rect};

const SIZE_PX:  f32 = 28.0;
const MARGIN:   i32 = 8;
const LINE_GAP: i32 = 4;
const Y_START:  i32 = 145;  // below clock + max rain block
const Y_END:    i32 = 428;  // above stock strip (SCREEN_H=480, STRIP_H=48, gap=4)

struct TokenCache {
    token:      String,
    expires_at: Instant,
}

#[derive(Clone)]
struct CalEvent {
    start_display: String,
    summary:       String,
    sort_key:      i32,   // minutes from midnight; -1 = all-day (sorts first)
}

pub struct GCalModule {
    events: Mutex<Vec<CalEvent>>,
    token:  Mutex<TokenCache>,
    client: reqwest::Client,
}

impl GCalModule {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent("PhotoPainter/1.0 (github.com/photopainter)")
            .timeout(Duration::from_secs(15))
            .build()
            .expect("failed to build HTTP client");
        Self {
            events: Mutex::new(Vec::new()),
            token:  Mutex::new(TokenCache {
                token:      String::new(),
                expires_at: Instant::now(),  // already expired → forces first refresh
            }),
            client,
        }
    }

    async fn access_token(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        {
            let t = self.token.lock().unwrap();
            if !t.token.is_empty() && t.expires_at > Instant::now() + Duration::from_secs(60) {
                return Ok(t.token.clone());
            }
        }
        let body = format!(
            "grant_type=refresh_token&client_id={}&client_secret={}&refresh_token={}",
            form_encode(CLIENT_ID), form_encode(CLIENT_SECRET), form_encode(REFRESH_TOKEN)
        );
        let resp: serde_json::Value = self.client
            .post("https://oauth2.googleapis.com/token")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send().await?.json().await?;
        let token      = resp["access_token"].as_str().ok_or("missing access_token")?.to_string();
        let expires_in = resp["expires_in"].as_u64().unwrap_or(3600);
        let mut t      = self.token.lock().unwrap();
        t.token      = token.clone();
        t.expires_at = Instant::now() + Duration::from_secs(expires_in);
        Ok(token)
    }

    pub async fn refresh(&self) {
        match self.fetch().await {
            Ok(events) => *self.events.lock().unwrap() = events,
            Err(e)     => tracing::warn!("gcal fetch failed: {e}"),
        }
    }

    async fn fetch(&self) -> Result<Vec<CalEvent>, Box<dyn std::error::Error + Send + Sync>> {
        let token    = self.access_token().await?;
        let now      = Local::now();
        let today    = now.date_naive();
        let tomorrow = today.succ_opt().unwrap_or(today);
        let tz       = now.format("%:z").to_string();
        let time_min = format!("{}T00:00:00{tz}", today.format("%Y-%m-%d"));
        let time_max = format!("{}T00:00:00{tz}", tomorrow.format("%Y-%m-%d"));

        let mut all: Vec<CalEvent> = Vec::new();

        for &cal_id in CALENDAR_IDS {
            let url = format!(
                "https://www.googleapis.com/calendar/v3/calendars/{}/events",
                percent_encode(cal_id)
            );
            let resp: serde_json::Value = self.client
                .get(&url)
                .query(&[
                    ("timeMin",      time_min.as_str()),
                    ("timeMax",      time_max.as_str()),
                    ("singleEvents", "true"),
                    ("orderBy",      "startTime"),
                ])
                .bearer_auth(&token)
                .send().await?.json().await?;

            let Some(items) = resp["items"].as_array() else { continue };

            for item in items {
                let summary = item["summary"].as_str().unwrap_or("(no title)").to_string();

                let (start_display, sort_key) =
                    if let Some(dt_str) = item["start"]["dateTime"].as_str() {
                        let parsed: DateTime<FixedOffset> = dt_str.parse()?;
                        let local = parsed.with_timezone(&Local);
                        let (_, h12) = local.hour12();
                        let ampm = if local.hour() < 12 { "AM" } else { "PM" };
                        let key  = local.hour() as i32 * 60 + local.minute() as i32;
                        (format!("{h12}:{:02} {ampm}", local.minute()), key)
                    } else if item["start"]["date"].as_str().is_some() {
                        ("All day".to_string(), -1)
                    } else {
                        continue;
                    };

                all.push(CalEvent { start_display, summary, sort_key });
            }
        }

        // All-day events (sort_key = -1) sort first, then chronological
        all.sort_by_key(|e| e.sort_key);
        // Remove exact duplicates that appear across multiple calendars
        all.dedup_by(|a, b| a.summary == b.summary && a.sort_key == b.sort_key);
        Ok(all)
    }
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' |
            b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn form_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' |
            b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl Module for GCalModule {
    fn render(&self, canvas: &mut E6Canvas, region: Rect) {
        let events  = self.events.lock().unwrap().clone();
        let ascent  = measure_text("A", SIZE_PX, false).1;
        let line_h  = ascent + LINE_GAP;
        let max_y   = region.y + Y_END;
        let mut y   = region.y + Y_START;

        if events.is_empty() {
            if y + ascent <= max_y {
                canvas.fill_rect(0, y, SCREEN_W, line_h, E6Color::Black);
                draw_text(canvas, region.x + MARGIN, y, "No events today.", SIZE_PX, E6Color::White, false);
            }
            return;
        }

        let now = Local::now();
        let current_minutes = now.hour() as i32 * 60 + now.minute() as i32;
        let mut found_next  = false;

        for event in &events {
            if y + ascent > max_y { break; }

            // Color scheme:
            //   all-day (sort_key = -1) or past timed → white on black
            //   next upcoming timed event             → yellow on blue
            //   further upcoming timed events         → white on blue
            let (text_color, bg_color) = if event.sort_key < 0 || event.sort_key < current_minutes {
                (E6Color::White, E6Color::Black)
            } else if !found_next {
                found_next = true;
                (E6Color::Yellow, E6Color::Blue)
            } else {
                (E6Color::White, E6Color::Blue)
            };

            let text = format!("{}  {}", event.start_display, event.summary);
            canvas.fill_rect(0, y, SCREEN_W, line_h, bg_color);
            draw_text(canvas, region.x + MARGIN, y, &text, SIZE_PX, text_color, false);
            y += line_h;
        }
    }

    fn data_refresh_interval(&self) -> Duration { Duration::from_secs(300) }
    fn suggested_poll_interval(&self) -> Option<Duration> { None }
}
