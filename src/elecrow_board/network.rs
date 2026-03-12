use alloc::string::String;

use crate::net::{self, Connection};
use crate::translate;
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

pub async fn test_stream(network: embassy_net::Stack<'static>, tls: &Tls<'static>) {
    /// Raw WAV file baked into flash. The PCM data starts at byte 44 (standard WAV header).
    const AUDIO_WAV: &[u8] = include_bytes!("../bin/assets/missile.wav");
    const WAV_HEADER_SIZE: usize = 44;

    let mut conn = net::deepgram_create_listen_socket(network, tls).await;

    // ---- Stream audio ---------------------------------------------------------

    let audio_data = &AUDIO_WAV[WAV_HEADER_SIZE..];
    let chunk_size = 2048;
    let mask_key: u32 = 0xDEAD_BEEF; // Fixed mask key for PoC

    info!(
        "Sending {} bytes of audio ({} chunks)...",
        audio_data.len(),
        audio_data.len().div_ceil(chunk_size)
    );

    for (i, chunk) in audio_data.chunks(chunk_size).enumerate() {
        edge_ws::io::send(
            &mut conn,
            edge_ws::FrameType::Binary(false),
            Some(mask_key),
            chunk,
        )
        .await
        .expect("Failed to send audio chunk");
        conn.flush().await.expect("Failed to flush audio chunk");

        if i % 10 == 0 {
            info!("  Sent chunk {}", i);
        }
    }
    info!("Audio sent! Keeping connection open for 10 seconds...");

    // ---- Read responses for 10 seconds ----------------------------------------
    //
    // Deepgram sends many partial transcript frames in rapid succession.
    // Rather than translating every frame, we buffer the latest transcript
    // JSON and only translate once Deepgram goes idle for
    // `TRANSLATE_IDLE_TIMEOUT`. This "trailing edge" approach ensures:
    //   - We never flood Google Translate with redundant partial requests.
    //   - The most recent transcript is always translated once speech pauses.

    /// How long to wait without receiving a new text frame before translating
    /// the most recently buffered transcript.
    const TRANSLATE_IDLE_TIMEOUT: Duration = Duration::from_secs(1);

    let deadline = embassy_time::Instant::now() + Duration::from_secs(10);
    let mut recv_buf = [0u8; 4096];
    let mut done = false;

    // Buffer for the most recent transcript JSON awaiting translation.
    // When the idle timer fires we translate this and clear it.
    let mut pending_json: Option<String> = None;

    while !done && embassy_time::Instant::now() < deadline {
        // Pick the shorter of the two timeouts: overall deadline, or idle
        // timer (if we have a buffered transcript waiting for a lull).
        let remaining = deadline - embassy_time::Instant::now();
        let timeout = if pending_json.is_some() {
            remaining.min(TRANSLATE_IDLE_TIMEOUT)
        } else {
            remaining
        };

        match embassy_time::with_timeout(timeout, edge_ws::io::recv(&mut conn, &mut recv_buf)).await
        {
            Err(_timeout) => {
                // No frame arrived before the timeout.
                // If we have a pending transcript, translate it now.
                if let Some(json) = pending_json.take() {
                    info!("Idle timeout — translating buffered transcript");
                    translate_response(network, tls, &json).await;
                }

                // If the overall deadline has also elapsed, break out.
                if embassy_time::Instant::now() >= deadline {
                    info!("10-second window elapsed.");
                    break;
                }
            }
            Ok(Ok((frame_type, len))) => match frame_type {
                edge_ws::FrameType::Text(_) => {
                    let json = core::str::from_utf8(&recv_buf[..len]).unwrap_or("<invalid UTF-8>");
                    info!("Received: {}", json);
                    // Buffer the latest transcript; it overwrites any previous
                    // pending frame so only the most recent partial gets
                    // translated once Deepgram goes idle.
                    pending_json = Some(String::from(json));
                }
                edge_ws::FrameType::Binary(_) => {
                    info!("Received binary frame ({} bytes)", len);
                }
                edge_ws::FrameType::Close => {
                    info!("WebSocket closed by server.");
                    done = true;
                }
                edge_ws::FrameType::Ping => {
                    info!("Ping received, sending pong");
                    let _ = edge_ws::io::send(
                        &mut conn,
                        edge_ws::FrameType::Pong,
                        Some(mask_key),
                        &recv_buf[..len],
                    )
                    .await;
                    let _ = conn.flush().await;
                }
                other => {
                    info!("Received {:?} frame ({} bytes)", other, len);
                }
            },
            Ok(Err(e)) => {
                info!("WebSocket recv error: {:?}", e);
                done = true;
            }
        }
    }

    // Translate any remaining buffered transcript before closing.
    if let Some(json) = pending_json.take() {
        info!("Translating final buffered transcript before close");
        translate_response(network, tls, &json).await;
    }

    // Signal end of audio stream
    if !done {
        let close_stream = b"{\"type\":\"CloseStream\"}";
        edge_ws::io::send(
            &mut conn,
            edge_ws::FrameType::Text(false),
            Some(mask_key),
            close_stream,
        )
        .await
        .expect("Failed to send CloseStream");
        conn.flush().await.expect("Failed to flush CloseStream");
        info!("Sent CloseStream");
    }

    info!("Done! Deepgram streaming complete.");
}

/// Extract a Deepgram transcript from `json`, translate it (en -> es) via
/// Google Translate, and cache the result. Skips the TLS round-trip on cache
/// hits.
async fn translate_response(stack: embassy_net::Stack<'static>, tls: &Tls<'_>, json: &str) {
    let Some(transcript) = translate::extract_transcript(json) else {
        info!("No transcript field found in response");
        return;
    };

    // Check cache — return early on hit.
    if let Some(result) = translate::check_translation_cache(transcript) {
        info!("Translation cache hit: \"{}\"", result.as_str());
        return;
    }

    let mut conn = match Connection::open_tcp_connection_with_tls(
        stack,
        env!("GOOGLE_TRANSLATE_HOST"),
        443,
        tls,
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            info!("Failed to connect to Google Translate: {:?}", e);
            return;
        }
    };

    translate::translate_response(&mut conn, transcript).await;

    // Close the connection cleanly so that PSA crypto resources are released
    // before the Session is dropped.
    conn.close().await;
}
