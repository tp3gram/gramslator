#![no_std]

defmt::timestamp!("{=u64:us}", embassy_time::Instant::now().as_micros());

/// PCM sample rate in Hz, shared between mic hardware init and Deepgram.
pub const SAMPLE_RATE: u32 = 8_000;

pub mod app_state;
pub mod elecrow_board;
pub mod networking;
pub mod rendering;
pub mod translation;
