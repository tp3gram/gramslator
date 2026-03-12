use core::ops::{Deref, DerefMut};

use alloc::boxed::Box;
use alloc::ffi::CString;

use defmt::info;
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::{ConnectError, TcpSocket};
use embassy_time::{Duration, Timer};
use embedded_io_async::{Read, Write};
use esp_hal::peripherals::{ADC1, RNG};
use esp_hal::rng::{Trng, TrngSource};
use mbedtls_rs::{
    AuthMode, ClientSessionConfig, Session, SessionConfig, SessionError, Tls, TlsVersion,
};
use smoltcp::wire::IpAddress;
use static_cell::StaticCell;

extern crate alloc;

// ---- Buffer pool for concurrent TCP connections ----------------------------

pub const MAX_CONNECTIONS: usize = 4;
const TCP_RX_SIZE: usize = 16384;
const TCP_TX_SIZE: usize = 4096;
const MAX_TCP_RETRIES: usize = 5;

static RX_BUFS: [StaticCell<[u8; TCP_RX_SIZE]>; MAX_CONNECTIONS] =
    [const { StaticCell::new() }; MAX_CONNECTIONS];
static TX_BUFS: [StaticCell<[u8; TCP_TX_SIZE]>; MAX_CONNECTIONS] =
    [const { StaticCell::new() }; MAX_CONNECTIONS];

// ---- Error type ------------------------------------------------------------

#[derive(Debug, defmt::Format)]
pub enum ConnectionError {
    /// No free buffer slots in the static pool.
    NoFreeBuffers,
    /// DNS resolution failed.
    DnsResolution(embassy_net::dns::Error),
    /// TCP connection failed after all retry attempts.
    TcpConnect(ConnectError),
    /// Failed to create the TLS session context.
    TlsSessionCreate(SessionError),
    /// TLS handshake did not complete successfully.
    TlsHandshake(SessionError),
}

// ---- TLS connection wrapper ------------------------------------------------

/// A fully established TLS connection backed by a pooled TCP socket.
///
/// The `session` field is public so callers can use it directly with
/// `embedded_io_async::Read`/`Write` or `edge_ws::io::send`/`recv`.
pub struct TlsConnection<'a> {
    pub session: Session<'a, TcpSocket<'static>>,
}

impl<'a> TlsConnection<'a> {
    /// Resolve `host`, claim a free buffer pair, open a TCP connection
    /// (with retries), create a TLS session, and perform the handshake.
    pub async fn init(
        network: embassy_net::Stack<'static>,
        host: &str,
        port: u16,
        tls: &'a Tls<'_>,
    ) -> Result<Self, ConnectionError> {
        // 1. DNS resolution
        let remote_ip = resolve(network, host).await?;

        // 2. Claim a free buffer pair from the static pool
        let (rx_buf, tx_buf) = Self::claim_buffers()?;

        // 3. Create and configure the TCP socket
        let mut socket = TcpSocket::new(network, rx_buf, tx_buf);
        socket.set_timeout(Some(Duration::from_secs(30)));

        // 4. TCP connect with retries
        let remote = (remote_ip, port);
        info!("TCP connecting to {}:{}...", remote_ip, port);
        let mut last_err = None;
        for attempt in 1..=MAX_TCP_RETRIES {
            match tcp_connect(&mut socket, remote).await {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                Err(e) => {
                    info!(
                        "TCP connect attempt {}/{} failed: {:?}",
                        attempt, MAX_TCP_RETRIES, e
                    );
                    last_err = Some(e);
                }
            }
        }
        if let Some(e) = last_err {
            return Err(ConnectionError::TcpConnect(e));
        }

        // 5. Build TLS config
        //    server_name must outlive the Session, so we leak a tiny heap CString.
        let host_cstring = CString::new(host).expect("hostname contains interior null byte");
        let host_cstr: &'static core::ffi::CStr = Box::leak(host_cstring.into_boxed_c_str());

        let tls_config = SessionConfig::Client(ClientSessionConfig {
            auth_mode: AuthMode::None,
            server_name: Some(host_cstr),
            min_version: TlsVersion::Tls1_2,
            ..ClientSessionConfig::new()
        });

        // 6. Create TLS session
        let mut session = Session::new(tls.reference(), socket, &tls_config)
            .map_err(ConnectionError::TlsSessionCreate)?;

        // 7. TLS handshake
        info!("TLS handshake...");
        session
            .connect()
            .await
            .map_err(ConnectionError::TlsHandshake)?;
        info!("TLS established!");

        Ok(Self { session })
    }

    #[allow(
        clippy::large_stack_frames,
        reason = "zero-init arrays are optimized to memset by the compiler"
    )]
    fn claim_buffers() -> Result<
        (
            &'static mut [u8; TCP_RX_SIZE],
            &'static mut [u8; TCP_TX_SIZE],
        ),
        ConnectionError,
    > {
        for i in 0..MAX_CONNECTIONS {
            if let Some(rx) = RX_BUFS[i].try_init([0; TCP_RX_SIZE]) {
                let tx = TX_BUFS[i]
                    .try_init([0; TCP_TX_SIZE])
                    .expect("buffer pool rx/tx out of sync");
                return Ok((rx, tx));
            }
        }
        Err(ConnectionError::NoFreeBuffers)
    }
}

impl<'a> Deref for TlsConnection<'a> {
    type Target = Session<'a, TcpSocket<'static>>;

    fn deref(&self) -> &Self::Target {
        &self.session
    }
}

impl<'a> DerefMut for TlsConnection<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.session
    }
}

// ---- TLS initialization ----------------------------------------------------

pub struct TlsHardware {
    pub rng: RNG<'static>,
    pub adc1: ADC1<'static>,
}

/// Initialise the True Random Number Generator and create the mbedTLS
/// singleton.  Must only be called once (the static cells will panic on a
/// second call).
pub fn init_tls(hardware: TlsHardware) -> Tls<'static> {
    // TrngSource configures the RNG peripheral; it must stay alive.
    static TRNG_SOURCE: StaticCell<TrngSource<'static>> = StaticCell::new();
    static TRNG: StaticCell<Trng> = StaticCell::new();

    let trng_source = TrngSource::new(hardware.rng, hardware.adc1);
    TRNG_SOURCE.init(trng_source);

    let trng = TRNG.init(Trng::try_new().expect("TrngSource not active"));

    let mut tls = Tls::new(trng).expect("Failed to create TLS instance");
    tls.set_debug(1);
    tls
}

// ---- Helpers ---------------------------------------------------------------

/// Resolve a hostname to an IPv4 address via DNS.
pub async fn resolve(
    network: embassy_net::Stack<'_>,
    host: &str,
) -> Result<IpAddress, ConnectionError> {
    info!("Resolving {}...", host);
    let ip_addrs = network
        .dns_query(host, DnsQueryType::A)
        .await
        .map_err(ConnectionError::DnsResolution)?;
    let ip = ip_addrs[0];
    info!("Resolved {} → {}", host, ip);
    Ok(ip)
}

/// Attempt a single TCP connection. On failure, resets the socket
/// (close -> 1 s delay -> abort) so the caller can retry immediately.
pub async fn tcp_connect(
    socket: &mut TcpSocket<'_>,
    remote: (IpAddress, u16),
) -> Result<(), ConnectError> {
    match socket.connect(remote).await {
        Ok(()) => {
            info!("TCP connected!");
            Ok(())
        }
        Err(e) => {
            socket.close();
            Timer::after(Duration::from_secs(1)).await;
            socket.abort();
            Err(e)
        }
    }
}

/// Find `\r\n\r\n` in a byte slice, returning the index of the first `\r`.
pub fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Perform the HTTP -> WebSocket upgrade handshake over an established
/// connection (typically TLS). Panics on failure.
pub async fn websocket_upgrade<S>(session: &mut S)
where
    S: Read + Write,
    S::Error: core::fmt::Debug,
{
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
