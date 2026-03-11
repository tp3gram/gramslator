use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::peripherals::Peripherals;

pub fn init_pin<'d>(peripherals: Peripherals) -> Output<'d> {
    let pin_buzzer = peripherals.GPIO8;

    Output::new(pin_buzzer, Level::Low, OutputConfig::default())
}
