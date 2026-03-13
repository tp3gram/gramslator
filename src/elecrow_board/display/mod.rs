//! Hardware-specific display initialisation for the ELECROW CrowPanel 3.5" HMI.
//!
//! Initialises the ILI9488 display controller via SPI using `mipidsi` for the
//! one-time register setup, then releases the driver and converts the SPI bus
//! to **async DMA mode**.  The returned [`AsyncDisplay`] streams pixel data
//! through [`flush_region`](AsyncDisplay::flush_region), yielding to the
//! Embassy executor during each ~4 KiB DMA chunk transfer.

mod async_display;
mod hardware;

pub use async_display::AsyncDisplay;
pub use hardware::{DisplayHardware, DisplaySPIBus};

use embedded_graphics::Drawable;
use embedded_graphics::pixelcolor::Rgb666;
use embedded_graphics::prelude::{DrawTarget, Point, Primitive, RgbColor};
use embedded_graphics::primitives::{Circle, PrimitiveStyle, Triangle};

use esp_hal::delay::Delay;
use esp_hal::dma::{DmaChannelFor, DmaRxBuf, DmaTxBuf};
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::spi::master::{AnySpi, Config, Spi};
use esp_hal::time::Rate;

use mipidsi::interface::SpiInterface;
use mipidsi::models::ILI9488Rgb666;
use mipidsi::options::{ColorInversion, ColorOrder, Orientation, Rotation};
use mipidsi::Builder;

use hardware::InitSpiDevice;

pub type PixelType = Rgb666;

// ---------------------------------------------------------------------------
// Initialisation
// ---------------------------------------------------------------------------

/// Initialise the ILI9488 display and return an async DMA display driver.
///
/// Uses `mipidsi` for the one-time controller register setup, then releases
/// the driver and converts the SPI bus to async mode.
pub fn init<'a>(
    display_hardware: DisplayHardware<'a>,
    dma_channel: impl DmaChannelFor<AnySpi<'a>>,
    dma_rx_buf: DmaRxBuf,
    dma_tx_buf: DmaTxBuf,
    mut delay: Delay,
) -> AsyncDisplay<'a> {
    // Power & backlight on
    let mut tft_power = Output::new(
        display_hardware.tft_power_pin,
        Level::Low,
        OutputConfig::default(),
    );
    tft_power.set_high();
    let mut backlight = Output::new(
        display_hardware.backlight_pin,
        Level::Low,
        OutputConfig::default(),
    );
    backlight.set_high();

    // SPI bus (blocking for mipidsi init, converted to async afterwards)
    let spi_dma_bus = Spi::new(
        display_hardware.spi.spi_peripheral,
        Config::default().with_frequency(Rate::from_mhz(40)),
    )
    .unwrap()
    .with_mosi(display_hardware.spi.mosi)
    .with_sck(display_hardware.spi.sck)
    .with_dma(dma_channel)
    .with_buffers(dma_rx_buf, dma_tx_buf);

    let cs = Output::new(
        display_hardware.spi.chip_select,
        Level::Low,
        OutputConfig::default(),
    );
    let dc = Output::new(
        display_hardware.spi.data_command,
        Level::Low,
        OutputConfig::default(),
    );

    // -- mipidsi init phase (blocking) ------------------------------------
    let device = InitSpiDevice { bus: spi_dma_bus, cs };
    let mut buffer = [0u8; 1024];
    let spi_interface = SpiInterface::new(device, dc, &mut buffer);

    let display = Builder::new(ILI9488Rgb666, spi_interface)
        .color_order(ColorOrder::Bgr)
        .invert_colors(ColorInversion::Inverted)
        .orientation(Orientation::new().rotate(Rotation::Deg270).flip_vertical())
        .init(&mut delay)
        .unwrap();

    // -- release mipidsi, reclaim SPI components --------------------------
    let (spi_interface, _model, _rst) = display.release();
    let (device, dc) = spi_interface.release();
    let InitSpiDevice { bus, cs } = device;

    // -- convert to async DMA mode ----------------------------------------
    let bus = bus.into_async();

    AsyncDisplay { bus, cs, dc }
}

// ---------------------------------------------------------------------------
// Embedded-graphics test helper
// ---------------------------------------------------------------------------

/// Example from: https://github.com/almindor/mipidsi/blob/master/examples/spi-ili9486-esp32-c3/src/main.rs
pub fn draw_smiley<T: DrawTarget<Color = PixelType>>(display: &mut T) -> Result<(), T::Error> {
    Circle::new(Point::new(50, 100), 40)
        .into_styled(PrimitiveStyle::with_fill(PixelType::WHITE))
        .draw(display)?;
    Circle::new(Point::new(50, 200), 40)
        .into_styled(PrimitiveStyle::with_fill(PixelType::WHITE))
        .draw(display)?;
    Triangle::new(
        Point::new(130, 140),
        Point::new(130, 200),
        Point::new(160, 170),
    )
    .into_styled(PrimitiveStyle::with_fill(PixelType::RED))
    .draw(display)?;
    Triangle::new(
        Point::new(130, 150),
        Point::new(130, 190),
        Point::new(150, 170),
    )
    .into_styled(PrimitiveStyle::with_fill(PixelType::BLACK))
    .draw(display)?;
    Ok(())
}
