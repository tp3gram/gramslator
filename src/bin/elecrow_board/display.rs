use embedded_graphics::pixelcolor::Rgb666;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::peripherals::Peripherals;
use esp_hal::spi::master::{Config, Spi};
use esp_hal::time::Rate;

use esp_println as _;

use embedded_hal_bus::spi::ExclusiveDevice;

use mipidsi::interface::SpiInterface;
use mipidsi::models::ILI9488Rgb666;
use mipidsi::options::{ColorInversion, ColorOrder};
use mipidsi::{Builder, Display};

pub type PixelType = Rgb666;

pub fn init<'a>(
    peripherals: Peripherals,
    buffer: &'a mut [u8],
    mut delay: Delay,
) -> Display<
    SpiInterface<
        'a,
        ExclusiveDevice<Spi<'a, esp_hal::Blocking>, Output<'a>, embedded_hal_bus::spi::NoDelay>,
        Output<'a>,
    >,
    ILI9488Rgb666,
    mipidsi::NoResetPin,
> {
    let pin_tft_power = peripherals.GPIO14;
    let pin_backlight = peripherals.GPIO38;

    let mut tft_power = Output::new(pin_tft_power, Level::Low, OutputConfig::default());
    tft_power.set_high();
    let mut backlight = Output::new(pin_backlight, Level::Low, OutputConfig::default());
    backlight.set_high();

    // Define the SPI pins and create the SPI interface
    let pin_spi_sck = peripherals.GPIO42;
    let pin_spi_mosi = peripherals.GPIO39;
    let pin_spi_data_command = peripherals.GPIO41;
    let pin_spi_chip_select = peripherals.GPIO40;
    // No reset pin for display

    let spi_device = Spi::new(
        peripherals.SPI2,
        Config::default()
            // Clock frequency sourced from ELECROW example: https://github.com/Elecrow-RD/CrowPanel-Advance-3.5-HMI-ESP32-S3-AI-Powered-IPS-Touch-Screen-480x320/blob/master/example/Arduino_Code_35/V1.0/ESP32-AI-Dialogue/Advance_Ai_chat_35/LGFX_Setup.h
            .with_frequency(Rate::from_mhz(40)),
    )
    .unwrap()
    .with_mosi(pin_spi_mosi)
    .with_sck(pin_spi_sck);

    let chip_select = Output::new(pin_spi_chip_select, Level::Low, OutputConfig::default());
    let data_command = Output::new(pin_spi_data_command, Level::Low, OutputConfig::default());

    // Wrap SPI with ExclusiveDevice for thread-safe access
    let spi_device_wrapper = ExclusiveDevice::new_no_delay(spi_device, chip_select);

    let mipi_spi_interface = SpiInterface::new(spi_device_wrapper, data_command, buffer);

    // Define the display from the display interface and initialize it
    let display = Builder::new(mipidsi::models::ILI9488Rgb666, mipi_spi_interface)
        // Display for the ELECROW has BGR color order and inverted colors.
        .color_order(ColorOrder::Bgr)
        .invert_colors(ColorInversion::Inverted)
        .init(&mut delay)
        .unwrap();

    display
}
