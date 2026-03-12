//! Hardware-specific display initialisation for the ELECROW CrowPanel 3.5" HMI.
//!
//! Initialises the ILI9488 display controller via SPI using `mipidsi` for the
//! one-time register setup, then releases the driver and converts the SPI bus
//! to **async DMA mode**.  The returned [`AsyncDisplay`] streams pixel data
//! through [`flush_region`](AsyncDisplay::flush_region), yielding to the
//! Embassy executor during each ~4 KiB DMA chunk transfer.

extern crate alloc;

use alloc::vec;

use esp_hal::delay::Delay;
use esp_hal::dma::{DmaChannelFor, DmaRxBuf, DmaTxBuf};
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::peripherals::{GPIO14, GPIO38, GPIO39, GPIO40, GPIO41, GPIO42, SPI2};
use esp_hal::spi::master::{AnySpi, Config, Spi, SpiDmaBus};
use esp_hal::time::Rate;

use embedded_hal::spi::{ErrorType, Operation, SpiBus, SpiDevice};

use mipidsi::interface::SpiInterface;
use mipidsi::models::ILI9488Rgb666;
use mipidsi::options::{ColorInversion, ColorOrder, Orientation, Rotation};
use mipidsi::Builder;

// Keep embedded-graphics imports for draw_smiley helper.
use embedded_graphics::Drawable;
use embedded_graphics::pixelcolor::Rgb666;
use embedded_graphics::prelude::{DrawTarget, Point, Primitive, RgbColor};
use embedded_graphics::primitives::{Circle, PrimitiveStyle, Triangle};

pub type PixelType = Rgb666;

// ---------------------------------------------------------------------------
// Hardware descriptor structs
// ---------------------------------------------------------------------------

pub struct DisplaySPIBus<'a> {
    pub spi_peripheral: SPI2<'a>,
    pub sck: GPIO42<'a>,
    pub mosi: GPIO39<'a>,
    pub data_command: GPIO41<'a>,
    pub chip_select: GPIO40<'a>,
}

pub struct DisplayHardware<'a> {
    pub spi: DisplaySPIBus<'a>,
    pub tft_power_pin: GPIO14<'a>,
    pub backlight_pin: GPIO38<'a>,
}

// ---------------------------------------------------------------------------
// Temporary blocking SpiDevice for mipidsi init
// ---------------------------------------------------------------------------

/// Wraps `SpiDmaBus<Blocking>` + CS pin into an `embedded_hal::spi::SpiDevice`
/// that can be decomposed after use.  Only lives during display initialisation.
struct InitSpiDevice<'a> {
    bus: SpiDmaBus<'a, esp_hal::Blocking>,
    cs: Output<'a>,
}

impl ErrorType for InitSpiDevice<'_> {
    type Error = esp_hal::spi::Error;
}

impl SpiDevice for InitSpiDevice<'_> {
    fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Result<(), Self::Error> {
        self.cs.set_low();
        let result = operations.iter_mut().try_for_each(|op| match op {
            Operation::Read(buf) => SpiBus::read(&mut self.bus, buf),
            Operation::Write(buf) => SpiBus::write(&mut self.bus, buf),
            Operation::Transfer(read, write) => SpiBus::transfer(&mut self.bus, read, write),
            Operation::TransferInPlace(buf) => SpiBus::transfer_in_place(&mut self.bus, buf),
            Operation::DelayNs(_) => Ok(()),
        });
        let flush = SpiBus::flush(&mut self.bus);
        self.cs.set_high();
        result?;
        flush
    }
}

// ---------------------------------------------------------------------------
// AsyncDisplay — post-init async DMA display driver
// ---------------------------------------------------------------------------

/// Async DMA display driver for the ILI9488.
///
/// Created by [`init`], which uses `mipidsi` for the one-time ILI9488 register
/// setup and then converts the SPI bus to async DMA mode.
///
/// Pixel data is streamed through [`flush_region`](Self::flush_region) which
/// yields to the Embassy executor during each ~4 KiB DMA chunk, freeing the
/// CPU for other tasks (audio capture, networking, …).
pub struct AsyncDisplay<'a> {
    bus: SpiDmaBus<'a, esp_hal::Async>,
    cs: Output<'a>,
    dc: Output<'a>,
}

impl<'a> AsyncDisplay<'a> {
    /// Wire-format conversion buffer size (matches DMA TX chunk capacity).
    const WIRE_BUF_SIZE: usize = 4092;

    // -- low-level helpers ------------------------------------------------

    /// Send a command byte + optional parameter bytes (blocking, tiny).
    fn send_cmd(&mut self, cmd: u8, args: &[u8]) {
        // Command phase: DC low
        self.dc.set_low();
        self.cs.set_low();
        SpiBus::write(&mut self.bus, &[cmd]).unwrap();
        self.cs.set_high();

        // Data phase: DC high
        if !args.is_empty() {
            self.dc.set_high();
            self.cs.set_low();
            SpiBus::write(&mut self.bus, args).unwrap();
            self.cs.set_high();
        }
    }

    /// Set the ILI9488 address window.
    fn set_address_window(&mut self, sx: u16, sy: u16, ex: u16, ey: u16) {
        // Column Address Set (0x2A)
        self.send_cmd(
            0x2A,
            &[
                (sx >> 8) as u8,
                sx as u8,
                (ex >> 8) as u8,
                ex as u8,
            ],
        );
        // Page Address Set (0x2B)
        self.send_cmd(
            0x2B,
            &[
                (sy >> 8) as u8,
                sy as u8,
                (ey >> 8) as u8,
                ey as u8,
            ],
        );
    }

    // -- public API -------------------------------------------------------

    /// Flush a rectangular region of an Rgb666 framebuffer to the display.
    ///
    /// The framebuffer stores 3 bytes per pixel `[r, g, b]`, each in 0–63.
    /// This method converts to the ILI9488 wire format (`r << 2, g << 2,
    /// b << 2`) in ~4 KiB chunks and streams each chunk via async DMA.
    ///
    /// Returns the number of pixels pushed.
    pub async fn flush_region(
        &mut self,
        fb_buf: &[u8],
        fb_width: u32,
        sx: u16,
        sy: u16,
        w: u16,
        h: u16,
    ) -> Result<usize, esp_hal::spi::Error> {
        let pixel_count = w as usize * h as usize;
        if pixel_count == 0 {
            return Ok(0);
        }

        // Set address window + Memory Write command (0x2C)
        self.set_address_window(sx, sy, sx + w - 1, sy + h - 1);
        self.dc.set_low();
        self.cs.set_low();
        SpiBus::write(&mut self.bus, &[0x2C]).unwrap();
        // Switch to data mode; CS stays low for the entire pixel stream.
        self.dc.set_high();

        // Heap-allocated wire-format conversion buffer (one DMA chunk).
        // Allocated once per flush in PSRAM — negligible cost.
        let mut wire_buf = vec![0u8; Self::WIRE_BUF_SIZE];
        let mut wire_pos = 0;

        for row in 0..h as usize {
            for col in 0..w as usize {
                let fb_idx =
                    ((sy as usize + row) * fb_width as usize + (sx as usize + col)) * 3;
                wire_buf[wire_pos] = fb_buf[fb_idx] << 2;
                wire_buf[wire_pos + 1] = fb_buf[fb_idx + 1] << 2;
                wire_buf[wire_pos + 2] = fb_buf[fb_idx + 2] << 2;
                wire_pos += 3;

                if wire_pos + 3 > Self::WIRE_BUF_SIZE {
                    self.bus.write_async(&wire_buf[..wire_pos]).await?;
                    wire_pos = 0;
                }
            }
        }
        if wire_pos > 0 {
            self.bus.write_async(&wire_buf[..wire_pos]).await?;
        }

        self.cs.set_high();
        Ok(pixel_count)
    }
}

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
