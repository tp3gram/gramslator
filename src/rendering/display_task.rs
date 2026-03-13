extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use defmt::info;
use embedded_graphics::prelude::*;

use super::font::FontRenderer;
use super::framebuffer::{Framebuffer, Point, Rectangle, Rgb666, Size};

// ===========================================================================
// Word-wrap helper
// ===========================================================================

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
        // Clamp the current offset so it never exceeds the new target.
        if self.scroll_offset > self.scroll_target {
            self.scroll_offset = self.scroll_target;
        }
        self.needs_redraw = true;
    }

    /// Returns `true` if a scroll animation is still in progress.
    fn is_animating(&self) -> bool {
        self.scroll_target - self.scroll_offset > 0.5
    }

    /// Advance the scroll animation by `amount` pixels.
    fn advance(&mut self, amount: f32) {
        if self.is_animating() {
            self.scroll_offset = (self.scroll_offset + amount).min(self.scroll_target);
            self.needs_redraw = true;
        }
    }
}

/// Render a section's text into the framebuffer with clipping and scroll.
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
    mut hw_display: crate::elecrow_board::display::AsyncDisplay<'static>,
    mut fb: Framebuffer,
    mut renderer: FontRenderer,
    display_signal: &'static crate::app_state::DisplaySignal,
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
            let _ = embassy_time::with_timeout(ANIM_FRAME_DURATION, display_signal.wait()).await;
        } else {
            display_signal.wait().await;
        }

        // ---- Check for updated text ------------------------------------
        let (transcript, translation) = crate::app_state::read_state();

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

        // Always redraw the separator.
        draw_separator(&mut fb);

        // ---- Flush dirty regions ---------------------------------------
        if fb.is_dirty() {
            if let Err(e) = fb.flush_async(&mut hw_display).await {
                info!("Display flush error: {:?}", e);
            }
        }
    }
}
