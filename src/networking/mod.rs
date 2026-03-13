pub(crate) mod connection;
mod deepgram;
mod tls;

pub use connection::{Connection, ConnectionError, MAX_CONNECTIONS};
pub use deepgram::{deepgram_create_listen_socket, deepgram_listen_socket_upgrade};
pub use tls::{init_global_tls, TlsHardware};

// ---- Shared helpers --------------------------------------------------------

use defmt::info;
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::{ConnectError, TcpSocket};
use embassy_time::{Duration, Timer};
use smoltcp::wire::IpAddress;

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
pub async fn single_tcp_connect(
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
