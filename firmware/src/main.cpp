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

// ── LED helpers ───────────────────────────────────────────────────────────────
static void leds_off() {
    digitalWrite(LED_RED,   LOW);
    digitalWrite(LED_GREEN, LOW);
}
static void leds_init() {
    pinMode(LED_RED,   OUTPUT);
    pinMode(LED_GREEN, OUTPUT);
    leds_off();
}
static void blink(int pin, int times, int on_ms = 150, int off_ms = 150) {
    for (int i = 0; i < times; ++i) {
        digitalWrite(pin, HIGH); delay(on_ms);
        digitalWrite(pin, LOW);  delay(off_ms);
    }
    leds_off();  // ensure both off after any blink sequence
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
    XPowersPMU pmu;
    if (!pmu.begin(Wire, AXP_ADDR, AXP_SDA, AXP_SCL)) return;
    pmu.setALDO1Voltage(3300); pmu.enableALDO1();
    pmu.setALDO2Voltage(3300); pmu.enableALDO2();
    pmu.setALDO3Voltage(3300); pmu.enableALDO3();
    pmu.setALDO4Voltage(3300); pmu.enableALDO4();
    pmu.setDC1Voltage(3300);   pmu.enableDC1();
    pmu.setVbusCurrentLimit(XPOWERS_AXP2101_VBUS_CUR_LIM_2000MA);
    // Disable the PMIC's built-in charge LED — it shares the green LED circuit
    pmu.setChargingLedMode(XPOWERS_CHG_LED_OFF);
}

// ── WiFi connect ──────────────────────────────────────────────────────────────
static bool wifi_connect() {
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
static bool poll_server() {
    static const char* kHeaders[] = { "ETag", "X-Poll-Interval", "X-Server-Time" };
    HTTPClient http;
    http.begin(SERVER_URL);
    http.setTimeout(HTTP_TIMEOUT_MS);
    http.collectHeaders(kHeaders, 3);
    http.addHeader("X-Device-ID",        WiFi.macAddress());
    http.addHeader("X-Firmware-Version", FIRMWARE_VERSION);
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
    leds_off();   // pmic_init may activate charge LED — force off after
}

void loop() {
    leds_off();   // guarantee off at the top of every cycle

    if (!wifi_connect()) {
        blink(LED_RED, 5, 400, 400);
        if (!DEBUG_NO_SLEEP) {
            esp_sleep_enable_timer_wakeup((uint64_t)s_poll_interval * 1000000ULL);
            esp_deep_sleep_start();
        }
        delay(s_poll_interval * 1000);
        return;
    }

    poll_server();

    WiFi.disconnect(true);
    WiFi.mode(WIFI_OFF);
    leds_off();   // wifi teardown can re-assert GPIO state on some builds

    if (!DEBUG_NO_SLEEP) {
        esp_sleep_enable_timer_wakeup((uint64_t)s_poll_interval * 1000000ULL);
        esp_deep_sleep_start();
    }
    delay(s_poll_interval * 1000);
}
