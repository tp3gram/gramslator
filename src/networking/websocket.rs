extern crate alloc;

use alloc::string::String;

use defmt::info;
use embedded_io_async::Write as _;

use crate::app_state::{self, DisplaySignal};
use crate::translation as translate;

use super::Connection;

/// Process a single incoming WebSocket frame.
///
/// Returns `true` when the connection should be considered closed (server sent
/// a Close frame or an unrecoverable condition was encountered).
pub async fn handle_ws_frame(
    frame_type: edge_ws::FrameType,
    payload: &[u8],
    conn: &mut Connection<'_>,
    mask_key: u32,
    translate_signal: &translate::TranslateSignal,
    display_signal: &DisplaySignal,
) -> bool {
    match frame_type {
        edge_ws::FrameType::Text(_) => {
            let json = core::str::from_utf8(payload).unwrap_or("<invalid UTF-8>");
            info!("Received: {}", json);

            // Extract the transcript and update shared state.
            if let Some(transcript) = translate::extract_transcript(json) {
                let changed = app_state::update_transcript(transcript);
                // Always wake the display so it shows the latest partial.
                display_signal.signal(());

                if changed {
                    // Forward to translation task only on unique transcripts.
                    translate_signal
                        .signal(translate::TranscriptMessage::DgJson(String::from(json)));
                }
            }
            false
        }
        edge_ws::FrameType::Binary(_) => {
            info!("Received binary frame ({} bytes)", payload.len());
            false
        }
        edge_ws::FrameType::Close => {
            info!("WebSocket closed by server.");
            true
        }
        edge_ws::FrameType::Ping => {
            info!("Ping received, sending pong");
            let _ = edge_ws::io::send(
                &mut *conn,
                edge_ws::FrameType::Pong,
                Some(mask_key),
                payload,
            )
            .await;
            let _ = conn.flush().await;
            false
        }
        other => {
            info!("Received {:?} frame ({} bytes)", other, payload.len());
            false
        }
    }
}
