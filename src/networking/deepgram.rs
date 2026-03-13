use const_format::concatcp;
use defmt::info;
use embedded_io_async::{Read, Write};
use mbedtls_rs::Tls;

use super::connection::{Connection, ConnectionError};
use super::find_header_end;

static DEEPGRAM_LISTEN_WEBSOCKET_REQUEST: &str = concatcp!(
    "GET /v2/listen?eot_threshold=0.7&eot_timeout_ms=5000&model=flux-general-en&encoding=linear16&sample_rate=",
    crate::SAMPLE_RATE,
    " HTTP/1.1\r\n",
    "Host: ",
    env!("DEEPGRAM_HOST"),
    "\r\n",
    "Upgrade: websocket\r\n",
    "Connection: Upgrade\r\n",
    "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n",
    "Sec-WebSocket-Version: 13\r\n",
    "Authorization: Token ",
    env!("DEEPGRAM_TOKEN"),
    "\r\n",
    "\r\n",
);

pub(crate) async fn deepgram_tcp_connect<'a>(
    network: embassy_net::Stack<'static>,
    tls: &'a Tls<'static>,
) -> Result<Connection<'a>, ConnectionError> {
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

    if DEEPGRAM_USE_TLS {
        info!(
            "Connecting to Deepgram over HTTPS (port {})...",
            DEEPGRAM_PORT
        );
        Connection::open_tcp_connection_with_tls(network, env!("DEEPGRAM_HOST"), DEEPGRAM_PORT, tls)
            .await
    } else {
        info!(
            "Connecting to Deepgram over HTTP (port {})...",
            DEEPGRAM_PORT
        );
        Connection::open_tcp_connection(network, env!("DEEPGRAM_HOST"), DEEPGRAM_PORT).await
    }
}

/// Perform the HTTP -> WebSocket upgrade handshake over an established
/// connection (typically TLS). Panics on failure.
pub async fn deepgram_listen_socket_upgrade<S>(session: &mut S)
where
    S: Read + Write,
    S::Error: core::fmt::Debug,
{
    info!("WebSocket upgrade...");

    // Use a fixed Sec-WebSocket-Key (fine for a proof of concept)
    let upgrade_request: &[u8] = DEEPGRAM_LISTEN_WEBSOCKET_REQUEST.as_bytes();

    session
        .write_all(upgrade_request)
        .await
        .expect("Failed to send upgrade request");
    session
        .flush()
        .await
        .expect("Failed to flush upgrade request");

    // Read the HTTP 101 response
    let mut http_buf = [0u8; 1024];
    let mut http_len = 0;
    loop {
        let n = session
            .read(&mut http_buf[http_len..])
            .await
            .expect("Failed reading HTTP response");
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
}

pub async fn deepgram_create_listen_socket<'a>(
    network: embassy_net::Stack<'static>,
    tls: &'a Tls<'static>,
) -> Result<Connection<'a>, ConnectionError> {
    // Wait for WiFi + DHCP before attempting any network I/O.
    network.wait_config_up().await;

    let mut conn: Connection<'_> = deepgram_tcp_connect(network, tls).await?;

    deepgram_listen_socket_upgrade(&mut conn).await;

    Ok(conn)
}
