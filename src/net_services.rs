//! Minimal DHCP + DNS so phones can join the Pico AP without the Mango.

use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint, Ipv4Address, Stack};
use embassy_time::Timer;
use scoreboard_ctrl::net_proto::{
    build_dns_reply, dhcp_msg_type, dns_qtype, offered_ip, requested_ip, skip_name, slot_for_mac,
    write_offer_or_ack, Lease, DHCP_ACK, DHCP_DISCOVER, DHCP_NAK, DHCP_OFFER, DHCP_REQUEST,
    POOL_SIZE, SERVER_IP,
};

async fn bind_retry(socket: &mut UdpSocket<'_>, port: u16, what: &str) {
    loop {
        match socket.bind(port) {
            Ok(()) => {
                crate::ap_log::emit(format_args!("{what} listen :{port}"));
                return;
            }
            Err(e) => {
                crate::ap_log::emit(format_args!("{what} bind :{port} {:?}, retry", e));
                Timer::after_secs(1).await;
            }
        }
    }
}

#[embassy_executor::task]
pub async fn dhcp_task(stack: Stack<'static>) -> ! {
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx_buf = [0u8; 600];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_buf = [0u8; 600];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    stack.wait_config_up().await;
    bind_retry(&mut socket, 67, "DHCP").await;

    let mut leases = [Lease::EMPTY; POOL_SIZE as usize];
    let mut lease_seq = 0u32;
    let mut pkt = [0u8; 600];
    let mut reply = [0u8; 400];

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
                let slot = slot_for_mac(&mut leases, &mut lease_seq, &mac);
                let yi = offered_ip(slot);
                let reply_type = match msg {
                    DHCP_DISCOVER => DHCP_OFFER,
                    DHCP_REQUEST => {
                        if let Some(want) = requested_ip(req) {
                            if want != yi {
                                DHCP_NAK
                            } else {
                                DHCP_ACK
                            }
                        } else {
                            DHCP_ACK
                        }
                    }
                    _ => continue,
                };
                let yiaddr = if reply_type == DHCP_NAK { [0; 4] } else { yi };
                let Some(len) = write_offer_or_ack(req, &mut reply, yiaddr, reply_type) else {
                    continue;
                };
                let dest = IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::BROADCAST), 68);
                if socket.send_to(&reply[..len], dest).await.is_ok() {
                    let kind = match reply_type {
                        DHCP_OFFER => "OFFER",
                        DHCP_NAK => "NAK",
                        _ => "ACK",
                    };
                    crate::ap_log::emit(format_args!(
                        "DHCP {} mac={:02x}{:02x}{:02x}{:02x}{:02x}{:02x} -> {}.{}.{}.{}",
                        kind,
                        mac[0],
                        mac[1],
                        mac[2],
                        mac[3],
                        mac[4],
                        mac[5],
                        yiaddr[0],
                        yiaddr[1],
                        yiaddr[2],
                        yiaddr[3]
                    ));
                }
            }
            Err(e) => crate::ap_log::emit(format_args!("DHCP recv {:?}", e)),
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
    bind_retry(&mut socket, 53, "DNS").await;
    crate::ap_log::emit(format_args!(
        "DNS hijack A -> {}.{}.{}.{}",
        SERVER_IP[0], SERVER_IP[1], SERVER_IP[2], SERVER_IP[3]
    ));

    let mut pkt = [0u8; 512];
    loop {
        match socket.recv_from(&mut pkt).await {
            Ok((n, meta)) => {
                let mut out = [0u8; 512];
                let Some(len) = build_dns_reply(&pkt, n, &mut out) else {
                    continue;
                };
                let qtype = dns_qtype(&pkt, n).unwrap_or(0);
                let dest = meta.endpoint;
                let mut name = [0u8; 80];
                let nlen = crate::ap_log::format_qname(&pkt, &mut name);
                let name = core::str::from_utf8(&name[..nlen]).unwrap_or("?");
                crate::ap_log::emit(format_args!(
                    "DNS {} {} from {:?} an={}",
                    crate::ap_log::qtype_name(qtype),
                    name,
                    dest,
                    out[7]
                ));
                let _ = socket.send_to(&out[..len], dest).await;
            }
            Err(e) => crate::ap_log::emit(format_args!("DNS recv {:?}", e)),
        }
    }
}

fn eq_label(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
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
                i = ((len & 0x3F) << 8) | (q[i + 1] as usize);
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
    let need = 12 + name.len() + 16;
    if out.len() < need {
        return None;
    }
    out[..12].fill(0);
    out[2] = 0x84;
    out[3] = 0x00;
    out[7] = 1;
    let mut i = 12;
    out[i..i + name.len()].copy_from_slice(name);
    i += name.len();
    out[i] = 0;
    out[i + 1] = 1;
    out[i + 2] = 0x80;
    out[i + 3] = 1;
    out[i + 4] = 0;
    out[i + 5] = 0;
    out[i + 6] = 0;
    out[i + 7] = 120;
    out[i + 8] = 0;
    out[i + 9] = 4;
    out[i + 10..i + 14].copy_from_slice(&SERVER_IP);
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
    bind_retry(&mut socket, 5353, "mDNS").await;
    crate::ap_log::emit(format_args!(
        "mDNS scoreboard.local/sb.local -> 192.168.0.1"
    ));

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
