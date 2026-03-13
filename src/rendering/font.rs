extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use embedded_graphics::prelude::*;

use super::framebuffer::{Point, Rectangle, Rgb666, Size};

/// Default font data baked into flash (Noto Sans JP Regular, OFL-licensed).
///
/// Subset to: Basic Latin, Latin-1/Extended, Greek, Cyrillic, Hiragana,
/// Katakana, CJK Unified Ideographs, CJK/General punctuation, Hangul Jamo,
/// Halfwidth/Fullwidth forms, and currency symbols.
const DEFAULT_FONT_DATA: &[u8] = include_bytes!("assets/NotoSansJP-Medium.ttf");

/// Default maximum number of cached glyphs before LRU eviction kicks in.
///
/// At ~2 KB average per glyph bitmap (50 px Latin/CJK mix), 256 entries ≈
/// 512 KB in PSRAM — a good balance between hit-rate and memory use.
const DEFAULT_MAX_CACHE_SIZE: usize = 256;

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
        let face = ttf_parser::Face::parse(font_data, 0).expect("Failed to parse TTF/OTF font");
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

        let advance_width = self.face.glyph_hor_advance(glyph_id).unwrap_or(0) as f32 * scale;

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
        const PAD: usize = 1;
        let rast_w = width + PAD * 2;
        let rast_h = height + PAD * 2;
        let mut rasterizer = ab_glyph_rasterizer::Rasterizer::new(rast_w, rast_h);

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
    fn ensure_cached(&mut self, ch: char, px: f32) {
        let key = (ch, px.to_bits());
        let stamp = self.access_counter;
        self.access_counter += 1;

        if let Some(entry) = self.cache.get_mut(&key) {
            entry.last_used = stamp;
            return;
        }

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
        let total_width: f32 = text.chars().map(|ch| self.char_advance(ch, px)).sum();
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
