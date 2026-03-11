use embedded_graphics::pixelcolor::Rgb666;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::spi::master::{Config, Spi};
use esp_hal::time::Rate;

use esp_println as _;

use embedded_hal_bus::spi::ExclusiveDevice;

use mipidsi::interface::SpiInterface;
use mipidsi::models::ILI9488Rgb666;
use mipidsi::options::{ColorInversion, ColorOrder};
use mipidsi::{Builder, Display};

pub type PixelType = Rgb666;

pub struct DisplaySPIBus<'a> {
    pub spi_peripheral: esp_hal::peripherals::SPI2<'a>,

    pub sck: esp_hal::peripherals::GPIO42<'a>,
    pub mosi: esp_hal::peripherals::GPIO39<'a>,
    pub data_command: esp_hal::peripherals::GPIO41<'a>,
    pub chip_select: esp_hal::peripherals::GPIO40<'a>,
}

pub struct DisplayHardware<'a> {
    pub spi: DisplaySPIBus<'a>,
    pub pin_tft_power: esp_hal::peripherals::GPIO14<'a>,
    pub pin_backlight: esp_hal::peripherals::GPIO38<'a>,
}

pub type DisplayType<'a> = Display<
    SpiInterface<
        'a,
        ExclusiveDevice<Spi<'a, esp_hal::Blocking>, Output<'a>, embedded_hal_bus::spi::NoDelay>,
        Output<'a>,
    >,
    ILI9488Rgb666,
    mipidsi::NoResetPin,
>;

pub fn init<'a>(
    display_hardware: DisplayHardware<'a>,
    buffer: &'a mut [u8],
    mut delay: Delay,
) -> DisplayType<'a> {
    let mut tft_power = Output::new(
        display_hardware.pin_tft_power,
        Level::Low,
        OutputConfig::default(),
    );
    tft_power.set_high();
    let mut backlight = Output::new(
        display_hardware.pin_backlight,
        Level::Low,
        OutputConfig::default(),
    );
    backlight.set_high();

    let spi_device = Spi::new(
        display_hardware.spi.spi_peripheral,
        Config::default()
            // Clock frequency sourced from ELECROW example: https://github.com/Elecrow-RD/CrowPanel-Advance-3.5-HMI-ESP32-S3-AI-Powered-IPS-Touch-Screen-480x320/blob/master/example/Arduino_Code_35/V1.0/ESP32-AI-Dialogue/Advance_Ai_chat_35/LGFX_Setup.h
            .with_frequency(Rate::from_mhz(40)),
    )
    .unwrap()
    .with_mosi(display_hardware.spi.mosi)
    .with_sck(display_hardware.spi.sck);

    let chip_select = Output::new(
        display_hardware.spi.chip_select,
        Level::Low,
        OutputConfig::default(),
    );
    let data_command = Output::new(
        display_hardware.spi.data_command,
        Level::Low,
        OutputConfig::default(),
    );

    // Wrap SPI with ExclusiveDevice for thread-safe access
    let spi_device_wrapper = ExclusiveDevice::new_no_delay(spi_device, chip_select);

    let mipi_spi_interface = SpiInterface::new(spi_device_wrapper, data_command, buffer);

    // Define the display from the display interface and initialize it
    Builder::new(mipidsi::models::ILI9488Rgb666, mipi_spi_interface)
        // Display for the ELECROW has BGR color order and inverted colors.
        .color_order(ColorOrder::Bgr)
        .invert_colors(ColorInversion::Inverted)
        .init(&mut delay)
        .unwrap()
}
