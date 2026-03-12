use alloc::string::String;

use crate::net::{self, Connection};
use crate::translate;
use defmt::info;
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_time::Duration;
use embedded_io_async::Write as _;
use esp_hal::peripherals::WIFI;
use esp_radio::wifi::{AuthMethod, ClientConfig, ModeConfig, WifiController, WifiDevice};
use mbedtls_rs::Tls;
use static_cell::StaticCell;

extern crate alloc;

pub struct NetworkHardware {
    pub wifi: WIFI<'static>,
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, WifiDevice<'static>>) {
    runner.run().await
}

/// Drives WiFi start, association, and DHCP to completion in the background.
#[embassy_executor::task]
async fn wifi_connect_task(
    wifi_controller: &'static mut WifiController<'static>,
    stack: embassy_net::Stack<'static>,
) {
    info!("Starting WiFi...");
    wifi_controller
        .start_async()
        .await
        .expect("Failed to start WiFi");
    info!("WiFi started!");

    info!("Connecting to '{}'...", env!("WIFI_SSID"));
    wifi_controller
        .connect_async()
        .await
        .expect("Failed to connect to WiFi");
    info!("WiFi connected!");

    info!("Waiting for DHCP...");
    stack.wait_config_up().await;
    info!("DHCP configured!");

    if let Some(config) = stack.config_v4() {
        info!("Got IP: {}", config.address);
    }
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

    // Wait for WiFi + DHCP before attempting any network I/O.
    network.wait_config_up().await;

    // ---- Deepgram connection ----------------------------------------------------
    //
    // DEEPGRAM_USE_TLS: "false" disables TLS (HTTP), defaults to "true" (HTTPS).
    // DEEPGRAM_PORT:    override the port, defaults to 443.
    const DEEPGRAM_USE_TLS: bool = konst::result::unwrap_ctx!(konst::primitive::parse_bool(
        match option_env!("DEEPGRAM_USE_TLS") {
            Some(v) => v,
            None => "true",
        }
    ));

    const DEEPGRAM_PORT: u16 = konst::result::unwrap_ctx!(konst::primitive::parse_u16(
        match option_env!("DEEPGRAM_PORT") {
            Some(v) => v,
            None => "443",
        }
    ));

    let mut conn = if DEEPGRAM_USE_TLS {
        info!(
            "Connecting to Deepgram over HTTPS (port {})...",
            DEEPGRAM_PORT
        );
        Connection::init_tls(network, env!("DEEPGRAM_HOST"), DEEPGRAM_PORT, tls)
            .await
            .expect("Failed to establish TLS connection")
    } else {
        info!(
            "Connecting to Deepgram over HTTP (port {})...",
            DEEPGRAM_PORT
        );
        Connection::init_tcp(network, env!("DEEPGRAM_HOST"), DEEPGRAM_PORT)
            .await
            .expect("Failed to establish TCP connection")
    };

    // ---- WebSocket upgrade ----------------------------------------------------

    net::websocket_upgrade(&mut conn).await;

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

    let mut conn = match Connection::init_tls(stack, env!("GOOGLE_TRANSLATE_HOST"), 443, tls).await
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
