#![feature(allocator_api)]
#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

mod display;

use defmt::info;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use gramslator::elecrow_board;
use gramslator::net;
use static_cell::StaticCell;
use tinyrlibc as _;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.2.0

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // ---- Heap setup -----------------------------------------------------------
    //
    // The global allocator tries regions in registration order.  PSRAM is
    // registered first so the default path (Box::new, Vec::new, String::new,
    // and — crucially — mbedTLS's internal malloc) lands in the 8 MB PSRAM
    // pool.  The small internal SRAM region is registered second as a fallback.
    //
    // For explicit placement use the standard allocator_api (enabled by the
    // `nightly` feature on esp-alloc):
    //
    //   Box::new_in(value, esp_alloc::InternalMemory)   // force SRAM
    //   Box::new_in(value, esp_alloc::ExternalMemory)   // force PSRAM
    //   Vec::<u8>::new_in(esp_alloc::InternalMemory)    // force SRAM
    //
    // ⚠ Atomic operations are unreliable on PSRAM for ESP32-S3.  Any
    //   heap-allocated Atomic* types MUST use InternalMemory explicitly.
    //   (Stack/static atomics are fine — they live in SRAM regardless.)

    // 1️⃣  External PSRAM (8 MB) — default for all heap allocations.
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    // 2️⃣  Internal SRAM (72 KB) — fallback & explicit via InternalMemory.
    esp_alloc::heap_allocator!(size: 72_000);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    info!("Embassy initialized!");

    // ---- Heap diagnostics -------------------------------------------------------
    info!(
        "Heap free: {} bytes, used: {} bytes",
        esp_alloc::HEAP.free(),
        esp_alloc::HEAP.used()
    );

    // ---- WiFi -----------------------------------------------------------------

    let network = elecrow_board::network::init(
        elecrow_board::network::NetworkHardware {
            wifi: peripherals.WIFI,
        },
        &spawner,
    );

    // ---- TLS initialization ---------------------------------------------------

    // True Random Number Generator + mbedTLS singleton.
    // Stored in a StaticCell so the spawned tasks can hold a
    // `&'static Tls<'static>` reference.
    static TLS: StaticCell<mbedtls_rs::Tls<'static>> = StaticCell::new();
    let tls: &'static mbedtls_rs::Tls<'static> = TLS.init(net::init_tls(net::TlsHardware {
        rng: peripherals.RNG,
        adc1: peripherals.ADC1,
    }));

    // ---- Display (SPI + async DMA) ---------------------------------------------
    let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) =
        esp_hal::dma_buffers!(4, 4092);
    let dma_rx_buf =
        esp_hal::dma::DmaRxBuf::new(rx_descriptors, rx_buffer).expect("DMA RX buf");
    let dma_tx_buf =
        esp_hal::dma::DmaTxBuf::new(tx_descriptors, tx_buffer).expect("DMA TX buf");

    let delay = esp_hal::delay::Delay::new();
    let hw_display = elecrow_board::display::init(
        elecrow_board::display::DisplayHardware {
            spi: elecrow_board::display::DisplaySPIBus {
                spi_peripheral: peripherals.SPI2,
                sck: peripherals.GPIO42,
                mosi: peripherals.GPIO39,
                data_command: peripherals.GPIO41,
                chip_select: peripherals.GPIO40,
            },
            pin_tft_power: peripherals.GPIO14,
            pin_backlight: peripherals.GPIO38,
        },
        peripherals.DMA_CH0,
        dma_rx_buf,
        dma_tx_buf,
        delay,
    );

    // ---- Framebuffer + font renderer -------------------------------------------
    let fb = display::Framebuffer::new(480, 320);
    info!(
        "Framebuffer allocated — Heap used: {} bytes, free: {} bytes",
        esp_alloc::HEAP.used(),
        esp_alloc::HEAP.free()
    );

    let renderer = display::FontRenderer::default_font();
    info!(
        "After font load — Heap used: {} bytes, free: {} bytes",
        esp_alloc::HEAP.used(),
        esp_alloc::HEAP.free()
    );

    // ---- Shared signals --------------------------------------------------------

    // Display signal — any task signals this to wake the display renderer.
    static DISPLAY_SIGNAL: StaticCell<gramslator::app_state::DisplaySignal> = StaticCell::new();
    let display_signal: &'static gramslator::app_state::DisplaySignal =
        DISPLAY_SIGNAL.init(gramslator::app_state::DisplaySignal::new());

    // Translate signal — the Deepgram task signals this to request a translation.
    static TRANSLATE_SIGNAL: StaticCell<gramslator::translate::TranslateSignal> = StaticCell::new();
    let translate_signal = TRANSLATE_SIGNAL.init(gramslator::translate::TranslateSignal::new());

    // ---- Spawn tasks -----------------------------------------------------------

    // Translation task (existing, now receives display_signal too).
    let translate_signal = gramslator::translate::spawn_translation_task(
        translate_signal,
        &spawner,
        network,
        tls,
        display_signal,
    );

    // Deepgram streaming task (persistent, reconnects on failure).
    spawner
        .spawn(elecrow_board::network::deepgram_task(
            network,
            tls,
            translate_signal,
            display_signal,
        ))
        .expect("Failed to spawn Deepgram task");
    info!("Deepgram streaming task spawned");

    // Display task (renders transcript + translation on signal).
    spawner
        .spawn(display::display_task(
            hw_display,
            fb,
            renderer,
            display_signal,
        ))
        .expect("Failed to spawn display task");
    info!("Display task spawned");

    // Main task has nothing else to do — just idle.
    loop {
        Timer::after(Duration::from_secs(60)).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.0.0/examples
}
