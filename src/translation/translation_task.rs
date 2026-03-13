use defmt::info;
use embassy_time::Duration;
use mbedtls_rs::Tls;

use crate::app_state::{self, DisplaySignal};
use crate::networking::Connection;

use super::helpers::*;

/// Maximum time to buffer partial transcripts before translating.
const TRANSLATE_DEBOUNCE_DEADLINE: Duration = Duration::from_secs(1);

/// Background task that receives Deepgram JSON via a signal, debounces
/// rapid partial transcripts, then translates the latest one (en -> es)
/// via Google Translate.  Skips the TLS round-trip on cache hits.
#[embassy_executor::task]
pub async fn translation_task(
    signal: &'static TranslateSignal,
    stack: embassy_net::Stack<'static>,
    tls: &'static Tls<'static>,
    display_signal: &'static DisplaySignal,
) {
    loop {
        // Block until the first transcript arrives.
        let mut pending_json = match signal.wait().await {
            TranscriptMessage::DgJson(json) => json,
            TranscriptMessage::Flush => continue,
        };

        // Debounce: keep buffering newer transcripts until the deadline
        // expires or a Flush arrives.
        let deadline = embassy_time::Instant::now() + TRANSLATE_DEBOUNCE_DEADLINE;
        loop {
            let remaining = deadline.saturating_duration_since(embassy_time::Instant::now());
            if remaining == Duration::from_ticks(0) {
                info!("Debounce deadline reached — translating buffered transcript");
                break;
            }
            match embassy_time::with_timeout(remaining, signal.wait()).await {
                Err(_timeout) => {
                    info!("Debounce deadline reached — translating buffered transcript");
                    break;
                }
                Ok(TranscriptMessage::DgJson(json)) => {
                    pending_json = json;
                }
                Ok(TranscriptMessage::Flush) => {
                    info!("Final transcript received — translating immediately");
                    break;
                }
            }
        }

        let Some(transcript) = extract_transcript(&pending_json) else {
            info!("No transcript field found in response");
            continue;
        };

        // Check cache — skip the network round-trip on hit.
        if let Some(result) = check_translation_cache(transcript) {
            info!("Translation cache hit: \"{}\"", result.as_str());
            if app_state::update_translation(result.as_str()) {
                display_signal.signal(());
            }
            continue;
        }

        let mut conn = match Connection::open_tcp_connection_with_tls(
            stack,
            env!("GOOGLE_TRANSLATE_HOST"),
            443,
            tls,
        )
        .await
        {
            Ok(c) => c,
            Err(e) => {
                info!("Failed to connect to Google Translate: {:?}", e);
                continue;
            }
        };

        translate_response(&mut conn, transcript, display_signal).await;

        conn.close().await;
    }
}
