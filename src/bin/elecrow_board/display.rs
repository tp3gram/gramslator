use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::peripherals::{GPIO14, GPIO38, GPIO39, GPIO40, GPIO41, GPIO42, SPI2};
use esp_hal::spi::master::{Config, Spi};
use esp_hal::time::Rate;

use esp_println as _;

use embedded_hal_bus::spi::ExclusiveDevice;

use mipidsi::interface::SpiInterface;
use mipidsi::models::ILI9488Rgb666;
use mipidsi::options::{ColorInversion, ColorOrder};
use mipidsi::{Builder, Display};

use embedded_graphics::Drawable;
use embedded_graphics::pixelcolor::Rgb666;
use embedded_graphics::prelude::{DrawTarget, Point, Primitive, RgbColor};
use embedded_graphics::primitives::{Circle, PrimitiveStyle, Triangle};

pub type PixelType = Rgb666;

pub struct DisplaySPIBus<'a> {
    pub spi_peripheral: SPI2<'a>,

    pub sck: GPIO42<'a>,
    pub mosi: GPIO39<'a>,
    pub data_command: GPIO41<'a>,
    pub chip_select: GPIO40<'a>,
}

pub struct DisplayHardware<'a> {
    pub spi: DisplaySPIBus<'a>,
    pub pin_tft_power: GPIO14<'a>,
    pub pin_backlight: GPIO38<'a>,
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

/// Example from: https://github.com/almindor/mipidsi/blob/master/examples/spi-ili9486-esp32-c3/src/main.rs
pub fn draw_smiley<T: DrawTarget<Color = PixelType>>(display: &mut T) -> Result<(), T::Error> {
    // Draw the left eye as a circle located at (50, 100), with a diameter of 40, filled with white
    Circle::new(Point::new(50, 100), 40)
        .into_styled(PrimitiveStyle::with_fill(PixelType::WHITE))
        .draw(display)?;

    // Draw the right eye as a circle located at (50, 200), with a diameter of 40, filled with white
    Circle::new(Point::new(50, 200), 40)
        .into_styled(PrimitiveStyle::with_fill(PixelType::WHITE))
        .draw(display)?;

    // Draw an upside down red triangle to represent a smiling mouth
    Triangle::new(
        Point::new(130, 140),
        Point::new(130, 200),
        Point::new(160, 170),
    )
    .into_styled(PrimitiveStyle::with_fill(PixelType::RED))
    .draw(display)?;

    // Cover the top part of the mouth with a black triangle so it looks closed instead of open
    Triangle::new(
        Point::new(130, 150),
        Point::new(130, 190),
        Point::new(150, 170),
    )
    .into_styled(PrimitiveStyle::with_fill(PixelType::BLACK))
    .draw(display)?;

    Ok(())
}
