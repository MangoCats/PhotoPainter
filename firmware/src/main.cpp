#include <Arduino.h>
#include <WiFi.h>
#include <HTTPClient.h>
#include "config.h"

#define LED_RED    45
#define LED_GREEN  42

static void blink(int pin, int times, int on_ms = 200, int off_ms = 200) {
    for (int i = 0; i < times; ++i) {
        digitalWrite(pin, HIGH);
        delay(on_ms);
        digitalWrite(pin, LOW);
        delay(off_ms);
    }
}

void setup() {
    pinMode(LED_RED,   OUTPUT);
    pinMode(LED_GREEN, OUTPUT);
    digitalWrite(LED_RED,   LOW);
    digitalWrite(LED_GREEN, LOW);

    // ── BOOT: both LEDs blink 3x ──────────────────────────────────────────────
    for (int i = 0; i < 3; ++i) {
        digitalWrite(LED_RED,   HIGH);
        digitalWrite(LED_GREEN, HIGH);
        delay(200);
        digitalWrite(LED_RED,   LOW);
        digitalWrite(LED_GREEN, LOW);
        delay(200);
    }
    delay(500);

    // ── WIFI: red rapid blink while connecting ────────────────────────────────
    WiFi.mode(WIFI_STA);
    WiFi.begin(WIFI_SSID, WIFI_PASSWORD);
    uint32_t t0 = millis();
    while (WiFi.status() != WL_CONNECTED) {
        if (millis() - t0 > 15000) break;
        digitalWrite(LED_RED, HIGH); delay(100);
        digitalWrite(LED_RED, LOW);  delay(100);
    }

    if (WiFi.status() != WL_CONNECTED) {
        // WiFi failed: red 5x slow blink, then halt
        blink(LED_RED, 5, 500, 500);
        while (true) delay(1000);
    }

    // WiFi connected: green steady 2s
    digitalWrite(LED_GREEN, HIGH);
    delay(2000);
    digitalWrite(LED_GREEN, LOW);
    delay(500);

    // ── HTTP: green rapid blink while requesting ──────────────────────────────
    HTTPClient http;
    http.begin(SERVER_URL);
    http.setTimeout(10000);

    // rapid green during request
    for (int i = 0; i < 5; ++i) {
        digitalWrite(LED_GREEN, HIGH); delay(100);
        digitalWrite(LED_GREEN, LOW);  delay(100);
    }

    int code = http.GET();
    http.end();

    // ── Result ────────────────────────────────────────────────────────────────
    if (code == 200) {
        // Success: green 3x long blink
        blink(LED_GREEN, 3, 500, 300);
    } else if (code == 304) {
        // Not modified: green 1x + red 1x
        blink(LED_GREEN, 1, 500, 300);
        blink(LED_RED,   1, 500, 300);
    } else {
        // HTTP error: red blinks = error code hundreds digit (e.g. 4xx = 4 blinks)
        int hundreds = (code > 0) ? (code / 100) : 9;
        blink(LED_RED, hundreds, 400, 300);
        delay(500);
        // then short red blinks = tens+units (e.g. 04 = 4 blinks)
        int remainder = (code > 0) ? (code % 100 / 10) : 9;
        blink(LED_RED, remainder > 0 ? remainder : 1, 150, 150);
    }

    // ── Done: both LEDs on steady ─────────────────────────────────────────────
    digitalWrite(LED_RED,   HIGH);
    digitalWrite(LED_GREEN, HIGH);
    while (true) delay(1000);  // halt so you can read the result
}

void loop() {}
