#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

mod elecrow_board;

use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::timer::timg::TimerGroup;

use defmt::info;
use esp_println as _;

use embassy_executor::Spawner;

use embedded_graphics::prelude::RgbColor;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

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

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    info!("Embassy initialized!");

    let radio_init = esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller");
    let (mut _wifi_controller, _interfaces) =
        esp_radio::wifi::new(&radio_init, peripherals.WIFI, Default::default())
            .expect("Failed to initialize Wi-Fi controller");

    // TODO: Spawn some tasks
    let _ = spawner;

    info!("Buzzer");

    let mut buzzer = elecrow_board::buzzer::init(elecrow_board::buzzer::BuzzerHardware {
        buzzer_pin: peripherals.GPIO8,
    });

    info!("Buzzer on!");

    // --- Display initialization ---
    let mut buffer = [0_u8; 512];
    let delay = Delay::new();
    let mut display = elecrow_board::display::init(
        elecrow_board::display::DisplayHardware {
            spi: elecrow_board::display::DisplaySPIBus {
                spi_peripheral: peripherals.SPI2,
                sck: peripherals.GPIO42,
                mosi: peripherals.GPIO39,
                data_command: peripherals.GPIO41,
                chip_select: peripherals.GPIO40,
            },
            pin_tft_power: peripherals.GPIO14,
            pin_backlight: peripherals.GPIO38,
        },
        &mut buffer,
        delay,
    );

    info!("Display initialized!");

    info!("Draw green");

    embedded_graphics::prelude::DrawTarget::clear(
        &mut display,
        elecrow_board::display::PixelType::GREEN,
    )
    .unwrap();

    info!("Drawing smiley face");

    // Draw a smiley face with white eyes and a red mouth
    elecrow_board::display::draw_smiley(&mut display).unwrap();

    info!("Smiley drawn!");

    loop {
        // buzzer.set_high();
        // Delay::new().delay_millis(10);
        // buzzer.set_low();
        // Delay::new().delay_millis(10);
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.0.0/examples
}
