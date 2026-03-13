use defmt::info;
use embassy_time::{Duration, Timer};
use esp_radio::wifi::WifiController;

use crate::app_state::{self, DisplaySignal, ServiceStatus};

const WIFI_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DHCP_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_WIFI_RETRIES: usize = 5;
const RETRY_DELAY: Duration = Duration::from_secs(2);

/// Drives WiFi start, association, and DHCP to completion in the background.
///
/// Association and DHCP are retried up to [`MAX_WIFI_RETRIES`] times with
/// timeouts so the device doesn't hang forever when the network is flaky.
#[embassy_executor::task]
pub(super) async fn wifi_connect_task(
    wifi_controller: &'static mut WifiController<'static>,
    stack: embassy_net::Stack<'static>,
    display_signal: &'static DisplaySignal,
) {
    // Starting the radio is a one-time operation — if this fails the hardware
    // is broken and there's nothing to retry.
    info!("Starting WiFi...");
    wifi_controller
        .start_async()
        .await
        .expect("Failed to start WiFi");
    info!("WiFi started!");

    for attempt in 1..=MAX_WIFI_RETRIES {
        // --- Associate with AP ---
        info!(
            "Connecting to '{}' (attempt {}/{})...",
            env!("WIFI_SSID"),
            attempt,
            MAX_WIFI_RETRIES
        );

        let connect_result =
            embassy_time::with_timeout(WIFI_CONNECT_TIMEOUT, wifi_controller.connect_async()).await;

        match connect_result {
            Ok(Ok(())) => {
                info!("WiFi connected!");
            }
            Ok(Err(e)) => {
                info!(
                    "WiFi connect error: {:?}, retrying in {} s...",
                    e,
                    RETRY_DELAY.as_secs()
                );
                Timer::after(RETRY_DELAY).await;
                continue;
            }
            Err(_timeout) => {
                info!(
                    "WiFi connect timed out, retrying in {} s...",
                    RETRY_DELAY.as_secs()
                );
                // Abort the pending connection so the driver resets cleanly.
                let _ = wifi_controller.disconnect_async().await;
                Timer::after(RETRY_DELAY).await;
                continue;
            }
        }

        // --- Wait for DHCP ---
        info!("Waiting for DHCP...");
        match embassy_time::with_timeout(DHCP_TIMEOUT, stack.wait_config_up()).await {
            Ok(()) => {
                info!("DHCP configured!");
                if let Some(config) = stack.config_v4() {
                    info!("Got IP: {}", config.address);
                }
                return; // Success!
            }
            Err(_timeout) => {
                info!(
                    "DHCP timed out (attempt {}/{}), disconnecting and retrying...",
                    attempt, MAX_WIFI_RETRIES
                );
                let _ = wifi_controller.disconnect_async().await;
                Timer::after(RETRY_DELAY).await;
            }
        }
    }

    defmt::error!(
        "Failed to connect to WiFi after {} attempts",
        MAX_WIFI_RETRIES
    );
    if app_state::update_wifi_status(ServiceStatus::Error) {
        display_signal.signal(());
    }
}
