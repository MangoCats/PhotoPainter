# PhotoPainter System Design

## Overview

Two independent projects share this repository:

1. **`firmware/`** — ESP32-S3 firmware for the PhotoPainter device. Wakes from deep sleep, polls a local server for a new dashboard image, updates the e-paper display if the image has changed, then sleeps again for a server-specified interval.

2. **`server/`** — Rust web server running on a local machine (PC, NAS, Raspberry Pi, etc.). Composes a multi-module dashboard image in the PhotoPainter's native 4bpp E6 format and serves it on demand. Starts with a digital clock; designed to add modules incrementally.

---

## Repository Structure

```
PhotoPainter/
├── DESIGN.md                   ← this document
├── LESSONS_LEARNED.md          ← hardware bring-up findings
├── scratch/                    ← existing bring-up/test sketches
│   ├── platformio.ini
│   └── src/main.cpp
├── firmware/                   ← production ESP32-S3 firmware
│   ├── platformio.ini
│   └── src/
│       └── main.cpp
└── server/                     ← Rust dashboard server
    ├── Cargo.toml
    ├── config.toml             ← server configuration
    └── src/
        ├── main.rs             ← HTTP server, routes, scheduler
        ├── renderer.rs         ← composes modules into final image
        ├── image.rs            ← E6 pixel buffer, palette, dithering
        └── modules/
            ├── mod.rs          ← Module trait definition
            ├── clock.rs        ← Phase 1: digital clock
            ├── calendar.rs     ← Phase 2: Google Calendar
            ├── weather.rs      ← Phase 3: weather data
            ├── homeassistant.rs← Phase 4: HA sensors
            ├── stocks.rs       ← Phase 5: stock quotes
            └── banking.rs      ← Phase 6: bank balances
```

---

## Communication Protocol

The device always initiates contact (server never pushes). All communication is plain HTTP on the local network.

### Poll Request

```
GET /api/image HTTP/1.1
Host: <server-ip>:<port>
X-Device-ID: e8:f6:0a:8f:03:6c
If-None-Match: "<image-version-token>"
```

- `X-Device-ID`: device MAC address, used for logging and future per-device config.
- `If-None-Match`: version token from the last successful response. Omitted on first boot.

### Server Responses

**New image available (`200 OK`):**
```
HTTP/1.1 200 OK
Content-Type: application/octet-stream
Content-Length: 192000
ETag: "<new-version-token>"
X-Poll-Interval: 300
X-Server-Time: 1745123456
[192,000 bytes of raw 4bpp E6 pixel data]
```

**No change (`304 Not Modified`):**
```
HTTP/1.1 304 Not Modified
ETag: "<current-version-token>"
X-Poll-Interval: 300
X-Server-Time: 1745123456
```

**`X-Poll-Interval`** — seconds until the device should poll again.
- Range: 60–3600 (1 minute to 1 hour).
- If absent or out of range: device uses 300 s (5 minutes).

**`X-Server-Time`** — Unix timestamp (seconds since epoch, UTC) at the moment the server generated the response.
- Present on every response (200 and 304).
- The device compares this against its local RTC. If the difference exceeds 30 seconds in either direction, the device updates its RTC to match the server time.
- The server is the sole time authority. No NTP client is needed on the firmware.
- Network transit latency (~1–5 ms on a local network) is negligible relative to the 30-second threshold and is not corrected for.

**Connection failure / any non-2xx/304 response:** device uses 300 s (5 minutes) default. RTC is not updated.

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

See `LESSONS_LEARNED.md` for full pin map, AXP2101 init sequence, and EPD driver details.

### Persistent State (RTC Memory)

Survives deep sleep without requiring flash writes:

```c
RTC_DATA_ATTR uint32_t poll_interval_sec = 300;
RTC_DATA_ATTR char     image_etag[64]    = "";
RTC_DATA_ATTR uint32_t boot_count        = 0;
```

WiFi credentials are stored in NVS (non-volatile storage) via the Arduino `Preferences` library, written once during provisioning.

> **RTC power note:** The ESP32-S3 RTC is internal — it has no dedicated VBAT pin and no separate RTC battery. It runs from the AXP2101's 3.3V rail, which is sustained by the main LiPo during deep sleep (~20 µA draw). RTC memory is lost if the battery dies or is removed. On cold boot both RTC variables fall back to their safe defaults (`poll_interval_sec = 300`, `image_etag = ""`), causing one unconditional poll at the default interval — no special handling required.

### Main Loop (runs once per wake)

```
Wake from deep sleep
│
├─ Init AXP2101 (I2C, enable power rails)
├─ Connect WiFi (fast reconnect using cached BSSID/channel from NVS)
│   └─ Timeout 10 s → on failure: skip poll, sleep poll_interval_sec
│
├─ HTTP GET /api/image
│   ├─ Send If-None-Match: image_etag
│   ├─ Read X-Poll-Interval → clamp to [60, 3600] → store in RTC
│   ├─ Read X-Server-Time → if |server_time - rtc_time| > 30 s: settimeofday()
│   │
│   ├─ 200 OK → receive 192,000 bytes
│   │   ├─ Init EPD (SPI, AXP2101 EPD power rail, init sequence)
│   │   ├─ Write pixel data to display
│   │   ├─ Trigger refresh → wait BUSY
│   │   ├─ Power off EPD
│   │   └─ Store new ETag in RTC
│   │
│   ├─ 304 Not Modified → no display update
│   │
│   └─ Error / timeout → poll_interval_sec = 300 (revert to default)
│
├─ Disconnect WiFi
└─ Deep sleep for poll_interval_sec
```

### Power Budget per Wake Cycle (no display update)

| Phase | Current | Duration | Energy |
|---|---|---|---|
| Boot + AXP init | 80 mA | 0.5 s | 0.011 mAh |
| WiFi connect | 200 mA | 1.0 s | 0.056 mAh |
| HTTP GET + 304 | 100 mA | 0.5 s | 0.014 mAh |
| WiFi disconnect | 50 mA | 0.2 s | 0.003 mAh |
| **Total per cycle** | | **~2.2 s** | **~0.084 mAh** |

At a 5-minute poll interval: 12 cycles/hr × 0.084 mAh = **~1 mAh/hr** baseline.

### WiFi Provisioning

First-boot (no credentials in NVS): start a temporary AP (`PhotoPainter-Setup`), serve a minimal HTML form to collect SSID, password, and server address, store in NVS, reboot.

### Configuration (`config.h`)

```c
#define DEFAULT_POLL_INTERVAL_SEC   300
#define MIN_POLL_INTERVAL_SEC        60
#define MAX_POLL_INTERVAL_SEC      3600
#define WIFI_CONNECT_TIMEOUT_MS   10000
#define HTTP_TIMEOUT_MS            8000
#define SERVER_URL    "http://192.168.1.x:8080/api/image"
```

---

## Server Design (`server/`)

### Technology Stack

- **Language:** Rust
- **HTTP framework:** `axum` (async, `tokio` runtime)
- **Image composition:** direct E6 pixel buffer — no RGB intermediary, no dithering (see palette section)
- **Scheduling:** `tokio` tasks per module, each refreshes its data on its own interval
- **Configuration:** `toml` file (`config.toml`)
- **Font rendering:** `ab_glyph` for clock digits and text, rendered directly in E6 colors
- **Host:** Raspberry Pi (dedicated, always-on)

### Module Trait

Each dashboard module implements a common trait:

```rust
pub trait Module: Send + Sync {
    /// Draw this module's content into the given rectangle of the canvas.
    fn render(&self, canvas: &mut E6Canvas, region: Rect);

    /// How often this module's backing data should be refreshed.
    fn data_refresh_interval(&self) -> Duration;

    /// Hint to the scheduler: how soon should the device poll after
    /// this module's data changes? None = defer to global default.
    fn suggested_poll_interval(&self) -> Option<Duration>;
}
```

### Renderer

The renderer holds a list of `(Module, Rect)` pairs. When the server receives a poll request:

1. Each module renders into its `Rect` on a shared `E6Canvas`.
2. The canvas is packed to 4bpp → 192,000-byte pixel buffer.
3. The buffer is content-hashed (SHA-256 truncated to 64 bits, hex-encoded) → ETag.
4. If ETag matches `If-None-Match` → `304 Not Modified`.
5. Otherwise → `200 OK` with pixel buffer.
6. `X-Poll-Interval` is set to the minimum of all modules' `suggested_poll_interval()` values, clamped to [60, 3600].

### Image Pipeline

```
Per-module renders (E6 color indices directly)
        │
        ▼
E6Canvas — [u8; 384000] (one byte per pixel, values 0x0–0x6)
        │
        ▼
Pack to 4bpp → [u8; 192000]
        │
        ▼
SHA-256 → ETag
        │
        ▼
Serve or 304
```

No RGB conversion. No dithering. Modules paint with named E6 colors. The `E6Canvas` exposes primitives: `fill_rect`, `draw_text`, `draw_line`, `draw_icon`. All content is text and line-drawn icons in solid colors.

### E6 Color Palette

Six valid colors. Modules reference them by name, not by index.

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum E6Color {
    Black  = 0x0,
    White  = 0x1,
    Yellow = 0x2,
    Red    = 0x3,
    Blue   = 0x5,
    Green  = 0x6,
}
```

> **Note:** Values 0x4 and 0x7 render as dark brown/purple on this panel and are excluded from the enum.

### Layout System

Modules are positioned by `Rect { x, y, width, height }` defined in `config.toml`. This allows layout changes without recompiling.

```toml
[layout]
background = "white"

[[layout.module]]
type = "clock"
region = { x = 0, y = 0, width = 800, height = 200 }

[[layout.module]]
type = "weather"
region = { x = 0, y = 200, width = 400, height = 280 }

[[layout.module]]
type = "calendar"
region = { x = 400, y = 200, width = 400, height = 280 }
```

---

## Dashboard Modules — Implementation Phases

### Phase 1 — Digital Clock (MVP)
- Large digits, current time, current date.
- No external data dependency.
- `suggested_poll_interval`: 60 s (updates every minute).
- Renders entirely from system time.

### Phase 2 — Google Calendar
- Next 3–5 upcoming events: title, date/time.
- OAuth2 refresh token stored in `config.toml`.
- Data refresh: every 5 minutes.
- `suggested_poll_interval`: time until next event starts (capped at 1 hour).

### Phase 3 — Weather
- Current conditions + today's forecast.
- Source TBD (OpenWeatherMap, Open-Meteo, or local weather station).
- Data refresh: every 15–30 minutes.
- `suggested_poll_interval`: 15 minutes.

### Phase 4 — Home Assistant Sensors
- Selected sensor values (temperature, humidity, door state, etc.) via HA REST API or WebSocket.
- Configurable sensor list in `config.toml`.
- Data refresh: configurable per sensor (30 s – 5 min).
- `suggested_poll_interval`: minimum sensor refresh interval.

### Phase 5 — Stock Quotes
- Configurable ticker list.
- Data refresh: every minute during market hours, suspended outside hours.
- `suggested_poll_interval`: 60 s during market hours, longer otherwise.

### Phase 6 — Bank Balances
- Read-only API access (institution-dependent, likely via Plaid or OFX).
- Data refresh: once per hour or on-demand.
- `suggested_poll_interval`: 1 hour.

### Future Modules (TBD)
- News headlines
- Transit / commute times
- Package tracking
- Sports scores
- Energy usage

---

## Resolved Design Decisions

| # | Question | Decision |
|---|---|---|
| 1 | WiFi provisioning | Hardcoded compile-time credentials (`config.h`) for now |
| 2 | Server host | Dedicated Raspberry Pi |
| 3 | Image transport | Full image every time (see note below); skip compression |
| 4 | ETag strategy | Content-addressed: SHA-256 of pixel buffer (truncated, hex) |
| 5 | Display orientation | Landscape (800 wide × 480 tall) |
| 6 | E6 palette | Direct E6 color indices; solid colors only; no dithering |
| 7 | Time sync | Server is time authority via `X-Server-Time`; firmware syncs RTC if delta > 30 s |

---

## Design Note: Full Image vs. Image Difference Transmission

Currently the server always transmits the full 192,000-byte pixel buffer when the image has changed. This section documents the trade-offs of switching to differential (delta) transmission as a future optimisation.

### Why a diff could be very effective here

The dashboard is composed of text and solid-color regions. Between two consecutive updates, most of the screen is unchanged — typically only the clock digits flip. A clock update might change four rectangular regions of roughly 80×120 pixels each, touching ~38,400 of 384,000 pixels (~10%). A simple dirty-rectangle diff for that update would be:

```
4 rectangles × ~10 bytes each = ~40 bytes
```

compared to 192,000 bytes for the full image — a **~4,800× reduction** for a clock-only update. Even a busier update (weather + calendar refresh) would typically affect far less than half the screen.

### What differential transmission requires

**Server side:**
- Must retain the last image sent *per device* (keyed on `X-Device-ID`) to compute the diff.
- Must choose a diff format and encode it. Options:
  - *Dirty rectangles:* list of `(x, y, w, h, color)` tuples — optimal for solid-color fills and text clears.
  - *Run-length encoded changed pixels:* efficient for arbitrary changes, simple to decode.
  - *XOR + RLE:* XOR current with previous, then RLE the non-zero spans.
- A separate endpoint or `Content-Type` header distinguishes full from diff responses.

**Device (firmware) side:**
- Must buffer the current displayed image in PSRAM (192 KB — fits easily in the 8 MB available).
- Must apply the diff to the buffer before writing to the display.
- State must be preserved across deep sleep (PSRAM loses content in deep sleep) — so the buffer would need to be stored in flash, adding ~192 KB flash writes per update, or the device must always request a full image on first wake after power loss.

**Failure recovery:**
- If the device misses a poll (connection failure), its buffer diverges from the server's `last_sent` state. The device signals this by omitting `If-None-Match` (cold boot) or by sending a new request header `X-Needs-Full: true`. The server responds with the full image unconditionally.
- This is the same mechanism already used for first boot.

### Recommendation

Implement full-image transmission now. The 192 KB transfer at typical local WiFi throughput (~1–5 Mbps) completes in well under a second — not a meaningful battery cost on a 5-minute poll cycle. Revisit differential transmission if poll intervals are shortened to 60 seconds or below, at which point the per-wake WiFi cost of downloading 192 KB repeatedly becomes significant relative to the baseline radio overhead.
