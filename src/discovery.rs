//! Finding Samsung TVs on the LAN: SSDP first, optional /24 port scan as a fallback.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::net::{TcpStream, UdpSocket};
use tokio::task::JoinSet;

use crate::rest::{self, DeviceInfo};
use crate::{Result, protocol};

const SSDP_ADDR: &str = "239.255.255.250:1900";
const SEARCH_TARGETS: &[&str] = &[
    "urn:samsung.com:device:RemoteControlReceiver:1",
    "urn:dial-multiscreen-org:service:dial:1",
];

#[derive(Debug, Clone)]
pub struct Discovered {
    pub ip: IpAddr,
    pub info: DeviceInfo,
}

/// SSDP M-SEARCH for Samsung TVs, then confirm each responder via the REST API.
pub async fn ssdp(wait: Duration) -> Result<Vec<Discovered>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    socket.set_multicast_ttl_v4(2).ok();
    for st in SEARCH_TARGETS {
        let msg = format!(
            "M-SEARCH * HTTP/1.1\r\nHOST: {SSDP_ADDR}\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {st}\r\n\r\n"
        );
        // Send twice; UDP multicast is lossy on Wi-Fi.
        for _ in 0..2 {
            socket.send_to(msg.as_bytes(), SSDP_ADDR).await?;
        }
    }

    let mut candidates = BTreeSet::new();
    let mut buf = [0u8; 2048];
    let deadline = tokio::time::Instant::now() + wait;
    while let Ok(Ok((n, from))) = tokio::time::timeout_at(deadline, socket.recv_from(&mut buf)).await {
        let text = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
        if text.contains("samsung") || text.contains("dial-multiscreen") {
            candidates.insert(from.ip());
        }
    }
    Ok(confirm(candidates).await)
}

/// Probe every host of the local /24 on the REST port. Slower but works where multicast is
/// filtered (mesh Wi-Fi, VLANs, some Windows firewall profiles).
pub async fn scan_subnet() -> Result<Vec<Discovered>> {
    let Some(IpAddr::V4(local)) = local_ipv4() else {
        return Ok(vec![]);
    };
    let [a, b, c, _] = local.octets();
    let mut probes = JoinSet::new();
    for d in 1..=254u8 {
        let ip = IpAddr::V4(Ipv4Addr::new(a, b, c, d));
        probes.spawn(async move {
            let addr = SocketAddr::new(ip, protocol::REST_PORT);
            matches!(tokio::time::timeout(Duration::from_millis(400), TcpStream::connect(addr)).await, Ok(Ok(_)))
                .then_some(ip)
        });
    }
    let mut open = BTreeSet::new();
    while let Some(res) = probes.join_next().await {
        if let Ok(Some(ip)) = res {
            open.insert(ip);
        }
    }
    Ok(confirm(open).await)
}

async fn confirm(ips: BTreeSet<IpAddr>) -> Vec<Discovered> {
    let mut checks = JoinSet::new();
    for ip in ips {
        checks.spawn(async move {
            rest::device_info(&ip.to_string(), Duration::from_secs(3)).await.ok().map(|info| Discovered { ip, info })
        });
    }
    let mut out = Vec::new();
    while let Some(res) = checks.join_next().await {
        if let Ok(Some(d)) = res {
            out.push(d);
        }
    }
    out.sort_by_key(|d| d.ip);
    out
}

/// The address of the interface that routes to the internet (no packets are sent).
/// This machine's LAN IPv4 address (the one used for the default route).
pub fn local_ipv4() -> Option<IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip())
}
