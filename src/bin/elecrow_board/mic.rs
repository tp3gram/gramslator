use esp_hal::Blocking;
use esp_hal::dma::DmaDescriptor;
use esp_hal::i2s::master::{BitOrder, Channels, Config, DataFormat, I2s, I2sRx};
use esp_hal::peripherals::{DMA_CH0, GPIO3, GPIO9, GPIO10, I2S0};
use esp_hal::time::Rate;

pub struct MicHardware<'a> {
    pub i2s: I2S0<'a>,

    pub dma_channel: DMA_CH0<'a>,
    pub bclk_pin: GPIO9<'a>,
    pub ws_pin: GPIO3<'a>,
    pub din_pin: GPIO10<'a>,
}

/// Setup hardware to interface with the I2S microphone on the ELECROW board.
///
/// Closest datasheet for `LMD3526B261-OFA03`: <https://jlcpcb.com/api/file/downloadByFileSystemAccessId/8604442987128901632>
/// Datasheet provided by ELECROW: <https://github.com/Elecrow-RD/CrowPanel-Advance-3.5-HMI-ESP32-S3-AI-Powered-IPS-Touch-Screen-480x320/blob/master/Datasheet/INMP441-Datasheet.pdf>
pub fn init<'d>(
    mic_hardware: MicHardware<'d>,
    dma_rx_descriptors: &'static mut [DmaDescriptor],
) -> I2sRx<'d, Blocking> {
    // ESP32-S3 I2S docs: https://docs.espressif.com/projects/rust/esp-hal/1.0.0/esp32s3/esp_hal/i2s/master/index.html

    let i2s = I2s::new(
        mic_hardware.i2s,
        mic_hardware.dma_channel,
        Config::new_tdm_philips()
            .with_sample_rate(Rate::from_khz(16))
            .with_data_format(DataFormat::Data16Channel16)
            .with_channels(Channels::RIGHT)
            .with_bit_order(BitOrder::MsbFirst),
    )
    .unwrap();

    i2s.i2s_rx
        .with_bclk(mic_hardware.bclk_pin)
        .with_ws(mic_hardware.ws_pin)
        .with_din(mic_hardware.din_pin)
        .build(dma_rx_descriptors)
}

/// Workaround for esp-hal bug: `rx_start` sets `rx_eof_num` to `buffer_bytes - 1`,
/// but the ESP32-S3 register counts in I2S words (each `RX_BITS_MOD+1` bits wide).
/// For Data16Channel16, each word is 16 bits = 2 bytes, so the correct value is
/// `(buffer_bytes / 2) - 1`. Without this fix, `rx_done` never fires and DMA hangs.
///
/// Must be called immediately after `read_dma()` starts the transfer.
pub fn fix_rx_eof_num(buffer_bytes: usize) {
    const I2S0_BASE: usize = 0x6002_D000;
    const RXEOF_NUM_OFFSET: usize = 0x64;
    const RX_CONF_OFFSET: usize = 0x20;

    // For Data16Channel16: each I2S word = 2 bytes
    let correct_eof_num = (buffer_bytes / 2) - 1;

    unsafe {
        let rxeof_num = (I2S0_BASE + RXEOF_NUM_OFFSET) as *mut u32;
        rxeof_num.write_volatile(correct_eof_num as u32);

        // Trigger rx_update (bit 8 of RX_CONF) to latch the new value
        let rx_conf = (I2S0_BASE + RX_CONF_OFFSET) as *mut u32;
        let val = rx_conf.read_volatile();
        rx_conf.write_volatile(val & !(1 << 8)); // clear rx_update first
        rx_conf.write_volatile(val | (1 << 8)); // set rx_update
    }
}
