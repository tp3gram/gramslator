use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU8, Ordering};

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

// ---- Reusable buffer pool for concurrent TCP connections -------------------

pub const MAX_CONNECTIONS: usize = 4;
const TCP_RX_SIZE: usize = 16384;
const TCP_TX_SIZE: usize = 4096;
const MAX_TCP_RETRIES: usize = 5;

/// Bitmask tracking which buffer slots are currently in use.
/// Bit `i` set = slot `i` is claimed. Atomic so it is safe across tasks.
static POOL_IN_USE: AtomicU8 = AtomicU8::new(0);

struct BufSlot<const N: usize>(UnsafeCell<[u8; N]>);

// SAFETY: Access is guarded by the POOL_IN_USE atomic bitmask — only the task
// that successfully claims a slot may access its buffers.
unsafe impl<const N: usize> Sync for BufSlot<N> {}

static RX_BUFS: [BufSlot<TCP_RX_SIZE>; MAX_CONNECTIONS] =
    [const { BufSlot(UnsafeCell::new([0; TCP_RX_SIZE])) }; MAX_CONNECTIONS];
static TX_BUFS: [BufSlot<TCP_TX_SIZE>; MAX_CONNECTIONS] =
    [const { BufSlot(UnsafeCell::new([0; TCP_TX_SIZE])) }; MAX_CONNECTIONS];

/// Try to claim a free buffer slot. Returns the slot index and mutable
/// references to the rx/tx buffers on success.
fn claim_buffers() -> Result<
    (
        usize,
        &'static mut [u8; TCP_RX_SIZE],
        &'static mut [u8; TCP_TX_SIZE],
    ),
    ConnectionError,
> {
    loop {
        let current = POOL_IN_USE.load(Ordering::Acquire);
        for i in 0..MAX_CONNECTIONS {
            let bit = 1u8 << i;
            if current & bit == 0 {
                // Try to atomically set this bit.
                if POOL_IN_USE
                    .compare_exchange(current, current | bit, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    // SAFETY: We just exclusively claimed slot `i` via the
                    // atomic CAS. No other task can access these buffers
                    // until we release the bit.
                    let rx = unsafe { &mut *RX_BUFS[i].0.get() };
                    let tx = unsafe { &mut *TX_BUFS[i].0.get() };
                    // Zero the buffers for the new connection.
                    rx.fill(0);
                    tx.fill(0);
                    return Ok((i, rx, tx));
                }
                // CAS failed — another task grabbed a slot; retry.
                break;
            }
        }
        if POOL_IN_USE.load(Ordering::Acquire).count_ones() as usize >= MAX_CONNECTIONS {
            return Err(ConnectionError::NoFreeBuffers);
        }
    }
}

/// Release a previously claimed buffer slot so it can be reused.
fn release_slot(index: usize) {
    let bit = 1u8 << index;
    POOL_IN_USE.fetch_and(!bit, Ordering::Release);
}

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

// ---- Connection wrapper ----------------------------------------------------

/// The underlying transport: either an encrypted TLS session or a plain TCP socket.
enum ConnectionInner<'a> {
    Tls(Session<'a, TcpSocket<'static>>),
    Tcp(TcpSocket<'static>),
}

/// A fully established connection backed by a pooled TCP socket.
///
/// May be either an encrypted TLS session (HTTPS) or a plain TCP connection
/// (HTTP). Implements `embedded_io_async::Read` and `Write` so callers can
/// use it generically regardless of the transport.
///
/// The connection holds a buffer-pool slot that is released when `close()` is
/// called. Callers **must** call `close()` before dropping.
pub struct Connection<'a> {
    inner: ConnectionInner<'a>,
    /// Index into the static buffer pool. Released on close.
    slot: usize,
}

impl<'a> Connection<'a> {
    /// Resolve `host`, claim a free buffer pair, open a TCP connection
    /// (with retries), create a TLS session, and perform the handshake.
    pub async fn init_tls(
        network: embassy_net::Stack<'static>,
        host: &str,
        port: u16,
        tls: &'a Tls<'_>,
    ) -> Result<Self, ConnectionError> {
        // 1. DNS + TCP connect (shared logic)
        let (slot, socket) = Self::connect_tcp(network, host, port).await?;

        // 2. Build TLS config
        //    server_name must outlive the Session, so we leak a tiny heap CString.
        let host_cstring = CString::new(host).expect("hostname contains interior null byte");
        let host_cstr: &'static core::ffi::CStr = Box::leak(host_cstring.into_boxed_c_str());

        let tls_config = SessionConfig::Client(ClientSessionConfig {
            auth_mode: AuthMode::None,
            server_name: Some(host_cstr),
            min_version: TlsVersion::Tls1_2,
            ..ClientSessionConfig::new()
        });

        // 3. Create TLS session
        let mut session = match Session::new(tls.reference(), socket, &tls_config) {
            Ok(s) => s,
            Err(e) => {
                release_slot(slot);
                return Err(ConnectionError::TlsSessionCreate(e));
            }
        };

        // 4. TLS handshake
        info!("TLS handshake...");
        if let Err(e) = session.connect().await {
            release_slot(slot);
            return Err(ConnectionError::TlsHandshake(e));
        }
        info!("TLS established!");

        Ok(Self {
            inner: ConnectionInner::Tls(session),
            slot,
        })
    }

    /// Resolve `host`, claim a free buffer pair, and open a plain TCP
    /// connection (with retries). No TLS handshake is performed.
    pub async fn init_tcp(
        network: embassy_net::Stack<'static>,
        host: &str,
        port: u16,
    ) -> Result<Self, ConnectionError> {
        let (slot, socket) = Self::connect_tcp(network, host, port).await?;
        Ok(Self {
            inner: ConnectionInner::Tcp(socket),
            slot,
        })
    }

    /// Shared helper: DNS resolution, buffer claim, TCP connect with retries.
    async fn connect_tcp(
        network: embassy_net::Stack<'static>,
        host: &str,
        port: u16,
    ) -> Result<(usize, TcpSocket<'static>), ConnectionError> {
        // 1. DNS resolution
        let remote_ip = resolve_dns(network, host).await?;

        // 2. Claim a free buffer pair from the static pool
        let (slot, rx_buf, tx_buf) = claim_buffers()?;

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
            release_slot(slot);
            return Err(ConnectionError::TcpConnect(e));
        }

        Ok((slot, socket))
    }

    /// Close the connection cleanly and release its buffer-pool slot.
    ///
    /// For TLS connections this performs the TLS close_notify handshake.
    /// For plain TCP connections this closes the socket.
    pub async fn close(&mut self) {
        match &mut self.inner {
            ConnectionInner::Tls(session) => {
                if let Err(e) = session.close().await {
                    info!("TLS close error (non-fatal): {:?}", e);
                }
            }
            ConnectionInner::Tcp(socket) => {
                socket.close();
            }
        }
        release_slot(self.slot);
    }
}

// ---- embedded_io_async trait impls -----------------------------------------

impl embedded_io::ErrorType for Connection<'_> {
    type Error = embedded_io::ErrorKind;
}

impl Read for Connection<'_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        match &mut self.inner {
            ConnectionInner::Tls(session) => session
                .read(buf)
                .await
                .map_err(|_| embedded_io::ErrorKind::Other),
            ConnectionInner::Tcp(socket) => socket
                .read(buf)
                .await
                .map_err(|_| embedded_io::ErrorKind::Other),
        }
    }
}

impl Write for Connection<'_> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        match &mut self.inner {
            ConnectionInner::Tls(session) => session
                .write(buf)
                .await
                .map_err(|_| embedded_io::ErrorKind::Other),
            ConnectionInner::Tcp(socket) => socket
                .write(buf)
                .await
                .map_err(|_| embedded_io::ErrorKind::Other),
        }
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        match &mut self.inner {
            ConnectionInner::Tls(session) => session
                .flush()
                .await
                .map_err(|_| embedded_io::ErrorKind::Other),
            ConnectionInner::Tcp(socket) => socket
                .flush()
                .await
                .map_err(|_| embedded_io::ErrorKind::Other),
        }
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
pub async fn resolve_dns(
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
