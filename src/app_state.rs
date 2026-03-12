//! Shared application state for cross-task communication.
//!
//! Provides a global [`AppState`] holding the current transcript and
//! translation, protected by a `critical_section::Mutex`.  Helper functions
//! allow any Embassy task to update or read the state atomically.
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

/// Application-visible state shared between tasks.
struct AppState {
    /// The most recent transcript received from Deepgram.
    transcript: String,
    /// The most recent translation received from Google Translate.
    translation: String,
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

/// Read a snapshot of the current transcript and translation.
///
/// Returns `(transcript, translation)` as cloned `String`s.  If no state
/// has been written yet, both strings are empty.
pub fn read_state() -> (String, String) {
    critical_section::with(|cs| {
        let borrow = STATE.borrow_ref(cs);
        match &*borrow {
            Some(state) => (state.transcript.clone(), state.translation.clone()),
            None => (String::new(), String::new()),
        }
    })
}
