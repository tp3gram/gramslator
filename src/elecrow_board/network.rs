use alloc::string::String;

use crate::app_state::{self, DisplaySignal};
use crate::elecrow_board::mic::MIC_PIPE;
use crate::networking::{self as net, Connection};
use crate::translation as translate;
use defmt::info;
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_time::{Duration, Timer};
use embedded_io_async::Write as _;
use esp_hal::peripherals::WIFI;
use esp_radio::wifi::{AuthMethod, ClientConfig, ModeConfig, WifiController, WifiDevice};
use mbedtls_rs::Tls;
use static_cell::StaticCell;

extern crate alloc;

const WIFI_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DHCP_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_WIFI_RETRIES: usize = 5;
const RETRY_DELAY: Duration = Duration::from_secs(2);

/// Delay before reconnecting to Deepgram after a connection error or stream
/// completion.
const RECONNECT_DELAY: Duration = Duration::from_secs(3);

pub struct NetworkHardware {
    pub wifi: WIFI<'static>,
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, WifiDevice<'static>>) {
    runner.run().await
}

/// Drives WiFi start, association, and DHCP to completion in the background.
///
/// Association and DHCP are retried up to [`MAX_WIFI_RETRIES`] times with
/// timeouts so the device doesn't hang forever when the network is flaky.
#[embassy_executor::task]
async fn wifi_connect_task(
    wifi_controller: &'static mut WifiController<'static>,
    stack: embassy_net::Stack<'static>,
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

    panic!(
        "Failed to connect to WiFi after {} attempts",
        MAX_WIFI_RETRIES
    );
}

/// Initializes Wi-Fi hardware and returns the network stack immediately.
///
/// The actual connection (start, associate, DHCP) happens in a background
/// Embassy task. Operations on the returned `Stack` will block until the
/// network is ready, so callers don't need to poll for readiness — they
/// can proceed with other initialization and the first network call will
/// naturally wait.
pub fn init(hardware: NetworkHardware, spawner: &Spawner) -> embassy_net::Stack<'static> {
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
        .spawn(wifi_connect_task(wifi_controller, stack))
        .expect("Failed to spawn WiFi connect task");

    stack
}

// ---------------------------------------------------------------------------
// WebSocket frame handler
// ---------------------------------------------------------------------------

/// Process a single incoming WebSocket frame.
///
/// Returns `true` when the connection should be considered closed (server sent
/// a Close frame or an unrecoverable condition was encountered).
async fn handle_ws_frame(
    frame_type: edge_ws::FrameType,
    payload: &[u8],
    conn: &mut Connection<'_>,
    mask_key: u32,
    translate_signal: &translate::TranslateSignal,
    display_signal: &DisplaySignal,
) -> bool {
    match frame_type {
        edge_ws::FrameType::Text(_) => {
            let json = core::str::from_utf8(payload).unwrap_or("<invalid UTF-8>");
            info!("Received: {}", json);

            // Extract the transcript and update shared state.
            if let Some(transcript) = translate::extract_transcript(json) {
                let changed = app_state::update_transcript(transcript);
                // Always wake the display so it shows the latest partial.
                display_signal.signal(());

                if changed {
                    // Forward to translation task only on unique transcripts.
                    translate_signal
                        .signal(translate::TranscriptMessage::DgJson(String::from(json)));
                }
            }
            false
        }
        edge_ws::FrameType::Binary(_) => {
            info!("Received binary frame ({} bytes)", payload.len());
            false
        }
        edge_ws::FrameType::Close => {
            info!("WebSocket closed by server.");
            true
        }
        edge_ws::FrameType::Ping => {
            info!("Ping received, sending pong");
            let _ = edge_ws::io::send(
                &mut *conn,
                edge_ws::FrameType::Pong,
                Some(mask_key),
                payload,
            )
            .await;
            let _ = conn.flush().await;
            false
        }
        other => {
            info!("Received {:?} frame ({} bytes)", other, payload.len());
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Persistent Deepgram streaming task
// ---------------------------------------------------------------------------

/// Raw WAV file baked into flash.  The PCM data starts at byte 44
/// (standard WAV header).
// const AUDIO_WAV: &[u8] = include_bytes!("../bin/assets/missile.wav");
// const WAV_HEADER_SIZE: usize = 44;

/// Persistent Embassy task that maintains a WebSocket connection to Deepgram,
/// streams example audio, and publishes received transcripts to the shared
/// [`AppState`](crate::app_state) and the translation signal.
///
/// On connection failure or stream completion the task waits briefly and
/// reconnects, running indefinitely.
#[embassy_executor::task]
pub async fn deepgram_task(
    network: embassy_net::Stack<'static>,
    tls: &'static Tls<'static>,
    translate_signal: &'static translate::TranslateSignal,
    display_signal: &'static DisplaySignal,
) {
    // Wait for WiFi + DHCP before attempting any network I/O.
    network.wait_config_up().await;
    info!("Deepgram task: network is up, starting streaming loop");

    loop {
        // ---- Connect to Deepgram ----------------------------------------
        let mut conn = match net::deepgram_create_listen_socket(network, tls).await {
            Ok(c) => c,
            Err(e) => {
                info!("Deepgram connect failed: {:?}, retrying...", e);
                Timer::after(RECONNECT_DELAY).await;
                continue;
            }
        };

        // ---- Stream audio & read responses (interleaved) ----------------
        //
        // We cannot split the TLS connection into independent read/write
        // halves, so instead we interleave: after sending each audio chunk
        // we attempt a brief non-blocking recv to drain any partial
        // transcripts Deepgram has ready.

        let mask_key: u32 = 0xDEAD_BEEF; // Fixed mask key for PoC

        let mut mic_read_buf = [0u8; 8000];
        let mut recv_buf = [0u8; 4096];
        let mut done = false;
        /// How long to poll for a response between audio chunks.
        const RECV_POLL: Duration = Duration::from_millis(5);

        info!("Starting microphone streaming...");

        loop {
            if done {
                break;
            }

            let n = MIC_PIPE.read(&mut mic_read_buf[..]).await;
            info!("Mic pipe read {} bytes", n);

            // Drop duplicate channel: Data16Channel16 outputs each mono PDM
            // sample twice (L and R slots identical). Keep every other 16-bit
            // sample to produce true mono: [S0,S0,S1,S1,...] → [S0,S1,...]
            let mono_len = n / 2;
            let mut j = 0;
            for i in (0..n).step_by(4) {
                if i + 1 < n {
                    mic_read_buf[j] = mic_read_buf[i];
                    mic_read_buf[j + 1] = mic_read_buf[i + 1];
                    j += 2;
                }
            }

            if let Err(e) = edge_ws::io::send(
                &mut conn,
                edge_ws::FrameType::Binary(false),
                Some(mask_key),
                &mic_read_buf[..mono_len],
            )
            .await
            {
                info!("Failed to send audio chunk: {:?}", e);
                done = true;
                break;
            }
            if let Err(e) = conn.flush().await {
                info!("Failed to flush audio chunk: {:?}", e);
                done = true;
                break;
            }

            // Drain any responses that arrived while we were sending.
            while !done {
                match embassy_time::with_timeout(
                    RECV_POLL,
                    edge_ws::io::recv(&mut conn, &mut recv_buf),
                )
                .await
                {
                    Err(_timeout) => break, // nothing ready — send next chunk
                    Ok(Ok((frame_type, len))) => {
                        done = handle_ws_frame(
                            frame_type,
                            &recv_buf[..len],
                            &mut conn,
                            mask_key,
                            translate_signal,
                            display_signal,
                        )
                        .await;
                    }
                    Ok(Err(e)) => {
                        info!("WebSocket recv error: {:?}", e);
                        done = true;
                    }
                }
            }
        }
        // Flush translation so partial transcripts aren't stuck.
        translate_signal.signal(translate::TranscriptMessage::Flush);

        conn.close().await;
        info!(
            "Deepgram connection lost. Reconnecting in {} s...",
            RECONNECT_DELAY.as_secs()
        );
        Timer::after(RECONNECT_DELAY).await;
    }
}
