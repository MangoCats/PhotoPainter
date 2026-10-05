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
├── .gitignore                  ← excludes location.rs, stock_creds.rs, mangosched_auth.json, mangosched_backoff.json
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
    ├── photopainter.service    ← systemd unit (Restart=always, LimitNOFILE, journald logging)
    ├── testdata/               ← synthetic mangoSched calendar page for the parser tests
    └── src/
        ├── main.rs             ← HTTP server, render loop, significant-change detection
        ├── renderer.rs         ← composes modules into final image
        ├── image.rs            ← E6Canvas pixel buffer and palette
        ├── font.rs             ← fontdue TTF rasterization (JetBrains Mono)
        ├── nws_cache.rs        ← shared cache of the NWS /points URLs (24 h)
        ├── location.rs         ← LAT/LON constants (gitignored, not in repo)
        ├── stock_creds.rs      ← Finnhub API key (gitignored)
        └── modules/
            ├── mod.rs          ← Module trait definition
            ├── battery.rs      ← parses the X-Battery request header
            ├── clock.rs        ← date and time display
            ├── icon_matrix.rs  ← development/demo icon grid (ICON_MATRIX=1)
            ├── weather.rs      ← NWS current temperature + H/L forecast + 84px weather icons
            ├── rain.rs         ← NWS QPF rain forecast
            ├── mangosched.rs   ← mangoSched schedule: one column per day of colored shift boxes
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

All responses carry `Connection: close`. The device deep-sleeps with its radio off straight after a poll and never closes
its TCP socket, so without it every full-image fetch leaked one socket on the server until its file-descriptor limit was hit.

**`X-Poll-Interval`** — seconds until the device should poll again.
- **11:00 PM – 5:45 AM:** 3600 s (overnight; device wakes once per hour).
- **5:45 AM – 6:45 AM:** exact seconds remaining until 6:45 AM (one final long sleep that lands precisely at wake-up time).
- **6:45 AM – 11:00 PM:** 300 s (daytime; 5-minute cadence).
- Device clamps received value to [60, 3600]; stores in RTC memory.

**`X-Server-Time`** — Unix timestamp (UTC seconds) at response generation time.
- Present on every response.
- Device updates its RTC if the difference exceeds 30 seconds. The server is the sole time authority; no NTP client is needed on the firmware.

**Non-2xx/304 response:** the HTTP error code is blinked on the red LED (blink count = HTTP status ÷ 100) and no display update happens. The firmware still reads `X-Poll-Interval` and `X-Server-Time` from any response that carries them, and keeps its stored interval when they are absent.

### Image Format

Raw pixel data, 192,000 bytes. 4 bits per pixel, 2 pixels per byte.
- Two pixels per byte: the high nibble is the first pixel of the pair in stream order, the low nibble the second.
- Stream order runs backwards through the canvas, from its last pixel (bottom-right) to its first (top-left): the order the panel expects.
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

Survives deep sleep; lost on full power-off or battery removal. (Light sleep keeps ordinary RAM as well.)

```c
RTC_DATA_ATTR char     s_etag[128]     = "";   // last received ETag
RTC_DATA_ATTR uint32_t s_poll_interval = DEFAULT_POLL_INTERVAL_SEC;   // seconds between polls
```

On cold boot both fall back to safe defaults: unconditional poll at the default interval.

### Main Loop

```
Wake from deep sleep (or return from light sleep, or loop() iteration in DEBUG_NO_SLEEP mode)
│
├─ leds_active()        — full red: about to do network work
├─ Init AXP2101         — enable all power rails at 3.3V
│                         enable battery detection and voltage ADC channels
├─ Sample battery       — getBatteryPercent(), getBattVoltage(), charge-state flags
│                         compute hrs estimate if discharging; build X-Battery value
│                         (no value at all when no battery is connected / the gauge reports pct < 0)
├─ Connect WiFi             — skipped when the association survived light sleep
│   └─ Timeout 10 s → blink red ×5, WiFi off, deep sleep for poll_interval
│
├─ HTTP GET /api/image
│   ├─ Send X-Device-ID, X-Firmware-Version, X-Battery
│   ├─ Send If-None-Match (if ETag cached)
│   ├─ Read X-Server-Time → sync RTC if delta > 30 s          (any response)
│   ├─ Read X-Poll-Interval → clamp [60, 3600] → store in RTC  (any response)
│   │
│   ├─ 200 OK → stream 192,000 bytes directly to EPD (no MCU-side buffer)
│   │   ├─ epd_init()
│   │   ├─ Write pixel data via SPI while receiving from WiFi
│   │   ├─ epd_refresh() — power on, trigger, wait BUSY, power off
│   │   └─ Store new ETag in RTC (only now; a failed transfer is retried next poll)
│   │
│   ├─ 304 Not Modified → no display update
│   │
│   └─ Error → blink red (count = status ÷ 100)
│
├─ modem power-save (WIFI_PS_MIN_MODEM); leds_idle() — dim red heartbeat
└─ poll_interval < 270 s (LIGHT_SLEEP_MAX_SEC) → light sleep; WiFi stays associated
   poll_interval ≥ 270 s → WiFi off, deep sleep (setup() runs again on wake)
   (or delay() if DEBUG_NO_SLEEP = true)
```

**Key implementation detail:** the HTTP body is streamed directly to the EPD over SPI as bytes arrive from the socket. No 192 KB frame buffer is allocated on the MCU. The EPD's internal buffer accumulates the data; the refresh command is sent only after all bytes have been written.

### Battery Status Reporting

The firmware must sample the AXP2101 PMIC once per wake cycle — after `pmic_init()` and before the HTTP request — and report the results in the `X-Battery` request header.

#### Header Format

```
X-Battery: pct=<0–100>, mv=<millivolts>, hrs=<decimal>, status=<token>
```

When a battery is connected all fields are present except `hrs`, which is omitted when an estimate is not meaningful (see below). When no battery is connected, or the gauge reports no data (`pct < 0`), the whole header is omitted. Field order is fixed; values are integers except `hrs` (one decimal place).

| Field | Source | Description |
|-------|--------|-------------|
| `pct` | `getBatteryPercent()` | State of charge, 0–100. |
| `mv` | `getBattVoltage()` | Battery terminal voltage in millivolts. Both `enableBattDetection()` and `enableBattVoltageMeasure()` must be called during `pmic_init()` before this is valid. |
| `hrs` | Computed (see below) | Estimated remaining hours on current charge. Omitted when `status` is `charging` or `standby`. |
| `status` | Derived from PMIC flags | One of the tokens defined below. |

#### Status Tokens

| Token | Condition |
|-------|-----------|
| `charging` | `isCharging()` is true (USB present, battery charging) |
| `discharging` | `isDischarge()` is true (running on battery) |
| `standby` | `isStandby()` is true (USB present, battery full or charge paused) |

If the PMIC returns an unexpected combination, the first matching token in the order `charging`, `standby`, `discharging` is reported.

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

`AVG_DISCHARGE_MA` should reflect observed consumption at the configured poll interval (see Power Budget table). At the default 300 s daytime interval (deep sleep between polls) the no-display-update baseline is ~1 mAh/hr (see the power budget); 6 mA is a conservative default that also covers display refreshes. Users must calibrate this for their battery and usage pattern.

**Accuracy caveats:** The AXP2101 fuel gauge (`getBatteryPercent()`) is coulomb-counter based and requires a full charge/discharge cycle to calibrate. The percent value is unreliable immediately after power-on or battery insertion. The `hrs` estimate additionally depends on `AVG_DISCHARGE_MA` being representative of actual load, which varies with display update frequency, WiFi signal strength, and temperature.

#### Server handling and display

The server parses `X-Battery` on every `GET /api/image`, logs it at `INFO` level beside the device-ID and response-code entry,
and keeps the latest reading in memory (it is lost on a server restart and returns with the next poll). A header that is
missing or malformed, or that reports `pct < 0`, clears the reading.

The weather block shows the reading as a battery icon plus `NN%` at the top right: fill colour blue while charging
or standby (with a yellow lightning bolt), green at ≥ 25 %, yellow at ≥ 10 %, red below 10 %. A re-render is triggered when the charging state
changes or the charge level moves by ≥ 5 %.

The display only reports when it polls, so a reading can go out of date. One older than twice the poll interval last given to the
display (never less than 30 minutes: 30 minutes by day, 2 hours overnight) is flagged by a small speckled-grey margin around
the readout, which itself stays on white. Going stale, or recovering, triggers a re-render.

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

At the 300 s daytime poll interval the firmware deep-sleeps between polls, so every poll is one full cycle: 12 cycles/hr × 0.084 mAh = **~1 mAh/hr** baseline (without display updates). Intervals shorter than 270 s use light sleep instead and are not covered by this table.

---

## Server Design (`server/`)

Two HTTP listeners run in one process: the device API on port **7654** (`GET /api/image`) and an unauthenticated browser
preview on port **17654** (`/` is a page that reloads every 60 s; `/image.png` is the current image as an RGB PNG).

### Technology Stack

- **Language:** Rust
- **HTTP framework:** `axum` (async, `tokio` runtime)
- **Font rendering:** `fontdue` TTF rasterizer with JetBrains Mono Regular and Bold
- **Image composition:** direct E6 pixel buffer — no RGB intermediary, no dithering
- **External data:** `reqwest` with `rustls-tls` (no OpenSSL dependency); the shared client's User-Agent is `PhotoPainter/1.0 (mangocats@gmail.com)` (the National Weather Service asks for contact details)
- **Time:** `chrono` for date/time formatting; `std::time::Instant` for refresh throttling
- **Hashing:** `sha2` (SHA-256) for ETag generation

### Render Architecture

The server does **not** re-render on every poll. Instead, a background task (`render_loop`) wakes every 60 seconds, calls every data module's `refresh()` in parallel (each throttles itself: weather and rain every 5 minutes, or 1 minute after a failure; schedule every 10 minutes), and checks whether any significant change has occurred. If so, it fetches fresh stock data and produces a new image; otherwise the cached image is served as-is.

```
render_loop (every 60 s):
  tokio::join!(weather, rain, sched).refresh()

  if significant_change:
    stock.refresh()          ← only when render is already happening
    image = render(modules)
    store image + ETag
```

Significant changes that trigger a re-render:
- More than 60 minutes since last render
- Current temperature changes ≥ 2°F
- Forecast high or low changes ≥ 3°F
- Near-term rain status (≤ 6-hour window) changes between None / Active / Imminent
- Battery charging state changes, or the charge level moves by ≥ 5 %
- The weather data goes stale (more than 30 minutes old) or recovers, or the battery reading goes stale or recovers
- The schedule changes (a different set of days or shifts, which includes the day labels rolling over after midnight)

Stock data is **only fetched when a render is already being triggered** by one of the above conditions. Stock changes do not trigger renders on their own. The stock strip is shown (and its data refreshed) every day, at all hours.

**Time and date.** The screen is only redrawn when one of the triggers above fires, so the clock line shows the time of the last render, not the current time, and the date and schedule columns stay as drawn until then. In practice a render happens at least hourly. After midnight "today" therefore stays on the old day until the next render: the schedule module's next refresh (within 10 minutes) sees the new day labels, which triggers a render, and an hourly render covers the case where that refresh fails. A delay of several hours is acceptable by design.

### Module Trait

```rust
pub trait Module: Send + Sync {
    fn render(&self, canvas: &mut E6Canvas, region: Rect);
}
```

Modules receive a `Rect` region from the renderer. Most modules receive `full_screen()` and self-manage their coordinates internally. The schedule module draws from `SCHEDULE_Y_START` (128) down to two pixels above the bottom of its region (`renderer::schedule_region()`, which ends at the top of the stock strip).

### Image Pipeline

```
Data modules refresh in parallel
        │
        ▼
Each module renders into E6Canvas [u8; 384000] (one byte per pixel)
        │
        ▼
Bottom: stock strip (y=448..480), or the version bar (first render, or after a firmware change)
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

### Layout

```
y=0   ┌──────────────────────────────────────────────────────────────────────┐
      │ [Clock] Tuesday, April 21st, 2026 8:15:30 PM        [Weather]        │
y=4   │  24px black, left-margin=4                          Current: 96px    │
      │                                                      bold green, R-  │
      │ [Rain] 0.04 in/hr rain to start in 3.5 hours.       justified        │
y≈26  │  28px blue, left-justified, max 500px wide           H/L: 43px green │
      │                                            [84px weather icon, R-just]│
y=128 ├── Schedule (mangoSched) — one column per day ─────────────────────────┤
      │ ┏Sun Oct 4┓ Mon Oct 5   Mon Oct 5 ›  Tue Oct 6   Wed Oct 7           │
      │ 7a-9:30a Ivan  7a-9:30a Eli  6:30p-9p Jo   7a-9:30a Fran  no shifts   │
      │ Dustin Music   Dustin Comp.  Conrad Comp.  Conrad Pers.              │
      │ box color = worker's color (nearest of blue/red/green/yellow);        │
      │ open shift = white box with black outline                             │
      │ ...a day continues in the next column when its boxes do not fit...    │
y=446 ├───────────────────────────────────────────────────────────────────────┤
      │  2px gap                                                              │
y=448 ├── Stock Strip (32px) ─────────────────────────────────────────────────┤
      │  ▓▓▓ MDT ▓▓▓│▓▓▓ RKLB ▓▓▓│▓▓▓ TSLA ▓▓▓│▓▓▓ BRK.B ▓▓▓              │
      │  green=up/flat, red=down vs open; white text; 5px white dividers      │
y=480 └───────────────────────────────────────────────────────────────────────┘
```

**Bottom strip switching:** The first render after server startup, and the render that follows a firmware change reported by the device (a change from a known version, not the version being learned again after a server restart), show the version bar (`SV: <git-version>   FW: <fw-version>`, right-justified, 19.2px) instead of the stock strip. Every other render shows the stock strip, every day.

**Weather / clock coexistence:** The weather module erases the area behind its temperature block (white fill_rect from `cur_x` to right edge) before drawing, eliminating any clock text that extends into the temperature region. When the weather data is more than 30 minutes old (the refresh keeps failing) that area, and the daytime icon cell, are filled with a grey speckled-black pattern (one black pixel in four) instead of plain white; the battery readout stays on white so it remains legible (a stale battery reading gets its own small speckled margin, see Battery Status Reporting).

---

## Module Reference

### Clock (`clock.rs`)

- **Font:** 24px JetBrains Mono Regular, black
- **Position:** x=4 (left margin), y=4 (top margin)
- **Content:** `Wednesday, April 21st, 2026 8:15:30 PM` (single line), as of the last render (see "Time and date" above)
- **Data source:** system clock (`chrono::Local::now()`)

### Weather (`weather.rs`)

- **Data source:** National Weather Service (`api.weather.gov`)
  - Two-step: `/points/{lat},{lon}` → gridpoints URLs (cached)
  - Current temp: first period of `forecastHourly`
  - H/L: first daytime and first nighttime period of `forecast`
- **Refresh:** every 5 minutes (1 minute after a failed attempt); data older than 30 minutes is flagged with the speckled background described above
- **Current temp:** 96px bold green, right-justified; auto-positions left of H/L column
- **H/L:** 43px green, right-justified, stacked with no gap; right margin = 4px
- **Battery indicator:** top right, see Battery Status Reporting
- **High** = first `isDaytime=true` NWS period (rolls to tomorrow's high after sunset)
- **Low** = first `isDaytime=false` period (tonight's minimum, ~now through 6 AM)

### Rain (`rain.rs`)

- **Data source:** NWS gridpoints QPF (`quantitativePrecipitation`), 168-hour window
- **Refresh:** every 5 minutes
- **Position:** below clock, left-justified, max width 500px (word-wrapped)
- **Font:** 28px JetBrains Mono Regular, blue
- **Content** (word-wrapped):
  - `No rain for 7 days.`
  - `Raining .06 in/hr.` while the current forecast period has rain (rates below 1 print without the leading zero; 1 or more print one decimal)
  - `<Category> <rate> <when>.` for the first period with rain, e.g. `Moderate .11 Tuesday @ 1:59PM.`; category by rate: Light < 0.10, Moderate < 0.30, Heavy < 2.0, Extreme above
  - `<when>` is `in/hr @ 3:05PM` when the start is within 91 minutes, later today, or before 6 AM tomorrow; `Tomorrow @ 8:10AM`; otherwise the weekday, `Wednesday @ 8:10AM`
  - Optional second line in the same format: the earliest later period of a heavier category than the first
- **Significant-change tracking:** changes in the ≤ 6-hour window (Active / Imminent / None) trigger a screen refresh

### Schedule (`mangosched.rs`)

- **Data source:** the mangoSched web app ("kodaCal" instance, `https://bluekoda.duckdns.org`). It has no API, so the
  module logs in as a read-only **viewer** account and parses the calendar page's `table.calendar-grid`
  (`div.daynum a` carries each day's offset from today; `div.shift` carries color class, time, type and worker).
  The page shows exactly what that role may see (cancelled shifts and "X"-state open shifts are not shown to a viewer).
- **Route:** connects to `smartboardpc.lan` (LAN) while keeping the public name for TLS and the virtual host; if the LAN
  route fails at the network level it retries once via the public name.
- **Authentication (no upkeep):** credentials are read at runtime from `server/mangosched_auth.json` (git-ignored, 0600).
  The session cookie is kept in memory and reused; mangoSched sessions last 30 days (FR-1a), after which the next fetch
  logs in again. A rejected login (wrong/reset password, locked account, forced password change) stops all attempts for
  30 minutes, so mangoSched's account lockout can never be triggered by this server. The suspension is saved to
  `server/mangosched_backoff.json` (keyed to a hash of the login), so restarting or crash-looping the server cannot retry
  either, and editing the password file lifts it within a minute. Login needs the CSRF token from the
  login form and, over HTTPS, `Origin`/`Referer` headers.
- **Refresh:** every 10 minutes (2 minutes after a network failure). A refresh that changes the schedule triggers a re-render. Stale after 1 hour: a red banner reads
  `(schedule offline)`; auth problems read `(schedule login failed)` / `(schedule not configured)`.
- **Scope:** today and every later day on the page. Past days are dropped; empty days are kept (label + "no shifts").
- **Layout:** 5 columns, 3 px apart. Each day starts in a new column: a 29 px header (black bar with a 19 px white label; today
  is a white bar with a black frame and a bold label), then one box per shift, 2 px apart. If the next box does not fit, the
  day continues in the next column with the header repeated and ` ›` appended. Days are placed until there is no room for
  another column. If the last column fills up in the middle of a day, that column's header shows `+N` (the number of
  shifts that did not fit) so nothing is dropped silently.
- **Box:** 16 px text on an 18 px line pitch, 1 px vertical padding. Line 1: compact time and worker short name
  (`9:30a-12p Jo`; overnight `10p→6a`); if the two do not fit together the worker moves to its own line. Then the shift
  type, word-wrapped to at most three lines. Open shifts show `OPEN`. Times follow the viewer account's own time zone and
  12/24-hour setting (a 24-hour setting is shown as-is and will truncate sooner).
- **Colors:** the worker's calendar color mapped to the nearest ink: blue, cyan, purple, lavender → blue (white text);
  red, pink, orange, brown → red (white text); yellow → yellow (black text); lime-green, dark-green → green (black text).
  Open shifts: white box, black outline. The worker's name distinguishes workers who share an ink.
- **Position:** y=128 downward, full width; the bottom is y=446, two pixels above the stock strip (`schedule_region()`).
- **Test hook:** if the `MANGOSCHED_BASE_URL` environment variable is set (e.g. `http://host:8099` for a development instance), that base URL is used instead of the production route; it is honoured in every build, including production.

### Stock Quotes (`stock.rs`)

- **Data source:** Finnhub free tier (`finnhub.io/api/v1/quote`)
  - 15–20 minute delay; 60 API calls/minute on free tier
  - Uses `c` (current price); falls back to `pc` (previous close) when market is closed
  - Up/down vs `o` (open); falls back to `pc` when market hasn't opened yet (flat)
- **Credentials:** `stock_creds.rs` (gitignored) — API_KEY
- **Ticker config:** `stock_tickers.txt` in server working directory, one symbol per line, `#` comments supported; read once at server startup
- **Position:** y=448 to y=480 (`STRIP_H` = 32 px), full width
- **Layout:** equal-width sections separated by 5px white vertical dividers
- **Font:** auto-sized from max 43px down to fit the widest label; centered in each section
- **Color:** green background = price ≥ open; red background = price < open; white text
- **Refresh policy:** fetched (all tickers concurrently) only when a screen render is already being triggered; shown every day, at all hours (no weekend or time-of-day gating)
- **Failures:** a ticker whose fetch fails keeps its previous quote; each quote older than 2 hours is shown with a `~` before the price (the `~` means "old", not "market closed")

---

## Sensitive Files (gitignored)

| File | Contents |
|------|----------|
| `server/src/location.rs` | `LAT` and `LON` constants for NWS API lookups |
| `server/src/stock_creds.rs` | Finnhub API_KEY |
| `server/mangosched_auth.json` | mangoSched viewer-account username and password (read at runtime, mode 0600) |
| `server/mangosched_backoff.json` | runtime state, not a secret: when logins are suspended after a rejection (mode 0600) |

The `.rs` credential files must be created manually on each deployment — they are compiled directly into the server binary as Rust constants. `mangosched_auth.json` is the exception: it is plain runtime data, not compiled in.

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
| 12 | Bank data source | *Abandoned (2026-10).* Was Teller.io; the bank module was removed — see git history |
| 13 | Bank mode schedule | *Removed with the bank module.* Every day: schedule above, stock strip below (the former weekend full-height layout without the strip was dropped 2026-10) |
| 14 | Poll interval | Three-zone: 3600 s overnight, countdown to 6:45 AM, 300 s daytime |
| 15 | Calendar scope | mangoSched schedule: today + every later day on its calendar page, one column per day (overflow continues in the next column) |
| 16 | Bank query rate | *Removed with the bank module* |
| 17 | Calendar source | mangoSched viewer-account login (runtime credentials, self-renewing session); replaced Google Calendar 2026-10 |
| 18 | Render triggers | Hourly, weather/rain/battery thresholds, weather going stale, and any change to the schedule |

---

## Design Note: Full Image vs. Differential Transmission

Currently the server transmits the full 192,000-byte pixel buffer when the image changes. A diff approach could reduce transfer size dramatically (a clock-only update might touch ~10% of pixels), but it would require:

- Server: per-device last-sent image buffer keyed on `X-Device-ID`
- Firmware: PSRAM-resident frame buffer (192 KB — fits in the 8 MB available); buffer must be written to flash to survive deep sleep

**Recommendation:** The current 192 KB transfer at local WiFi speeds (~1–5 Mbps) completes in under a second and is not a meaningful battery cost at the 300-second poll interval. Revisit if poll intervals are shortened further or if the server moves off the local network.
