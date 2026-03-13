extern crate alloc;

use alloc::string::String;

use defmt::info;
use embedded_graphics::prelude::*;

use super::font::FontRenderer;
use super::framebuffer::Framebuffer;
use super::layout::*;

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
