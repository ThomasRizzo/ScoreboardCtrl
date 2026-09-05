//! Minimal DHCP + DNS so phones can join the Pico AP without the Mango.

use core::net::Ipv4Addr;

use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint, Ipv4Address, Stack};
use embassy_time::Timer;

const SERVER_IP: Ipv4Addr = Ipv4Addr::new(192, 168, 0, 1);
const POOL_START: u8 = 10;
const POOL_SIZE: u8 = 8;

const DHCP_MAGIC: [u8; 4] = [99, 130, 83, 99];
const DHCP_DISCOVER: u8 = 1;
const DHCP_OFFER: u8 = 2;
const DHCP_REQUEST: u8 = 3;
const DHCP_ACK: u8 = 5;

fn server_ip_octets() -> [u8; 4] {
    SERVER_IP.octets()
}

fn offered_ip(slot: u8) -> [u8; 4] {
    [192, 168, 0, POOL_START + slot]
}

fn find_option(packet: &[u8], code: u8) -> Option<&[u8]> {
    if packet.len() < 240 || packet[236..240] != DHCP_MAGIC {
        return None;
    }
    let mut i = 240;
    while i < packet.len() {
        match packet[i] {
            0 => i += 1,
            255 => break,
            c => {
                if i + 1 >= packet.len() {
                    break;
                }
                let len = packet[i + 1] as usize;
                let start = i + 2;
                let end = start.saturating_add(len);
                if end > packet.len() {
                    break;
                }
                if c == code {
                    return Some(&packet[start..end]);
                }
                i = end;
            }
        }
    }
    None
}

fn dhcp_msg_type(packet: &[u8]) -> Option<u8> {
    find_option(packet, 53).and_then(|v| v.first().copied())
}

fn write_offer_or_ack(req: &[u8], out: &mut [u8], yiaddr: [u8; 4], msg_type: u8) -> Option<usize> {
    if req.len() < 240 || out.len() < 360 {
        return None;
    }
    out[..240].fill(0);
    out[0] = 2; // BOOTREPLY
    out[1] = 1;
    out[2] = 6;
    out[4..8].copy_from_slice(&req[4..8]); // xid
    out[16..20].copy_from_slice(&yiaddr); // yiaddr
    out[20..24].copy_from_slice(&server_ip_octets()); // siaddr
    out[28..44].copy_from_slice(&req[28..44]); // chaddr
    out[236..240].copy_from_slice(&DHCP_MAGIC);

    let mut i = 240;
    let mut push = |code: u8, data: &[u8]| {
        if i + 2 + data.len() >= out.len() {
            return;
        }
        out[i] = code;
        out[i + 1] = data.len() as u8;
        out[i + 2..i + 2 + data.len()].copy_from_slice(data);
        i += 2 + data.len();
    };

    // RFC 8910 / RFC 7710: phones that honor these open the portal without
    // waiting on generate_204 / hotspot-detect. HTTP is all we can serve.
    const CAPTIVE_URI: &[u8] = b"http://192.168.0.1/";

    push(53, &[msg_type]);
    push(54, &server_ip_octets());
    push(1, &[255, 255, 255, 0]);
    push(3, &server_ip_octets());
    push(6, &server_ip_octets());
    push(51, &[0, 1, 81, 128]); // 1 day
    push(28, &[192, 168, 0, 255]);
    push(15, b"lan");
    push(114, CAPTIVE_URI);
    push(160, CAPTIVE_URI);
    out[i] = 255;
    i += 1;
    Some(i.max(300))
}

fn slot_for_mac(leases: &mut [[u8; 6]; POOL_SIZE as usize], mac: &[u8; 6]) -> u8 {
    if let Some(i) = leases.iter().position(|m| m == mac) {
        return i as u8;
    }
    if let Some(i) = leases.iter().position(|m| m == &[0; 6]) {
        leases[i] = *mac;
        return i as u8;
    }
    let i = (mac[5] as usize) % leases.len();
    leases[i] = *mac;
    i as u8
}

#[embassy_executor::task]
pub async fn dhcp_task(stack: Stack<'static>) -> ! {
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx_buf = [0u8; 600];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_buf = [0u8; 600];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    stack.wait_config_up().await;
    socket.bind(67).expect("dhcp bind");

    let mut leases = [[0u8; 6]; POOL_SIZE as usize];
    let mut pkt = [0u8; 600];
    let mut reply = [0u8; 400];
    crate::ap_log::emit(format_args!("DHCP listen :67"));

    loop {
        match socket.recv_from(&mut pkt).await {
            Ok((n, _meta)) => {
                let req = &pkt[..n];
                if n < 240 {
                    continue;
                }
                let mut mac = [0u8; 6];
                mac.copy_from_slice(&req[28..34]);
                let Some(msg) = dhcp_msg_type(req) else {
                    continue;
                };
                let slot = slot_for_mac(&mut leases, &mac);
                let yi = offered_ip(slot);
                let reply_type = match msg {
                    DHCP_DISCOVER => DHCP_OFFER,
                    DHCP_REQUEST => DHCP_ACK,
                    _ => continue,
                };
                let Some(len) = write_offer_or_ack(req, &mut reply, yi, reply_type) else {
                    continue;
                };
                let dest = IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::BROADCAST), 68);
                if socket.send_to(&reply[..len], dest).await.is_ok() {
                    crate::ap_log::emit(format_args!(
                        "DHCP {} mac={:02x}{:02x}{:02x}{:02x}{:02x}{:02x} -> 192.168.0.{}",
                        if reply_type == DHCP_OFFER {
                            "OFFER"
                        } else {
                            "ACK"
                        },
                        mac[0],
                        mac[1],
                        mac[2],
                        mac[3],
                        mac[4],
                        mac[5],
                        yi[3]
                    ));
                }
            }
            Err(e) => crate::ap_log::emit(format_args!("DHCP recv {:?}", e)),
        }
    }
}

fn skip_name(q: &[u8], mut i: usize) -> Option<usize> {
    loop {
        if i >= q.len() {
            return None;
        }
        let len = q[i] as usize;
        if len == 0 {
            return Some(i + 1);
        }
        if len & 0xC0 == 0xC0 {
            return if i + 1 < q.len() { Some(i + 2) } else { None };
        }
        i = i.checked_add(1 + len)?;
        if i > q.len() {
            return None;
        }
    }
}

#[embassy_executor::task]
pub async fn dns_task(stack: Stack<'static>) -> ! {
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx_buf = [0u8; 512];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_buf = [0u8; 512];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    stack.wait_config_up().await;
    socket.bind(53).expect("dns bind");

    let mut pkt = [0u8; 512];
    crate::ap_log::emit(format_args!("DNS listen :53 hijack A -> 192.168.0.1"));

    loop {
        match socket.recv_from(&mut pkt).await {
            Ok((n, meta)) => {
                if n < 12 {
                    continue;
                }
                let Some(name_end) = skip_name(&pkt, 12) else {
                    continue;
                };
                if name_end + 4 > n {
                    continue;
                }
                let qtype = u16::from_be_bytes([pkt[name_end], pkt[name_end + 1]]);

                let mut out = [0u8; 512];
                out[..n].copy_from_slice(&pkt[..n]);
                out[2] = 0x81; // response, recursion available
                out[3] = 0x80;
                out[6] = 0;
                out[7] = if qtype == 1 { 1 } else { 0 }; // ANCOUNT
                out[8] = 0;
                out[9] = 0;
                out[10] = 0;
                out[11] = 0;

                let mut len = n;
                if qtype == 1 && len + 16 <= out.len() {
                    // pointer to QNAME at offset 12
                    out[len] = 0xC0;
                    out[len + 1] = 12;
                    out[len + 2] = 0;
                    out[len + 3] = 1; // A
                    out[len + 4] = 0;
                    out[len + 5] = 1; // IN
                    out[len + 6] = 0;
                    out[len + 7] = 0;
                    out[len + 8] = 0;
                    out[len + 9] = 30; // TTL 30s
                    out[len + 10] = 0;
                    out[len + 11] = 4;
                    out[len + 12..len + 16].copy_from_slice(&server_ip_octets());
                    len += 16;
                }

                let dest = meta.endpoint;
                let mut name = [0u8; 80];
                let nlen = crate::ap_log::format_qname(&pkt, &mut name);
                let name = core::str::from_utf8(&name[..nlen]).unwrap_or("?");
                crate::ap_log::emit(format_args!(
                    "DNS {} {} from {:?} an={}",
                    crate::ap_log::qtype_name(qtype),
                    name,
                    dest,
                    if qtype == 1 { 1 } else { 0 }
                ));
                let _ = socket.send_to(&out[..len], dest).await;
            }
            Err(e) => crate::ap_log::emit(format_args!("DNS recv {:?}", e)),
        }
    }
}

fn eq_label(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.to_ascii_lowercase() == y.to_ascii_lowercase())
}

fn qname_matches(q: &[u8], mut i: usize, labels: &[&[u8]]) -> bool {
    let mut hops = 0u8;
    for lab in labels {
        loop {
            hops = hops.saturating_add(1);
            if hops > 8 || i >= q.len() {
                return false;
            }
            let len = q[i] as usize;
            if len & 0xC0 == 0xC0 {
                if i + 1 >= q.len() {
                    return false;
                }
                i = (((len & 0x3F) as usize) << 8) | (q[i + 1] as usize);
                continue;
            }
            if len == 0 || len != lab.len() || i + 1 + len > q.len() {
                return false;
            }
            if !eq_label(&q[i + 1..i + 1 + len], lab) {
                return false;
            }
            i += 1 + len;
            break;
        }
    }
    i < q.len() && q[i] == 0
}

fn mdns_name_is_ours(q: &[u8]) -> bool {
    qname_matches(q, 12, &[b"scoreboard", b"local"]) || qname_matches(q, 12, &[b"sb", b"local"])
}

fn write_mdns_a(out: &mut [u8], name: &[u8]) -> Option<usize> {
    // name is e.g. b"\x0ascoreboard\x05local\x00"
    let need = 12 + name.len() + 16;
    if out.len() < need {
        return None;
    }
    out[..12].fill(0);
    out[2] = 0x84; // response, authoritative
    out[3] = 0x00;
    out[7] = 1; // ANCOUNT
    let mut i = 12;
    out[i..i + name.len()].copy_from_slice(name);
    i += name.len();
    out[i] = 0;
    out[i + 1] = 1; // A
    out[i + 2] = 0x80;
    out[i + 3] = 1; // IN + cache flush
    out[i + 4] = 0;
    out[i + 5] = 0;
    out[i + 6] = 0;
    out[i + 7] = 120; // TTL
    out[i + 8] = 0;
    out[i + 9] = 4;
    out[i + 10..i + 14].copy_from_slice(&server_ip_octets());
    Some(i + 14)
}

/// Answers `scoreboard.local` / `sb.local` on the link, so phones don't use public DNS.
#[embassy_executor::task]
pub async fn mdns_task(stack: Stack<'static>) -> ! {
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx_buf = [0u8; 512];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_buf = [0u8; 512];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    stack.wait_config_up().await;
    let mdns_ip = IpAddress::Ipv4(Ipv4Address::new(224, 0, 0, 251));
    if stack.join_multicast_group(mdns_ip).is_err() {
        crate::ap_log::emit(format_args!("mDNS multicast join failed"));
    }
    socket.bind(5353).expect("mdns bind");
    crate::ap_log::emit(format_args!("mDNS scoreboard.local/sb.local -> 192.168.0.1"));

    let mcast = IpEndpoint::new(mdns_ip, 5353);
    let long_name: &[u8] = b"\x0ascoreboard\x05local\x00";
    let mut announce = [0u8; 64];
    if let Some(len) = write_mdns_a(&mut announce, long_name) {
        for _ in 0..3 {
            let _ = socket.send_to(&announce[..len], mcast).await;
            Timer::after_millis(400).await;
        }
    }

    let mut pkt = [0u8; 512];
    loop {
        match socket.recv_from(&mut pkt).await {
            Ok((n, meta)) => {
                if n < 12 || pkt[2] & 0x80 != 0 {
                    continue;
                }
                let Some(name_end) = skip_name(&pkt, 12) else {
                    continue;
                };
                if name_end + 4 > n || !mdns_name_is_ours(&pkt) {
                    continue;
                }
                let qtype = u16::from_be_bytes([pkt[name_end], pkt[name_end + 1]]);
                if qtype != 1 && qtype != 255 {
                    continue;
                }
                let name = if qname_matches(&pkt, 12, &[b"sb", b"local"]) {
                    b"\x02sb\x05local\x00".as_slice()
                } else {
                    long_name
                };
                let mut out = [0u8; 64];
                let Some(len) = write_mdns_a(&mut out, name) else {
                    continue;
                };
                let mut qn = [0u8; 40];
                let nlen = crate::ap_log::format_qname(&pkt, &mut qn);
                let qn = core::str::from_utf8(&qn[..nlen]).unwrap_or("?");
                crate::ap_log::emit(format_args!(
                    "mDNS {} {} from {:?}",
                    crate::ap_log::qtype_name(qtype),
                    qn,
                    meta.endpoint
                ));
                let _ = socket.send_to(&out[..len], meta.endpoint).await;
                let _ = socket.send_to(&out[..len], mcast).await;
            }
            Err(e) => crate::ap_log::emit(format_args!("mDNS recv {:?}", e)),
        }
    }
}
