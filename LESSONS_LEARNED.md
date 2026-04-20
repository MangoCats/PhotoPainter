# Waveshare ESP32-S3-PhotoPainter — Lessons Learned

## Hardware Identity

- **Chip:** ESP32-S3 (QFN56) rev v0.2, dual core 240 MHz
- **RAM:** 320 KB internal + 8 MB OPI PSRAM (AP_3v3)
- **Flash:** 16 MB
- **USB:** Native USB-Serial/JTAG (Espressif VID `303A`, PID `1001`) — no CH343 or other USB-UART bridge
- **MAC:** `e8:f6:0a:8f:03:6c`
- **Display:** 7.3" 6-color ACeP e-paper (E6 panel), 800×480

---

## USB Driver

The CH343 driver is **not needed**. Windows uses its built-in CDC driver for the ESP32-S3's native USB. The device enumerates as **COM4** when in bootloader mode or when firmware initializes USB CDC.

---

## AXP2101 PMIC — USB + Battery Power Issue

**Known hardware issue:** The AXP2101 PMIC becomes unstable when both USB (PC, with data lines active) and a LiPo battery are connected simultaneously. The device power-cycles approximately every 10 seconds.

| Power source | Result |
|---|---|
| Battery only | Stable — normal operation |
| USB only (no battery) | Stable — for development |
| USB charger (no data) | Stable |
| PC USB + battery | Unstable — 10 s cutoff |

**Workaround for flashing:** Hold **BOOT**, then plug in USB. This forces the ESP32-S3 into ROM bootloader mode before the firmware (and PMIC shutdown logic) can run.

---

## Factory Firmware Behavior

The factory firmware starts in WiFi AP mode and displays **nothing on the e-paper screen** until:
- WiFi credentials are configured via `http://192.168.4.1`, or
- Images are present on an SD card.

Blank screen after flashing the factory firmware is expected — it is not a hardware fault.

---

## Entering Bootloader Mode

Hold **BOOT** at the moment USB is plugged in (or at the moment RESET is pressed). The BOOT pin is only sampled during that instant. Release immediately after — the device stays in bootloader mode until flashing completes.

With `ARDUINO_USB_CDC_ON_BOOT=1` in firmware, PlatformIO can auto-reset into bootloader via the 1200-baud trick without holding BOOT.

---

## PlatformIO Configuration (`platformio.ini`)

```ini
[env:photopainter]
platform = espressif32
board = esp32-s3-devkitc-1
framework = arduino

board_build.flash_size = 16MB
board_build.partitions = huge_app.csv
board_build.arduino.memory_type = qio_opi

build_flags =
    -DARDUINO_USB_MODE=1
    -DARDUINO_USB_CDC_ON_BOOT=1
    -DXPOWERS_CHIP_AXP2101

upload_port = COM4
monitor_port = COM4
monitor_speed = 115200

lib_deps =
    lewisxhe/XPowersLib@^0.2.6
```

- `qio_opi` is required for the 8 MB OPI PSRAM.
- `-DXPOWERS_CHIP_AXP2101` is **required** — without it `XPowersPMU` is undefined (the typedef is conditional on this flag).

---

## Pin Mapping

### E-paper Display (SPI)

| Signal | GPIO |
|---|---|
| SCK | 10 |
| MOSI | 11 |
| CS | 9 |
| DC | 8 |
| RST | 12 |
| BUSY | 13 |
| PWR (enable) | 6 |

### AXP2101 (I2C)

| Signal | GPIO |
|---|---|
| SDA | 47 |
| SCL | 48 |
| I2C address | 0x34 |

### LEDs

| LED | GPIO |
|---|---|
| Red | 45 |
| Green | 42 |

---

## AXP2101 Initialization

All four ALDO rails and DC1 must be enabled at 3.3 V before the display will function. The VBUS current limit should be set to 2000 mA for stable USB operation.

```cpp
Wire.begin(47, 48);
pmu.begin(Wire, 0x34, 47, 48);
pmu.setDC1Voltage(3300);   pmu.enableDC1();
pmu.setALDO1Voltage(3300); pmu.enableALDO1();
pmu.setALDO2Voltage(3300); pmu.enableALDO2();
pmu.setALDO3Voltage(3300); pmu.enableALDO3();
pmu.setALDO4Voltage(3300); pmu.enableALDO4();
pmu.setVbusCurrentLimit(XPOWERS_AXP2101_VBUS_CUR_LIM_2000MA);
```

---

## E-paper Display Driver

### BUSY Pin Polarity
`HIGH = idle`, `LOW = busy`. Wait for HIGH before proceeding.

### Correct Init Sequence
The init sequence must match the **Waveshare official epd7in3f Arduino driver**, not the sequence found in community forks (which is missing several critical registers):

```
0xAA: 49 55 20 08 09 18
0x01: 3F 00 32 2A 0E 2A   ← 6 bytes required (forks only send 1)
0x00: 5F 69
0x03: 00 54 00 44
0x05: 40 1F 1F 2C
0x06: 6F 1F 1F 22         ← forks had wrong values here
0x08: 6F 1F 1F 22
0x13: 00 04               ← missing from forks
0x30: 3C                  ← forks sent 0x03
0x41: 00                  ← missing from forks (temperature compensation)
0x50: 3F
0x60: 02 00
0x61: 03 20 01 E0
0x82: 1E                  ← missing from forks
0x84: 00                  ← forks sent 0x01
0x86: 00                  ← missing from forks
0xE3: 2F
0xE0: 00                  ← missing from forks
0xE6: 00                  ← missing from forks
```

### Display Refresh Sequence
Write pixel data **before** powering on the panel:

```
1. Send 0x10 + all pixel bytes (192,000 bytes for 800×480 at 4bpp)
2. 0x04 0x00  (power on)  → wait BUSY HIGH
3. 0x12 0x00  (refresh)   → wait BUSY LOW then HIGH (refresh takes ~30 s)
4. 0x02 0x00  (power off) → wait BUSY HIGH
```

---

## E6 Panel Color Table

The 6-color E6 panel uses a **different color order** than the Waveshare 7in3f (7-color) driver. Empirically determined:

| Value | Color |
|---|---|
| 0x0 | Black |
| 0x1 | White |
| 0x2 | Yellow |
| 0x3 | Red |
| 0x4 | ~~invalid~~ (renders dark brown/purple) |
| 0x5 | Blue |
| 0x6 | Green |
| 0x7 | ~~invalid~~ (renders dark brown/purple) |

Valid color values for this panel: **0x0, 0x1, 0x2, 0x3, 0x5, 0x6**.

---

## Pixel Data Format

4 bits per pixel, 2 pixels per byte. High nibble = left pixel, low nibble = right pixel.

```cpp
uint8_t pixel_byte = (color << 4) | color;  // solid fill
```

Total bytes for full frame: `800 × 480 / 2 = 192,000 bytes`.
