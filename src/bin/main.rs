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
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use tinyrlibc as _;

extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.2.0

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // mbedTLS alone needs 40+ KB for session state, so we use regular SRAM.
    esp_alloc::heap_allocator!(size: 150_000);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    info!("Embassy initialized!");

    // ---- WiFi -----------------------------------------------------------------

    let network = elecrow_board::network::init(
        elecrow_board::network::NetworkHardware {
            wifi: peripherals.WIFI,
        },
        &spawner,
    )
    .await;

    // ---- TLS initialization ---------------------------------------------------

    // True Random Number Generator + mbedTLS singleton
    let tls = elecrow_board::network::init_tls(elecrow_board::network::TlsHardware {
        rng: peripherals.RNG,
        adc1: peripherals.ADC1,
    });

    elecrow_board::network::test_stream(network, &tls).await;

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.0.0/examples
}
