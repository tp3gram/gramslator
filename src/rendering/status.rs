use crate::app_state::ServiceStatus;

use embedded_graphics::prelude::*;

use super::font::FontRenderer;
use super::framebuffer::{Framebuffer, Point, Rectangle, Rgb666, Size};
use super::layout::*;

/// Build centered status text for WiFi/Deepgram issues (plain English).
/// Returns empty string when both are healthy.
pub(super) fn primary_status_text(wifi: ServiceStatus, deepgram: ServiceStatus) -> &'static str {
    match wifi {
        ServiceStatus::Idle | ServiceStatus::Connecting => return "Connecting to WiFi...",
        ServiceStatus::Error => return "WiFi disconnected",
        ServiceStatus::Connected => {}
    }
    match deepgram {
        ServiceStatus::Idle | ServiceStatus::Connecting => return "Connecting to Deepgram...",
        ServiceStatus::Error => return "Deepgram disconnected",
        ServiceStatus::Connected => {}
    }
    ""
}

/// Build small indicator text for Translate status (top-right corner).
pub(super) fn translate_status_text(translate: ServiceStatus) -> &'static str {
    match translate {
        ServiceStatus::Connecting => "TR...",
        ServiceStatus::Error => "TR!",
        _ => "",
    }
}

/// Draw the primary status overlay (WiFi / Deepgram) centered in the
/// translation region.  Shows the SSID as a subtitle for WiFi states.
pub(super) fn draw_primary_status(
    fb: &mut Framebuffer,
    renderer: &mut FontRenderer,
    wifi_status: ServiceStatus,
    primary_status: &str,
    section_h: i32,
) {
    // Clear the translation region.
    let clear_rect = Rectangle::new(
        Point::new(0, TRANSLATION_Y),
        Size::new(SCREEN_W as u32, section_h as u32),
    );
    fb.fill_solid(&clear_rect, BG).unwrap();

    if primary_status.is_empty() {
        return;
    }

    let status_px = PRIMARY_STATUS_PX;
    let show_ssid = !matches!(wifi_status, ServiceStatus::Connected);

    if show_ssid {
        // Two lines: status text + SSID underneath.
        let subtitle_px = PRIMARY_STATUS_SUBTITLE_PX;
        let line1_h = renderer.line_height(status_px) as i32;
        let gap = 6;
        let line2_h = renderer.line_height(subtitle_px) as i32;
        let total_h = line1_h + gap + line2_h;
        let y1 = TRANSLATION_Y + (section_h - total_h) / 2;
        let y2 = y1 + line1_h + gap;

        let _ = renderer.draw_text_centered(
            fb,
            primary_status,
            y1,
            SCREEN_W,
            status_px,
            Rgb666::new(63, 63, 63),
            BG,
        );
        let _ = renderer.draw_text_centered(
            fb,
            env!("WIFI_SSID"),
            y2,
            SCREEN_W,
            subtitle_px,
            Rgb666::new(40, 40, 40),
            BG,
        );
    } else {
        // Single centered line.
        let line_h = renderer.line_height(status_px) as i32;
        let y = TRANSLATION_Y + (section_h - line_h) / 2;
        let _ = renderer.draw_text_centered(
            fb,
            primary_status,
            y,
            SCREEN_W,
            status_px,
            Rgb666::new(63, 63, 63),
            BG,
        );
    }
}

/// Draw the translate status indicator in the top-right corner.
pub(super) fn draw_translate_status(
    fb: &mut Framebuffer,
    renderer: &mut FontRenderer,
    tr_status: &str,
) {
    let status_rect = Rectangle::new(
        Point::new(TR_STATUS_X, TR_STATUS_Y),
        Size::new(TR_STATUS_W, TR_STATUS_H),
    );
    fb.fill_solid(&status_rect, BG).unwrap();

    if !tr_status.is_empty() {
        let _ = renderer.draw_text(
            fb,
            tr_status,
            Point::new(TR_STATUS_X, TR_STATUS_Y),
            TR_STATUS_PX,
            TR_STATUS_COLOR,
            BG,
        );
    }
}
