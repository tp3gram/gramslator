use esp_hal::peripherals::{ADC1, RNG};
use esp_hal::rng::{Trng, TrngSource};
use mbedtls_rs::Tls;
use static_cell::StaticCell;

pub struct TlsHardware {
    pub rng: RNG<'static>,
    pub adc1: ADC1<'static>,
}

/// Initialise the True Random Number Generator and create the mbedTLS
/// singleton.  Must only be called once (the static cells will panic on a
/// second call).
pub fn init_global_tls(hardware: TlsHardware) -> Tls<'static> {
    // TrngSource configures the RNG peripheral; it must stay alive.
    static TRNG_SOURCE: StaticCell<TrngSource<'static>> = StaticCell::new();
    static TRNG: StaticCell<Trng> = StaticCell::new();

    let trng_source = TrngSource::new(hardware.rng, hardware.adc1);
    TRNG_SOURCE.init(trng_source);

    let trng = TRNG.init(Trng::try_new().expect("TrngSource not active"));

    let mut tls = Tls::new(trng).expect("Failed to create TLS instance");
    tls.set_debug(1);
    tls
}
