extern crate alloc;

use alloc::vec::Vec;

use embedded_graphics::prelude::*;

pub use embedded_graphics::geometry::Size;
pub use embedded_graphics::pixelcolor::Rgb666;
pub use embedded_graphics::prelude::{DrawTarget, Point, RgbColor};
pub use embedded_graphics::primitives::Rectangle;

/// PSRAM-backed framebuffer with dirty-region tracking.
///
/// All drawing goes into an in-memory pixel buffer.  Call [`flush`](Self::flush)
/// to push **only the changed region** to the hardware display via SPI.
///
/// The framebuffer stores 3 bytes per pixel (R6, G6, B6) in row-major order,
/// totalling `width × height × 3` bytes.  For a 480×320 display this is
/// ~461 KB — comfortably in PSRAM.
///
/// ## Dirty-rect tracking
///
/// Every [`DrawTarget`] operation marks the affected area dirty.  `flush()`
/// sends only the bounding box of all dirty regions since the last flush,
/// then resets the tracker.  Typical translator-UI updates (one or two lines
/// of text) push 5–20 % of the screen, yielding 30–60+ effective FPS at
/// 40 MHz SPI.
pub struct Framebuffer {
    width: u32,
    height: u32,
    /// Pixel storage: 3 bytes per pixel `[r, g, b]`, each 0–63 (Rgb666).
    buf: Vec<u8>,
    /// Bounding box of all modifications since the last [`flush`](Self::flush).
    dirty: Option<Rectangle>,
    /// Optional clip rectangle — when set, all drawing operations are
    /// restricted to this region.  Pixels outside the clip are silently
    /// discarded.  Use [`set_clip`](Self::set_clip) to enable/disable.
    clip: Option<Rectangle>,
}

impl Framebuffer {
    /// Create a new black framebuffer of the given dimensions.
    ///
    /// The backing buffer is allocated via the global allocator (PSRAM when
    /// registered first).
    pub fn new(width: u32, height: u32) -> Self {
        let size = (width as usize) * (height as usize) * 3;
        let buf = alloc::vec![0u8; size];
        Self {
            width,
            height,
            buf,
            dirty: None,
            clip: None,
        }
    }

    /// Returns `true` if any region has been modified since the last flush.
    pub fn is_dirty(&self) -> bool {
        self.dirty.is_some()
    }

    /// Returns the current dirty bounding box, if any.
    pub fn dirty_rect(&self) -> Option<Rectangle> {
        self.dirty
    }

    /// Set (or clear) the clip rectangle.
    ///
    /// When set, all drawing operations are restricted to pixels inside
    /// this rectangle.  Pass `None` to disable clipping.
    pub fn set_clip(&mut self, clip: Option<Rectangle>) {
        self.clip = clip;
    }

    /// Check whether a pixel coordinate falls inside the current clip rect.
    #[inline]
    fn clip_contains(&self, x: u32, y: u32) -> bool {
        match &self.clip {
            None => true,
            Some(c) => {
                let cx = c.top_left.x as u32;
                let cy = c.top_left.y as u32;
                x >= cx && x < cx + c.size.width && y >= cy && y < cy + c.size.height
            }
        }
    }

    /// Union a rectangle into the dirty tracker (clamped to FB bounds and clip).
    fn mark_dirty(&mut self, rect: Rectangle) {
        let fb_rect = Rectangle::new(Point::new(0, 0), Size::new(self.width, self.height));
        let mut clamped = rect.intersection(&fb_rect);
        if let Some(ref clip) = self.clip {
            clamped = clamped.intersection(clip);
        }
        if clamped.size.width == 0 || clamped.size.height == 0 {
            return;
        }
        self.dirty = Some(match self.dirty {
            Some(existing) => bounding_box_union(existing, clamped),
            None => clamped,
        });
    }

    /// Write a single pixel (unchecked — caller must guarantee in-bounds).
    #[inline]
    fn set_px(&mut self, x: u32, y: u32, color: Rgb666) {
        let idx = ((y * self.width + x) * 3) as usize;
        self.buf[idx] = color.r();
        self.buf[idx + 1] = color.g();
        self.buf[idx + 2] = color.b();
    }

    /// Flush the dirty region to the hardware display.
    ///
    /// Only the bounding box of all changes since the last flush is
    /// transferred.  Returns the number of pixels pushed (0 if clean).
    pub fn flush<D>(&mut self, display: &mut D) -> Result<usize, D::Error>
    where
        D: DrawTarget<Color = Rgb666>,
    {
        let dirty = match self.dirty.take() {
            Some(d) => d,
            None => return Ok(0),
        };

        let sx = dirty.top_left.x as u32;
        let sy = dirty.top_left.y as u32;
        let w = dirty.size.width;
        let h = dirty.size.height;
        let fb_w = self.width;
        let buf = self.buf.as_slice();

        let pixel_count = (w * h) as usize;
        let pixels = (0..pixel_count).map(move |i| {
            let col = (i as u32) % w;
            let row = (i as u32) / w;
            let idx = (((sy + row) * fb_w + (sx + col)) * 3) as usize;
            Rgb666::new(buf[idx], buf[idx + 1], buf[idx + 2])
        });

        display.fill_contiguous(&dirty, pixels)?;
        Ok(pixel_count)
    }

    /// Flush the dirty region to an [`AsyncDisplay`] via async DMA.
    ///
    /// Converts Rgb666 framebuffer pixels to ILI9488 wire format and
    /// streams them in ~4 KiB DMA chunks, yielding to the Embassy executor
    /// between chunks so other tasks can run.
    ///
    /// Returns the number of pixels pushed (0 if nothing was dirty).
    pub async fn flush_async(
        &mut self,
        display: &mut crate::elecrow_board::display::AsyncDisplay<'_>,
    ) -> Result<usize, esp_hal::spi::Error> {
        let dirty = match self.dirty.take() {
            Some(d) => d,
            None => return Ok(0),
        };

        let sx = dirty.top_left.x as u16;
        let sy = dirty.top_left.y as u16;
        let w = dirty.size.width as u16;
        let h = dirty.size.height as u16;

        display
            .flush_region(self.buf.as_slice(), self.width, sx, sy, w, h)
            .await
    }
}

impl OriginDimensions for Framebuffer {
    fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }
}

impl DrawTarget for Framebuffer {
    type Color = Rgb666;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_y = i32::MIN;
        let mut any = false;

        for Pixel(point, color) in pixels {
            let x = point.x;
            let y = point.y;
            if x >= 0
                && y >= 0
                && (x as u32) < self.width
                && (y as u32) < self.height
                && self.clip_contains(x as u32, y as u32)
            {
                self.set_px(x as u32, y as u32, color);
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
                any = true;
            }
        }

        if any {
            self.mark_dirty(Rectangle::new(
                Point::new(min_x, min_y),
                Size::new((max_x - min_x + 1) as u32, (max_y - min_y + 1) as u32),
            ));
        }
        Ok(())
    }

    fn fill_contiguous<I>(&mut self, area: &Rectangle, colors: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Self::Color>,
    {
        let w = area.size.width;
        if w == 0 {
            return Ok(());
        }
        self.mark_dirty(*area);

        let mut col = 0u32;
        let mut row = 0u32;
        for color in colors {
            let px = area.top_left.x + col as i32;
            let py = area.top_left.y + row as i32;
            if px >= 0
                && py >= 0
                && (px as u32) < self.width
                && (py as u32) < self.height
                && self.clip_contains(px as u32, py as u32)
            {
                let idx = ((py as u32 * self.width + px as u32) * 3) as usize;
                self.buf[idx] = color.r();
                self.buf[idx + 1] = color.g();
                self.buf[idx + 2] = color.b();
            }
            col += 1;
            if col >= w {
                col = 0;
                row += 1;
            }
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        let fb_rect = Rectangle::new(Point::new(0, 0), Size::new(self.width, self.height));
        let mut clipped = area.intersection(&fb_rect);
        if let Some(ref clip) = self.clip {
            clipped = clipped.intersection(clip);
        }
        if clipped.size.width == 0 || clipped.size.height == 0 {
            return Ok(());
        }
        self.mark_dirty(clipped);

        let r = color.r();
        let g = color.g();
        let b = color.b();
        let x = clipped.top_left.x as u32;
        let y = clipped.top_left.y as u32;
        let w = clipped.size.width;
        let h = clipped.size.height;

        for row in y..y + h {
            let row_start = ((row * self.width + x) * 3) as usize;
            for c in 0..w {
                let idx = row_start + (c * 3) as usize;
                self.buf[idx] = r;
                self.buf[idx + 1] = g;
                self.buf[idx + 2] = b;
            }
        }
        Ok(())
    }
}

/// Compute the axis-aligned bounding box of two rectangles.
fn bounding_box_union(a: Rectangle, b: Rectangle) -> Rectangle {
    let min_x = a.top_left.x.min(b.top_left.x);
    let min_y = a.top_left.y.min(b.top_left.y);
    let max_x = (a.top_left.x + a.size.width as i32).max(b.top_left.x + b.size.width as i32);
    let max_y = (a.top_left.y + a.size.height as i32).max(b.top_left.y + b.size.height as i32);
    Rectangle::new(
        Point::new(min_x, min_y),
        Size::new((max_x - min_x) as u32, (max_y - min_y) as u32),
    )
}
