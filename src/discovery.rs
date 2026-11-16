//! Finding Samsung TVs on the LAN: SSDP first, a port scan of nearby /24 subnets as a fallback.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::rest::{self, DeviceInfo};
use crate::{Result, protocol};

const SSDP_ADDR: &str = "239.255.255.250:1900";
const SEARCH_TARGETS: &[&str] =
    &["urn:samsung.com:device:RemoteControlReceiver:1", "urn:dial-multiscreen-org:service:dial:1"];

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
        let msg =
            format!("M-SEARCH * HTTP/1.1\r\nHOST: {SSDP_ADDR}\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {st}\r\n\r\n");
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

/// The `x` in `a.b.x.*` that [`scan_subnet`] also probes, besides this machine's own /24. Mesh
/// Wi-Fi and some routers put devices on neighbouring subnets, e.g. `192.168.0.*` and `192.168.1.*`.
const NEARBY_SUBNETS: std::ops::RangeInclusive<u8> = 0..=5;

/// How many connection attempts are in flight at once, to stay well under the open-file limit.
const SCAN_CONCURRENCY: usize = 256;

/// Probe every host on the REST port, across this machine's /24 and the nearby ones (`a.b.0.*`
/// to `a.b.5.*`). Slower than SSDP but works where multicast is filtered (mesh Wi-Fi, VLANs,
/// some Windows firewall profiles).
pub async fn scan_subnet() -> Result<Vec<Discovered>> {
    let Some(IpAddr::V4(local)) = local_ipv4() else {
        return Ok(vec![]);
    };
    let mut remaining = scan_targets(local);
    let mut open = BTreeSet::new();
    // In a burst of connections, the first one to a sleepy Wi-Fi TV is sometimes lost (often
    // while the network is still resolving its address), so whatever didn't answer gets a second try.
    for _ in 0..2 {
        let found = probe(&remaining).await;
        remaining.retain(|ip| !found.contains(ip));
        open.extend(found.into_iter().map(IpAddr::V4));
    }
    Ok(confirm(open).await)
}

/// Which of `ips` accept a connection on the REST port.
async fn probe(ips: &[Ipv4Addr]) -> BTreeSet<Ipv4Addr> {
    let limit = Arc::new(Semaphore::new(SCAN_CONCURRENCY));
    let mut probes = JoinSet::new();
    for &ip in ips {
        let limit = limit.clone();
        probes.spawn(async move {
            let _permit = limit.acquire_owned().await.ok()?;
            let addr = SocketAddr::new(IpAddr::V4(ip), protocol::REST_PORT);
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
    open
}

/// Every host address [`scan_subnet`] probes for a machine at `local`, its own /24 first.
fn scan_targets(local: Ipv4Addr) -> Vec<Ipv4Addr> {
    let [a, b, own, _] = local.octets();
    let mut subnets = vec![own];
    subnets.extend(NEARBY_SUBNETS.filter(|&c| c != own));
    subnets
        .into_iter()
        .flat_map(|c| (1..=254).map(move |d| Ipv4Addr::new(a, b, c, d)))
        .filter(|&ip| ip != local)
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_own_subnet_and_neighbours() {
        let targets = scan_targets(Ipv4Addr::new(192, 168, 1, 20));
        assert_eq!(targets.len(), 6 * 254 - 1);
        assert_eq!(targets[0], Ipv4Addr::new(192, 168, 1, 1));
        assert!(!targets.contains(&Ipv4Addr::new(192, 168, 1, 20)));
        assert!(targets.contains(&Ipv4Addr::new(192, 168, 0, 17)));
        assert!(targets.contains(&Ipv4Addr::new(192, 168, 5, 254)));
        assert!(!targets.contains(&Ipv4Addr::new(192, 168, 6, 1)));

        // A machine outside 0-5 still scans its own subnet, plus 0-5.
        let targets = scan_targets(Ipv4Addr::new(10, 0, 42, 7));
        assert_eq!(targets.len(), 7 * 254 - 1);
        assert_eq!(targets[0], Ipv4Addr::new(10, 0, 42, 1));
    }
}
