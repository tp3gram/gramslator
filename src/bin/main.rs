#![feature(allocator_api)]
#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

extern crate alloc;

use defmt::info;
use embassy_executor::Spawner;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::dma_circular_buffers;
use esp_hal::timer::timg::TimerGroup;
use gramslator::elecrow_board;
use gramslator::net;
use tinyrlibc as _;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // ---- Heap setup -----------------------------------------------------------
    //
    // The global allocator tries regions in registration order.  PSRAM is
    // registered first so the default path (Box::new, Vec::new, String::new,
    // and — crucially — mbedTLS's internal malloc) lands in the 8 MB PSRAM
    // pool.  The small internal SRAM region is registered second as a fallback.
    //
    // For explicit placement use the standard allocator_api (enabled by the
    // `nightly` feature on esp-alloc):
    //
    //   Box::new_in(value, esp_alloc::InternalMemory)   // force SRAM
    //   Box::new_in(value, esp_alloc::ExternalMemory)   // force PSRAM
    //   Vec::<u8>::new_in(esp_alloc::InternalMemory)    // force SRAM
    //
    // ⚠ Atomic operations are unreliable on PSRAM for ESP32-S3.  Any
    //   heap-allocated Atomic* types MUST use InternalMemory explicitly.
    //   (Stack/static atomics are fine — they live in SRAM regardless.)

    // 1️⃣  External PSRAM (8 MB) — default for all heap allocations.
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    // 2️⃣  Internal SRAM (72 KB) — fallback & explicit via InternalMemory.
    esp_alloc::heap_allocator!(size: 72_000);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    info!("Embassy initialized!");

    // ---- WiFi -----------------------------------------------------------------

    let network = elecrow_board::network::init(
        elecrow_board::network::NetworkHardware {
            wifi: peripherals.WIFI,
        },
        &spawner,
    );

    // ---- TLS initialization ---------------------------------------------------

    // True Random Number Generator + mbedTLS singleton
    let tls = net::init_tls(net::TlsHardware {
        rng: peripherals.RNG,
        adc1: peripherals.ADC1,
    });

    // elecrow_board::network::test_stream(network, &tls).await;

    // ---- Analog switch: route GPIO9/10 to microphone -------------------------

    let _mic_switch = elecrow_board::mic_wireless_module_switch::MicWirelessModuleSwitchHardware::init(
        peripherals.GPIO45,
        elecrow_board::mic_wireless_module_switch::SwitchState::Mic,
    );

    // ---- Microphone (I2S RX) -------------------------------------------------

    let (mut rx_buffer, rx_descriptors, _, _) = dma_circular_buffers!(32000, 0);

    let mut i2s_rx = elecrow_board::mic::init(
        elecrow_board::mic::MicHardware {
            i2s: peripherals.I2S0,
            dma_channel: peripherals.DMA_CH0,
            clk_pin: peripherals.GPIO9,
            din_pin: peripherals.GPIO10,
        },
        rx_descriptors,
        16_000,
    );

    info!("Microphone I2S configured, starting circular DMA...");

    let mut transfer = i2s_rx
        .read_dma_circular(&mut rx_buffer)
        .expect("Failed to start I2S circular DMA read");

    // Heap-allocated pop buffer — must be >= max available() to satisfy pop() API
    let mut buf = alloc::vec![0u8; 32000];

    // Rolling RMS over the last ~100 ms of audio.
    // At ~39 kHz mono 16-bit: 100 ms ≈ 3906 samples.
    const RMS_WINDOW_SAMPLES: usize = 3906;
    let mut ring = alloc::vec![0i16; RMS_WINDOW_SAMPLES];
    let mut ring_pos: usize = 0;
    let mut sum_sq: u64 = 0; // running sum of squares over the window

    loop {
        match transfer.available() {
            Err(e) => {
                info!("DMA error: {}", e);
                break;
            }
            Ok(0) => {} // nothing ready yet
            Ok(_) => {
                let read = transfer.pop(&mut buf).expect("pop failed");

                // Feed samples into the rolling window and update sum_sq
                for sample_bytes in buf[..read].chunks_exact(2) {
                    let sample = i16::from_le_bytes([sample_bytes[0], sample_bytes[1]]);

                    // Remove oldest sample's contribution
                    let old = ring[ring_pos] as i64;
                    sum_sq -= (old * old) as u64;

                    // Insert new sample
                    ring[ring_pos] = sample;
                    let new = sample as i64;
                    sum_sq += (new * new) as u64;

                    ring_pos = (ring_pos + 1) % RMS_WINDOW_SAMPLES;
                }

                let mean_sq = sum_sq / RMS_WINDOW_SAMPLES as u64;

                // dBFS = 20 * log10(rms / 32767) = 10 * log10(mean_sq / 32767^2)
                let dbfs = if mean_sq == 0 {
                    -96.0_f32
                } else {
                    let mean_sq_f = mean_sq as f32;
                    // 32767^2 = 1_073_676_289
                    10.0 * libm::log10f(mean_sq_f / 1_073_676_289.0)
                };

                // RMS for reference
                let rms = libm::sqrtf(mean_sq as f32) as u32;

                // Log dBFS as fixed-point tenths to avoid defmt float formatting
                let dbfs_int = dbfs as i32;
                let dbfs_frac = (libm::fabsf(dbfs * 10.0) as u32) % 10;
                info!(
                    "Mic: read={},\trms={},\tdBFS={}.{}",
                    read, rms, dbfs_int, dbfs_frac
                );
            }
        }
    }

    info!("DMA loop exited, halting.");
    loop {}
}
