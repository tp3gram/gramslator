use esp_hal::peripherals::GPIO45;

pub struct I2SBus {}

pub struct MicHardware<'a> {
    pub enable_mic_pin: GPIO45<'a>,
}

/// Setup hardware to interface with the `LMD3526B261-OFA01` I2S microphone on the ELECROW board.
///
/// Closest datasheet I can find for `LMD3526B261-OFA03`: https://jlcpcb.com/api/file/downloadByFileSystemAccessId/8604442987128901632
/// Datasheet provided by ELECROW in board GitHub: https://github.com/Elecrow-RD/CrowPanel-Advance-3.5-HMI-ESP32-S3-AI-Powered-IPS-Touch-Screen-480x320/blob/master/Datasheet/INMP441-Datasheet.pdf
pub fn init(mic_hardware: MicHardware) {}
