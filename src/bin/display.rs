//! Text rendering utilities for the display.
//!
//! This module is display-hardware-agnostic — it works with any
//! `embedded_graphics::DrawTarget`.  Hardware-specific initialization
//! lives in [`elecrow_board::display`](gramslator::elecrow_board::display).
//!
//! Two text-drawing approaches are provided:
//!
//! 1. **Bitmap fonts** ([`draw_text`], [`draw_text_styled`], [`draw_text_centered`])
//!    — lightweight, fixed-size X11 fonts built into `embedded-graphics`.
//!    Suitable for small UI labels and status text (up to 10×20 px).
//!
//! 2. **TrueType renderer** ([`FontRenderer`]) — `ttf-parser` +
//!    `ab_glyph_rasterizer`-based renderer with a lazy glyph cache.
//!    Glyphs are rasterised **on demand** — no upfront curve decomposition,
//!    so even a 5 MB CJK font only costs heap proportional to the
//!    characters actually drawn.  Ideal for large translated-text output.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use embedded_graphics::mono_font::ascii;
use embedded_graphics::mono_font::{MonoFont, MonoTextStyle, MonoTextStyleBuilder};
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};

// Re-export for convenience so callers don't need a separate
// `use embedded_graphics::…` just to name common types.
pub use embedded_graphics::geometry::Size;
pub use embedded_graphics::pixelcolor::Rgb666;
pub use embedded_graphics::prelude::{DrawTarget, Point, RgbColor};
pub use embedded_graphics::primitives::Rectangle;

/// Default font data baked into flash (Noto Sans JP Regular, OFL-licensed).
///
/// Subset to: Basic Latin, Latin-1/Extended, Greek, Cyrillic, Hiragana,
/// Katakana, CJK Unified Ideographs, CJK/General punctuation, Hangul Jamo,
/// Halfwidth/Fullwidth forms, and currency symbols.
const DEFAULT_FONT_DATA: &[u8] = include_bytes!("assets/NotoSansJP-Medium.ttf");

// ===========================================================================
// Bitmap font helpers (small text)
// ===========================================================================

/// Font size selection for the built-in bitmap fonts.
///
/// Each variant maps to an X11 bitmap font from the `embedded-graphics`
/// built-in collection.  Sizes are chosen to cover a reasonable range for a
/// 480×320 display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontSize {
    /// 6×10 px — fine print, status bars.
    Small,
    /// 7×14 px — body text.
    Medium,
    /// 9×18 px — headings, prominent text.
    Large,
    /// 10×20 px — titles, large callouts.
    ExtraLarge,
}

/// Font weight / style for the built-in bitmap fonts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FontStyle {
    /// Normal weight, upright.
    #[default]
    Regular,
    /// Heavy weight.
    Bold,
    /// Slanted (only available at Medium; other sizes fall back to Regular).
    Italic,
}

impl FontSize {
    /// Returns the `MonoFont` for this size and style combination.
    ///
    /// Falls back to `Regular` when the requested style variant doesn't exist
    /// for the chosen size.
    pub fn mono_font(self, style: FontStyle) -> &'static MonoFont<'static> {
        match (self, style) {
            // Small — only regular exists at 6×10
            (Self::Small, _) => &ascii::FONT_6X10,

            // Medium — full set at 7×13/7×14
            (Self::Medium, FontStyle::Regular) => &ascii::FONT_7X14,
            (Self::Medium, FontStyle::Bold) => &ascii::FONT_7X14_BOLD,
            (Self::Medium, FontStyle::Italic) => &ascii::FONT_7X13_ITALIC,

            // Large — regular + bold at 9×18
            (Self::Large, FontStyle::Regular | FontStyle::Italic) => &ascii::FONT_9X18,
            (Self::Large, FontStyle::Bold) => &ascii::FONT_9X18_BOLD,

            // ExtraLarge — only regular at 10×20
            (Self::ExtraLarge, _) => &ascii::FONT_10X20,
        }
    }

    /// Approximate line height in pixels (font cell height + 2 px spacing).
    pub const fn line_height(self) -> i32 {
        match self {
            Self::Small => 12,
            Self::Medium => 16,
            Self::Large => 20,
            Self::ExtraLarge => 22,
        }
    }
}

/// Draw text at the given position (top-left corner of the first glyph).
///
/// Uses [`FontStyle::Regular`] and no background fill.
///
/// Returns the [`Point`] just past the last character, so you can chain
/// calls to continue drawing on the same line:
///
/// ```ignore
/// let next = display::draw_text(&mut d, "Hello ", p, FontSize::Large, WHITE)?;
/// display::draw_text(&mut d, "world!", next, FontSize::Large, GREEN)?;
/// ```
pub fn draw_text<D: DrawTarget>(
    display: &mut D,
    text: &str,
    position: Point,
    size: FontSize,
    color: D::Color,
) -> Result<Point, D::Error> {
    let style = MonoTextStyle::new(size.mono_font(FontStyle::Regular), color);
    let layout = TextStyleBuilder::new().baseline(Baseline::Top).build();
    Text::with_text_style(text, position, style, layout).draw(display)
}

/// Draw text with explicit font style and optional background color.
///
/// Position is the top-left corner.  When `background` is `Some`, the
/// glyph bounding boxes are filled with that color first, producing an
/// "inverted" / highlighted appearance.
pub fn draw_text_styled<D: DrawTarget>(
    display: &mut D,
    text: &str,
    position: Point,
    size: FontSize,
    font_style: FontStyle,
    color: D::Color,
    background: Option<D::Color>,
) -> Result<Point, D::Error> {
    let font = size.mono_font(font_style);
    let char_style = match background {
        Some(bg) => MonoTextStyleBuilder::new()
            .font(font)
            .text_color(color)
            .background_color(bg)
            .build(),
        None => MonoTextStyleBuilder::new()
            .font(font)
            .text_color(color)
            .build(),
    };
    let layout = TextStyleBuilder::new().baseline(Baseline::Top).build();
    Text::with_text_style(text, position, char_style, layout).draw(display)
}

/// Draw text horizontally centered at the given Y coordinate.
///
/// `display_width` is the pixel width of the display (e.g. 480).
/// The text baseline is at the top of the glyph cell.
pub fn draw_text_centered<D: DrawTarget>(
    display: &mut D,
    text: &str,
    y: i32,
    display_width: i32,
    size: FontSize,
    color: D::Color,
) -> Result<Point, D::Error> {
    let style = MonoTextStyle::new(size.mono_font(FontStyle::Regular), color);
    let layout = TextStyleBuilder::new()
        .alignment(Alignment::Center)
        .baseline(Baseline::Top)
        .build();
    Text::with_text_style(text, Point::new(display_width / 2, y), style, layout).draw(display)
}

// ===========================================================================
// Framebuffer with dirty-rect tracking
// ===========================================================================

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
        display: &mut gramslator::elecrow_board::display::AsyncDisplay<'_>,
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

// ===========================================================================
// TrueType font renderer with lazy glyph cache  (ttf-parser + ab_glyph)
// ===========================================================================

/// A cached rasterised glyph bitmap.
struct CachedGlyph {
    width: usize,
    height: usize,
    /// Horizontal offset from pen position to left edge of bitmap (pixels).
    bearing_x: i32,
    /// Vertical offset from baseline to **top** edge of bitmap (pixels, positive = above baseline).
    bearing_y: i32,
    /// Horizontal advance width in pixels.
    advance_width: f32,
    /// Coverage values 0..=255, row-major, top-left origin.
    bitmap: Vec<u8>,
    /// Monotonic counter stamped on every access — lowest value = least recently used.
    last_used: u64,
}

/// Lazy-caching TrueType font renderer.
///
/// Uses `ttf-parser` for on-demand glyph outline extraction and
/// `ab_glyph_rasterizer` for coverage rasterisation.  **No** glyph data is
/// pre-processed at construction time — the raw font bytes stay in flash and
/// individual glyphs are rasterised the first time they are drawn.
///
/// This means a large CJK font (e.g. 5 MB Noto Sans JP) is perfectly
/// usable: heap cost is proportional only to the characters that have
/// actually been rendered, not to the total glyph count in the file.
///
/// # Example
///
/// ```ignore
/// let mut renderer = display::FontRenderer::default_font();
/// renderer.draw_text(&mut display, "Hello!", Point::new(0, 0), 80.0,
///                    Rgb666::WHITE, Rgb666::BLACK)?;
/// ```
/// Default maximum number of cached glyphs before LRU eviction kicks in.
///
/// At ~2 KB average per glyph bitmap (50 px Latin/CJK mix), 256 entries ≈
/// 512 KB in PSRAM — a good balance between hit-rate and memory use.
const DEFAULT_MAX_CACHE_SIZE: usize = 256;

pub struct FontRenderer {
    face: ttf_parser::Face<'static>,
    /// Cache keyed on `(char, f32::to_bits())`.
    cache: BTreeMap<(char, u32), CachedGlyph>,
    /// Monotonic counter incremented on every cache access.
    access_counter: u64,
    /// Maximum number of entries before LRU eviction.
    max_cache_size: usize,
}

impl FontRenderer {
    /// Create a renderer from raw font bytes.
    ///
    /// The data must be `'static` (e.g. from `include_bytes!`).
    /// Construction is essentially free — no glyph outlines are processed.
    pub fn new(font_data: &'static [u8]) -> Self {
        let face =
            ttf_parser::Face::parse(font_data, 0).expect("Failed to parse TTF/OTF font");
        Self {
            face,
            cache: BTreeMap::new(),
            access_counter: 0,
            max_cache_size: DEFAULT_MAX_CACHE_SIZE,
        }
    }

    /// Create a renderer using the built-in Inter Regular font.
    pub fn default_font() -> Self {
        Self::new(DEFAULT_FONT_DATA)
    }

    /// Set the maximum number of cached glyphs.
    ///
    /// When the cache is full, the least-recently-used entry is evicted to
    /// make room.  Does **not** shrink the cache immediately — excess
    /// entries are evicted lazily on the next insert.
    pub fn set_max_cache_size(&mut self, max: usize) {
        self.max_cache_size = max;
    }

    /// Scale factor: pixels per font-design-unit at the given pixel size.
    fn scale(&self, px: f32) -> f32 {
        px / self.face.units_per_em() as f32
    }

    /// Line height (ascent − descent) at the given pixel size.
    pub fn line_height(&self, px: f32) -> f32 {
        let s = self.scale(px);
        (self.face.ascender() as f32 - self.face.descender() as f32) * s
    }

    /// Ascent above the baseline at the given pixel size.
    fn ascent(&self, px: f32) -> f32 {
        self.face.ascender() as f32 * self.scale(px)
    }

    /// Compute the advance width of a character at the given pixel size.
    pub fn char_advance(&self, ch: char, px: f32) -> f32 {
        let scale = self.scale(px);
        self.face
            .glyph_index(ch)
            .and_then(|id| self.face.glyph_hor_advance(id))
            .unwrap_or(0) as f32
            * scale
    }

    /// Total advance width of a string at the given pixel size.
    pub fn text_width(&self, text: &str, px: f32) -> f32 {
        text.chars().map(|ch| self.char_advance(ch, px)).sum()
    }

    /// Collect all printable Unicode codepoints the font can render.
    ///
    /// Filters to codepoints that are valid Rust `char`s and not control
    /// characters.  The result is allocated in the default heap (PSRAM).
    pub fn available_chars(&self) -> Vec<char> {
        let mut chars = Vec::new();
        if let Some(cmap) = self.face.tables().cmap {
            for subtable in cmap.subtables {
                if !subtable.is_unicode() {
                    continue;
                }
                subtable.codepoints(|cp| {
                    if let Some(ch) = char::from_u32(cp) {
                        if !ch.is_control() && self.face.glyph_index(ch).is_some() {
                            chars.push(ch);
                        }
                    }
                });
                break; // one Unicode subtable is sufficient
            }
        }
        chars.sort_unstable();
        chars.dedup();
        chars
    }

    /// Rasterise a single glyph on demand and return a [`CachedGlyph`].
    fn rasterize_glyph(&self, ch: char, px: f32) -> CachedGlyph {
        let scale = self.scale(px);

        let glyph_id = match self.face.glyph_index(ch) {
            Some(id) => id,
            None => {
                // Missing glyph — use space advance as fallback.
                let advance = self
                    .face
                    .glyph_index(' ')
                    .and_then(|id| self.face.glyph_hor_advance(id))
                    .unwrap_or(0) as f32
                    * scale;
                return CachedGlyph {
                    width: 0,
                    height: 0,
                    bearing_x: 0,
                    bearing_y: 0,
                    advance_width: advance,
                    bitmap: Vec::new(),
                    last_used: 0,
                };
            }
        };

        let advance_width = self
            .face
            .glyph_hor_advance(glyph_id)
            .unwrap_or(0) as f32
            * scale;

        let bbox = match self.face.glyph_bounding_box(glyph_id) {
            Some(bb) => bb,
            None => {
                // No outline (e.g. space) — zero-size bitmap.
                return CachedGlyph {
                    width: 0,
                    height: 0,
                    bearing_x: 0,
                    bearing_y: 0,
                    advance_width,
                    bitmap: Vec::new(),
                    last_used: 0,
                };
            }
        };

        // Pixel-space bounding box (font coordinates: y-up).
        let px_x_min = bbox.x_min as f32 * scale;
        let px_y_min = bbox.y_min as f32 * scale;
        let px_x_max = bbox.x_max as f32 * scale;
        let px_y_max = bbox.y_max as f32 * scale;

        // Bitmap dimensions (ceil to cover partial pixels).
        let width = f32_ceil(px_x_max - px_x_min) as usize;
        let height = f32_ceil(px_y_max - px_y_min) as usize;

        if width == 0 || height == 0 {
            return CachedGlyph {
                width: 0,
                height: 0,
                bearing_x: f32_round(px_x_min),
                bearing_y: f32_round(px_y_max),
                advance_width,
                bitmap: Vec::new(),
                last_used: 0,
            };
        }

        // Rasterise the outline into coverage values.
        //
        // We add padding around the rasterizer so that edge contributions
        // landing exactly on the glyph bounding-box boundary are not
        // clipped.  `ab_glyph_rasterizer` uses a single accumulator across
        // the flat pixel buffer; if an edge contribution is lost (OOB skip),
        // the accumulator drifts and all subsequent rows get wrong coverage,
        // producing the classic "inverted first glyph" artefact.
        const PAD: usize = 1;
        let rast_w = width + PAD * 2;
        let rast_h = height + PAD * 2;
        let mut rasterizer =
            ab_glyph_rasterizer::Rasterizer::new(rast_w, rast_h);

        // The bridge translates font-unit coordinates to pixel-space
        // bitmap coordinates, offset by PAD so the glyph sits inside the
        // padded area:
        //   px_x = font_x * scale − px_x_min + PAD
        //   px_y = px_y_max − font_y * scale + PAD   (flips Y)
        let mut builder = OutlineToRasterizer {
            rasterizer: &mut rasterizer,
            scale,
            offset_x: -px_x_min + PAD as f32,
            offset_y: px_y_max + PAD as f32,
            start_x: 0.0,
            start_y: 0.0,
            last_x: 0.0,
            last_y: 0.0,
        };

        let _ = self.face.outline_glyph(glyph_id, &mut builder);

        // Collect coverage → u8 bitmap, extracting only the inner
        // (non-padded) region.
        let mut bitmap = alloc::vec![0u8; width * height];
        rasterizer.for_each_pixel_2d(|rx, ry, coverage| {
            let rx = rx as usize;
            let ry = ry as usize;
            if rx >= PAD && rx < PAD + width && ry >= PAD && ry < PAD + height {
                let bx = rx - PAD;
                let by = ry - PAD;
                let idx = by * width + bx;
                bitmap[idx] = (coverage.min(1.0) * 255.0 + 0.5) as u8;
            }
        });

        CachedGlyph {
            width,
            height,
            bearing_x: f32_round(px_x_min),
            bearing_y: f32_round(px_y_max),
            advance_width,
            bitmap,
            last_used: 0,
        }
    }

    /// Ensure the glyph is in the cache (rasterise if needed) and mark it
    /// as most-recently-used.
    ///
    /// If the cache has reached `max_cache_size`, the least-recently-used
    /// entry is evicted before the new glyph is inserted.
    fn ensure_cached(&mut self, ch: char, px: f32) {
        let key = (ch, px.to_bits());
        let stamp = self.access_counter;
        self.access_counter += 1;

        if let Some(entry) = self.cache.get_mut(&key) {
            // Already cached — just refresh the LRU stamp.
            entry.last_used = stamp;
            return;
        }

        // Evict the least-recently-used entry if we're at capacity.
        if self.cache.len() >= self.max_cache_size {
            self.evict_lru();
        }

        let mut glyph = self.rasterize_glyph(ch, px);
        glyph.last_used = stamp;
        self.cache.insert(key, glyph);
    }

    /// Evict the single least-recently-used cache entry.
    fn evict_lru(&mut self) {
        let lru_key = self
            .cache
            .iter()
            .min_by_key(|(_, g)| g.last_used)
            .map(|(k, _)| *k);
        if let Some(key) = lru_key {
            self.cache.remove(&key);
        }
    }

    /// Return a shared reference to a previously-cached glyph.
    fn get_cached(&self, ch: char, px: f32) -> &CachedGlyph {
        let key = (ch, px.to_bits());
        self.cache
            .get(&key)
            .expect("glyph should have been cached by ensure_cached")
    }

    /// Draw anti-aliased text at the given position (top-left of the text
    /// bounding box).
    ///
    /// Each glyph is alpha-blended between `color` (foreground) and `bg`
    /// (background) using the coverage bitmap.  The entire glyph rectangle
    /// is filled, so `bg` should match whatever is behind the text
    /// (typically `Rgb666::BLACK` after a `display.clear()`).
    ///
    /// Returns the [`Point`] just past the last character on the same line.
    pub fn draw_text<D: DrawTarget<Color = Rgb666>>(
        &mut self,
        display: &mut D,
        text: &str,
        position: Point,
        px: f32,
        color: Rgb666,
        bg: Rgb666,
    ) -> Result<Point, D::Error> {
        let ascent = self.ascent(px) as i32;
        let mut pen_x = position.x;
        let baseline_y = position.y + ascent;

        let fg_r = color.r() as u16;
        let fg_g = color.g() as u16;
        let fg_b = color.b() as u16;
        let bg_r = bg.r() as u16;
        let bg_g = bg.g() as u16;
        let bg_b = bg.b() as u16;

        for ch in text.chars() {
            self.ensure_cached(ch, px);
            let glyph = self.get_cached(ch, px);

            if glyph.width == 0 || glyph.height == 0 {
                pen_x += glyph.advance_width as i32;
                continue;
            }

            // Position bitmap: bearing_x offsets from pen, bearing_y is
            // distance from baseline to top of glyph (positive = above).
            let glyph_x = pen_x + glyph.bearing_x;
            let glyph_y = baseline_y - glyph.bearing_y;

            let area = Rectangle::new(
                Point::new(glyph_x, glyph_y),
                Size::new(glyph.width as u32, glyph.height as u32),
            );

            // Alpha-blend each pixel: out = fg * α + bg * (1 − α)
            let colors = glyph.bitmap.iter().map(|&coverage| {
                let c = coverage as u16;
                let inv = 255 - c;
                let r = ((fg_r * c + bg_r * inv) / 255) as u8;
                let g = ((fg_g * c + bg_g * inv) / 255) as u8;
                let b = ((fg_b * c + bg_b * inv) / 255) as u8;
                Rgb666::new(r, g, b)
            });

            display.fill_contiguous(&area, colors)?;
            pen_x += glyph.advance_width as i32;
        }

        Ok(Point::new(pen_x, position.y))
    }

    /// Draw anti-aliased text horizontally centered at the given Y
    /// coordinate.
    ///
    /// `display_width` is the pixel width of the display (e.g. 480).
    pub fn draw_text_centered<D: DrawTarget<Color = Rgb666>>(
        &mut self,
        display: &mut D,
        text: &str,
        y: i32,
        display_width: i32,
        px: f32,
        color: Rgb666,
        bg: Rgb666,
    ) -> Result<Point, D::Error> {
        // Measure total advance width first.
        let total_width: f32 = text
            .chars()
            .map(|ch| self.char_advance(ch, px))
            .sum();
        let x = (display_width - total_width as i32) / 2;
        self.draw_text(display, text, Point::new(x, y), px, color, bg)
    }
}

// ===========================================================================
// Bridge: ttf_parser::OutlineBuilder → ab_glyph_rasterizer::Rasterizer
// ===========================================================================

/// Adapts the `ttf_parser::OutlineBuilder` callbacks into
/// `ab_glyph_rasterizer::Rasterizer` draw calls, applying scale and
/// coordinate transforms (font y-up → bitmap y-down).
struct OutlineToRasterizer<'a> {
    rasterizer: &'a mut ab_glyph_rasterizer::Rasterizer,
    scale: f32,
    /// Added to scaled X to shift glyph left edge to bitmap x = 0.
    offset_x: f32,
    /// Equals `px_y_max`; used to flip Y axis.
    offset_y: f32,
    /// Position recorded by `move_to` for `close`.
    start_x: f32,
    start_y: f32,
    /// Current pen position in bitmap coordinates.
    last_x: f32,
    last_y: f32,
}

impl OutlineToRasterizer<'_> {
    /// Transform font-unit coordinates to bitmap pixel coordinates.
    #[inline]
    fn transform(&self, x: f32, y: f32) -> (f32, f32) {
        let px = x * self.scale + self.offset_x;
        let py = self.offset_y - y * self.scale;
        (px, py)
    }
}

impl ttf_parser::OutlineBuilder for OutlineToRasterizer<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let (px, py) = self.transform(x, y);
        self.start_x = px;
        self.start_y = py;
        self.last_x = px;
        self.last_y = py;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let (px, py) = self.transform(x, y);
        self.rasterizer.draw_line(
            ab_glyph_rasterizer::point(self.last_x, self.last_y),
            ab_glyph_rasterizer::point(px, py),
        );
        self.last_x = px;
        self.last_y = py;
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (cx, cy) = self.transform(x1, y1);
        let (px, py) = self.transform(x, y);
        self.rasterizer.draw_quad(
            ab_glyph_rasterizer::point(self.last_x, self.last_y),
            ab_glyph_rasterizer::point(cx, cy),
            ab_glyph_rasterizer::point(px, py),
        );
        self.last_x = px;
        self.last_y = py;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (c1x, c1y) = self.transform(x1, y1);
        let (c2x, c2y) = self.transform(x2, y2);
        let (px, py) = self.transform(x, y);
        self.rasterizer.draw_cubic(
            ab_glyph_rasterizer::point(self.last_x, self.last_y),
            ab_glyph_rasterizer::point(c1x, c1y),
            ab_glyph_rasterizer::point(c2x, c2y),
            ab_glyph_rasterizer::point(px, py),
        );
        self.last_x = px;
        self.last_y = py;
    }

    fn close(&mut self) {
        // Close the sub-path by drawing a line back to the move_to point.
        if self.last_x != self.start_x || self.last_y != self.start_y {
            self.rasterizer.draw_line(
                ab_glyph_rasterizer::point(self.last_x, self.last_y),
                ab_glyph_rasterizer::point(self.start_x, self.start_y),
            );
        }
        self.last_x = self.start_x;
        self.last_y = self.start_y;
    }
}

// ===========================================================================
// no_std float helpers (avoid depending on std for ceil / round)
// ===========================================================================

/// `f32::ceil` equivalent usable in `no_std`.
#[inline]
fn f32_ceil(x: f32) -> f32 {
    let t = x as i32 as f32;
    if t < x { t + 1.0 } else { t }
}

/// `f32::round` → `i32`, ties away from zero.
#[inline]
fn f32_round(x: f32) -> i32 {
    if x >= 0.0 {
        (x + 0.5) as i32
    } else {
        (x - 0.5) as i32
    }
}

// ===========================================================================
// Word-wrap helper
// ===========================================================================

use alloc::string::String;

use defmt::info;

/// Split `text` into lines that fit within `max_width` pixels at font size
/// `px`.  Word boundaries are preferred; if a single word is wider than the
/// line, it is broken mid-word.
///
/// Returns a `Vec<String>` of lines (without trailing newlines).
fn word_wrap(renderer: &FontRenderer, text: &str, px: f32, max_width: f32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current_line = String::new();
    let mut current_width: f32 = 0.0;
    let space_advance = renderer.text_width(" ", px);

    for word in text.split_whitespace() {
        let word_width = renderer.text_width(word, px);

        if current_line.is_empty() {
            // First word on the line — always accept it.
            if word_width <= max_width {
                current_line.push_str(word);
                current_width = word_width;
            } else {
                // Word itself is wider than the line — break it character
                // by character using char_advance (no allocation needed).
                for ch in word.chars() {
                    let cw = renderer.char_advance(ch, px);
                    if current_width + cw > max_width && !current_line.is_empty() {
                        lines.push(core::mem::replace(&mut current_line, String::new()));
                        current_width = 0.0;
                    }
                    current_line.push(ch);
                    current_width += cw;
                }
            }
        } else if current_width + space_advance + word_width <= max_width {
            // Word fits on the current line.
            current_line.push(' ');
            current_line.push_str(word);
            current_width += space_advance + word_width;
        } else {
            // Word doesn't fit — start a new line.
            lines.push(core::mem::replace(&mut current_line, String::from(word)));
            current_width = word_width;
        }
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }

    // If the input was empty, return one empty line so the caller can
    // still compute layout bounds.
    if lines.is_empty() {
        lines.push(String::new());
    }

    lines
}

// ===========================================================================
// Display task — live transcript + translation renderer
// ===========================================================================

/// Screen dimensions.
const SCREEN_W: i32 = 480;
const SCREEN_H: i32 = 320;

/// Horizontal padding from the screen edges.
const H_PAD: i32 = 8;

/// ---- Translation region (top ~2/3 of screen) ----
/// Y coordinate where the translation region starts (top of screen).
const TRANSLATION_Y: i32 = 4;
/// Font size (px) for the translation.
const TRANSLATION_PX: f32 = 48.0;
/// Foreground colour for the translation text.
const TRANSLATION_COLOR: Rgb666 = Rgb666::new(32, 58, 63); // light cyan

/// Y coordinate of the separator line between regions.
const SEPARATOR_Y: i32 = 264;

/// ---- Transcript region (bottom strip) ----
/// Y coordinate where the transcript region starts.
const TRANSCRIPT_Y: i32 = SEPARATOR_Y + 3;
/// Font size (px) for the transcript.
const TRANSCRIPT_PX: f32 = 20.0;
/// Foreground colour for the transcript text.
const TRANSCRIPT_COLOR: Rgb666 = Rgb666::new(48, 48, 48); // light grey

/// Background colour for the entire screen.
const BG: Rgb666 = Rgb666::BLACK;

/// Scroll animation speed in pixels per second.
const SCROLL_SPEED: f32 = 700.0;
/// Duration of one animation frame (~30 fps).
const ANIM_FRAME_MS: u64 = 33;
const ANIM_FRAME_DURATION: embassy_time::Duration = embassy_time::Duration::from_millis(ANIM_FRAME_MS);

// ===========================================================================
// Per-section scroll / animation state
// ===========================================================================

/// Tracks the word-wrapped text, total height, and scroll animation for one
/// display section (translation or transcript).
struct Section {
    /// Word-wrapped lines (cached so we don't re-wrap every animation frame).
    lines: Vec<String>,
    /// Total height of the wrapped text in pixels.
    total_height: f32,
    /// Current vertical scroll offset (0 = no scroll, positive = scrolled up).
    scroll_offset: f32,
    /// Target scroll offset the animation is heading toward.
    scroll_target: f32,
    /// Whether this section needs to be redrawn on the next frame.
    needs_redraw: bool,
}

impl Section {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            total_height: 0.0,
            scroll_offset: 0.0,
            scroll_target: 0.0,
            needs_redraw: false,
        }
    }

    /// Recompute word-wrap and recalculate the scroll target.
    ///
    /// The current scroll offset is **preserved** (clamped to the new
    /// target) so that incoming partial-transcript updates don't jerk the
    /// view back to the top while the user is still reading.
    fn update_text(
        &mut self,
        text: &str,
        renderer: &FontRenderer,
        px: f32,
        max_width: f32,
        section_height: f32,
    ) {
        self.lines = word_wrap(renderer, text, px, max_width);
        let line_h = renderer.line_height(px) + 2.0;
        self.total_height = self.lines.len() as f32 * line_h;
        self.scroll_target = if self.total_height > section_height {
            self.total_height - section_height
        } else {
            0.0
        };
        // Clamp the current offset so it never exceeds the new target
        // (e.g. when the text got shorter).
        if self.scroll_offset > self.scroll_target {
            self.scroll_offset = self.scroll_target;
        }
        self.needs_redraw = true;
    }

    /// Returns `true` if a scroll animation is still in progress.
    fn is_animating(&self) -> bool {
        self.scroll_target - self.scroll_offset > 0.5
    }

    /// Advance the scroll animation by `amount` pixels.  Sets
    /// `needs_redraw` if the offset actually changed.
    fn advance(&mut self, amount: f32) {
        if self.is_animating() {
            self.scroll_offset = (self.scroll_offset + amount).min(self.scroll_target);
            self.needs_redraw = true;
        }
    }
}

/// Render a section's text into the framebuffer with clipping and scroll.
///
/// The clip rectangle is set to `[0, section_y) .. [SCREEN_W, section_y + section_height)`
/// so that glyphs straddling the boundary are cleanly cut off.
fn render_section(
    fb: &mut Framebuffer,
    renderer: &mut FontRenderer,
    section: &Section,
    px: f32,
    color: Rgb666,
    section_y: i32,
    section_height: i32,
) {
    // Clip all drawing to this section's bounds.
    fb.set_clip(Some(Rectangle::new(
        Point::new(0, section_y),
        Size::new(SCREEN_W as u32, section_height as u32),
    )));

    // Clear the section.
    let clear_rect = Rectangle::new(
        Point::new(0, section_y),
        Size::new(SCREEN_W as u32, section_height as u32),
    );
    fb.fill_solid(&clear_rect, BG).unwrap();

    // Render word-wrapped lines, offset by the scroll amount.
    let line_h = renderer.line_height(px) as i32 + 2;
    let mut y = section_y - section.scroll_offset as i32;

    for line in &section.lines {
        // Skip lines that are entirely above the visible section.
        if y + line_h <= section_y {
            y += line_h;
            continue;
        }
        // Stop once we're entirely below the section.
        if y >= section_y + section_height {
            break;
        }
        renderer
            .draw_text(fb, line, Point::new(H_PAD, y), px, color, BG)
            .unwrap();
        y += line_h;
    }

    // Remove clip so subsequent operations are unrestricted.
    fb.set_clip(None);
}

// ===========================================================================
// Display task — live transcript + translation renderer with scroll animation
// ===========================================================================

/// Embassy task: renders the current translation (large, top 2/3) and
/// transcript (small, bottom 1/3).
///
/// When text overflows its section, an automatic scroll animation reveals
/// the remaining lines at a comfortable reading speed.  Each section is
/// clipped to its bounds, so scrolling text in the bottom section never
/// bleeds above the separator.
#[embassy_executor::task]
pub async fn display_task(
    mut hw_display: gramslator::elecrow_board::display::AsyncDisplay<'static>,
    mut fb: Framebuffer,
    mut renderer: FontRenderer,
    display_signal: &'static gramslator::app_state::DisplaySignal,
) {
    let max_text_width = (SCREEN_W - 2 * H_PAD) as f32;
    let translation_section_h = SEPARATOR_Y - TRANSLATION_Y;
    let transcript_section_h = SCREEN_H - TRANSCRIPT_Y;
    let scroll_per_frame = SCROLL_SPEED * (ANIM_FRAME_MS as f32 / 1000.0);

    // Initial full-screen clear + separator.
    fb.clear(BG).unwrap();
    draw_separator(&mut fb);
    fb.flush_async(&mut hw_display).await.unwrap();
    info!("Display task started — waiting for state updates");

    let mut last_transcript = String::new();
    let mut last_translation = String::new();
    let mut translation_sec = Section::new();
    let mut transcript_sec = Section::new();

    loop {
        // If either section is animating, poll with a short timeout so we
        // keep advancing the scroll.  Otherwise block on the signal.
        let animating = translation_sec.is_animating() || transcript_sec.is_animating();
        if animating {
            // Either a new signal arrives, or we get a timeout (animation tick).
            let _ = embassy_time::with_timeout(ANIM_FRAME_DURATION, display_signal.wait()).await;
        } else {
            display_signal.wait().await;
        }

        // ---- Check for updated text ------------------------------------
        let (transcript, translation) = gramslator::app_state::read_state();

        if translation != last_translation {
            translation_sec.update_text(
                &translation,
                &renderer,
                TRANSLATION_PX,
                max_text_width,
                translation_section_h as f32,
            );
            last_translation = translation;
        }
        if transcript != last_transcript {
            transcript_sec.update_text(
                &transcript,
                &renderer,
                TRANSCRIPT_PX,
                max_text_width,
                transcript_section_h as f32,
            );
            last_transcript = transcript;
        }

        // ---- Advance scroll animations ---------------------------------
        translation_sec.advance(scroll_per_frame);
        transcript_sec.advance(scroll_per_frame);

        // ---- Render sections that need it ------------------------------
        if translation_sec.needs_redraw {
            render_section(
                &mut fb,
                &mut renderer,
                &translation_sec,
                TRANSLATION_PX,
                TRANSLATION_COLOR,
                TRANSLATION_Y,
                translation_section_h,
            );
            translation_sec.needs_redraw = false;
        }

        if transcript_sec.needs_redraw {
            render_section(
                &mut fb,
                &mut renderer,
                &transcript_sec,
                TRANSCRIPT_PX,
                TRANSCRIPT_COLOR,
                TRANSCRIPT_Y,
                transcript_section_h,
            );
            transcript_sec.needs_redraw = false;
        }

        // Always redraw the separator (covers any potential bleed from the
        // translation section's descenders).
        draw_separator(&mut fb);

        // ---- Flush dirty regions ---------------------------------------
        if fb.is_dirty() {
            if let Err(e) = fb.flush_async(&mut hw_display).await {
                info!("Display flush error: {:?}", e);
            }
        }
    }
}

/// Draw a thin horizontal separator line at [`SEPARATOR_Y`].
fn draw_separator(fb: &mut Framebuffer) {
    let separator_color = Rgb666::new(20, 20, 20); // dark grey
    let sep_rect = Rectangle::new(
        Point::new(H_PAD, SEPARATOR_Y),
        Size::new((SCREEN_W - 2 * H_PAD) as u32, 1),
    );
    fb.fill_solid(&sep_rect, separator_color).unwrap();
}

// ===========================================================================
// Bouncing-text screensaver demo (retained for reference / debugging)
// ===========================================================================

// /// Rainbow palette for text colouring.
// const RAINBOW: [Rgb666; 7] = [
//     Rgb666::new(63, 0, 0),  // red
//     Rgb666::new(63, 31, 0), // orange
//     Rgb666::new(63, 63, 0), // yellow
//     Rgb666::new(0, 63, 0),  // green
//     Rgb666::new(0, 31, 63), // blue
//     Rgb666::new(18, 0, 63), // indigo
//     Rgb666::new(40, 0, 63), // violet
// ];
//
// /// Simple xorshift32 PRNG.
// struct Xorshift32(u32);
// impl Xorshift32 {
//     fn next(&mut self) -> u32 {
//         let mut s = self.0;
//         s ^= s << 13;
//         s ^= s >> 17;
//         s ^= s << 5;
//         self.0 = s;
//         s
//     }
//     fn usize_mod(&mut self, len: usize) -> usize {
//         (self.next() as usize) % len
//     }
// }
//
// /// Embassy task: bounces "Hello world!" around the screen.
// #[embassy_executor::task]
// pub async fn bouncing_text(
//     mut hw_display: gramslator::elecrow_board::display::AsyncDisplay<'static>,
//     mut fb: Framebuffer,
//     mut renderer: FontRenderer,
// ) {
//     const TEXT: &str = "Hello world!";
//     const PX: f32 = 40.0;
//     const SCREEN_W: i32 = 480;
//     const SCREEN_H: i32 = 320;
//     let bg = Rgb666::BLACK;
//     let text_w = renderer.text_width(TEXT, PX) as i32 + 2;
//     let text_h = renderer.line_height(PX) as i32 + 2;
//     let mut x: i32 = 20; let mut y: i32 = 40;
//     let mut dx: i32 = 3; let mut dy: i32 = 2;
//     let mut prev_w: i32 = text_w;
//     fb.clear(bg).unwrap();
//     fb.flush_async(&mut hw_display).await.unwrap();
//     loop {
//         let erase_w = prev_w.max(text_w) as u32;
//         let old_rect = Rectangle::new(Point::new(x, y), Size::new(erase_w, text_h as u32));
//         fb.fill_solid(&old_rect, bg).unwrap();
//         x += dx; y += dy;
//         if x <= 0 { x = 0; dx = dx.abs(); }
//         else if x + text_w >= SCREEN_W { x = SCREEN_W - text_w; dx = -(dx.abs()); }
//         if y <= 0 { y = 0; dy = dy.abs(); }
//         else if y + text_h >= SCREEN_H { y = SCREEN_H - text_h; dy = -(dy.abs()); }
//         let mut pen = Point::new(x, y);
//         for (i, ch) in TEXT.chars().enumerate() {
//             let mut buf = [0u8; 4];
//             let s = ch.encode_utf8(&mut buf);
//             pen = renderer.draw_text(&mut fb, s, pen, PX,
//                 [Rgb666::new(63,0,0), Rgb666::new(63,31,0), Rgb666::new(63,63,0),
//                  Rgb666::new(0,63,0), Rgb666::new(0,31,63), Rgb666::new(18,0,63),
//                  Rgb666::new(40,0,63)][i % 7], bg).unwrap();
//         }
//         prev_w = (pen.x - x) + 4;
//         fb.flush_async(&mut hw_display).await.unwrap();
//     }
// }
