//! Shared application state for cross-task communication.
//!
//! Provides a global [`AppState`] holding the current transcript,
//! translation, target language, and service connection statuses,
//! protected by a `critical_section::Mutex`.  Helper functions allow any
//! Embassy task to update or read the state atomically.
//!
//! A [`DisplaySignal`] is used to wake the display task whenever the
//! visible state changes.

extern crate alloc;

use alloc::string::String;
use core::cell::RefCell;

use critical_section::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;

/// Signal type used to wake the display task on state changes.
///
/// "Latest wins" semantics — multiple rapid signals coalesce into one
/// wakeup, which is exactly what we want (the display always reads the
/// latest state on wake).
pub type DisplaySignal = Signal<CriticalSectionRawMutex, ()>;

/// Connection status for a remote service (WiFi, Deepgram, Google Translate).
#[derive(Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum ServiceStatus {
    /// Not yet attempted.
    Idle,
    /// TCP/TLS/WS handshake or WiFi association in progress.
    Connecting,
    /// Actively connected and operational.
    Connected,
    /// Last attempt failed; will retry.
    Error,
}

/// Supported target language codes, cycled through via left/right touch.
pub const LANGUAGES: &[&str] = &["es", "fr", "de", "ja", "ga", "hi", "uk"];

/// Application-visible state shared between tasks.
struct AppState {
    /// The most recent transcript received from Deepgram.
    transcript: String,
    /// The most recent translation received from Google Translate.
    translation: String,
    /// WiFi connection status.
    wifi_status: ServiceStatus,
    /// Deepgram WebSocket connection status.
    deepgram_status: ServiceStatus,
    /// Google Translate connection status.
    translate_status: ServiceStatus,
    /// Index into [`LANGUAGES`] for the current target language.
    target_lang_index: usize,
}

/// Snapshot of the shared state, returned by [`read_state`].
pub struct StateSnapshot {
    pub transcript: String,
    pub translation: String,
    pub wifi_status: ServiceStatus,
    pub deepgram_status: ServiceStatus,
    pub translate_status: ServiceStatus,
    pub target_lang: &'static str,
}

/// Global shared state, initialised lazily on first write.
static STATE: Mutex<RefCell<Option<AppState>>> = Mutex::new(RefCell::new(None));

/// Ensure the state is initialised, returning a mutable reference to it
/// inside the critical section closure.
fn with_state<R>(f: impl FnOnce(&mut AppState) -> R) -> R {
    critical_section::with(|cs| {
        let mut borrow = STATE.borrow_ref_mut(cs);
        let state = borrow.get_or_insert_with(|| AppState {
            transcript: String::new(),
            translation: String::new(),
            wifi_status: ServiceStatus::Idle,
            deepgram_status: ServiceStatus::Idle,
            translate_status: ServiceStatus::Idle,
            target_lang_index: 0,
        });
        f(state)
    })
}

/// Update the current transcript.
///
/// Returns `true` if the transcript actually changed (i.e. it differs from
/// the previously stored value).  Callers should only fire translation
/// requests and display updates when this returns `true`.
pub fn update_transcript(text: &str) -> bool {
    with_state(|state| {
        if state.transcript == text {
            return false;
        }
        state.transcript.clear();
        state.transcript.push_str(text);
        true
    })
}

/// Update the current translation.
///
/// Always overwrites the stored value.  Returns `true` if the value
/// actually changed.
pub fn update_translation(text: &str) -> bool {
    with_state(|state| {
        if state.translation == text {
            return false;
        }
        state.translation.clear();
        state.translation.push_str(text);
        true
    })
}

/// Update the WiFi connection status.  Returns `true` if it changed.
pub fn update_wifi_status(status: ServiceStatus) -> bool {
    with_state(|state| {
        if state.wifi_status == status {
            return false;
        }
        state.wifi_status = status;
        true
    })
}

/// Update the Deepgram connection status.  Returns `true` if it changed.
pub fn update_deepgram_status(status: ServiceStatus) -> bool {
    with_state(|state| {
        if state.deepgram_status == status {
            return false;
        }
        state.deepgram_status = status;
        true
    })
}

/// Update the Google Translate connection status.  Returns `true` if it changed.
pub fn update_translate_status(status: ServiceStatus) -> bool {
    with_state(|state| {
        if state.translate_status == status {
            return false;
        }
        state.translate_status = status;
        true
    })
}

/// Read a snapshot of the current state.
///
/// If no state has been written yet, all strings are empty, all
/// statuses are [`ServiceStatus::Idle`], and the language defaults to
/// the first entry in [`LANGUAGES`].
pub fn read_state() -> StateSnapshot {
    critical_section::with(|cs| {
        let borrow = STATE.borrow_ref(cs);
        match &*borrow {
            Some(state) => StateSnapshot {
                transcript: state.transcript.clone(),
                translation: state.translation.clone(),
                wifi_status: state.wifi_status,
                deepgram_status: state.deepgram_status,
                translate_status: state.translate_status,
                target_lang: LANGUAGES[state.target_lang_index],
            },
            None => StateSnapshot {
                transcript: String::new(),
                translation: String::new(),
                wifi_status: ServiceStatus::Idle,
                deepgram_status: ServiceStatus::Idle,
                translate_status: ServiceStatus::Idle,
                target_lang: LANGUAGES[0],
            },
        }
    })
}

/// Cycle the target language forward (`forward = true`) or backward.
///
/// Returns the new language code.
pub fn cycle_target_lang(forward: bool) -> &'static str {
    with_state(|state| {
        let len = LANGUAGES.len();
        state.target_lang_index = if forward {
            (state.target_lang_index + 1) % len
        } else {
            (state.target_lang_index + len - 1) % len
        };
        LANGUAGES[state.target_lang_index]
    })
}

/// Read the current target language code without cloning other state.
pub fn read_target_lang() -> &'static str {
    critical_section::with(|cs| {
        let borrow = STATE.borrow_ref(cs);
        match &*borrow {
            Some(state) => LANGUAGES[state.target_lang_index],
            None => LANGUAGES[0],
        }
    })
}
