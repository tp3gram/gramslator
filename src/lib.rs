#![no_std]

defmt::timestamp!("{=u64:us}", embassy_time::Instant::now().as_micros());

pub mod app_state;
pub mod elecrow_board;
pub mod networking;
pub mod rendering;
pub mod translation;
