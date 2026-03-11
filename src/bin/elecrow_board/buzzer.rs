use esp_hal::gpio::{Level, Output, OutputConfig};

pub struct BuzzerHardware<'a> {
    pub buzzer_pin: esp_hal::peripherals::GPIO8<'a>,
}

pub fn init<'d>(buzzer_hardware: BuzzerHardware<'d>) -> Output<'d> {
    let pin_buzzer = buzzer_hardware.buzzer_pin;

    Output::new(pin_buzzer, Level::Low, OutputConfig::default())
}
