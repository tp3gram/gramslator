use defmt::info;

use crate::app_state::{self, DisplaySignal, ServiceStatus};

/// Background task that monitors WiFi link and DHCP state, updating the
/// shared [`ServiceStatus`] so the display can show connection progress.
///
/// Uses `embassy_net::Stack` async waiters — zero CPU when idle.
#[embassy_executor::task]
pub(super) async fn wifi_status_task(
    stack: embassy_net::Stack<'static>,
    display_signal: &'static DisplaySignal,
) {
    // Signal initial "Connecting" — WiFi association is already in progress
    // by the time this task starts.
    if app_state::update_wifi_status(ServiceStatus::Connecting) {
        display_signal.signal(());
    }

    loop {
        // Wait for the WiFi link (L2) to come up.
        stack.wait_link_up().await;
        info!("WiFi status: link up, waiting for DHCP...");
        if app_state::update_wifi_status(ServiceStatus::Connecting) {
            display_signal.signal(());
        }

        // Wait for DHCP / IP configuration.
        stack.wait_config_up().await;
        info!("WiFi status: connected (IP configured)");
        if app_state::update_wifi_status(ServiceStatus::Connected) {
            display_signal.signal(());
        }

        // Wait for the link to drop.
        stack.wait_link_down().await;
        info!("WiFi status: link down");
        if app_state::update_wifi_status(ServiceStatus::Error) {
            display_signal.signal(());
        }
    }
}
