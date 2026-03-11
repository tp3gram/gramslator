//! @TODO: If the wireless module was actually implemented, IO9 and IO10 are shared for both the mic and the modules.
//! The mic and this module would need to be refactored to properly represent that case, such that there is one structure owning the shared pins and switching them between the two modes if possible.
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::peripherals::GPIO45;

/// Controls the analog switch (U8/SGM3799).
///
/// The mic and wireless module is wired through an analog switch (U8) controlled by the [`MicWirelessModuleSwitchHardware`].
/// Setting GPIO45 LOW routes GPIO9 (BCLK) and GPIO10 (DIN) to the microphone.
pub struct MicWirelessModuleSwitchHardware<'a> {
    switch_output: Output<'a>,
}

pub enum SwitchState {
    Mic,
    WirelessModule,
}

impl<'a> MicWirelessModuleSwitchHardware<'a> {
    pub fn init(switch_pin: GPIO45<'a>, initial_state: SwitchState) -> Self {
        let mut hardware = Self {
            switch_output: Output::new(switch_pin, Level::Low, OutputConfig::default()),
        };

        hardware.set_state(initial_state);
        hardware
    }

    pub fn set_state(&mut self, state: SwitchState) {
        match state {
            SwitchState::Mic => self.switch_output.set_low(),
            SwitchState::WirelessModule => self.switch_output.set_high(),
        }
    }
}
