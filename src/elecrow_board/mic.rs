extern crate alloc;

use defmt::{error, info};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::pipe::Pipe;
use esp_hal::Blocking;
use esp_hal::dma::DmaDescriptor;
use esp_hal::i2s::master::{Config, DataFormat, I2s, I2sRx};
use esp_hal::peripherals::{DMA_CH0, GPIO9, GPIO10, I2S0};
use esp_hal::time::Rate;

/// Size of the circular DMA buffer in bytes.
/// Must match the size passed to `dma_circular_buffers!()` in the caller.
pub const DMA_BUF_SIZE: usize = 32000;

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
/// The clock trick: `pcm_sample_rate` is the target PCM output rate. The I2S sample rate is
/// derived at runtime so that the PDM clock lands at the right frequency for the mic (1–3.25 MHz)
/// and DSR_8S (÷64) decimation produces the desired PCM rate:
///
/// ```text
/// i2s_rate = pcm_sample_rate × DSR_DIVISOR / BITS_PER_FRAME
///          = pcm_sample_rate × 64 / 32
///          = pcm_sample_rate × 2
/// bclk     = i2s_rate × 2 × 16      = pcm_sample_rate × 64   (PDM clock on WS pin)
/// mclk     = i2s_rate × 256          = pcm_sample_rate × 512
/// mclk_div = 160 MHz / mclk
/// bclk_div = mclk / bclk = 256 / 32 = 8                      (PDM minimum)
/// pcm_rate = bclk / 64              = pcm_sample_rate         ✓
/// ```
///
/// E.g. `pcm_sample_rate = 16_000`:
///   `i2s_rate = 32,000`, `bclk = 1,024,000 Hz`, `mclk = 8,192,000 Hz`,
///   `mclk_div = 160 MHz / 8.192 MHz ≈ 19.5` (fractional), `bclk_div = 8`.
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
    pcm_sample_rate: u32,
) -> I2sRx<'d, Blocking> {
    // Step 1: Init I2S in TDM mode via esp-hal.
    // Derive i2s_rate from the target PCM output rate (see doc comment for full derivation):
    //   i2s_rate = pcm_sample_rate * DSR_DIVISOR / BITS_PER_FRAME
    //   bclk     = i2s_rate * 2 * 16             (PDM clock on WS pin)
    //   mclk     = i2s_rate * 256
    //   mclk_div = 160 MHz / mclk                (may be fractional)
    //   bclk_div = mclk / bclk = 256 / 32 = 8   (PDM minimum)
    // With DSR_8S (÷64): PCM output = bclk / 64 = pcm_sample_rate.
    const BITS_PER_FRAME: u32 = 32; // Data16Channel16: 2 channels × 16 bits
    const DSR_DIVISOR: u32 = 64; // DSR_8S mode (rx_pdm_sinc_dsr_16_en = 0)
    let i2s_sample_rate = pcm_sample_rate * DSR_DIVISOR / BITS_PER_FRAME;
    let pdm_clock = i2s_sample_rate * BITS_PER_FRAME;

    info!(
        "Mic rates: i2s_rate={} Hz, pdm_clock={} Hz, pcm_rate={} Hz",
        i2s_sample_rate, pdm_clock, pcm_sample_rate
    );

    let i2s = I2s::new(
        mic_hardware.i2s,
        mic_hardware.dma_channel,
        Config::new_tdm_philips()
            .with_sample_rate(Rate::from_hz(i2s_sample_rate))
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
        w.rx_pdm_en().set_bit(); // bit 20: enable PDM RX
        w.rx_tdm_en().clear_bit(); // bit 19: disable TDM RX
        w.rx_pdm2pcm_en().set_bit(); // bit 21: enable PDM→PCM filter
        w.rx_pdm_sinc_dsr_16_en().clear_bit() // bit 22: DSR_8S (÷64 down-sampling)
    });

    // rx_conf1: PDM requires half_sample_bits = 16 − 1 = 15
    i2s0.rx_conf1().modify(|_, w| unsafe {
        w.rx_half_sample_bits().bits(15) // bits 18:23
    });

    // Latch register changes into the I2S clock domain via rx_update (bit 8).
    i2s0.rx_conf().modify(|_, w| w.rx_update().clear_bit());
    i2s0.rx_conf().modify(|_, w| w.rx_update().set_bit());
}

/// Async pipe bridging the blocking DMA read loop and async consumers.
/// The blocking side pushes via `try_write`; async readers await via [`read_mic`].
pub static MIC_PIPE: Pipe<CriticalSectionRawMutex, DMA_BUF_SIZE> = Pipe::new();

/// Starts circular DMA and runs a blocking read loop, pushing popped audio
/// data into [`MIC_PIPE`]. Does not return unless a DMA error occurs.
pub fn read_mic_dma_loop_blocking(
    i2s_rx: &mut I2sRx<'_, Blocking>,
    rx_buffer: &mut [u8; DMA_BUF_SIZE],
) {
    info!("Mic DMA");

    let mut transfer = i2s_rx
        .read_dma_circular(rx_buffer)
        .expect("Failed to start I2S circular DMA read");

    let mut buf = alloc::vec![0u8; DMA_BUF_SIZE];

    loop {
        match transfer.available() {
            Err(e) => {
                info!("DMA error: {}", e);
                break;
            }
            Ok(0) => {} // nothing ready yet
            Ok(_) => {
                let read = transfer.pop(&mut buf).expect("pop failed");
                // info!("DMA read: {}", read);

                // Non-blocking write into the pipe; drops data if the pipe is full.
                let _ = MIC_PIPE.try_write(&buf[..read]).map_err(|e| {
                    error!("Pipe write error: {}", e);
                });
            }
        }
    }

    info!("DMA loop exited.");
}
