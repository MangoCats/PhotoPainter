#include <Arduino.h>
#include <WiFi.h>
#include <HTTPClient.h>
#include <sys/time.h>
#include <XPowersLib.h>
#include "config.h"
#include "version.h"

#define LED_RED    45
#define LED_GREEN  42

// DEBUG: keeps USB alive between polls; set false for battery use
#define DEBUG_NO_SLEEP true

// ── RTC-retained state ────────────────────────────────────────────────────────
RTC_DATA_ATTR static char     s_etag[128]     = "";
RTC_DATA_ATTR static uint32_t s_poll_interval = DEFAULT_POLL_INTERVAL_SEC;

// ── PMIC global ───────────────────────────────────────────────────────────────
static XPowersPMU pmu;
static bool       s_pmu_ok = false;

// ── LED helpers ───────────────────────────────────────────────────────────────
// Both LEDs are ACTIVE-LOW: GPIO HIGH = off, GPIO LOW = on.
// Red  — PWM via analogWrite (inverted: high duty = mostly off = dim).
// Green — digital only: HIGH = off, LOW = on (errors only).
//
// analogWrite duty for active-low red:
//   255 → always HIGH → LED off
//   249 → HIGH 97.6 %, LOW 2.4 % → barely-visible heartbeat
//     0 → always LOW  → LED fully on
#define RED_OFF_DUTY   255   // LED off
#define RED_IDLE_DUTY  249   // ~2 % on — barely-visible heartbeat
#define RED_FULL_DUTY  0     // LED fully on

static void leds_init() {
    pinMode(LED_GREEN, OUTPUT);
    digitalWrite(LED_GREEN, HIGH);      // HIGH = off (active-low)
    pinMode(LED_RED, OUTPUT);
    analogWrite(LED_RED, RED_OFF_DUTY); // start fully off
}
// Between polls: dim red heartbeat, green off.
static void leds_idle() {
    analogWrite(LED_RED, RED_IDLE_DUTY);
    digitalWrite(LED_GREEN, HIGH);      // HIGH = off
}
// During WiFi, HTTP transfer, and EPD refresh: full red, green off.
static void leds_active() {
    analogWrite(LED_RED, RED_FULL_DUTY);
    digitalWrite(LED_GREEN, HIGH);      // HIGH = off
}
// Error blinks (LED_RED or LED_GREEN); returns to idle when done.
static void blink(int pin, int times, int on_ms = 150, int off_ms = 150) {
    for (int i = 0; i < times; ++i) {
        if (pin == LED_RED) analogWrite(LED_RED, RED_FULL_DUTY); // 0 = fully on
        else                digitalWrite(pin, LOW);               // LOW = on
        delay(on_ms);
        if (pin == LED_RED) analogWrite(LED_RED, RED_OFF_DUTY);  // 255 = off
        else                digitalWrite(pin, HIGH);              // HIGH = off
        delay(off_ms);
    }
    leds_idle();
}

// ── EPD SPI (bit-banged) ──────────────────────────────────────────────────────
static void epd_spi_byte(uint8_t b) {
    for (int i = 7; i >= 0; --i) {
        digitalWrite(EPD_MOSI, (b >> i) & 1);
        digitalWrite(EPD_SCK,  HIGH);
        digitalWrite(EPD_SCK,  LOW);
    }
}
static void epd_cmd(uint8_t cmd) {
    digitalWrite(EPD_DC, LOW);
    digitalWrite(EPD_CS, LOW);  epd_spi_byte(cmd);  digitalWrite(EPD_CS, HIGH);
}
static void epd_data(uint8_t d) {
    digitalWrite(EPD_DC, HIGH);
    digitalWrite(EPD_CS, LOW);  epd_spi_byte(d);    digitalWrite(EPD_CS, HIGH);
}
static void epd_wait_busy() {
    // BUSY HIGH = idle, LOW = working
    while (digitalRead(EPD_BUSY) == LOW) delay(10);
}

// ── EPD init (Waveshare official epd7in3f sequence) ───────────────────────────
static void epd_init() {
    pinMode(EPD_SCK,  OUTPUT); pinMode(EPD_MOSI, OUTPUT);
    pinMode(EPD_CS,   OUTPUT); pinMode(EPD_DC,   OUTPUT);
    pinMode(EPD_RST,  OUTPUT); pinMode(EPD_BUSY, INPUT);
    pinMode(EPD_PWR,  OUTPUT);
    digitalWrite(EPD_PWR, HIGH); delay(20);
    digitalWrite(EPD_RST, HIGH); delay(20);
    digitalWrite(EPD_RST, LOW);  delay(2);
    digitalWrite(EPD_RST, HIGH); delay(20);
    epd_wait_busy();

    epd_cmd(0xAA);
    epd_data(0x49); epd_data(0x55); epd_data(0x20); epd_data(0x08);
    epd_data(0x09); epd_data(0x18);
    epd_cmd(0x01);
    epd_data(0x3F); epd_data(0x00); epd_data(0x32); epd_data(0x2A);
    epd_data(0x0E); epd_data(0x2A);
    epd_cmd(0x00); epd_data(0x5F); epd_data(0x69);
    epd_cmd(0x03); epd_data(0x00); epd_data(0x54); epd_data(0x00); epd_data(0x44);
    epd_cmd(0x05); epd_data(0x40); epd_data(0x1F); epd_data(0x1F); epd_data(0x2C);
    epd_cmd(0x06); epd_data(0x6F); epd_data(0x1F); epd_data(0x1F); epd_data(0x22);
    epd_cmd(0x08); epd_data(0x6F); epd_data(0x1F); epd_data(0x1F); epd_data(0x22);
    epd_cmd(0x13); epd_data(0x00); epd_data(0x04);
    epd_cmd(0x30); epd_data(0x3C);
    epd_cmd(0x41); epd_data(0x00);
    epd_cmd(0x50); epd_data(0x3F);
    epd_cmd(0x60); epd_data(0x02); epd_data(0x00);
    epd_cmd(0x61); epd_data(0x03); epd_data(0x20); epd_data(0x01); epd_data(0xE0);
    epd_cmd(0x82); epd_data(0x1E);
    epd_cmd(0x84); epd_data(0x00);
    epd_cmd(0x86); epd_data(0x00);
    epd_cmd(0xE3); epd_data(0x2F);
    epd_cmd(0xE0); epd_data(0x00);
    epd_cmd(0xE6); epd_data(0x00);
}

// ── EPD refresh trigger (call after all pixel data is written) ────────────────
static void epd_refresh() {
    epd_cmd(0x04); epd_data(0x00);   // power on
    epd_wait_busy();
    epd_cmd(0x12); epd_data(0x00);   // refresh
    delay(200);
    while (digitalRead(EPD_BUSY) == HIGH) delay(10);  // wait LOW (started)
    while (digitalRead(EPD_BUSY) == LOW)  delay(10);  // wait HIGH (done ~30s)
    epd_cmd(0x02); epd_data(0x00);   // power off
    epd_wait_busy();
}

// ── Solid-colour fill — no buffer required ────────────────────────────────────
// packed_byte: high nibble = left pixel, low nibble = right pixel
// e.g. 0x11=white, 0x22=yellow, 0x33=red, 0x55=blue, 0x66=green, 0x00=black
static void epd_fill(uint8_t packed_byte) {
    epd_init();
    epd_cmd(0x10);
    digitalWrite(EPD_DC, HIGH);
    digitalWrite(EPD_CS, LOW);
    for (int i = 0; i < EPD_IMAGE_BYTES; ++i) epd_spi_byte(packed_byte);
    digitalWrite(EPD_CS, HIGH);
    epd_refresh();
}

// ── AXP2101 init ─────────────────────────────────────────────────────────────
static void pmic_init() {
    if (!pmu.begin(Wire, AXP_ADDR, AXP_SDA, AXP_SCL)) return;
    pmu.setALDO1Voltage(3300); pmu.enableALDO1();
    pmu.setALDO2Voltage(3300); pmu.enableALDO2();
    pmu.setALDO3Voltage(3300); pmu.enableALDO3();
    pmu.setALDO4Voltage(3300); pmu.enableALDO4();
    pmu.setDC1Voltage(3300);   pmu.enableDC1();
    pmu.setVbusCurrentLimit(XPOWERS_AXP2101_VBUS_CUR_LIM_2000MA);
    pmu.enableBattDetection();
    pmu.enableBattVoltageMeasure();
    // Disable the PMIC's built-in charge LED — it shares the green LED circuit
    pmu.setChargingLedMode(XPOWERS_CHG_LED_OFF);
    s_pmu_ok = true;
}

// ── Battery header builder ────────────────────────────────────────────────────
static void build_battery_header(char* buf, size_t len) {
    if (!s_pmu_ok || !pmu.isBatteryConnect()) { buf[0] = '\0'; return; }

    int      pct      = pmu.getBatteryPercent();
    uint32_t mv       = pmu.getBattVoltage();
    bool     charging = pmu.isCharging() || pmu.isStandby();

    if (pct < 0) { buf[0] = '\0'; return; }  // no battery data

    const char* status = pmu.isCharging() ? "charging"
                       : pmu.isStandby()  ? "standby"
                                          : "discharging";

    if (charging) {
        snprintf(buf, len, "pct=%d, mv=%u, status=%s", pct, (unsigned)mv, status);
    } else {
        float hrs = ((float)BATTERY_CAPACITY_MAH * (pct / 100.0f)) / AVG_DISCHARGE_MA;
        snprintf(buf, len, "pct=%d, mv=%u, hrs=%.1f, status=%s",
                 pct, (unsigned)mv, hrs, status);
    }
}

// ── WiFi connect ──────────────────────────────────────────────────────────────
static bool wifi_connect() {
    // Tear down any prior state before starting fresh — a stuck half-connected
    // state from a previous failed attempt would prevent reassociation otherwise.
    WiFi.disconnect(true);   // disconnect + WIFI_OFF
    WiFi.mode(WIFI_STA);
    WiFi.begin(WIFI_SSID, WIFI_PASSWORD);
    uint32_t t0 = millis();
    while (WiFi.status() != WL_CONNECTED) {
        if (millis() - t0 > WIFI_CONNECT_TIMEOUT_MS) return false;
        delay(100);
    }
    return true;
}

// ── RTC sync ──────────────────────────────────────────────────────────────────
static void maybe_sync_rtc(const String& s) {
    if (s.isEmpty()) return;
    int64_t ts = s.toInt();
    if (ts <= 0) return;
    struct timeval now{};
    gettimeofday(&now, nullptr);
    if (llabs(ts - (int64_t)now.tv_sec) > 30) {
        struct timeval tv = { (time_t)ts, 0 };
        settimeofday(&tv, nullptr);
    }
}

// ── HTTP poll — streams body directly to EPD, no large buffer needed ──────────
static bool poll_server(const char* batt_hdr) {
    static const char* kHeaders[] = { "ETag", "X-Poll-Interval", "X-Server-Time" };
    HTTPClient http;
    http.begin(SERVER_URL);
    http.setTimeout(HTTP_TIMEOUT_MS);
    http.collectHeaders(kHeaders, 3);
    http.addHeader("X-Device-ID",        WiFi.macAddress());
    http.addHeader("X-Firmware-Version", FIRMWARE_VERSION);
    if (batt_hdr && batt_hdr[0] != '\0') http.addHeader("X-Battery", batt_hdr);
    if (s_etag[0] != '\0') http.addHeader("If-None-Match", s_etag);

    int code = http.GET();

    maybe_sync_rtc(http.header("X-Server-Time"));
    String intvl = http.header("X-Poll-Interval");
    if (!intvl.isEmpty()) {
        uint32_t v = constrain((uint32_t)intvl.toInt(),
                               MIN_POLL_INTERVAL_SEC, MAX_POLL_INTERVAL_SEC);
        s_poll_interval = v;
    }

    if (code == 304) { http.end(); return false; }

    if (code != 200) {
        blink(LED_RED, max(1, code / 100), 300, 300);
        http.end();
        return false;
    }

    String etag = http.header("ETag");
    if (!etag.isEmpty()) etag.toCharArray(s_etag, sizeof(s_etag));

    // ── Stream HTTP body directly to EPD ──────────────────────────────────────
    epd_init();
    epd_cmd(0x10);
    digitalWrite(EPD_DC, HIGH);
    digitalWrite(EPD_CS, LOW);

    WiFiClient* stream = http.getStreamPtr();
    uint8_t chunk[256];
    size_t received = 0;
    uint32_t t_last = millis();

    while (received < (size_t)EPD_IMAGE_BYTES) {
        int avail = stream->available();
        if (avail > 0) {
            size_t want = min((size_t)avail,
                              min(sizeof(chunk), (size_t)EPD_IMAGE_BYTES - received));
            size_t got  = stream->readBytes(chunk, want);
            for (size_t i = 0; i < got; ++i) epd_spi_byte(chunk[i]);
            received += got;
            t_last = millis();
        } else if (millis() - t_last > HTTP_TIMEOUT_MS) {
            digitalWrite(EPD_CS, HIGH);
            blink(LED_RED, 6, 100, 100);  // stream timeout
            http.end();
            return false;
        } else {
            delay(1);
        }
    }

    digitalWrite(EPD_CS, HIGH);
    http.end();

    epd_refresh();

    return true;
}

// ── Entry point ───────────────────────────────────────────────────────────────
void setup() {
    leds_init();
    pmic_init();
    leds_idle();  // pmic_init may activate charge LED — force idle state after
}

void loop() {
    char batt_hdr[80];
    build_battery_header(batt_hdr, sizeof(batt_hdr));

    leds_active();  // full red: about to do WiFi + network work

    if (!wifi_connect()) {
        WiFi.disconnect(true);        // shut radio down before sleeping
        WiFi.mode(WIFI_OFF);
        blink(LED_RED, 5, 400, 400);  // error → blink ends in leds_idle()
        if (!DEBUG_NO_SLEEP) {
            esp_sleep_enable_timer_wakeup((uint64_t)s_poll_interval * 1000000ULL);
            esp_deep_sleep_start();
        }
        delay(s_poll_interval * 1000);
        return;
    }

    poll_server(batt_hdr);

    WiFi.disconnect(true);
    WiFi.mode(WIFI_OFF);
    leds_idle();    // work done — dim red until next poll

    if (!DEBUG_NO_SLEEP) {
        esp_sleep_enable_timer_wakeup((uint64_t)s_poll_interval * 1000000ULL);
        esp_deep_sleep_start();
    }
    delay(s_poll_interval * 1000);
}
