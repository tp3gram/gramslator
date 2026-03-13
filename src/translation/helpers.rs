extern crate alloc;

use alloc::string::String;

use defmt::info;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embedded_io_async::{Read, Write};

use core::cell::RefCell;
use critical_section::Mutex;

use crate::app_state::{self, DisplaySignal};

use super::client::translate_text;

/// Cached last translation: `(input_text, translated_text)`.
static LAST_TRANSLATION: Mutex<RefCell<Option<(String, String)>>> = Mutex::new(RefCell::new(None));

/// A message sent from the WebSocket receive loop to the translation task.
pub enum TranscriptMessage {
    /// A Deepgram JSON frame containing a (possibly partial) transcript.
    DgJson(String),
    /// No more frames are coming — translate the buffered transcript
    /// immediately, skipping the idle-timeout debounce.
    Flush,
}

/// Concrete signal type used for translation requests.
/// "Latest wins" — the producer overwrites any pending value.
pub type TranslateSignal = Signal<CriticalSectionRawMutex, TranscriptMessage>;

/// Extract the `"transcript"` value from a Deepgram JSON response.
///
/// Returns `None` if the field is missing, malformed, or empty.
pub fn extract_transcript(json: &str) -> Option<&str> {
    let needle = "\"transcript\":\"";
    let key_start = json.find(needle)?;
    let value_start = key_start + needle.len();
    let end = json[value_start..].find('"')?;
    let transcript = &json[value_start..value_start + end];

    if transcript.is_empty() {
        info!("Empty transcript, skipping translation");
        return None;
    }
    Some(transcript)
}

/// Check the single-entry translation cache. Returns the cached translation
/// if `transcript` matches the most recently translated input.
pub fn check_translation_cache(transcript: &str) -> Option<String> {
    critical_section::with(|cs| {
        let borrow = LAST_TRANSLATION.borrow_ref(cs);
        if let Some((ref prev_input, ref prev_result)) = *borrow
            && prev_input == transcript
        {
            return Some(prev_result.clone());
        }
        None
    })
}

/// Translate a Deepgram transcript from English to Spanish via Google
/// Translate, using the provided TLS session. Updates both the translation
/// cache and the shared [`AppState`](crate::app_state) on success, then
/// signals the display task.
pub async fn translate_response<S>(
    session: &mut S,
    transcript: &str,
    display_signal: &DisplaySignal,
) where
    S: Read + Write,
{
    info!("Translating: \"{}\" (en -> es)...", transcript);

    let mut rx_buf = [0u8; 512];
    match translate_text(session, transcript, "en", "es", &mut rx_buf).await {
        Ok(len) => {
            let translated = core::str::from_utf8(&rx_buf[..len]).unwrap_or("<invalid UTF-8>");
            info!("Translation result: {}", translated);

            // Update local cache.
            critical_section::with(|cs| {
                *LAST_TRANSLATION.borrow_ref_mut(cs) =
                    Some((String::from(transcript), String::from(translated)));
            });

            // Update shared app state and wake the display.
            if app_state::update_translation(translated) {
                display_signal.signal(());
            }
        }
        Err(e) => {
            defmt::error!("Translation failed: {:?}", e);
        }
    }
}

