#include <Arduino.h>
#include <WiFi.h>
#include <HTTPClient.h>
#include <sys/time.h>
#include <XPowersLib.h>
#include "config.h"

// ── RTC-retained state ────────────────────────────────────────────────────────
RTC_DATA_ATTR static char  s_etag[128]      = "";
RTC_DATA_ATTR static uint32_t s_poll_interval = DEFAULT_POLL_INTERVAL_SEC;

// ── EPD pin helpers ───────────────────────────────────────────────────────────
static void epd_select()   { digitalWrite(EPD_CS,  LOW);  }
static void epd_deselect() { digitalWrite(EPD_CS,  HIGH); }
static void epd_dc_cmd()   { digitalWrite(EPD_DC,  LOW);  }
static void epd_dc_data()  { digitalWrite(EPD_DC,  HIGH); }

static void epd_spi_byte(uint8_t b) {
    for (int i = 7; i >= 0; --i) {
        digitalWrite(EPD_MOSI, (b >> i) & 1);
        digitalWrite(EPD_SCK,  HIGH);
        digitalWrite(EPD_SCK,  LOW);
    }
}

static void epd_cmd(uint8_t cmd) {
    epd_dc_cmd(); epd_select(); epd_spi_byte(cmd); epd_deselect();
}
static void epd_data(uint8_t d) {
    epd_dc_data(); epd_select(); epd_spi_byte(d); epd_deselect();
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

    // Hardware reset
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

// ── Send image buffer to EPD and trigger refresh ──────────────────────────────
static void epd_show(const uint8_t* packed, size_t len) {
    epd_cmd(0x10);
    epd_dc_data(); epd_select();
    for (size_t i = 0; i < len; ++i) epd_spi_byte(packed[i]);
    epd_deselect();

    epd_cmd(0x04); epd_data(0x00);  // power on
    epd_wait_busy();

    epd_cmd(0x12); epd_data(0x00);  // refresh
    delay(200);
    while (digitalRead(EPD_BUSY) == HIGH) delay(10);  // wait LOW (refresh started)
    while (digitalRead(EPD_BUSY) == LOW)  delay(10);  // wait HIGH (refresh done)

    epd_cmd(0x02); epd_data(0x00);  // power off
    epd_wait_busy();
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
}

// ── WiFi connect (blocking with timeout) ─────────────────────────────────────
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

// ── Sync RTC from server header ───────────────────────────────────────────────
static void maybe_sync_rtc(const String& server_time_str) {
    if (server_time_str.isEmpty()) return;
    int64_t server_ts = server_time_str.toInt();
    if (server_ts <= 0) return;
    struct timeval tv_now{};
    gettimeofday(&tv_now, nullptr);
    int64_t delta = server_ts - (int64_t)tv_now.tv_sec;
    if (delta > 30 || delta < -30) {
        struct timeval tv = { (time_t)server_ts, 0 };
        settimeofday(&tv, nullptr);
    }
}

// ── HTTP poll, returns true if display was updated ────────────────────────────
static bool poll_server() {
    HTTPClient http;
    http.begin(SERVER_URL);
    http.setTimeout(HTTP_TIMEOUT_MS);
    http.addHeader("X-Device-ID", WiFi.macAddress());
    if (s_etag[0] != '\0') http.addHeader("If-None-Match", s_etag);

    int code = http.GET();

    // Always grab these headers regardless of status
    maybe_sync_rtc(http.header("X-Server-Time"));

    String new_interval = http.header("X-Poll-Interval");
    if (!new_interval.isEmpty()) {
        uint32_t v = (uint32_t)new_interval.toInt();
        v = max(v, (uint32_t)MIN_POLL_INTERVAL_SEC);
        v = min(v, (uint32_t)MAX_POLL_INTERVAL_SEC);
        s_poll_interval = v;
    }

    if (code == 304) {
        http.end();
        return false;
    }

    if (code != 200) {
        http.end();
        return false;
    }

    // Save new ETag
    String etag = http.header("ETag");
    if (!etag.isEmpty()) {
        etag.toCharArray(s_etag, sizeof(s_etag));
    }

    // Stream payload into PSRAM buffer
    int content_len = http.getSize();
    if (content_len != EPD_IMAGE_BYTES) {
        http.end();
        return false;
    }

    uint8_t* buf = (uint8_t*)ps_malloc(EPD_IMAGE_BYTES);
    if (!buf) { http.end(); return false; }

    WiFiClient* stream = http.getStreamPtr();
    size_t received = 0;
    while (received < EPD_IMAGE_BYTES) {
        int avail = stream->available();
        if (avail > 0) {
            size_t chunk = min((size_t)avail, EPD_IMAGE_BYTES - received);
            stream->readBytes(buf + received, chunk);
            received += chunk;
        } else {
            delay(1);
        }
    }
    http.end();

    epd_init();
    epd_show(buf, EPD_IMAGE_BYTES);

    free(buf);
    return true;
}

// ── Entry point ───────────────────────────────────────────────────────────────
void setup() {
    Serial.begin(115200);

    pmic_init();

    if (!wifi_connect()) {
        Serial.println("WiFi failed — sleeping");
        esp_sleep_enable_timer_wakeup((uint64_t)s_poll_interval * 1000000ULL);
        esp_deep_sleep_start();
    }

    poll_server();

    WiFi.disconnect(true);
    WiFi.mode(WIFI_OFF);

    esp_sleep_enable_timer_wakeup((uint64_t)s_poll_interval * 1000000ULL);
    esp_deep_sleep_start();
}

void loop() {}
