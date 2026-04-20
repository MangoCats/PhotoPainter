#include <Arduino.h>
#include <Wire.h>
#include <SPI.h>
#include <XPowersLib.h>

// AXP2101 power management IC
#define AXP_SDA     47
#define AXP_SCL     48
#define AXP_ADDR    0x34

// E-paper display pins
#define EPD_SCK     10
#define EPD_MOSI    11
#define EPD_CS       9
#define EPD_DC       8
#define EPD_RST     12
#define EPD_BUSY    13
#define EPD_PWR      6

// Display resolution
#define EPD_WIDTH   800
#define EPD_HEIGHT  480

// 4-bit color values — empirically mapped for this E6 panel
#define COLOR_BLACK   0x0
#define COLOR_WHITE   0x1
#define COLOR_YELLOW  0x2
#define COLOR_RED     0x3
#define COLOR_BLUE    0x5
#define COLOR_GREEN   0x6

XPowersPMU pmu;

static void epd_cmd(uint8_t cmd) {
    digitalWrite(EPD_DC, LOW);
    digitalWrite(EPD_CS, LOW);
    SPI.transfer(cmd);
    digitalWrite(EPD_CS, HIGH);
}

static void epd_data(uint8_t data) {
    digitalWrite(EPD_DC, HIGH);
    digitalWrite(EPD_CS, LOW);
    SPI.transfer(data);
    digitalWrite(EPD_CS, HIGH);
}

static void epd_wait_busy() {
    // BUSY HIGH = idle on this panel
    while (digitalRead(EPD_BUSY) == LOW) delay(10);
}

static void epd_reset() {
    digitalWrite(EPD_RST, HIGH); delay(50);
    digitalWrite(EPD_RST, LOW);  delay(20);
    digitalWrite(EPD_RST, HIGH); delay(50);
    epd_wait_busy();
}

// Init sequence matching Waveshare official epd7in3f driver
static void epd_init() {
    epd_reset();

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

static void epd_refresh() {
    epd_cmd(0x04); epd_data(0x00);
    epd_wait_busy();
    epd_cmd(0x12); epd_data(0x00);
    delay(200);
    while (digitalRead(EPD_BUSY) == HIGH) delay(10);
    while (digitalRead(EPD_BUSY) == LOW)  delay(10);
    Serial.println("Refresh complete.");
    epd_cmd(0x02); epd_data(0x00);
    epd_wait_busy();
}

static void epd_fill(uint8_t color) {
    uint8_t pixel_byte = (color << 4) | color;
    epd_cmd(0x10);
    digitalWrite(EPD_DC, HIGH);
    digitalWrite(EPD_CS, LOW);
    for (uint32_t i = 0; i < (EPD_WIDTH * EPD_HEIGHT / 2); i++) {
        SPI.transfer(pixel_byte);
    }
    digitalWrite(EPD_CS, HIGH);
    epd_refresh();
}

void setup() {
    Serial.begin(115200);
    Serial.println("PhotoPainter display test");

    Wire.begin(AXP_SDA, AXP_SCL);
    if (!pmu.begin(Wire, AXP_ADDR, AXP_SDA, AXP_SCL)) {
        Serial.println("AXP2101 init failed");
    } else {
        Serial.println("AXP2101 OK");
        pmu.setDC1Voltage(3300);   pmu.enableDC1();
        pmu.setALDO1Voltage(3300); pmu.enableALDO1();
        pmu.setALDO2Voltage(3300); pmu.enableALDO2();
        pmu.setALDO3Voltage(3300); pmu.enableALDO3();
        pmu.setALDO4Voltage(3300); pmu.enableALDO4();
        pmu.setVbusCurrentLimit(XPOWERS_AXP2101_VBUS_CUR_LIM_2000MA);
        Serial.println("Power rails enabled");
    }
    delay(100);

    pinMode(EPD_PWR, OUTPUT);
    digitalWrite(EPD_PWR, HIGH);
    delay(100);
    Serial.println("EPD power on");

    pinMode(EPD_CS,   OUTPUT); digitalWrite(EPD_CS, HIGH);
    pinMode(EPD_DC,   OUTPUT);
    pinMode(EPD_RST,  OUTPUT);
    pinMode(EPD_BUSY, INPUT);
    SPI.begin(EPD_SCK, -1, EPD_MOSI, EPD_CS);
    SPI.beginTransaction(SPISettings(4000000, MSBFIRST, SPI_MODE0));

    Serial.println("Initialising display...");
    epd_init();

    Serial.println("Filling screen red (~30s)...");
    epd_fill(COLOR_RED);
    Serial.println("Done.");
}

void loop() {
    delay(1000);
}
