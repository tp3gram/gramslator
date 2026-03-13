use embedded_graphics::mono_font::ascii;
use embedded_graphics::mono_font::{MonoFont, MonoTextStyle, MonoTextStyleBuilder};
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};

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
/// calls to continue drawing on the same line.
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
