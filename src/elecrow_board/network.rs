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

/// Process a single incoming WebSocket frame.
///
/// Returns `true` when the connection should be considered closed (server sent
/// a Close frame or an unrecoverable condition was encountered).
async fn handle_ws_frame(
    frame_type: edge_ws::FrameType,
    payload: &[u8],
    conn: &mut Connection<'_>,
    mask_key: u32,
    signal: &translate::TranslateSignal,
) -> bool {
    match frame_type {
        edge_ws::FrameType::Text(_) => {
            let json = core::str::from_utf8(payload).unwrap_or("<invalid UTF-8>");
            info!("Received: {}", json);
            signal.signal(translate::TranscriptMessage::DgJson(String::from(json)));
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

pub async fn test_stream(
    network: embassy_net::Stack<'static>,
    tls: &'static Tls<'static>,
    signal: &'static translate::TranslateSignal,
) {
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

    // ---- Stream audio & read responses (interleaved) -------------------------
    //
    // We cannot split the TLS connection into independent read/write halves, so
    // instead we interleave: after sending each audio chunk we attempt a brief
    // non-blocking recv to drain any partial transcripts Deepgram has ready.
    // This lets us forward results to the translation task while still streaming
    // audio rather than waiting until all audio is sent.

    let audio_data = &AUDIO_WAV[WAV_HEADER_SIZE..];
    let chunk_size = 2048;
    let mask_key: u32 = 0xDEAD_BEEF; // Fixed mask key for PoC

    info!(
        "Sending {} bytes of audio ({} chunks)...",
        audio_data.len(),
        audio_data.len().div_ceil(chunk_size)
    );

    let mut recv_buf = [0u8; 4096];
    let mut done = false;
    /// How long to poll for a response between audio chunks.
    const RECV_POLL: Duration = Duration::from_millis(5);

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

        // Drain any responses that arrived while we were sending.
        while !done {
            match embassy_time::with_timeout(RECV_POLL, edge_ws::io::recv(&mut conn, &mut recv_buf))
                .await
            {
                Err(_timeout) => break, // nothing ready right now — send next chunk
                Ok(Ok((frame_type, len))) => {
                    done =
                        handle_ws_frame(frame_type, &recv_buf[..len], &mut conn, mask_key, signal)
                            .await;
                }
                Ok(Err(e)) => {
                    info!("WebSocket recv error: {:?}", e);
                    done = true;
                }
            }
        }
    }
    info!("Audio sent! Keeping connection open for 5 seconds...");

    // ---- Read remaining responses for up to 5 seconds -------------------------
    //
    // Every text frame is forwarded to the translation task immediately via
    // a Signal.  The translation task handles idle-timeout debouncing so
    // that rapid partial transcripts don't flood Google Translate.

    let deadline = embassy_time::Instant::now() + Duration::from_secs(5);

    while !done && embassy_time::Instant::now() < deadline {
        let remaining = deadline - embassy_time::Instant::now();

        match embassy_time::with_timeout(remaining, edge_ws::io::recv(&mut conn, &mut recv_buf))
            .await
        {
            Err(_timeout) => {
                info!("5-second window elapsed.");
                break;
            }
            Ok(Ok((frame_type, len))) => {
                done = handle_ws_frame(frame_type, &recv_buf[..len], &mut conn, mask_key, signal)
                    .await;
            }
            Ok(Err(e)) => {
                info!("WebSocket recv error: {:?}", e);
                done = true;
            }
        }
    }

    // Tell the translation task to flush immediately — no more frames coming.
    signal.signal(translate::TranscriptMessage::Flush);

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
