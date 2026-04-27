use std::sync::Mutex;
use std::time::{Duration, Instant};
use chrono::{DateTime, FixedOffset, Local, Timelike};
use crate::font::{draw_text, measure_text};
use crate::image::{E6Canvas, E6Color};
use crate::gcal_creds::{CLIENT_ID, CLIENT_SECRET, REFRESH_TOKEN, CALENDAR_IDS};
use super::{Module, Rect};
use super::rain;

const SIZE_PX:  f32 = 28.0;
const MARGIN:   i32 = 8;
const LINE_GAP: i32 = 4;
const Y_START:  i32 = rain::GCAL_Y_START;
const STALE_AFTER: Duration = Duration::from_secs(3600);

const AFTER_6PM_MINUTES:  i32 = 18 * 60;       // 6:00 PM
const HIDE_BEFORE_MINUTES: i32 = 12 * 60 + 1;  // 12:01 PM

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
    today:      Mutex<Vec<CalEvent>>,
    tomorrow:   Mutex<Vec<CalEvent>>,
    day_after:  Mutex<Vec<CalEvent>>,
    token:      Mutex<TokenCache>,
    client:     reqwest::Client,
    last_ok:    Mutex<Option<Instant>>,
}

impl GCalModule {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            today:     Mutex::new(Vec::new()),
            tomorrow:  Mutex::new(Vec::new()),
            day_after: Mutex::new(Vec::new()),
            token:     Mutex::new(TokenCache {
                token:      String::new(),
                expires_at: Instant::now(),  // already expired → forces first refresh
            }),
            client,
            last_ok: Mutex::new(None),
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
            Ok((today, tomorrow, day_after)) => {
                *self.today.lock().unwrap()     = today;
                *self.tomorrow.lock().unwrap()  = tomorrow;
                *self.day_after.lock().unwrap() = day_after;
                *self.last_ok.lock().unwrap()   = Some(Instant::now());
            }
            Err(e) => tracing::warn!("gcal fetch failed: {e}"),
        }
    }

    async fn fetch(&self) -> Result<(Vec<CalEvent>, Vec<CalEvent>, Vec<CalEvent>), Box<dyn std::error::Error + Send + Sync>> {
        let token      = self.access_token().await?;
        let now        = Local::now();
        let today      = now.date_naive();
        let tomorrow   = today.succ_opt().unwrap_or(today);
        let day_after  = tomorrow.succ_opt().unwrap_or(tomorrow);
        let two_after  = day_after.succ_opt().unwrap_or(day_after);
        let tz         = now.format("%:z").to_string();

        let today_events     = self.fetch_range(&token,
            &today.format("%Y-%m-%d").to_string(),
            &tomorrow.format("%Y-%m-%d").to_string(),
            &tz).await?;
        let tomorrow_events  = self.fetch_range(&token,
            &tomorrow.format("%Y-%m-%d").to_string(),
            &day_after.format("%Y-%m-%d").to_string(),
            &tz).await?;
        let day_after_events = self.fetch_range(&token,
            &day_after.format("%Y-%m-%d").to_string(),
            &two_after.format("%Y-%m-%d").to_string(),
            &tz).await?;

        Ok((today_events, tomorrow_events, day_after_events))
    }

    async fn fetch_range(
        &self,
        token:         &str,
        date_str:      &str,
        next_date_str: &str,
        tz:            &str,
    ) -> Result<Vec<CalEvent>, Box<dyn std::error::Error + Send + Sync>> {
        let time_min = format!("{date_str}T00:00:00{tz}");
        let time_max = format!("{next_date_str}T00:00:00{tz}");

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
                .bearer_auth(token)
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

        all.sort_by_key(|e| e.sort_key);
        all.dedup_by(|a, b| a.summary == b.summary && a.sort_key == b.sort_key);
        Ok(all)
    }
}

fn encode_bytes(s: &str, space_as_plus: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' |
            b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' if space_as_plus      => out.push('+'),
            _                          => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn percent_encode(s: &str) -> String { encode_bytes(s, false) }
fn form_encode(s: &str)    -> String { encode_bytes(s, true)  }

impl Module for GCalModule {
    fn render(&self, canvas: &mut E6Canvas, region: Rect) {
        let today_events     = self.today.lock().unwrap().clone();
        let tomorrow_events  = self.tomorrow.lock().unwrap().clone();
        let day_after_events = self.day_after.lock().unwrap().clone();
        let stale  = self.last_ok.lock().unwrap()
            .map(|t| t.elapsed() > STALE_AFTER)
            .unwrap_or(true);
        let ascent = measure_text("A", SIZE_PX, false).1;
        let line_h = ascent + LINE_GAP;
        let max_y  = region.y + region.height - 2 + line_h;
        let mut y  = region.y + Y_START;

        if stale {
            if y + ascent <= max_y {
                canvas.fill_rect(region.x, y, region.width, line_h, E6Color::Red);
                draw_text(canvas, region.x + MARGIN, y, "(calendar offline)", SIZE_PX, E6Color::White, false);
                y += line_h;
            }
        }

        let now             = Local::now();
        let current_minutes = now.hour() as i32 * 60 + now.minute() as i32;
        let after_6pm       = current_minutes >= AFTER_6PM_MINUTES;

        // After 6pm: hide timed events that started before 12:01pm
        let today_visible: Vec<&CalEvent> = today_events.iter()
            .filter(|e| !(after_6pm && e.sort_key >= 0 && e.sort_key < HIDE_BEFORE_MINUTES))
            .collect();

        if today_visible.is_empty() && tomorrow_events.is_empty() && day_after_events.is_empty() {
            if y + ascent <= max_y {
                canvas.fill_rect(region.x, y, region.width, line_h, E6Color::Black);
                draw_text(canvas, region.x + MARGIN, y, "No events today.", SIZE_PX, E6Color::White, false);
            }
            return;
        }

        let mut found_next = false;

        for event in &today_visible {
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
            canvas.fill_rect(region.x, y, region.width, line_h, bg_color);
            draw_text(canvas, region.x + MARGIN, y, &text, SIZE_PX, text_color, false);
            y += line_h;
        }

        // Tomorrow's events: black text on green background
        for event in &tomorrow_events {
            if y + ascent > max_y { break; }

            let text = format!("{}  {}", event.start_display, event.summary);
            canvas.fill_rect(region.x, y, region.width, line_h, E6Color::Green);
            draw_text(canvas, region.x + MARGIN, y, &text, SIZE_PX, E6Color::Black, false);
            y += line_h;
        }

        // Day-after-tomorrow's events: black text on yellow background
        for event in &day_after_events {
            if y + ascent > max_y { break; }

            let text = format!("{}  {}", event.start_display, event.summary);
            canvas.fill_rect(region.x, y, region.width, line_h, E6Color::Yellow);
            draw_text(canvas, region.x + MARGIN, y, &text, SIZE_PX, E6Color::Black, false);
            y += line_h;
        }
    }
}
