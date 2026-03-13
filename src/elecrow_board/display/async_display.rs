extern crate alloc;

use alloc::vec;

use embedded_hal::spi::SpiBus;
use esp_hal::gpio::Output;
use esp_hal::spi::master::SpiDmaBus;

/// Async DMA display driver for the ILI9488.
///
/// Created by [`init`](super::init), which uses `mipidsi` for the one-time
/// ILI9488 register setup and then converts the SPI bus to async DMA mode.
///
/// Pixel data is streamed through [`flush_region`](Self::flush_region) which
/// yields to the Embassy executor during each ~4 KiB DMA chunk, freeing the
/// CPU for other tasks (audio capture, networking, …).
pub struct AsyncDisplay<'a> {
    pub(super) bus: SpiDmaBus<'a, esp_hal::Async>,
    pub(super) cs: Output<'a>,
    pub(super) dc: Output<'a>,
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
            &[(sx >> 8) as u8, sx as u8, (ex >> 8) as u8, ex as u8],
        );
        // Page Address Set (0x2B)
        self.send_cmd(
            0x2B,
            &[(sy >> 8) as u8, sy as u8, (ey >> 8) as u8, ey as u8],
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
                let fb_idx = ((sy as usize + row) * fb_width as usize + (sx as usize + col)) * 3;
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
