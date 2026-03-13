//! ELECROW ESP32-S3 3.5in touchscreen platform.
//!
//! Device GitHub: https://github.com/Elecrow-RD/CrowPanel-Advance-3.5-HMI-ESP32-S3-AI-Powered-IPS-Touch-Screen-480x320
//! Device Schematic: https://github.com/Elecrow-RD/CrowPanel-Advance-3.5-HMI-ESP32-S3-AI-Powered-IPS-Touch-Screen-480x320/blob/master/Eagle_SCH%26PCB/1.2/ESP32%20Display%203.5%20inch%20V1.2(1).pdf
//!
//! ESP32 Chipset: `ESP32-S3-WROOM-1-N16R8` ([Datasheet](https://github.com/Elecrow-RD/CrowPanel-Advance-3.5-HMI-ESP32-S3-AI-Powered-IPS-Touch-Screen-480x320/blob/master/Datasheet/esp32-s3-wroom-1_wroom-1u_datasheet_en.pdf))
pub mod buzzer;
pub mod display;
pub mod mic;
pub mod mic_wireless_module_switch;
pub mod wifi;
