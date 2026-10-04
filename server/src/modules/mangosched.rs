//! mangoSched schedule module.
//!
//! Logs in to the mangoSched web app (the "kodaCal" instance) as a read-only viewer
//! account, scrapes the calendar grid, and draws each day as a column of colored shift
//! boxes: the day label on top, one box per shift below it, continuing into the next
//! column when a day has more boxes than fit.  Every new day starts a new column.
//!
//! Authentication is designed to need no upkeep:
//!   * credentials are read at runtime from `mangosched_auth.json` (git-ignored, 0600);
//!   * the session cookie is kept in memory and reused; mangoSched sessions last 30 days,
//!     after which the next fetch simply logs in again with the stored password;
//!   * after a rejected login the module stops trying for 30 minutes, so a changed or
//!     reset password can never trip mangoSched's account lockout.

use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::redirect::Policy;
use scraper::{ElementRef, Html, Selector};

use crate::font::{draw_text, measure_text};
use crate::image::{E6Canvas, E6Color};
use super::{Module, Rect};
use super::rain;

// ── Where and how to connect ──────────────────────────────────────────────────

const PUBLIC_HOST: &str = "bluekoda.duckdns.org";   // also the TLS identity
const LAN_HOST:    &str = "smartboardpc.lan";        // preferred route: same machine, no internet hop
const AUTH_FILE:   &str = "mangosched_auth.json";    // {"username": "...", "password": "..."}

const FETCH_OK_INTERVAL:    Duration = Duration::from_secs(10 * 60);
const FETCH_RETRY_INTERVAL: Duration = Duration::from_secs(2 * 60);
const AUTH_BACKOFF:         Duration = Duration::from_secs(30 * 60);
const STALE_AFTER:          Duration = Duration::from_secs(60 * 60);

// ── Layout ────────────────────────────────────────────────────────────────────

const COLS:        i32 = 5;      // columns across the screen
const COL_GAP:     i32 = 3;
const HEADER_H:    i32 = 29;     // day label bar
const HEADER_PX:   f32 = 19.0;
const BOX_PX:      f32 = 16.0;
const BOX_PAD_X:   i32 = 4;
const BOX_PAD_Y:   i32 = 1;
const BOX_GAP:     i32 = 2;
const BANNER_H:    i32 = 22;
const BANNER_PX:   f32 = 18.0;
const MAX_TYPE_LINES: usize = 3;

// ── Data ──────────────────────────────────────────────────────────────────────

/// The display's inks that a shift box can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ink { Blue, Red, Green, Yellow, Open }

#[derive(Clone, Debug, PartialEq)]
pub struct Shift {
    pub when: String,   // compact time range, e.g. "9:30a-12p"
    pub kind: String,   // shift type, e.g. "Conrad Companion"
    pub who:  String,   // worker short name, or "OPEN"
    pub ink:  Ink,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Day {
    pub offset: i32,        // days from today (0 = today), straight from mangoSched's own grid
    pub label:  String,     // "Sun Oct 4"
    pub shifts: Vec<Shift>,
}

enum Status {
    Pending,
    Ok,
    NotConfigured,
    AuthFailed,
    Offline,
}

enum FetchError {
    NotConfigured(String),
    Auth(String),
    Network(String),
    Layout(String),
}

struct State {
    days:         Vec<Day>,
    last_ok:      Option<Instant>,
    next_attempt: Option<Instant>,
    status:       Status,
}

#[derive(Clone)]
struct Session {
    client: reqwest::Client,
    base:   String,     // scheme://host[:port], no trailing slash
    lan:    bool,
}

pub struct MangoSchedModule {
    state:   Mutex<State>,
    session: Mutex<Option<Session>>,
}

// ── Parsing the calendar page ─────────────────────────────────────────────────

fn sel(s: &str) -> Selector { Selector::parse(s).expect("static selector") }

fn text_of(el: ElementRef) -> String {
    el.text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// "/?view=day&offset=-3" → -3
fn parse_offset(href: &str) -> Option<i32> {
    let rest = &href[href.find("offset=")? + 7..];
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '-')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// "Sun Oct 04" → "Sun Oct 4"
fn tidy_label(raw: &str) -> String {
    let mut parts: Vec<String> = raw.split_whitespace().map(str::to_string).collect();
    if let Some(last) = parts.last_mut() {
        if last.len() == 2 && last.starts_with('0') { *last = last[1..].to_string(); }
    }
    parts.join(" ")
}

/// "7:00am" → "7a", "12:30pm" → "12:30p".  None if the token is not a 12-hour time.
fn compact_time(tok: &str) -> Option<String> {
    let t = tok.to_ascii_lowercase();
    let (body, suffix) = if let Some(b) = t.strip_suffix("am") { (b, "a") }
                         else if let Some(b) = t.strip_suffix("pm") { (b, "p") }
                         else { return None };
    let (h, m) = match body.split_once(':') { Some((h, m)) => (h, m), None => (body, "00") };
    if h.is_empty() || h.len() > 2 || m.len() != 2
        || !h.chars().all(|c| c.is_ascii_digit()) || !m.chars().all(|c| c.is_ascii_digit()) { return None; }
    Some(if m == "00" { format!("{h}{suffix}") } else { format!("{h}:{m}{suffix}") })
}

/// "7:00am–9:30am" → "7a-9:30a";  "Oct 08 10:00pm → Oct 09 6:00am" → "10p→6a".
/// Anything unrecognised (e.g. a 24-hour display setting) is passed through unchanged.
pub fn compact_when(raw: &str) -> String {
    let flat: String = raw.chars().map(|c| if matches!(c, '–' | '—' | '→' | '-') { ' ' } else { c }).collect();
    let times: Vec<String> = flat.split_whitespace().filter_map(compact_time).collect();
    if times.len() == 2 {
        let sep = if raw.contains('→') { "→" } else { "-" };
        return format!("{}{}{}", times[0], sep, times[1]);
    }
    raw.trim().to_string()
}

fn ink_for(code: &str) -> Ink {
    match code {
        "open"                                   => Ink::Open,
        "red" | "pink" | "orange" | "brown"      => Ink::Red,
        "yellow"                                 => Ink::Yellow,
        "lime-green" | "dark-green"              => Ink::Green,
        _ /* blue, cyan, purple, lavender, ? */  => Ink::Blue,
    }
}

fn parse_shift(el: ElementRef) -> Option<Shift> {
    let code = el.value().classes().find_map(|c| c.strip_prefix("color-"))?.to_string();
    let when = el.select(&sel("span.when")).next().map(text_of)?;
    let kind = el.select(&sel("span.type")).next().map(text_of).unwrap_or_default();
    let who_el = el.select(&sel("span.who")).next();
    let open = code == "open" || who_el.map_or(false, |w| w.value().classes().any(|c| c == "open-label"));
    let who  = if open { "OPEN".to_string() } else { who_el.map(text_of).unwrap_or_default() };
    Some(Shift { when: compact_when(&when), kind, who, ink: if open { Ink::Open } else { ink_for(&code) } })
}

/// Parse mangoSched's calendar page into today's and upcoming days (past days are dropped).
pub fn parse_calendar(html: &str) -> Result<Vec<Day>, String> {
    let doc  = Html::parse_document(html);
    let grid = doc.select(&sel("table.calendar-grid")).next().ok_or("calendar grid not found")?;
    let (td, daynum, shift) = (sel("td"), sel("div.daynum a"), sel("div.shift"));
    let mut days: Vec<Day> = Vec::new();
    for cell in grid.select(&td) {
        let Some(a) = cell.select(&daynum).next() else { continue };
        let Some(offset) = parse_offset(a.value().attr("href").unwrap_or("")) else { continue };
        if offset < 0 || days.iter().any(|d| d.offset == offset) { continue; }
        days.push(Day {
            offset,
            label:  tidy_label(&text_of(a)),
            shifts: cell.select(&shift).filter_map(parse_shift).collect(),
        });
    }
    if days.is_empty() { return Err("calendar grid had no days".into()); }
    days.sort_by_key(|d| d.offset);
    Ok(days)
}

fn csrf_token(html: &str) -> Option<String> {
    Html::parse_document(html).select(&sel("input[name=\"gorilla.csrf.Token\"]")).next()
        .and_then(|i| i.value().attr("value")).map(str::to_string)
}

// ── Talking to mangoSched ─────────────────────────────────────────────────────

struct Creds { username: String, password: String }

fn load_creds() -> Result<Creds, FetchError> {
    let txt = std::fs::read_to_string(AUTH_FILE)
        .map_err(|e| FetchError::NotConfigured(format!("{AUTH_FILE}: {e}")))?;
    let v: serde_json::Value = serde_json::from_str(&txt)
        .map_err(|e| FetchError::NotConfigured(format!("{AUTH_FILE}: {e}")))?;
    match (v["username"].as_str(), v["password"].as_str()) {
        (Some(u), Some(p)) if !u.is_empty() && !p.is_empty() =>
            Ok(Creds { username: u.to_string(), password: p.to_string() }),
        _ => Err(FetchError::NotConfigured(format!("{AUTH_FILE} needs username and password"))),
    }
}

async fn new_session(prefer_lan: bool) -> Session {
    let mut builder = reqwest::Client::builder()
        .cookie_store(true)
        .redirect(Policy::none())
        .timeout(Duration::from_secs(15))
        .user_agent("PhotoPainter/1.0 (schedule display)");

    // Test hook: point at a development instance instead of production.
    if let Ok(base) = std::env::var("MANGOSCHED_BASE_URL") {
        return Session { client: builder.build().expect("http client"), base: base.trim_end_matches('/').to_string(), lan: false };
    }
    let mut lan = false;
    if prefer_lan {
        if let Ok(mut addrs) = tokio::net::lookup_host((LAN_HOST, 0)).await {
            if let Some(a) = addrs.find(|a| a.is_ipv4()) {
                // Keep the real hostname (for TLS and the virtual host), but connect to the LAN address.
                builder = builder.resolve(PUBLIC_HOST, SocketAddr::new(a.ip(), 0));
                lan = true;
            }
        }
    }
    Session { client: builder.build().expect("http client"), base: format!("https://{PUBLIC_HOST}"), lan }
}

enum Page { Calendar(String), Login, ForceChange }

async fn get_calendar(s: &Session) -> Result<Page, FetchError> {
    let resp = s.client.get(format!("{}/", s.base)).send().await
        .map_err(|e| FetchError::Network(format!("GET /: {e}")))?;
    let status = resp.status();
    let location = resp.headers().get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    if status.is_redirection() {
        return if location.contains("/force-password-change") { Ok(Page::ForceChange) }
               else if location.contains("/login")            { Ok(Page::Login) }
               else { Err(FetchError::Layout(format!("unexpected redirect to {location}"))) };
    }
    let body = resp.text().await.map_err(|e| FetchError::Network(format!("reading /: {e}")))?;
    if status.is_success() && body.contains("calendar-grid") { return Ok(Page::Calendar(body)); }
    Err(FetchError::Layout(format!("GET / returned http {status} without a calendar")))
}

async fn login(s: &Session, c: &Creds) -> Result<(), FetchError> {
    let page = s.client.get(format!("{}/login", s.base)).send().await
        .map_err(|e| FetchError::Network(format!("GET /login: {e}")))?
        .text().await.map_err(|e| FetchError::Network(format!("reading /login: {e}")))?;
    let token = csrf_token(&page).ok_or_else(|| FetchError::Layout("no CSRF token on the login page".into()))?;
    // gorilla/csrf's same-origin check over HTTPS wants Origin/Referer on the POST.
    let mut post = s.client.post(format!("{}/login", s.base));
    if s.base.starts_with("https://") {
        post = post.header("Origin", &s.base).header("Referer", format!("{}/login", s.base));
    }
    let resp = post
        .form(&[("gorilla.csrf.Token", token.as_str()), ("username", c.username.as_str()), ("password", c.password.as_str())])
        .send().await.map_err(|e| FetchError::Network(format!("POST /login: {e}")))?;
    match resp.status().as_u16() {
        302 | 303 => Ok(()),
        200       => Err(FetchError::Auth("login rejected (wrong or reset password, or locked account)".into())),
        429       => Err(FetchError::Network("login rate-limited".into())),
        other     => Err(FetchError::Layout(format!("POST /login returned http {other}"))),
    }
}

async fn fetch_with(s: &Session, c: &Creds) -> Result<Vec<Day>, FetchError> {
    let html = match get_calendar(s).await? {
        Page::Calendar(h) => h,
        Page::ForceChange => return Err(FetchError::Auth("account requires a password change".into())),
        Page::Login => {
            login(s, c).await?;
            match get_calendar(s).await? {
                Page::Calendar(h) => h,
                _ => return Err(FetchError::Auth("login did not produce a session".into())),
            }
        }
    };
    parse_calendar(&html).map_err(FetchError::Layout)
}

// ── Layout (pure, so it can be tested) ────────────────────────────────────────

pub struct Geometry { pub x: i32, pub top: i32, pub bottom: i32, pub width: i32 }

impl Geometry {
    fn col_w(&self) -> i32 { (self.width - (COLS - 1) * COL_GAP) / COLS }
    fn col_x(&self, col: i32) -> i32 { self.x + col * (self.col_w() + COL_GAP) }
}

pub struct Placed {
    pub col:   i32,
    pub day:   usize,
    pub shift: Option<usize>,   // None = the day header for this column
    pub cont:  bool,            // header of a continuation column
    pub y:     i32,
    pub h:     i32,
}

/// Lay days out column by column.  A day's boxes stack under its label; when the next box
/// would not fit, the day continues in the next column (label repeated).  The next day
/// always starts in a fresh column, and nothing is placed once the columns run out.  If the
/// columns run out in the middle of a day, the second value says which column and how many
/// of that day's shifts did not fit, so the caller can flag it rather than drop them silently.
pub fn layout(days: &[Day], geo: &Geometry, box_h: &dyn Fn(&Shift) -> i32) -> (Vec<Placed>, Option<(i32, usize)>) {
    let mut out = Vec::new();
    let mut cut = None;
    let mut col = 0;
    let first_y = geo.top + HEADER_H + BOX_GAP;
    'days: for (di, day) in days.iter().enumerate() {
        if col >= COLS { break; }
        out.push(Placed { col, day: di, shift: None, cont: false, y: geo.top, h: HEADER_H });
        let mut y = first_y;
        for (si, shift) in day.shifts.iter().enumerate() {
            let h = box_h(shift);
            if y + h > geo.bottom && y > first_y {
                col += 1;
                if col >= COLS { cut = Some((COLS - 1, day.shifts.len() - si)); break 'days; }
                out.push(Placed { col, day: di, shift: None, cont: true, y: geo.top, h: HEADER_H });
                y = first_y;
            }
            out.push(Placed { col, day: di, shift: Some(si), cont: false, y, h });
            y += h + BOX_GAP;
        }
        col += 1;
    }
    (out, cut)
}

// ── Drawing ───────────────────────────────────────────────────────────────────

fn fit(text: &str, max_chars: usize) -> String {
    let n = text.chars().count();
    if n <= max_chars { return text.to_string(); }
    if max_chars == 0 { return String::new(); }
    let mut s: String = text.chars().take(max_chars - 1).collect();
    s.push('…');
    s
}

/// Word-wrap `text` to at most `max_lines` lines of `max_chars`, ellipsizing what does not fit.
fn wrap(text: &str, max_chars: usize, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let candidate = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if candidate.chars().count() <= max_chars { cur = candidate; continue; }
        if !cur.is_empty() { lines.push(std::mem::take(&mut cur)); }
        cur = word.to_string();
    }
    if !cur.is_empty() { lines.push(cur); }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        let last = lines.pop().unwrap_or_default();
        lines.push(fit(&format!("{last}…"), max_chars));
    }
    lines.into_iter().map(|l| fit(&l, max_chars)).collect()
}

fn char_w() -> i32 { measure_text("M", BOX_PX, false).0.max(1) }
fn line_h() -> i32 { BOX_PX as i32 + 2 }

/// Text lines for one box: time and worker (together if they fit, else on two lines), then the
/// shift type (wrapped).
fn box_lines(shift: &Shift, inner_w: i32) -> Vec<String> {
    let max_chars = (inner_w / char_w()).max(4) as usize;
    let when = fit(&shift.when, max_chars);
    let mut lines = if shift.who.is_empty() {
        vec![when]
    } else if when.chars().count() + 1 + shift.who.chars().count() <= max_chars {
        vec![format!("{when} {}", shift.who)]            // "9a-3p Shay"
    } else {
        vec![when, fit(&shift.who, max_chars)]           // name too long to share the line
    };
    if !shift.kind.is_empty() { lines.extend(wrap(&shift.kind, max_chars, MAX_TYPE_LINES)); }
    lines
}

fn box_height(shift: &Shift, inner_w: i32) -> i32 {
    box_lines(shift, inner_w).len() as i32 * line_h() + 2 * BOX_PAD_Y
}

fn colors(ink: Ink) -> (E6Color, E6Color) {   // (background, text)
    match ink {
        Ink::Blue   => (E6Color::Blue,   E6Color::White),
        Ink::Red    => (E6Color::Red,    E6Color::White),
        Ink::Green  => (E6Color::Green,  E6Color::Black),
        Ink::Yellow => (E6Color::Yellow, E6Color::Black),
        Ink::Open   => (E6Color::White,  E6Color::Black),
    }
}

impl MangoSchedModule {
    pub fn new() -> Self {
        Self {
            state:   Mutex::new(State { days: Vec::new(), last_ok: None, next_attempt: None, status: Status::Pending }),
            session: Mutex::new(None),
        }
    }

    #[cfg(test)]
    pub fn with_days(days: Vec<Day>) -> Self {
        let m = Self::new();
        {
            let mut st = m.state.lock().unwrap();
            st.days = days;
            st.last_ok = Some(Instant::now());
            st.status = Status::Ok;
        }
        m
    }

    async fn fetch(&self) -> Result<Vec<Day>, FetchError> {
        let creds = load_creds()?;
        let cached = self.session.lock().unwrap().clone();
        let sess = match cached { Some(s) => s, None => new_session(true).await };
        let mut result = fetch_with(&sess, &creds).await;
        let mut used = sess.clone();
        // The LAN route failing at the network level is not proof the service is down: retry
        // once through the public name before giving up.
        if sess.lan && matches!(result, Err(FetchError::Network(_))) {
            let public = new_session(false).await;
            result = fetch_with(&public, &creds).await;
            used = public;
        }
        if !matches!(result, Err(FetchError::Network(_))) { *self.session.lock().unwrap() = Some(used); }
        else { *self.session.lock().unwrap() = None; }   // rebuild (and re-resolve the LAN address) next time
        result
    }

    /// Refresh the schedule if due.  Cheap to call every minute; it throttles itself.
    pub async fn refresh(&self) {
        if let Some(next) = self.state.lock().unwrap().next_attempt {
            if Instant::now() < next { return; }
        }
        let result = self.fetch().await;
        let now = Instant::now();
        let mut st = self.state.lock().unwrap();
        match result {
            Ok(days) => {
                tracing::info!(days = days.len(), shifts = days.iter().map(|d| d.shifts.len()).sum::<usize>(), "schedule refreshed");
                st.days = days;
                st.last_ok = Some(now);
                st.status = Status::Ok;
                st.next_attempt = Some(now + FETCH_OK_INTERVAL);
            }
            Err(FetchError::NotConfigured(m)) => {
                tracing::warn!("schedule not configured: {m}");
                st.status = Status::NotConfigured;
                st.next_attempt = Some(now + AUTH_BACKOFF);
            }
            Err(FetchError::Auth(m)) => {
                tracing::warn!("schedule login failed: {m}; not retrying for 30 minutes");
                st.status = Status::AuthFailed;
                st.next_attempt = Some(now + AUTH_BACKOFF);
            }
            Err(FetchError::Network(m)) | Err(FetchError::Layout(m)) => {
                tracing::warn!("schedule fetch failed: {m}");
                st.status = Status::Offline;
                st.next_attempt = Some(now + FETCH_RETRY_INTERVAL);
            }
        }
    }
}

impl Module for MangoSchedModule {
    fn render(&self, canvas: &mut E6Canvas, region: Rect) {
        let (days, banner) = {
            let st = self.state.lock().unwrap();
            let stale = st.last_ok.map_or(true, |t| t.elapsed() > STALE_AFTER);
            let banner = match &st.status {
                Status::NotConfigured => Some("(schedule not configured)"),
                Status::AuthFailed => Some("(schedule login failed)"),
                _ if stale            => Some("(schedule offline)"),
                _                     => None,
            };
            (st.days.clone(), banner)
        };

        let mut top = region.y + rain::GCAL_Y_START;
        if let Some(text) = banner {
            canvas.fill_rect(region.x, top, region.width, BANNER_H, E6Color::Red);
            draw_text(canvas, region.x + 8, top + 1, text, BANNER_PX, E6Color::White, false);
            top += BANNER_H + BOX_GAP;
        }
        let geo = Geometry { x: region.x, top, bottom: region.y + region.height - 2, width: region.width };
        let inner_w = geo.col_w() - 2 * BOX_PAD_X;
        let (placed, cut) = layout(&days, &geo, &|s| box_height(s, inner_w));
        let col_w = geo.col_w();

        for p in &placed {
            let x = geo.col_x(p.col);
            let day = &days[p.day];
            match p.shift {
                None => {
                    let today = day.offset == 0;
                    let label = fit(&if p.cont { format!("{} ›", day.label) } else { day.label.clone() }, (col_w / measure_text("M", HEADER_PX, true).0.max(1)) as usize);
                    if today {
                        canvas.fill_rect(x, p.y, col_w, p.h, E6Color::Black);
                        canvas.fill_rect(x + 2, p.y + 2, col_w - 4, p.h - 4, E6Color::White);
                        draw_text(canvas, x + 6, p.y + 3, &label, HEADER_PX, E6Color::Black, true);
                    } else {
                        canvas.fill_rect(x, p.y, col_w, p.h, E6Color::Black);
                        draw_text(canvas, x + 6, p.y + 3, &label, HEADER_PX, E6Color::White, false);
                    }
                    if let Some((cut_col, n)) = cut {
                        if cut_col == p.col {
                            let tag = format!("+{n}");
                            let w = measure_text(&tag, HEADER_PX, true).0;
                            draw_text(canvas, x + col_w - w - 6, p.y + 3, &tag, HEADER_PX, if today { E6Color::Black } else { E6Color::White }, true);
                        }
                    }
                    if day.shifts.is_empty() {
                        draw_text(canvas, x + 6, p.y + p.h + 4, "no shifts", BOX_PX, E6Color::Black, false);
                    }
                }
                Some(si) => {
                    let shift = &day.shifts[si];
                    let (bg, fg) = colors(shift.ink);
                    if shift.ink == Ink::Open {
                        canvas.fill_rect(x, p.y, col_w, p.h, E6Color::Black);
                        canvas.fill_rect(x + 1, p.y + 1, col_w - 2, p.h - 2, bg);
                    } else {
                        canvas.fill_rect(x, p.y, col_w, p.h, bg);
                    }
                    for (i, line) in box_lines(shift, inner_w).iter().enumerate() {
                        draw_text(canvas, x + BOX_PAD_X, p.y + BOX_PAD_Y + i as i32 * line_h(), line, BOX_PX, fg, false);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../testdata/mangosched_calendar.html");

    #[test]
    fn compacts_times() {
        assert_eq!(compact_when("7:00am–9:30am"), "7a-9:30a");
        assert_eq!(compact_when("12:00pm–3:30pm"), "12p-3:30p");
        assert_eq!(compact_when("10:30am–12:30pm"), "10:30a-12:30p");
        assert_eq!(compact_when("Oct 08 10:00pm → Oct 09 6:00am"), "10p→6a");
        assert_eq!(compact_when("07:00–09:30"), "07:00–09:30");   // 24-hour display: passed through
    }

    #[test]
    fn maps_colors_to_inks() {
        for (c, i) in [("blue", Ink::Blue), ("cyan", Ink::Blue), ("purple", Ink::Blue), ("lavender", Ink::Blue),
                       ("red", Ink::Red), ("pink", Ink::Red), ("orange", Ink::Red), ("brown", Ink::Red),
                       ("yellow", Ink::Yellow), ("lime-green", Ink::Green), ("dark-green", Ink::Green), ("open", Ink::Open)] {
            assert_eq!(ink_for(c), i, "{c}");
        }
    }

    #[test]
    fn parses_fixture() {
        let days = parse_calendar(FIXTURE).unwrap();
        assert_eq!(days[0].offset, 0, "past days are dropped; today first");
        assert_eq!(days[0].label.split(' ').count(), 3);
        assert!(!days[0].label.contains(" 0"), "leading zero stripped: {}", days[0].label);
        assert!(days.windows(2).all(|w| w[0].offset < w[1].offset));
        let total: usize = days.iter().map(|d| d.shifts.len()).sum();
        assert!(total > 30, "expected the seeded shifts, got {total}");
        assert!(days.iter().flat_map(|d| &d.shifts).any(|s| s.ink == Ink::Open && s.who == "OPEN"));
        assert!(days.iter().flat_map(|d| &d.shifts).any(|s| s.when.contains('→')), "overnight shift present");
        assert!(days.iter().any(|d| d.shifts.is_empty()), "an empty day is kept");
    }

    #[test]
    fn rejects_non_calendar_pages() {
        assert!(parse_calendar("<html><body>please log in</body></html>").is_err());
    }

    fn day(off: i32, n: usize) -> Day {
        Day { offset: off, label: format!("D{off}"), shifts: (0..n).map(|i| Shift {
            when: "9a-12p".into(), kind: format!("T{i}"), who: "Jo".into(), ink: Ink::Blue }).collect() }
    }

    #[test]
    fn layout_rules() {
        let geo = Geometry { x: 0, top: 128, bottom: 428, width: 800 };
        let h = |_: &Shift| 40;                       // (428-128-24-2) / 42 = 6 boxes per column
        let days = vec![day(0, 3), day(1, 9), day(2, 0), day(3, 2), day(4, 1), day(5, 1), day(6, 1), day(7, 1), day(8, 1)];
        let (placed, cut) = layout(&days, &geo, &h);

        // each day starts in a fresh column, and a column never mixes days
        let mut col_day = std::collections::HashMap::new();
        for p in &placed { assert_eq!(*col_day.entry(p.col).or_insert(p.day), p.day, "column {} mixes days", p.col); }
        // day 1's 9 boxes overflow: 6 + 3 across two columns, label repeated as a continuation
        let d1: Vec<_> = placed.iter().filter(|p| p.day == 1 && p.shift.is_some()).collect();
        assert_eq!(d1.len(), 9);
        assert_eq!(d1.iter().map(|p| p.col).collect::<std::collections::BTreeSet<_>>().len(), 2);
        assert!(placed.iter().any(|p| p.day == 1 && p.shift.is_none() && p.cont));
        // days are placed until there is no room for another column
        let mut used = 0; let mut expected_last = 0;
        for (i, d) in days.iter().enumerate() {
            let need = ((d.shifts.len() as i32 + 5) / 6).max(1);   // 6 boxes per column
            if used + need > COLS { break; }
            used += need; expected_last = i;
        }
        assert_eq!(placed.iter().map(|p| p.day).max().unwrap(), expected_last);
        assert!(placed.iter().all(|p| p.col < COLS));
        // nothing below the bottom
        assert!(placed.iter().all(|p| p.y + p.h <= geo.bottom));
        assert!(cut.is_none(), "nothing was cut mid-day in this scenario");
    }

    #[test]
    fn flags_shifts_that_do_not_fit() {
        let geo = Geometry { x: 0, top: 128, bottom: 428, width: 800 };
        // 6 boxes per column; one day with 6 * COLS + 4 shifts cannot fit in COLS columns
        let n = 6 * COLS as usize + 4;
        let (placed, cut) = layout(&[day(0, n)], &geo, &|_| 40);
        assert_eq!(placed.iter().filter(|p| p.shift.is_some()).count(), 6 * COLS as usize);
        assert_eq!(cut, Some((COLS - 1, 4)));
    }

    #[test]
    fn oversized_box_still_terminates() {
        let geo = Geometry { x: 0, top: 128, bottom: 200, width: 800 };
        let (placed, _) = layout(&[day(0, 3)], &geo, &|_| 500);
        assert!(placed.iter().filter(|p| p.shift.is_some()).count() <= 3);
    }

    #[test]
    fn text_fitting() {
        assert_eq!(fit("Conrad Companion", 8), "Conrad …");
        assert_eq!(wrap("Conrad Personal Supports", 15, 2), vec!["Conrad Personal".to_string(), "Supports".to_string()]);
        assert_eq!(wrap("A B C D E F G H", 3, 2).len(), 2);
    }

    // ── Live tests: ignored by default; run with `cargo test --release live -- --ignored --nocapture`

    /// Real login against production using server/mangosched_auth.json.  Prints counts only.
    #[tokio::test]
    #[ignore]
    async fn live_production_fetch() {
        let m = MangoSchedModule::new();
        let days = match m.fetch().await { Ok(d) => d, Err(e) => panic!("fetch failed: {}", match e {
            FetchError::NotConfigured(s) | FetchError::Auth(s) | FetchError::Network(s) | FetchError::Layout(s) => s }) };
        let shifts: usize = days.iter().map(|d| d.shifts.len()).sum();
        println!("production: {} days ({}..{}), {} shifts, {} open, route lan={}",
            days.len(), days.first().map(|d| d.label.as_str()).unwrap_or("-"), days.last().map(|d| d.label.as_str()).unwrap_or("-"),
            shifts, days.iter().flat_map(|d| &d.shifts).filter(|s| s.ink == Ink::Open).count(),
            m.session.lock().unwrap().as_ref().map_or(false, |s| s.lan));
        assert!(days[0].offset == 0);
        let module = MangoSchedModule::with_days(days.clone());
        let mut canvas = E6Canvas::new(E6Color::White);
        module.render(&mut canvas, crate::renderer::gcal_region());
        std::fs::write("/tmp/ms_live_preview.png", crate::packed_to_png(&canvas.pack())).unwrap();
        // second fetch reuses the cached session (no new login): must still work
        assert!(m.fetch().await.is_ok());
        // dropping the session (as after the 30-day expiry) transparently logs in again
        *m.session.lock().unwrap() = None;
        assert!(m.fetch().await.is_ok());
        println!("session reuse and re-login: ok");
    }

    /// Against a dev instance (MANGOSCHED_BASE_URL) with a WRONG password: refresh() must make
    /// exactly one login attempt, then back off instead of retrying.
    #[tokio::test]
    #[ignore]
    async fn live_wrong_password_backs_off() {
        let dir = std::env::temp_dir().join("ms_badpw_test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(AUTH_FILE), r#"{"username":"display","password":"definitely-wrong"}"#).unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let m = MangoSchedModule::new();
        m.refresh().await;
        let kind = match &m.state.lock().unwrap().status { Status::AuthFailed => "AuthFailed", Status::Offline => "Offline", Status::NotConfigured => "NotConfigured", Status::Pending => "Pending", Status::Ok => "Ok" };
        assert_eq!(kind, "AuthFailed", "unexpected status after a wrong password");
        for _ in 0..5 { m.refresh().await; }          // inside the back-off window: no further attempts
        println!("wrong password: 1 attempt, then backed off (check failed_login_attempts == 1)");
    }
}
