extern crate alloc;

use alloc::string::String;

use defmt::info;
use embedded_graphics::prelude::*;

use super::font::FontRenderer;
use super::framebuffer::{Framebuffer, Point, Rectangle, Rgb666, Size};
use super::layout::*;
use super::status::{
    draw_primary_status, draw_translate_status, primary_status_text, translate_status_text,
};

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
    let mut last_primary_status: &str = "";
    let mut last_tr_status: &str = "";
    let mut last_lang: &str = crate::app_state::read_target_lang();
    let mut translation_sec = Section::new();
    let mut transcript_sec = Section::new();

    /// How long the centered language overlay stays on screen.
    const LANG_OVERLAY_DURATION: embassy_time::Duration = embassy_time::Duration::from_secs(1);

    // Show the default language overlay immediately on boot.
    render_lang_overlay(&mut fb, &mut renderer, last_lang);
    fb.flush_async(&mut hw_display).await.unwrap();
    let mut lang_overlay_until: Option<embassy_time::Instant> =
        Some(embassy_time::Instant::now() + LANG_OVERLAY_DURATION);

    loop {
        // ---- Wait for something to happen ------------------------------
        let animating = translation_sec.is_animating() || transcript_sec.is_animating();

        if animating {
            // Scroll animation in progress — keep advancing at ~30 fps.
            let _ = embassy_time::with_timeout(ANIM_FRAME_DURATION, display_signal.wait()).await;
        } else if let Some(deadline) = lang_overlay_until {
            // Overlay is showing — sleep until it expires or a signal
            // arrives, whichever comes first.  No busy loop.
            let remaining = deadline.saturating_duration_since(embassy_time::Instant::now());
            let _ = embassy_time::with_timeout(remaining, display_signal.wait()).await;
        } else {
            // Nothing animating, no overlay — block until signalled.
            display_signal.wait().await;
        }

        // ---- Check for updated state ------------------------------------
        let state = crate::app_state::read_state();

        if state.translation != last_translation {
            translation_sec.update_text(
                &state.translation,
                &renderer,
                TRANSLATION_PX,
                max_text_width,
                translation_section_h as f32,
            );
            last_translation = state.translation;
        }
        if state.transcript != last_transcript {
            transcript_sec.update_text(
                &state.transcript,
                &renderer,
                TRANSCRIPT_PX,
                max_text_width,
                transcript_section_h as f32,
            );
            last_transcript = state.transcript;
        }

        // ---- Primary status (WiFi / Deepgram) — centered, large, white -
        let primary_status = primary_status_text(state.wifi_status, state.deepgram_status);

        if primary_status != last_primary_status {
            draw_primary_status(
                &mut fb,
                &mut renderer,
                state.wifi_status,
                primary_status,
                translation_section_h,
            );
            if primary_status.is_empty() {
                // Status cleared — force redraw of translation text.
                translation_sec.needs_redraw = true;
            }
            last_primary_status = primary_status;
        }

        // ---- Translate status — small indicator, top-right corner -------
        let tr_status = translate_status_text(state.translate_status);

        if tr_status != last_tr_status {
            draw_translate_status(&mut fb, &mut renderer, tr_status);
            last_tr_status = tr_status;
        }

        // ---- Detect language change ------------------------------------
        let lang_changed = state.target_lang != last_lang;
        if lang_changed {
            last_lang = state.target_lang;
            lang_overlay_until = Some(embassy_time::Instant::now() + LANG_OVERLAY_DURATION);

            // Draw the overlay exactly once now and flush immediately.
            render_lang_overlay(&mut fb, &mut renderer, last_lang);
            if let Err(e) = fb.flush_async(&mut hw_display).await {
                info!("Display flush error: {:?}", e);
            }
        }

        // ---- Check overlay expiry — force full redraw to restore -------
        let overlay_visible = if let Some(deadline) = lang_overlay_until {
            if embassy_time::Instant::now() >= deadline {
                lang_overlay_until = None;
                // Force both sections to repaint so the normal content
                // returns after the overlay disappears.
                translation_sec.needs_redraw = true;
                transcript_sec.needs_redraw = true;
                false
            } else {
                true
            }
        } else {
            false
        };

        // ---- Advance scroll animations ---------------------------------
        // Always advance so is_animating() converges even during overlay.
        translation_sec.advance(scroll_per_frame);
        transcript_sec.advance(scroll_per_frame);

        // While the overlay is visible, skip section rendering — the text
        // data has already been captured above and will render on expiry.
        if overlay_visible {
            continue;
        }

        // ---- Render sections that need it ------------------------------
        // Skip translation rendering while a primary status overlay is shown.
        if translation_sec.needs_redraw && last_primary_status.is_empty() {
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

// ---------------------------------------------------------------------------
// Centered language overlay
// ---------------------------------------------------------------------------

/// Font size for the large centred language code.
const LANG_OVERLAY_PX: f32 = 96.0;
const LANG_OVERLAY_COLOR: Rgb666 = Rgb666::new(63, 50, 20); // warm yellow

/// Render the target-language code in large letters, centred both
/// horizontally and vertically on the screen.  Drawn *on top of* the
/// normal section content each frame while the overlay is active.
fn render_lang_overlay(fb: &mut Framebuffer, renderer: &mut FontRenderer, lang: &str) {
    let text_w = renderer.text_width(lang, LANG_OVERLAY_PX);
    let text_h = renderer.line_height(LANG_OVERLAY_PX);
    let x = ((SCREEN_W as f32 - text_w) / 2.0) as i32;
    let y = ((SCREEN_H as f32 - text_h) / 2.0) as i32;

    // Clear a region behind the text so it's readable over any content.
    let pad = 16;
    let rect = Rectangle::new(
        Point::new(x - pad, y - pad),
        Size::new(
            text_w as u32 + 2 * pad as u32,
            text_h as u32 + 2 * pad as u32,
        ),
    );
    fb.fill_solid(&rect, BG).unwrap();

    renderer
        .draw_text(
            fb,
            lang,
            Point::new(x, y),
            LANG_OVERLAY_PX,
            LANG_OVERLAY_COLOR,
            BG,
        )
        .unwrap();
}
