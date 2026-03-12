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

use defmt::info;
use embassy_executor::Spawner;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::dma_circular_buffers;
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::system::Stack;
use esp_hal::timer::timg::TimerGroup;
use gramslator::elecrow_board;
use gramslator::net;
use tinyrlibc as _;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) {
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
        8_000,
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

    // ---- WiFi -----------------------------------------------------------------

    let network = elecrow_board::network::init(
        elecrow_board::network::NetworkHardware {
            wifi: peripherals.WIFI,
        },
        &spawner,
    );

    // ---- TLS initialization ---------------------------------------------------

    // True Random Number Generator + mbedTLS singleton
    let tls = net::init_global_tls(net::TlsHardware {
        rng: peripherals.RNG,
        adc1: peripherals.ADC1,
    });

    let mut conn = net::deepgram_create_listen_socket(network, &tls).await;

    elecrow_board::mic::send_audio_from_mic_pipe(&mut conn).await;
}
