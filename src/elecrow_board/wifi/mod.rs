extern crate alloc;

mod wifi_connect_task;
mod wifi_status_task;

use alloc::string::String;

use embassy_executor::Spawner;
use embassy_net::StackResources;
use esp_hal::peripherals::WIFI;
use esp_radio::wifi::{AuthMethod, ClientConfig, ModeConfig, WifiController, WifiDevice};
use static_cell::StaticCell;

use crate::app_state::DisplaySignal;
use wifi_connect_task::wifi_connect_task;
use wifi_status_task::wifi_status_task;

pub struct NetworkHardware {
    pub wifi: WIFI<'static>,
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, WifiDevice<'static>>) {
    runner.run().await
}

/// Initializes Wi-Fi hardware and returns the network stack immediately.
///
/// The actual connection (start, associate, DHCP) happens in a background
/// Embassy task. Operations on the returned `Stack` will block until the
/// network is ready, so callers don't need to poll for readiness — they
/// can proceed with other initialization and the first network call will
/// naturally wait.
pub fn init(
    hardware: NetworkHardware,
    spawner: &Spawner,
    display_signal: &'static DisplaySignal,
) -> embassy_net::Stack<'static> {
    static RADIO_CONTROLLER: StaticCell<esp_radio::Controller<'static>> = StaticCell::new();
    let radio_init = RADIO_CONTROLLER
        .init(esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller"));

    static WIFI_CONTROLLER: StaticCell<WifiController<'static>> = StaticCell::new();
    let (wifi_controller, interfaces) =
        esp_radio::wifi::new(radio_init, hardware.wifi, Default::default())
            .expect("Failed to initialize Wi-Fi controller");
    let wifi_controller = WIFI_CONTROLLER.init(wifi_controller);

    let client_config = ClientConfig::default()
        .with_ssid(String::from(env!("WIFI_SSID")))
        .with_password(String::from(env!("WIFI_PASSWORD")))
        .with_auth_method(AuthMethod::WpaWpa2Personal);

    wifi_controller
        .set_config(&ModeConfig::Client(client_config))
        .expect("Failed to set WiFi configuration");

    let net_config = embassy_net::Config::dhcpv4(Default::default());

    static RESOURCES: StaticCell<StackResources<5>> = StaticCell::new();
    let resources = RESOURCES.init(StackResources::new());

    let seed = 1234; // TODO: use hardware RNG for a proper random seed
    let (stack, runner) = embassy_net::new(interfaces.sta, net_config, resources, seed);

    spawner
        .spawn(net_task(runner))
        .expect("Failed to spawn net task");

    spawner
        .spawn(wifi_connect_task(wifi_controller, stack, display_signal))
        .expect("Failed to spawn WiFi connect task");

    spawner
        .spawn(wifi_status_task(stack, display_signal))
        .expect("Failed to spawn WiFi status task");

    stack
}
