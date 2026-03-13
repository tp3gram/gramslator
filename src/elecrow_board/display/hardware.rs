use esp_hal::gpio::Output;
use esp_hal::peripherals::{GPIO14, GPIO38, GPIO39, GPIO40, GPIO41, GPIO42, SPI2};
use esp_hal::spi::master::SpiDmaBus;

use embedded_hal::spi::{ErrorType, Operation, SpiBus, SpiDevice};

// ---------------------------------------------------------------------------
// Hardware descriptor structs
// ---------------------------------------------------------------------------

pub struct DisplaySPIBus<'a> {
    pub spi_peripheral: SPI2<'a>,
    pub sck: GPIO42<'a>,
    pub mosi: GPIO39<'a>,
    pub data_command: GPIO41<'a>,
    pub chip_select: GPIO40<'a>,
}

pub struct DisplayHardware<'a> {
    pub spi: DisplaySPIBus<'a>,
    pub tft_power_pin: GPIO14<'a>,
    pub backlight_pin: GPIO38<'a>,
}

// ---------------------------------------------------------------------------
// Temporary blocking SpiDevice for mipidsi init
// ---------------------------------------------------------------------------

/// Wraps `SpiDmaBus<Blocking>` + CS pin into an `embedded_hal::spi::SpiDevice`
/// that can be decomposed after use.  Only lives during display initialisation.
pub(super) struct InitSpiDevice<'a> {
    pub bus: SpiDmaBus<'a, esp_hal::Blocking>,
    pub cs: Output<'a>,
}

impl ErrorType for InitSpiDevice<'_> {
    type Error = esp_hal::spi::Error;
}

impl SpiDevice for InitSpiDevice<'_> {
    fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Result<(), Self::Error> {
        self.cs.set_low();
        let result = operations.iter_mut().try_for_each(|op| match op {
            Operation::Read(buf) => SpiBus::read(&mut self.bus, buf),
            Operation::Write(buf) => SpiBus::write(&mut self.bus, buf),
            Operation::Transfer(read, write) => SpiBus::transfer(&mut self.bus, read, write),
            Operation::TransferInPlace(buf) => SpiBus::transfer_in_place(&mut self.bus, buf),
            Operation::DelayNs(_) => Ok(()),
        });
        let flush = SpiBus::flush(&mut self.bus);
        self.cs.set_high();
        result?;
        flush
    }
}
