#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

mod elecrow_board;

use defmt::info;
use embassy_executor::Spawner;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::dma_circular_buffers;
use esp_hal::timer::timg::TimerGroup;

extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(size: 150_000);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    info!("Embassy initialized!");

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
    );

    info!("Microphone I2S configured, starting circular DMA...");

    let mut transfer = i2s_rx
        .read_dma_circular(&mut rx_buffer)
        .expect("Failed to start I2S circular DMA read");

    // Heap-allocated pop buffer — must be >= max available() to satisfy pop() API
    let mut buf = alloc::vec![0u8; 32000];
    let mut iteration: u32 = 0;
    loop {
        match transfer.available() {
            Err(e) => {
                info!("DMA error: {}", e);
                break;
            }
            Ok(0) => {} // nothing ready yet
            Ok(_) => {
                let read = transfer.pop(&mut buf).expect("pop failed");

                // Only log every 64th chunk to avoid falling behind
                if iteration % 64 == 0 {
                    let mut peak: i16 = 0;
                    let mut nonzero_bytes: u32 = 0;
                    for sample_bytes in buf[..read].chunks_exact(2) {
                        let sample =
                            i16::from_le_bytes([sample_bytes[0], sample_bytes[1]]);
                        let abs = sample.saturating_abs();
                        if abs > peak {
                            peak = abs;
                        }
                        if sample != 0 {
                            nonzero_bytes += 1;
                        }
                    }
                    // Log first 16 raw bytes for diagnosis
                    let n = if read >= 16 { 16 } else { read };
                    info!(
                        "Mic: read={}, peak={}, nonzero_samples={}, raw={=[u8]:x}",
                        read, peak, nonzero_bytes, &buf[..n]
                    );
                }
                iteration = iteration.wrapping_add(1);
            }
        }
    }

    info!("DMA loop exited, halting.");
    loop {}
}
