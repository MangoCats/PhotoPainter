# PhotoPainter

Custom firmware and companion server for the [Waveshare ESP32-S3-PhotoPainter](https://www.waveshare.com/esp32-s3-photopainter.htm) — a 7.3" 6-color ACeP e-paper display driven by an ESP32-S3.

## How it works

The device wakes from deep sleep on a configurable interval, fetches a pre-rendered image from a local Rust server over WiFi, updates the display if the image has changed (ETag/304 caching avoids unnecessary redraws), then returns to deep sleep. The server renders the dashboard image on a background task and serves it as a raw 4bpp payload.

```
┌─────────────────────┐   HTTP GET /api/image   ┌──────────────────────┐
│  ESP32-S3 firmware  │ ──────────────────────► │  Rust server (Pi)    │
│  (deep sleep cycle) │ ◄────────────────────── │  (always-on)         │
└─────────────────────┘   200 image / 304 hit   └──────────────────────┘
```

## Repository layout

```
PhotoPainter/
├── firmware/                  # PlatformIO ESP32-S3 firmware
│   ├── platformio.ini         # Board config, build flags, upload port
│   ├── include/
│   │   └── config.h           # WiFi credentials, server URL, pin map (git-ignored)
│   └── src/
│       └── main.cpp           # Full firmware: PMIC init, WiFi, HTTP poll, EPD update, deep sleep
│
├── server/                    # Rust dashboard server (runs on Raspberry Pi)
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs            # axum HTTP server on :7654, background render task
│       ├── renderer.rs        # Compose modules onto canvas, pack to 4bpp, SHA-256 ETag
│       ├── image.rs           # E6Canvas (800×480, 6-color palette), fill_rect, pack
│       └── modules/
│           ├── mod.rs         # Module trait (render, data_refresh_interval, suggested_poll_interval)
│           └── clock.rs       # 7-segment HH:MM clock, 20% screen height, centered
│
├── scratch/                   # Hardware bring-up and test sketches (not production)
│   └── src/
│       └── main.cpp           # Color-band test used during initial hardware verification
│
├── DESIGN.md                  # Architecture decisions and resolved design questions
├── LESSONS_LEARNED.md         # Hardware reference: pin map, AXP2101 init, EPD sequence, color table
└── ESP32-S3-PhotoPainter-Fac.bin  # Waveshare factory firmware (for recovery)
```

## Hardware

| Component | Detail |
|-----------|--------|
| MCU | ESP32-S3 (QFN56), 16 MB flash, 8 MB OPI PSRAM |
| Display | 7.3" ACeP 6-color e-paper, 800×480, SPI |
| PMIC | AXP2101, I2C addr 0x34, SDA=GPIO47, SCL=GPIO48 |
| USB | Espressif native USB (VID 303A) — no CH343 driver needed |

EPD SPI pins: SCK=10, MOSI=11, CS=9, DC=8, RST=12, BUSY=13, PWR=6

E6 color palette (empirically verified): `0x0` Black, `0x1` White, `0x2` Yellow, `0x3` Red, `0x5` Blue, `0x6` Green

## Setup

### Firmware

1. Copy `firmware/include/config.h.example` to `firmware/include/config.h` and fill in your WiFi credentials and server IP:
   ```cpp
   #define WIFI_SSID      "your-network"
   #define WIFI_PASSWORD  "your-password"
   #define SERVER_URL     "http://192.168.1.x:7654/api/image"
   ```
   See `firmware/include/config.h.example` for the full template.
2. Open the `firmware/` folder in VS Code with the PlatformIO extension installed.
3. Upload: **PlatformIO: Upload** (hold BOOT if the device doesn't enter download mode automatically).

### Server

Requires Rust. Run on the Raspberry Pi (or any always-on host on the same LAN):

```bash
cd server
cargo build --release
./target/release/photopainter-server
```

Listens on `0.0.0.0:7654`. Set `RUST_LOG=info` for request logging.

## HTTP protocol

| Header | Direction | Meaning |
|--------|-----------|---------|
| `If-None-Match` | →server | ETag from previous response |
| `ETag` | →device | SHA-256 of image payload |
| `X-Poll-Interval` | →device | Suggested sleep duration (seconds) |
| `X-Server-Time` | →device | Unix epoch; firmware syncs RTC if delta > 30 s |
| `X-Device-ID` | →server | Device MAC address |

A `304 Not Modified` response means the image hasn't changed; the firmware skips the display update and goes back to sleep without touching the EPD.

## Adding display modules

1. Create `server/src/modules/your_module.rs` and implement the `Module` trait.
2. `pub mod your_module;` in `modules/mod.rs`.
3. Pass an instance and a `Rect` region to `renderer::render()` in `main.rs`.
