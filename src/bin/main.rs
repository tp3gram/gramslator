#![feature(allocator_api)]
#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

extern crate alloc;

mod main_task;

use defmt::info;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::dma_circular_buffers;
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::system::Stack;
use esp_hal::timer::timg::TimerGroup;
use gramslator::elecrow_board;
use gramslator::networking;
use gramslator::rendering;
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

    // ---- Analog switch: route GPIO9/10 to microphone -------------------------

    let _mic_switch =
        elecrow_board::mic_wireless_module_switch::MicWirelessModuleSwitchHardware::init(
            peripherals.GPIO45,
            elecrow_board::mic_wireless_module_switch::SwitchState::Mic,
        );

    // ---- Microphone (I2S RX) -------------------------------------------------

    let (rx_buffer, rx_descriptors, _, _) = dma_circular_buffers!(32000, 0);

    let mut i2s_rx = elecrow_board::mic::init(
        elecrow_board::mic::MicHardware {
            i2s: peripherals.I2S0,
            dma_channel: peripherals.DMA_CH0,
            clk_pin: peripherals.GPIO9,
            din_pin: peripherals.GPIO10,
        },
        rx_descriptors,
        gramslator::SAMPLE_RATE,
    );

    // ---- Second core: blocking DMA read loop ---------------------------------

    let software_interrupt = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    static mut CORE1_STACK: Stack<8192> = Stack::new();

    // Start blocking DMA read loop on second core
    esp_rtos::start_second_core(
        peripherals.CPU_CTRL,
        software_interrupt.software_interrupt0,
        software_interrupt.software_interrupt1,
        unsafe { &mut *core::ptr::addr_of_mut!(CORE1_STACK) },
        move || {
            elecrow_board::mic::read_mic_dma_loop_blocking(&mut i2s_rx, rx_buffer);
        },
    );

    // ---- Display (SPI + async DMA) — spawned in parallel on core 0 -----------

    let (disp_rx_buffer, disp_rx_descriptors, disp_tx_buffer, disp_tx_descriptors) =
        esp_hal::dma_buffers!(4, 4092);
    let dma_rx_buf =
        esp_hal::dma::DmaRxBuf::new(disp_rx_descriptors, disp_rx_buffer).expect("DMA RX buf");
    let dma_tx_buf =
        esp_hal::dma::DmaTxBuf::new(disp_tx_descriptors, disp_tx_buffer).expect("DMA TX buf");

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
            tft_power_pin: peripherals.GPIO14,
            backlight_pin: peripherals.GPIO38,
        },
        peripherals.DMA_CH1,
        dma_rx_buf,
        dma_tx_buf,
        delay,
    );

    // ---- Shared signals (created early so WiFi init can use display_signal) ----

    // Display signal — any task signals this to wake the display renderer.
    static DISPLAY_SIGNAL: StaticCell<gramslator::app_state::DisplaySignal> = StaticCell::new();
    let display_signal: &'static gramslator::app_state::DisplaySignal =
        DISPLAY_SIGNAL.init(gramslator::app_state::DisplaySignal::new());
    // ---- Touch (GT911 via I2C) ------------------------------------------------

    let i2c_touch = elecrow_board::touch::init(elecrow_board::touch::TouchHardware {
        i2c: peripherals.I2C0,
        sda: peripherals.GPIO15,
        scl: peripherals.GPIO16,
        rst: peripherals.GPIO48,
    });

    // ---- WiFi -----------------------------------------------------------------

    let network = elecrow_board::wifi::init(
        elecrow_board::wifi::NetworkHardware {
            wifi: peripherals.WIFI,
        },
        &spawner,
        display_signal,
    );

    // ---- TLS initialization ---------------------------------------------------

    // True Random Number Generator + mbedTLS singleton.
    // Stored in a StaticCell so the spawned tasks can hold a
    // `&'static Tls<'static>` reference.
    static TLS: StaticCell<mbedtls_rs::Tls<'static>> = StaticCell::new();
    let tls: &'static mbedtls_rs::Tls<'static> =
        TLS.init(networking::init_global_tls(networking::TlsHardware {
            rng: peripherals.RNG,
            adc1: peripherals.ADC1,
        }));

    // ---- Framebuffer + font renderer -------------------------------------------
    let fb = rendering::Framebuffer::new(480, 320);
    info!(
        "Framebuffer allocated — Heap used: {} bytes, free: {} bytes",
        esp_alloc::HEAP.used(),
        esp_alloc::HEAP.free()
    );

    // The font lives in a dedicated flash partition (flashed once with
    // `espflash write-bin`) so it doesn't bloat the app image.  We map it
    // into the CPU address space via the MMU at runtime.
    const FONT_PARTITION_OFFSET: u32 = 0xA0_0000; // must match partitions.csv
    const FONT_SIZE: usize = 5_369_728; // exact byte size of NotoSansJP-Medium.ttf
    let font_data = gramslator::flash_data::map_flash_region(FONT_PARTITION_OFFSET, FONT_SIZE);
    let renderer = rendering::FontRenderer::new(font_data);
    info!(
        "After font mapping — Heap used: {} bytes, free: {} bytes",
        esp_alloc::HEAP.used(),
        esp_alloc::HEAP.free()
    );

    // Translate signal — the Deepgram task signals this to request a translation.
    static TRANSLATE_SIGNAL: StaticCell<gramslator::translation::TranslateSignal> =
        StaticCell::new();
    let translate_signal = TRANSLATE_SIGNAL.init(gramslator::translation::TranslateSignal::new());

    // ---- Spawn tasks -----------------------------------------------------------

    // Translation task.
    spawner
        .spawn(gramslator::translation::translation_task(
            translate_signal,
            network,
            tls,
            display_signal,
        ))
        .expect("Failed to spawn translation task");

    // Deepgram streaming task (persistent, reconnects on failure).
    spawner
        .spawn(main_task::read_mic_and_send_loop_task(
            network,
            tls,
            translate_signal,
            display_signal,
        ))
        .expect("Failed to spawn Deepgram task");
    info!("Deepgram streaming task spawned");

    // Display task (renders transcript + translation on signal).
    spawner
        .spawn(rendering::display_task(
            hw_display,
            fb,
            renderer,
            display_signal,
        ))
        .expect("Failed to spawn display task");
    info!("Display task spawned");

    // Touch task (polls GT911, logs left/right zone presses).
    spawner
        .spawn(elecrow_board::touch::touch_task(i2c_touch))
        .expect("Failed to spawn touch task");
    info!("Touch task spawned");

    // Main task has nothing else to do — just idle.
    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
