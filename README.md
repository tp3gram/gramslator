# Gramslator™

Designed for ELECROW ESP32-S3 3.5in touchscreen platform:

- Product Page: https://a.co/d/089DIeBc
- GitHub (Device spec, schematics, IC datasheets): https://github.com/Elecrow-RD/CrowPanel-Advance-3.5-HMI-ESP32-S3-AI-Powered-IPS-Touch-Screen-480x320


## Development notes

Guide for ESP32 with related screen using `esp-generate`: https://esp32.implrust.com/tft-display/index.html

`esp-generate` tool: https://github.com/esp-rs/esp-generate

### ELECROW Board Pin Definitions

I2C (Touchscreen, RTC clock)
- SDA IO15
- SCL IO16

SD Card (High-speed SPI)
- MOSI IO6
- MISO IO4
- SCK IO5
- CS IO7

Microphone (I2S in)
- Enable Mic: IO45 low (pull-up resistor)

- CLK: IO9
- SD: IO10

Buzzer: IO8

Speaker (I2S out)
- Mute speaker amplifier: IO21
- DOUT/SDIN IO12
- BCLK IO13
- LRCLK IO11

Screen (ILI9488 driver)
- SCK IO42
- SDA IO39

- RS IO41
- CS IO40

- LED Backlight: IO38
- Screen power: IO14

- Touch screen (GT911 controller)
  - Interfaced over I2C

  - INT IO47
  - RST IO48