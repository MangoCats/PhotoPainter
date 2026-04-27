# PhotoPainter System Design

## Overview

Two independent projects share this repository:

1. **`firmware/`** — ESP32-S3 firmware for the PhotoPainter device. Wakes from deep sleep, polls a local server for a new dashboard image, updates the e-paper display if the image has changed, then sleeps again for a server-specified interval.

2. **`server/`** — Rust web server running on a local machine (PC, NAS, Raspberry Pi, etc.). Composes a multi-module dashboard image in the PhotoPainter's native 4bpp E6 format and serves it on demand.

---

## Repository Structure

```
PhotoPainter/
├── DESIGN.md                   ← this document
├── LESSONS_LEARNED.md          ← hardware bring-up findings
├── .gitignore                  ← excludes location.rs, gcal_creds.rs, stock_creds.rs
├── scratch/                    ← bring-up and test sketches
├── firmware/
│   ├── platformio.ini
│   ├── include/
│   │   ├── config.h            ← WiFi credentials, server URL, poll timing, pin assignments
│   │   └── version.h           ← firmware version string (set by build script)
│   └── src/
│       └── main.cpp
└── server/
    ├── Cargo.toml
    ├── stock_tickers.txt       ← editable ticker list, one symbol per line (read at launch)
    └── src/
        ├── main.rs             ← HTTP server, render loop, significant-change detection
        ├── renderer.rs         ← composes modules into final image
        ├── image.rs            ← E6Canvas pixel buffer and palette
        ├── font.rs             ← fontdue TTF rasterization (JetBrains Mono)
        ├── location.rs         ← LAT/LON constants (gitignored, not in repo)
        ├── gcal_creds.rs       ← Google Calendar OAuth credentials (gitignored)
        ├── stock_creds.rs      ← Finnhub API key (gitignored)
        ├── teller_creds.rs     ← Teller.io access token + account ID (gitignored)
        └── modules/
            ├── mod.rs          ← Module trait definition
            ├── clock.rs        ← date and time display
            ├── weather.rs      ← NWS current temperature + H/L forecast + 84px weather icons
            ├── rain.rs         ← NWS QPF rain forecast
            ├── gcal.rs         ← Google Calendar: today + tomorrow + day-after-tomorrow
            ├── bank.rs         ← Teller.io balance + recent transactions
            └── stock.rs        ← Finnhub stock quotes
```

---

## Communication Protocol

The device always initiates contact (server never pushes). All communication is plain HTTP on the local network.

### Poll Request

```
GET /api/image HTTP/1.1
Host: homeassistant.lan:7654
X-Device-ID: e8:f6:0a:8f:03:6c
X-Firmware-Version: <git-hash>
X-Battery: pct=87, mv=3954, hrs=14.2, status=discharging
If-None-Match: "<sha256-hex>"
```

- `X-Device-ID`: device MAC address, used for logging.
- `X-Firmware-Version`: firmware git hash. A change triggers an immediate server re-render.
- `X-Battery`: battery status sampled once per wake cycle before HTTP. See [Battery Status Reporting](#battery-status-reporting) for field definitions, estimation algorithm, and omission rules.
- `If-None-Match`: ETag from the last successful 200 response. Omitted on first boot or after a full reset (RTC memory cleared).

### Server Responses

**New image available (`200 OK`):**
```
HTTP/1.1 200 OK
Content-Type: application/octet-stream
ETag: "<sha256-hex>"
X-Poll-Interval: 60
X-Server-Time: 1745123456
Cache-Control: no-store
[192,000 bytes of raw 4bpp E6 pixel data]
```

**No change (`304 Not Modified`):**
```
HTTP/1.1 304 Not Modified
ETag: "<sha256-hex>"
X-Poll-Interval: 60
X-Server-Time: 1745123456
Cache-Control: no-store
```

**`X-Poll-Interval`** — seconds until the device should poll again.
- **11:00 PM – 5:45 AM:** 3600 s (overnight; device wakes once per hour).
- **5:45 AM – 6:45 AM:** exact seconds remaining until 6:45 AM (one final long sleep that lands precisely at wake-up time).
- **6:45 AM – 11:00 PM:** 60 s (daytime; normal 1-minute cadence).
- Device clamps received value to [60, 3600]; stores in RTC memory.

**`X-Server-Time`** — Unix timestamp (UTC seconds) at response generation time.
- Present on every response.
- Device updates its RTC if the difference exceeds 30 seconds. The server is the sole time authority; no NTP client is needed on the firmware.

**Non-2xx/304 response:** device uses its stored poll interval unchanged; RTC is not updated. HTTP error code blinked on the red LED (blink count = HTTP status ÷ 100).

### Image Format

Raw pixel data, 192,000 bytes. 4 bits per pixel, 2 pixels per byte.
- High nibble = left pixel, low nibble = right pixel.
- Row-major, top-to-bottom, left-to-right.
- Dimensions: 800 × 480 pixels.

E6 palette (empirically confirmed for this panel):

| Value | Color  |
|-------|--------|
| 0x0   | Black  |
| 0x1   | White  |
| 0x2   | Yellow |
| 0x3   | Red    |
| 0x5   | Blue   |
| 0x6   | Green  |

Values 0x4 and 0x7 are invalid for this panel and must not be used.

---

## Firmware Design (`firmware/`)

### Hardware Reference

See `LESSONS_LEARNED.md` for the full pin map, AXP2101 init sequence, and EPD driver details. Key facts:

- **EPD SPI (bit-banged):** SCK=10, MOSI=11, CS=9, DC=8, RST=12, BUSY=13, PWR=6
- **AXP2101 (I2C):** SDA=47, SCL=48, addr=0x34
- **LEDs:** GPIO 45 (red), GPIO 42 (green) — both **active-low** (HIGH=off, LOW=on)
- **BUSY signal:** HIGH = idle, LOW = working

### LED Behavior

Both LEDs are active-low. The red LED uses PWM (`analogWrite`); the green LED is digital-only.

| State | Red duty | Green |
|-------|----------|-------|
| Idle (between polls) | 249 (≈2% on, dim heartbeat) | HIGH (off) |
| Active (WiFi, HTTP, EPD) | 0 (fully on) | HIGH (off) |
| Error blink | 0/255 alternating | HIGH (off) |
| Green error blink | — | LOW/HIGH alternating |

### Persistent State (RTC Memory)

Survives deep sleep; lost on full power-off or battery removal.

```c
RTC_DATA_ATTR char     s_etag[128]     = "";   // last received ETag
RTC_DATA_ATTR uint32_t s_poll_interval = 60;   // seconds between polls
```

On cold boot both fall back to safe defaults: unconditional poll at the default interval.

### Main Loop

```
Wake from deep sleep (or loop() iteration in DEBUG_NO_SLEEP mode)
│
├─ leds_active()        — full red: about to do network work
├─ Init AXP2101         — enable all power rails at 3.3V
│                         enable battery detection and voltage ADC channels
├─ Sample battery       — getBatteryPercent(), getBattVoltage(), charge-state flags
│                         compute hrs estimate if discharging; build X-Battery value
├─ Connect WiFi
│   └─ Timeout 10 s → blink red ×5, sleep/delay poll_interval, return
│
├─ HTTP GET /api/image
│   ├─ Send X-Device-ID, X-Firmware-Version, X-Battery
│   ├─ Send If-None-Match (if ETag cached)
│   ├─ Read X-Server-Time → sync RTC if delta > 30 s
│   ├─ Read X-Poll-Interval → clamp [60, 3600] → store in RTC
│   │
│   ├─ 200 OK → stream 192,000 bytes directly to EPD (no MCU-side buffer)
│   │   ├─ epd_init()
│   │   ├─ Write pixel data via SPI while receiving from WiFi
│   │   ├─ epd_refresh() — power on, trigger, wait BUSY, power off
│   │   └─ Store new ETag in RTC
│   │
│   ├─ 304 Not Modified → no display update
│   │
│   └─ Error → blink red (count = status ÷ 100), keep old poll interval
│
├─ WiFi disconnect
├─ leds_idle()          — dim red heartbeat
└─ Deep sleep for poll_interval seconds
   (or delay() if DEBUG_NO_SLEEP = true)
```

**Key implementation detail:** the HTTP body is streamed directly to the EPD over SPI as bytes arrive from the socket. No 192 KB frame buffer is allocated on the MCU. The EPD's internal buffer accumulates the data; the refresh command is sent only after all bytes have been written.

### Battery Status Reporting

The firmware must sample the AXP2101 PMIC once per wake cycle — after `pmic_init()` and before the HTTP request — and report the results in the `X-Battery` request header.

#### Header Format

```
X-Battery: pct=<0–100>, mv=<millivolts>, hrs=<decimal>, status=<token>
```

All fields are always present except `hrs`, which is omitted when an estimate is not meaningful (see below). Field order is fixed; values are integers except `hrs` (one decimal place).

| Field | Source | Description |
|-------|--------|-------------|
| `pct` | `getBatteryPercent()` | State of charge, 0–100. Reports `-1` when no battery is detected. |
| `mv` | `getBattVoltage()` | Battery terminal voltage in millivolts. Reports `0` when no battery is detected. Both `enableBattDetection()` and `enableBattVoltageMeasure()` must be called during `pmic_init()` before this is valid. |
| `hrs` | Computed (see below) | Estimated remaining hours on current charge. Omitted when `status` is `charging`, `standby`, or `no-battery`. |
| `status` | Derived from PMIC flags | One of the tokens defined below. |

#### Status Tokens

| Token | Condition |
|-------|-----------|
| `charging` | `isCharging()` is true (USB present, battery charging) |
| `discharging` | `isDischarge()` is true (running on battery) |
| `standby` | `isStandby()` is true (USB present, battery full or charge paused) |
| `no-battery` | `isBatteryConnect()` is false |

These states are mutually exclusive. If the PMIC returns an unexpected combination, report the first matching token in the order listed above.

#### Remaining-Life Estimation

The `hrs` field is computed only when `status=discharging`:

```
hrs = (pct / 100.0) × BATTERY_CAPACITY_MAH / AVG_DISCHARGE_MA
```

Both constants must be defined in `config.h`:

```c
#define BATTERY_CAPACITY_MAH   2000u   // rated cell capacity in mAh
#define AVG_DISCHARGE_MA          6u   // empirical average; see power budget
```

`AVG_DISCHARGE_MA` should reflect observed consumption at the configured poll interval (see Power Budget table). At a 60-second interval with infrequent display updates the baseline is ~5 mAh/hr; 6 mA is a reasonable starting conservative default. Users must calibrate this for their battery and usage pattern.

**Accuracy caveats:** The AXP2101 fuel gauge (`getBatteryPercent()`) is coulomb-counter based and requires a full charge/discharge cycle to calibrate. The percent value is unreliable immediately after power-on or battery insertion. The `hrs` estimate additionally depends on `AVG_DISCHARGE_MA` being representative of actual load, which varies with display update frequency, WiFi signal strength, and temperature.

#### Server Requirements

The server must:
1. Parse `X-Battery` from every incoming `GET /api/image` request.
2. Log all four fields (or three when `hrs` is absent) at `INFO` level alongside the existing device-ID and response-code log entry.
3. Make the parsed values available to future server features (e.g., low-battery display indicator) without requiring a protocol change.

No display change is required at this time.

---

### Configuration (`firmware/include/config.h`)

```c
#define WIFI_SSID                "..."
#define WIFI_PASSWORD            "..."
#define SERVER_URL               "http://homeassistant.lan:7654/api/image"
#define DEFAULT_POLL_INTERVAL_SEC  60u
#define MIN_POLL_INTERVAL_SEC      60u
#define MAX_POLL_INTERVAL_SEC    3600u
#define WIFI_CONNECT_TIMEOUT_MS  10000u
#define HTTP_TIMEOUT_MS           8000u
#define BATTERY_CAPACITY_MAH     2000u  // rated cell capacity; calibrate per battery
#define AVG_DISCHARGE_MA            6u  // average load at configured poll interval
```

Credentials are compile-time constants in `config.h`. There is no runtime provisioning.

### Power Budget per Wake Cycle (no display update)

| Phase | Current | Duration | Energy |
|---|---|---|---|
| Boot + AXP init | 80 mA | 0.5 s | 0.011 mAh |
| WiFi connect | 200 mA | 1.0 s | 0.056 mAh |
| HTTP GET + 304 | 100 mA | 0.5 s | 0.014 mAh |
| WiFi disconnect | 50 mA | 0.2 s | 0.003 mAh |
| **Total per cycle** | | **~2.2 s** | **~0.084 mAh** |

At a 60-second poll interval: 60 cycles/hr × 0.084 mAh = **~5 mAh/hr** baseline (without display updates).

---

## Server Design (`server/`)

### Technology Stack

- **Language:** Rust
- **HTTP framework:** `axum` (async, `tokio` runtime)
- **Font rendering:** `fontdue` TTF rasterizer with JetBrains Mono Regular and Bold
- **Image composition:** direct E6 pixel buffer — no RGB intermediary, no dithering
- **External data:** `reqwest` with `rustls-tls` (no OpenSSL dependency)
- **Time:** `chrono` for date/time formatting; `std::time::Instant` for token expiry
- **Hashing:** `sha2` (SHA-256) for ETag generation

### Render Architecture

The server does **not** re-render on every poll. Instead, a background task (`render_loop`) wakes every 60 seconds, refreshes all data modules in parallel, and checks whether any significant change has occurred. If so, it fetches fresh stock data and produces a new image; otherwise the cached image is served as-is.

```
render_loop (every 60 s):
  if bank_mode:
    tokio::join!(weather, rain, gcal, bank).refresh()  ← bank throttled to 1/20min
    bank_changed = bank returned new data
  else:
    tokio::join!(weather, rain, gcal).refresh()
    bank_changed = false

  if bank_changed OR significant_change:
    if not weekend AND not bank_mode:
      stock.refresh()        ← only when render is already happening
    image = render(modules)
    store image + ETag
```

Significant changes that trigger a re-render:
- More than 60 minutes since last render
- Current temperature changes ≥ 2°F
- Forecast high or low changes ≥ 3°F
- Near-term rain status (≤ 6-hour window) changes between None / Active / Imminent
- Bank balance or transactions changed (bank mode only)

Stock data is **only fetched when a render is already being triggered** by one of the above conditions. Stock changes do not trigger renders on their own. Stock is not fetched or displayed during bank mode or on weekends.

### Module Trait

```rust
pub trait Module: Send + Sync {
    fn render(&self, canvas: &mut E6Canvas, region: Rect);
}
```

Modules receive a `Rect` region from the renderer. Most modules receive `full_screen()` and self-manage their coordinates internally. The GCal module uses `region.y` as a vertical offset to position itself below the bank block when bank mode is active, and `region.height` to determine how many lines fit.

### Image Pipeline

```
Data modules refresh in parallel (bank throttled to 1/20 min)
        │
        ▼
Each module renders into E6Canvas [u8; 384000] (one byte per pixel)
        │
        ▼
Bottom 48px: stock strip (normal mode only) or version bar (first render)
        │
        ▼
Pack to 4bpp → [u8; 192000]
        │
        ▼
SHA-256 → ETag (full 64-hex-char digest)
        │
        ▼
Cache; serve on next GET or 304 if ETag matches
```

### E6 Color Palette

```rust
pub enum E6Color {
    Black  = 0x0,
    White  = 0x1,
    Yellow = 0x2,
    Red    = 0x3,
    Blue   = 0x5,
    Green  = 0x6,
}
```

Values 0x4 and 0x7 render as dark brown/purple on this panel and are excluded.

---

## Screen Layout

All coordinates are pixels from top-left (0,0). Screen is 800 × 480 px landscape.

### Normal mode (weekdays 8:00 AM – 3:00 PM)

```
y=0   ┌──────────────────────────────────────────────────────────────────────┐
      │ [Clock] Tuesday, April 21st, 2026 8:15:30 PM        [Weather]        │
y=4   │  24px black, left-margin=4                          Current: 96px    │
      │                                                      bold green, R-  │
      │ [Rain] 0.04 in/hr rain to start in 3.5 hours.       justified        │
y≈26  │  28px blue, left-justified, max 500px wide           H/L: 43px green │
      │                                            [84px weather icon, R-just]│
y=128 ├── Google Calendar ────────────────────────────────────────────────────┤
      │  8:30 AM  Dentist appointment        ← past/all-day: white on black   │
      │  All day  School holiday             ← next upcoming: yellow on blue  │
      │  2:00 PM  Team standup               ← further upcoming: white on blue│
      │  [tomorrow's events]                 ← black on green                 │
      │  [day-after events]                  ← black on yellow                │
      │  ...up to ~13 lines...                                                │
y=430 ├───────────────────────────────────────────────────────────────────────┤
      │  2px gap                                                              │
y=432 ├── Stock Strip (48px) ─────────────────────────────────────────────────┤
      │  ▓▓▓ MDT ▓▓▓│▓▓▓ RKLB ▓▓▓│▓▓▓ TSLA ▓▓▓│▓▓▓ BRK.B ▓▓▓              │
      │  green=up/flat, red=down vs open; white text; 5px white dividers      │
y=480 └───────────────────────────────────────────────────────────────────────┘
```

### Bank mode (weekends all day; weekdays 3:00 PM – 8:00 AM; or BANK_MODE=1)

```
y=0   ┌──────────────────────────────────────────────────────────────────────┐
      │ [Clock]                                             [Weather]         │
      │ [Rain]                                              [84px icon]       │
      │                                                                       │
y=96  │ ████████████████ Balance: $1,234.56 ████  ← black on yellow, 40% wide│
y=128 ├── Bank Transactions (5 lines, 16px font) ──────────────────────────── │
      │  04/27  -$42.10  P  Amazon                 ← white on green          │
      │  04/26  -$8.50      Starbucks               ← white on green          │
      │  ...up to 5 transactions...                                           │
      ├── Google Calendar ────────────────────────────────────────────────────┤
      │  [today's events]  [tomorrow's events]  [day-after events]            │
      │  ...up to ~13 lines (no stock strip at bottom)...                     │
y=480 └───────────────────────────────────────────────────────────────────────┘
```

**Balance line:** 28px font, black on yellow, left 40% of screen width, positioned one line-height above the transaction block (overlapping the lower rain/weather area where there is horizontal clearance).

**Transaction lines:** 16px font, white on green. Amounts: `-` prefix = debit (money out), `+` prefix = credit (money in). Pending transactions show a ` P` suffix.

**Bank query throttle:** Teller.io is queried at most once every 20 minutes. A repaint is triggered immediately when balance or transactions change.

### After 6:00 PM (all modes)

Today's calendar events starting before 12:01 PM are hidden to reduce clutter, showing only afternoon and evening events plus tomorrow's and the day-after's events.

**Bottom strip switching:** On the very first render after server startup, the bottom area shows the version bar (`SV: <git-version>   FW: <fw-version>`, right-justified, 19.2px) instead of the stock strip. All subsequent renders show the stock strip (normal mode only).

**Weather / clock coexistence:** The weather module erases the area behind its temperature block (white fill_rect from `cur_x` to right edge) before drawing, eliminating any clock text that extends into the temperature region.

---

## Module Reference

### Clock (`clock.rs`)

- **Font:** 24px JetBrains Mono Regular, black
- **Position:** x=4 (left margin), y=4 (top margin)
- **Content:** `Wednesday, April 21st, 2026 8:15:30 PM` (single line)
- **Data source:** system clock (`chrono::Local::now()`)

### Weather (`weather.rs`)

- **Data source:** National Weather Service (`api.weather.gov`)
  - Two-step: `/points/{lat},{lon}` → gridpoints URLs (cached)
  - Current temp: first period of `forecastHourly`
  - H/L: first daytime and first nighttime period of `forecast`
- **Refresh:** every 5 minutes
- **Current temp:** 96px bold green, right-justified; auto-positions left of H/L column
- **H/L:** 43px green, right-justified, stacked with 8px gap; right margin = 16px
- **High** = first `isDaytime=true` NWS period (rolls to tomorrow's high after sunset)
- **Low** = first `isDaytime=false` period (tonight's minimum, ~now through 6 AM)

### Rain (`rain.rs`)

- **Data source:** NWS gridpoints QPF (`quantitativePrecipitation`), 168-hour window
- **Refresh:** every 5 minutes
- **Position:** below clock, left-justified, max width 500px (word-wrapped)
- **Font:** 28px JetBrains Mono Regular, blue
- **Content:**
  - `No rain forecast for 7 days.`
  - `Currently raining X.XX in/hr.`
  - `X.XX in/hr rain forecast to start in <duration>.`
  - Optional second line: heavy rain (> 0.30 in/hr) following lighter rain
- **Duration formatting:** < 60 min → integer minutes; < 48 hr → N.N hours; ≥ 48 hr → N days
- **Significant-change tracking:** changes in the ≤ 6-hour window (Active / Imminent / None) trigger a screen refresh

### Google Calendar (`gcal.rs`)

- **Data source:** Google Calendar API v3, OAuth 2.0 refresh token flow
- **Credentials:** `gcal_creds.rs` (gitignored) — CLIENT_ID, CLIENT_SECRET, REFRESH_TOKEN, CALENDAR_IDS
- **Calendars:** multiple calendar IDs merged; exact-duplicate events (same summary + time) deduplicated
- **Scope:** today, tomorrow, and day-after-tomorrow (three separate API fetches per refresh)
- **Refresh:** every 5 minutes; access token cached with 60-second expiry margin
- **Font:** 28px JetBrains Mono Regular
- **Position:** y=128 downward; bottom boundary extends one line-height past region.height to use all available space
- **Color coding:**
  - Today — all-day events and past timed events: white text, black background
  - Today — next upcoming timed event: yellow text, blue background
  - Today — further upcoming timed events: white text, blue background
  - Tomorrow's events: black text, green background
  - Day-after-tomorrow's events: black text, yellow background
- **After 6:00 PM filter:** today's events starting before 12:01 PM are hidden
- **Sort order:** all-day events first (sort_key = -1), then chronological by start time

### Bank (`bank.rs`)

- **Data source:** Teller.io API (`api.teller.io`)
- **Authentication:** mTLS (certificate + private key PEM files) + HTTP Basic auth (access token as username, empty password)
- **Credentials:** `teller_creds.rs` (gitignored) — ACCESS_TOKEN, ACCOUNT_ID, CERT_PATH, KEY_PATH
- **Certificate files:** `teller_cert.pem` and `teller_key.pem` in the server working directory (not in repo)
- **Queries:** `GET /accounts/{id}/balances` and `GET /accounts/{id}/transactions`
- **Throttle:** at most one Teller API call pair every 20 minutes; repaint triggered immediately on data change
- **Stale threshold:** "(bank offline)" shown in red if last successful fetch was > 1 hour ago
- **Font:** 28px balance line, 16px transaction lines
- **Balance line:** black on yellow, left 40% of screen width
- **Transaction lines:** white on green; sign convention: `-` = debit (money out), `+` = credit; ` P` suffix = pending
- **Active schedule:** all day Saturday and Sunday; weekdays 3:00 PM – 8:00 AM; always on if `BANK_MODE=1`

### Stock Quotes (`stock.rs`)

- **Data source:** Finnhub free tier (`finnhub.io/api/v1/quote`)
  - 15–20 minute delay; 60 API calls/minute on free tier
  - Uses `c` (current price); falls back to `pc` (previous close) when market is closed
  - Up/down vs `o` (open); falls back to `pc` when market hasn't opened yet (flat)
- **Credentials:** `stock_creds.rs` (gitignored) — API_KEY
- **Ticker config:** `stock_tickers.txt` in server working directory, one symbol per line, `#` comments supported; read once at server startup
- **Position:** bottom 48px strip (y=432 to y=480)
- **Layout:** equal-width sections separated by 5px white vertical dividers
- **Font:** auto-sized from max 43px down to fit the widest label; centered in each section
- **Color:** green background = price ≥ open; red background = price < open; white text
- **Refresh policy:** fetched only when a screen render is already being triggered; not fetched or displayed during bank mode or on weekends

---

## Sensitive Files (gitignored)

| File | Contents |
|------|----------|
| `server/src/location.rs` | `LAT` and `LON` constants for NWS API lookups |
| `server/src/gcal_creds.rs` | Google Calendar CLIENT_ID, CLIENT_SECRET, REFRESH_TOKEN, CALENDAR_IDS |
| `server/src/stock_creds.rs` | Finnhub API_KEY |
| `server/src/teller_creds.rs` | Teller.io ACCESS_TOKEN, ACCOUNT_ID, CERT_PATH, KEY_PATH |
| `server/teller_cert.pem` | Teller.io mTLS client certificate |
| `server/teller_key.pem` | Teller.io mTLS private key |

The `.rs` credential files must be created manually on each deployment — they are compiled directly into the server binary as Rust constants. The PEM files must be present in the server working directory at runtime. See `GOOGLE_CREDENTIALS.md` and `TELLER_CREDENTIALS.md` for setup instructions.

---

## Resolved Design Decisions

| # | Question | Decision |
|---|---|---|
| 1 | WiFi credentials | Compile-time constants in `config.h` |
| 2 | Image transport | Full 192 KB image on change; 304 when unchanged |
| 3 | ETag strategy | Content-addressed: full SHA-256 of pixel buffer, hex-encoded |
| 4 | Display orientation | Landscape (800 × 480) |
| 5 | Color strategy | Direct E6 color indices; solid colors only; no dithering |
| 6 | Time sync | Server is time authority via `X-Server-Time`; firmware syncs RTC if delta > 30 s |
| 7 | Render trigger | Significant-change detection, not per-poll; stock data piggybacks on other triggers |
| 8 | MCU frame buffer | None — HTTP body streamed directly to EPD over SPI |
| 9 | Font library | `fontdue` (pure Rust, no system deps); `ab_glyph` was considered and rejected |
| 10 | Layout config | Hardcoded per-module constants; no runtime config file for layout |
| 11 | Battery reporting | Single `X-Battery` request header; always sends `pct` + `mv` + `status`; adds `hrs` estimate only when discharging; estimate uses compile-time capacity and average-current constants |
| 12 | Bank data source | Teller.io free tier (mTLS + Basic auth); chosen over Plaid (no free tier) |
| 13 | Bank mode schedule | Auto-active weekends + weekdays 3 PM–8 AM; `BANK_MODE=1` forces 24/7 |
| 14 | Poll interval | Three-zone: 3600 s overnight, countdown to 6:45 AM, 60 s daytime |
| 15 | Calendar scope | Today + tomorrow + day-after; after 6 PM hides today's morning events |
| 16 | Bank query rate | Throttled to once per 20 minutes; repaint on change, not on schedule |

---

## Design Note: Full Image vs. Differential Transmission

Currently the server transmits the full 192,000-byte pixel buffer when the image changes. A diff approach could reduce transfer size dramatically (a clock-only update might touch ~10% of pixels), but it would require:

- Server: per-device last-sent image buffer keyed on `X-Device-ID`
- Firmware: PSRAM-resident frame buffer (192 KB — fits in the 8 MB available); buffer must be written to flash to survive deep sleep

**Recommendation:** The current 192 KB transfer at local WiFi speeds (~1–5 Mbps) completes in under a second and is not a meaningful battery cost at a 60-second poll interval. Revisit if poll intervals are shortened further or if the server moves off the local network.
