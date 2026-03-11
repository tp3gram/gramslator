#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use esp_hal::clock::CpuClock;
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;

use esp_println as _;
use defmt::info;

use embassy_executor::Spawner;
use embassy_net::{Runner, StackResources};
use embassy_time::{Duration, Timer};

use esp_radio::wifi::{ClientConfig, ModeConfig, WifiController, WifiDevice, WifiEvent};

use static_cell::StaticCell;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

const SSID: &str = env!("SSID");
const PASSWORD: &str = env!("PASSWORD");

macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static STATIC_CELL: StaticCell<$t> = StaticCell::new();
        STATIC_CELL.uninit().write($val)
    }};
}

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    info!("Embassy initialized!");

    // Initialize the radio controller.
    // The controller must live for 'static since spawned tasks borrow from it.
    let radio_init = mk_static!(
        esp_radio::Controller<'static>,
        esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller")
    );
    let (wifi_controller, interfaces) =
        esp_radio::wifi::new(radio_init, peripherals.WIFI, Default::default())
            .expect("Failed to initialize Wi-Fi controller");
    let sta_device = interfaces.sta;

    // Configure embassy-net with DHCPv4.
    let net_config = embassy_net::Config::dhcpv4(Default::default());

    let seed = {
        let rng = Rng::new();
        (rng.random() as u64) << 32 | rng.random() as u64
    };

    let (stack, runner) = embassy_net::new(
        sta_device,
        net_config,
        mk_static!(StackResources<5>, StackResources::<5>::new()),
        seed,
    );

    // Spawn background tasks for WiFi connection management and network stack.
    spawner.spawn(connection(wifi_controller)).unwrap();
    spawner.spawn(net_task(runner)).unwrap();

    // Wait for the WiFi link to come up.
    info!("Waiting for link...");
    loop {
        if stack.is_link_up() {
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    // Wait for a DHCP lease.
    info!("Waiting for DHCP...");
    loop {
        if let Some(config) = stack.config_v4() {
            info!("Got IP: {}", config.address);
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    loop {
        info!("Done. Sleeping...");
        Timer::after(Duration::from_secs(60)).await;
    }
}

/// Manages the WiFi connection lifecycle: configure, start, connect, and
/// automatically reconnect on disconnection.
#[allow(
    clippy::large_stack_frames,
    reason = "WifiController's async state is inherently large"
)]
#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    info!("WiFi: configuring as station...");
    let client_config = ModeConfig::Client(
        ClientConfig::default()
            .with_ssid(SSID.into())
            .with_password(PASSWORD.into()),
    );
    controller.set_config(&client_config).unwrap();

    info!("WiFi: starting...");
    controller.start_async().await.unwrap();
    info!("WiFi: started!");

    loop {
        match controller.connect_async().await {
            Ok(()) => info!("WiFi: connected!"),
            Err(e) => {
                info!("WiFi: connect failed: {}", e);
                Timer::after(Duration::from_secs(5)).await;
                continue;
            }
        }

        // Stay here while connected; returns when we disconnect.
        controller.wait_for_event(WifiEvent::StaDisconnected).await;
        info!("WiFi: disconnected, reconnecting in 5s...");
        Timer::after(Duration::from_secs(5)).await;
    }
}

/// Runs the embassy-net network stack (packet processing).
#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, WifiDevice<'static>>) {
    runner.run().await
}
