#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use alloc::string::String;
use embassy_executor::Spawner;
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::StackResources;
use embassy_time::{Duration, Timer};
use embedded_io_async::Write as _;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::rng::{Trng, TrngSource};
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::{AuthMethod, ClientConfig, ModeConfig, WifiDevice};
use log::info;
use mbedtls_rs::{AuthMode, ClientSessionConfig, Session, SessionConfig, Tls};
use static_cell::StaticCell;
use tinyrlibc as _;

extern crate alloc;

const DEEPGRAM_HOST_CSTR: &core::ffi::CStr = match core::ffi::CStr::from_bytes_with_nul(
    concat!(env!("DEEPGRAM_HOST"), "\0").as_bytes(),
) {
    Ok(s) => s,
    Err(_) => panic!("DEEPGRAM_HOST contains an interior null byte"),
};

/// Raw WAV file baked into flash. The PCM data starts at byte 44 (standard WAV header).
const AUDIO_WAV: &[u8] = include_bytes!("assets/missile.wav");
const WAV_HEADER_SIZE: usize = 44;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();


#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, WifiDevice<'static>>) {
    runner.run().await
}

/// Find `\r\n\r\n` in a byte slice, returning the index of the first `\r`.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.2.0

    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // mbedTLS alone needs 40+ KB for session state, so we use regular SRAM.
    esp_alloc::heap_allocator!(size: 150_000);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    // True Random Number Generator — needs ADC1 as entropy source
    let _trng_source = TrngSource::new(peripherals.RNG, peripherals.ADC1);
    static TRNG: StaticCell<Trng> = StaticCell::new();
    let trng = TRNG.init(Trng::try_new().expect("TrngSource not active"));

    // Create mbedtls-rs TLS instance (singleton — needs &'static mut Trng)
    let mut tls = Tls::new(trng).expect("Failed to create TLS instance");
    tls.set_debug(1);

    info!("Embassy initialized!");

    // ---- WiFi -----------------------------------------------------------------

    static RADIO_CONTROLLER: StaticCell<esp_radio::Controller<'static>> = StaticCell::new();
    let radio_init = RADIO_CONTROLLER
        .init(esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller"));

    let (mut wifi_controller, interfaces) =
        esp_radio::wifi::new(radio_init, peripherals.WIFI, Default::default())
            .expect("Failed to initialize Wi-Fi controller");

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

    // ---- DNS ------------------------------------------------------------------

    info!("Resolving {}...", env!("DEEPGRAM_HOST"));
    let ip_addrs = stack
        .dns_query(env!("DEEPGRAM_HOST"), DnsQueryType::A)
        .await
        .expect("DNS resolution failed");
    let remote_ip = ip_addrs[0];
    info!("Resolved {} → {}", env!("DEEPGRAM_HOST"), remote_ip);

    // ---- TCP ------------------------------------------------------------------

    static TCP_RX_BUF: StaticCell<[u8; 16384]> = StaticCell::new();
    static TCP_TX_BUF: StaticCell<[u8; 4096]> = StaticCell::new();
    let tcp_rx_buf = TCP_RX_BUF.init([0u8; 16384]);
    let tcp_tx_buf = TCP_TX_BUF.init([0u8; 4096]);

    let mut tcp_socket = TcpSocket::new(stack, tcp_rx_buf, tcp_tx_buf);
    tcp_socket.set_timeout(Some(Duration::from_secs(30)));

    let remote_endpoint = (remote_ip, 443);
    info!("TCP connecting to {}:443...", remote_ip);

    const MAX_TCP_RETRIES: usize = 5;
    for attempt in 1..=MAX_TCP_RETRIES {
        match tcp_socket.connect(remote_endpoint).await {
            Ok(()) => {
                info!("TCP connected!");
                break;
            }
            Err(e) => {
                info!(
                    "TCP connect attempt {}/{} failed: {:?}",
                    attempt, MAX_TCP_RETRIES, e
                );
                if attempt == MAX_TCP_RETRIES {
                    panic!("TCP connect failed after {} attempts", MAX_TCP_RETRIES);
                }
                // Close and re-create the socket before retrying
                tcp_socket.close();
                Timer::after(Duration::from_secs(1)).await;
                tcp_socket.abort();
            }
        }
    }

    // ---- TLS ------------------------------------------------------------------

    let tls_config = SessionConfig::Client(ClientSessionConfig {
        // Skip certificate verification for this PoC
        // (production should use a CA chain and AuthMode::Required)
        auth_mode: AuthMode::None,
        server_name: Some(DEEPGRAM_HOST_CSTR),
        min_version: mbedtls_rs::TlsVersion::Tls1_2,
        ..ClientSessionConfig::new()
    });

    let mut session = Session::new(tls.reference(), tcp_socket, &tls_config)
        .expect("Failed to create TLS session");

    info!("TLS handshake...");
    if let Err(e) = session.connect().await {
        panic!("TLS handshake failed: {:?}", e);
    }
    info!("TLS established!");

    // ---- WebSocket upgrade ----------------------------------------------------

    // Build the HTTP upgrade request. All parts are known at compile time except
    // the concatenation, which we do into a small stack buffer.
    info!("WebSocket upgrade...");

    // Use a fixed Sec-WebSocket-Key (fine for a proof of concept)
    let upgrade_request: &[u8] = concat!(
        "GET /v2/listen?eot_threshold=0.7&eot_timeout_ms=5000&model=flux-general-en&encoding=linear16&sample_rate=8000 HTTP/1.1\r\n",
        "Host: ", env!("DEEPGRAM_HOST"), "\r\n",
        "Upgrade: websocket\r\n",
        "Connection: Upgrade\r\n",
        "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n",
        "Sec-WebSocket-Version: 13\r\n",
        "Authorization: Token ", env!("DEEPGRAM_TOKEN"), "\r\n",
        "\r\n",
    ).as_bytes();

    session.write_all(upgrade_request).await.expect("Failed to send upgrade request");
    session.flush().await.expect("Failed to flush upgrade request");

    // Read the HTTP 101 response
    let mut http_buf = [0u8; 1024];
    let mut http_len = 0;
    loop {
        let n = session.read(&mut http_buf[http_len..]).await.expect("Failed reading HTTP response");
        if n == 0 {
            panic!("Connection closed during WebSocket upgrade");
        }
        http_len += n;

        if let Some(end) = find_header_end(&http_buf[..http_len]) {
            let status_line_end = http_buf[..end]
                .windows(2)
                .position(|w| w == b"\r\n")
                .unwrap_or(end);
            let status_line =
                core::str::from_utf8(&http_buf[..status_line_end]).unwrap_or("<invalid UTF-8>");
            info!("HTTP response: {}", status_line);

            if !status_line.contains("101") {
                panic!("WebSocket upgrade failed: {}", status_line);
            }
            break;
        }

        if http_len >= http_buf.len() {
            panic!("HTTP response headers too large");
        }
    }
    info!("WebSocket connected!");

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
            &mut session,
            edge_ws::FrameType::Binary(false),
            Some(mask_key),
            chunk,
        )
        .await
        .expect("Failed to send audio chunk");
        session.flush().await.expect("Failed to flush audio chunk");

        if i % 10 == 0 {
            info!("  Sent chunk {}", i);
        }
    }
    info!("Audio sent! Keeping connection open for 10 seconds...");

    // ---- Read responses for 10 seconds ----------------------------------------

    let deadline = embassy_time::Instant::now() + Duration::from_secs(10);
    let mut recv_buf = [0u8; 4096];
    let mut done = false;

    while !done && embassy_time::Instant::now() < deadline {
        let remaining = deadline - embassy_time::Instant::now();

        match embassy_time::with_timeout(remaining, edge_ws::io::recv(&mut session, &mut recv_buf))
            .await
        {
            Err(_timeout) => {
                info!("10-second window elapsed.");
                break;
            }
            Ok(Ok((frame_type, len))) => match frame_type {
                edge_ws::FrameType::Text(_) => {
                    let text = core::str::from_utf8(&recv_buf[..len]).unwrap_or("<invalid UTF-8>");
                    info!("Received: {}", text);
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
                        &mut session,
                        edge_ws::FrameType::Pong,
                        Some(mask_key),
                        &recv_buf[..len],
                    )
                    .await;
                    let _ = session.flush().await;
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

    // Signal end of audio stream
    if !done {
        let close_stream = b"{\"type\":\"CloseStream\"}";
        edge_ws::io::send(
            &mut session,
            edge_ws::FrameType::Text(false),
            Some(mask_key),
            close_stream,
        )
        .await
        .expect("Failed to send CloseStream");
        session.flush().await.expect("Failed to flush CloseStream");
        info!("Sent CloseStream");
    }

    info!("Done! Deepgram streaming complete.");

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.0.0/examples
}
