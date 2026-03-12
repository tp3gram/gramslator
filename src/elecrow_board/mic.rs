use esp_hal::Blocking;
use esp_hal::dma::DmaDescriptor;
use esp_hal::i2s::master::{Config, DataFormat, I2s, I2sRx};
use esp_hal::peripherals::{DMA_CH0, GPIO9, GPIO10, I2S0};
use esp_hal::time::Rate;

pub struct MicHardware<'a> {
    pub i2s: I2S0<'a>,
    pub dma_channel: DMA_CH0<'a>,
    /// PDM clock output pin (routed via I2S WS signal path).
    pub clk_pin: GPIO9<'a>,
    /// PDM data input pin.
    pub din_pin: GPIO10<'a>,
}

/// Setup hardware to interface with the `LMD3526B261-OFA01` PDM microphone on the ELECROW board.
///
/// esp-hal 1.0 only supports TDM mode, so we initialize I2S in TDM mode to get DMA and clocks
/// working, then patch the I2S0 registers to switch to PDM RX with hardware PDM→PCM conversion.
///
/// The clock trick: setting `sample_rate=78125` with `Data16Channel16` stereo produces
/// `bclk = 78125 × 2 × 16 = 2.5 MHz` and `bclk_div = 8`, giving the mic its required
/// ~2.5 MHz PDM clock. With DSR_8S (÷64), the PCM output rate is ~39 kHz.
///
/// Closest datasheet for `LMD3526B261-OFA03`: <https://jlcpcb.com/api/file/downloadByFileSystemAccessId/8604442987128901632>
/// Datasheet provided by ELECROW: <https://github.com/Elecrow-RD/CrowPanel-Advance-3.5-HMI-ESP32-S3-AI-Powered-IPS-Touch-Screen-480x320/blob/master/Datasheet/INMP441-Datasheet.pdf>
///
/// See also:
/// - esp-hal PDM gap: <https://github.com/esp-rs/esp-hal/issues/3704>
/// - ESP-IDF PDM RX implementation: `components/esp_driver_i2s/i2s_pdm.c`
/// - ESP-IDF PDM RX low-level: `components/hal/esp32s3/include/hal/i2s_ll.h`
pub fn init<'d>(
    mic_hardware: MicHardware<'d>,
    dma_rx_descriptors: &'static mut [DmaDescriptor],
) -> I2sRx<'d, Blocking> {
    // Step 1: Init I2S in TDM mode via esp-hal.
    // sample_rate=78125 tricks esp-hal into generating a 2.5 MHz PDM clock:
    //   bclk = 78125 * 2 * 16 = 2,500,000 Hz  (PDM clock on WS pin)
    //   mclk = 78125 * 256    = 20,000,000 Hz
    //   mclk_div = 160 MHz / 20 MHz = 8        (exact, no fractional)
    //   bclk_div = 20 MHz / 2.5 MHz = 8        (PDM minimum)
    // With DSR_8S (÷64): PCM output ≈ 39 kHz.
    let i2s = I2s::new(
        mic_hardware.i2s,
        mic_hardware.dma_channel,
        Config::new_tdm_philips()
            .with_sample_rate(Rate::from_hz(78125))
            .with_data_format(DataFormat::Data16Channel16),
    )
    .expect("I2S init failed");

    // Step 2: Patch I2S0 registers to switch from TDM → PDM RX mode.
    enable_pdm_rx();

    // Step 3: Build RX channel with correct GPIO mapping.
    // On ESP32-S3, PDM clock is output on the WS signal path (not BCLK).
    i2s.i2s_rx
        .with_ws(mic_hardware.clk_pin)
        .with_din(mic_hardware.din_pin)
        .build(dma_rx_descriptors)
}

/// Patch I2S0 registers to switch from TDM to PDM RX mode.
///
/// Replicates the register writes from ESP-IDF's `i2s_pdm.c` / `i2s_ll.h`:
/// - Enable PDM RX, disable TDM RX
/// - Enable hardware PDM→PCM decimation filter (only available on I2S0)
/// - Set down-sampling rate to DSR_8S (÷64)
/// - Set `rx_half_sample_bits = 15` (16 − 1), as ESP-IDF does for PDM
///
/// Must be called after `I2s::new()` and before starting DMA.
fn enable_pdm_rx() {
    // Safety: We have exclusive ownership of the I2S0 peripheral (it was moved into
    // `I2s::new` above). These are the same register writes ESP-IDF performs for PDM RX.
    let i2s0 = unsafe { &*esp32s3::I2S0::PTR };

    // rx_conf: flip mode from TDM to PDM with hardware decimation
    i2s0.rx_conf().modify(|_, w| {
        w.rx_pdm_en().set_bit();                // bit 20: enable PDM RX
        w.rx_tdm_en().clear_bit();              // bit 19: disable TDM RX
        w.rx_pdm2pcm_en().set_bit();            // bit 21: enable PDM→PCM filter
        w.rx_pdm_sinc_dsr_16_en().clear_bit()   // bit 22: DSR_8S (÷64 down-sampling)
    });

    // rx_conf1: PDM requires half_sample_bits = 16 − 1 = 15
    i2s0.rx_conf1().modify(|_, w| unsafe {
        w.rx_half_sample_bits().bits(15)        // bits 18:23
    });

    // Latch register changes into the I2S clock domain via rx_update (bit 8).
    i2s0.rx_conf().modify(|_, w| w.rx_update().clear_bit());
    i2s0.rx_conf().modify(|_, w| w.rx_update().set_bit());
}
