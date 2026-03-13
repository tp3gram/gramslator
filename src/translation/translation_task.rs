extern crate alloc;

use alloc::string::String;

use defmt::info;
use embassy_time::Duration;
use mbedtls_rs::Tls;

use crate::app_state::{self, DisplaySignal, ServiceStatus};
use crate::networking::Connection;

use super::helpers::*;

/// Maximum time to buffer partial transcripts before translating.
const TRANSLATE_DEBOUNCE_DEADLINE: Duration = Duration::from_millis(500);

/// Background task that receives Deepgram JSON via a signal, debounces
/// rapid partial transcripts, then translates the latest one via Google
/// Translate.  Skips the TLS round-trip on cache hits.
///
/// A [`TranscriptMessage::Retranslate`] message causes the task to
/// re-translate the most recent transcript in the (now-changed) target
/// language without waiting for new Deepgram data.
#[embassy_executor::task]
pub async fn translation_task(
    signal: &'static TranslateSignal,
    stack: embassy_net::Stack<'static>,
    tls: &'static Tls<'static>,
    display_signal: &'static DisplaySignal,
) {
    // Remember the last JSON so Retranslate can re-use it.
    let mut last_json: Option<String> = None;

    loop {
        // Block until the first transcript arrives.
        let mut pending_json = match signal.wait().await {
            TranscriptMessage::DgJson(json) => json,
            TranscriptMessage::Flush => continue,
            TranscriptMessage::Retranslate => {
                // Re-translate the last transcript with the new language.
                match last_json {
                    Some(ref json) => json.clone(),
                    None => continue, // nothing to re-translate yet
                }
            }
        };

        // Debounce: keep buffering newer transcripts until the deadline
        // expires, a Flush arrives, or a Retranslate arrives.
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
                Ok(TranscriptMessage::Retranslate) => {
                    info!("Language changed — translating immediately");
                    break;
                }
            }
        }

        // Stash the JSON for future Retranslate requests.
        last_json = Some(pending_json);

        let Some(transcript) = extract_transcript(last_json.as_deref().unwrap()) else {
            info!("No transcript field found in response");
            continue;
        };

        // Read the current target language at translation time so a touch
        // that changed the language mid-debounce takes effect.
        let target_lang = app_state::read_target_lang();

        // Check cache — skip the network round-trip on hit.
        if let Some(result) = check_translation_cache(transcript, target_lang) {
            info!("Translation cache hit: \"{}\"", result.as_str());
            if app_state::update_translation(result.as_str()) {
                display_signal.signal(());
            }
            continue;
        }

        if app_state::update_translate_status(ServiceStatus::Connecting) {
            display_signal.signal(());
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
                if app_state::update_translate_status(ServiceStatus::Error) {
                    display_signal.signal(());
                }
                continue;
            }
        };

        if app_state::update_translate_status(ServiceStatus::Connected) {
            display_signal.signal(());
        }

        translate_response(&mut conn, transcript, target_lang, display_signal).await;

        conn.close().await;
        if app_state::update_translate_status(ServiceStatus::Idle) {
            display_signal.signal(());
        }
    }
}
