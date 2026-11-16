//! Wake-on-LAN. Frames in network standby wake on a magic packet when
//! "Power On with Mobile" / "Network Standby" is enabled on the TV.

use std::net::{Ipv4Addr, UdpSocket};

use crate::{Error, Result};

pub fn parse_mac(mac: &str) -> Result<[u8; 6]> {
    let hex: String = mac.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 12 {
        return Err(Error::Other(format!("invalid MAC address `{mac}`")));
    }
    let mut out = [0u8; 6];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte =
            u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|_| Error::Other(format!("invalid MAC `{mac}`")))?;
    }
    Ok(out)
}

pub fn magic_packet(mac: [u8; 6]) -> Vec<u8> {
    let mut packet = vec![0xFFu8; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&mac);
    }
    packet
}

/// Broadcast a magic packet. `host` (the TV's last known IP) also gets a unicast copy, which
/// helps when broadcasts are filtered but the switch still knows the TV's port.
pub fn wake(mac: &str, host: Option<&str>) -> Result<()> {
    let packet = magic_packet(parse_mac(mac)?);
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    for port in [9u16, 7] {
        socket.send_to(&packet, (Ipv4Addr::BROADCAST, port))?;
        if let Some(h) = host
            && let Ok(ip) = h.parse::<Ipv4Addr>()
        {
            let [a, b, c, _] = ip.octets();
            let _ = socket.send_to(&packet, (Ipv4Addr::new(a, b, c, 255), port));
            let _ = socket.send_to(&packet, (ip, port));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_and_packet() {
        let mac = parse_mac("AA-bb:cc.dd:EE:ff").unwrap();
        assert_eq!(mac, [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        let p = magic_packet(mac);
        assert_eq!(p.len(), 102);
        assert_eq!(&p[..6], &[0xFF; 6]);
        assert_eq!(&p[96..], &mac);
        assert!(parse_mac("nope").is_err());
    }
}
