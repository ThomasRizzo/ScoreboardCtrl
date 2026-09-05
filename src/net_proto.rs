//! Host-testable DHCP/DNS packet helpers (no Embassy).

pub const SERVER_IP: [u8; 4] = [192, 168, 0, 1];
pub const POOL_START: u8 = 10;
pub const POOL_SIZE: u8 = 8;

pub const DHCP_MAGIC: [u8; 4] = [99, 130, 83, 99];
pub const DHCP_DISCOVER: u8 = 1;
pub const DHCP_OFFER: u8 = 2;
pub const DHCP_REQUEST: u8 = 3;
pub const DHCP_ACK: u8 = 5;
pub const DHCP_NAK: u8 = 6;

pub fn offered_ip(slot: u8) -> [u8; 4] {
    [192, 168, 0, POOL_START + slot]
}

pub fn find_option(packet: &[u8], code: u8) -> Option<&[u8]> {
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

pub fn dhcp_msg_type(packet: &[u8]) -> Option<u8> {
    find_option(packet, 53).and_then(|v| v.first().copied())
}

pub fn requested_ip(packet: &[u8]) -> Option<[u8; 4]> {
    find_option(packet, 50).and_then(|v| v.try_into().ok())
}

pub fn write_offer_or_ack(
    req: &[u8],
    out: &mut [u8],
    yiaddr: [u8; 4],
    msg_type: u8,
) -> Option<usize> {
    if req.len() < 240 || out.len() < 360 {
        return None;
    }
    out[..240].fill(0);
    out[0] = 2; // BOOTREPLY
    out[1] = 1;
    out[2] = 6;
    out[4..8].copy_from_slice(&req[4..8]); // xid
    if msg_type != DHCP_NAK {
        out[16..20].copy_from_slice(&yiaddr); // yiaddr
    }
    out[20..24].copy_from_slice(&SERVER_IP); // siaddr
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

    const CAPTIVE_URI: &[u8] = b"http://192.168.0.1/";

    push(53, &[msg_type]);
    push(54, &SERVER_IP);
    if msg_type != DHCP_NAK {
        push(1, &[255, 255, 255, 0]);
        push(3, &SERVER_IP);
        push(6, &SERVER_IP);
        push(51, &[0, 1, 81, 128]); // 1 day
        push(28, &[192, 168, 0, 255]);
        push(15, b"lan");
        push(114, CAPTIVE_URI);
        push(160, CAPTIVE_URI);
    }
    out[i] = 255;
    i += 1;
    Some(i.max(300))
}

#[derive(Clone, Copy)]
pub struct Lease {
    pub mac: [u8; 6],
    pub seq: u32,
}

impl Lease {
    pub const EMPTY: Self = Self {
        mac: [0; 6],
        seq: 0,
    };
}

/// Assign a pool slot. Reuses the MAC's existing lease; otherwise an empty
/// slot; otherwise the least-recently-used lease (never `mac[5] % n`).
pub fn slot_for_mac(leases: &mut [Lease], seq: &mut u32, mac: &[u8; 6]) -> u8 {
    if let Some(i) = leases.iter().position(|m| &m.mac == mac) {
        *seq = seq.wrapping_add(1);
        leases[i].seq = *seq;
        return i as u8;
    }
    if let Some(i) = leases.iter().position(|m| m.mac == [0; 6]) {
        *seq = seq.wrapping_add(1);
        leases[i] = Lease {
            mac: *mac,
            seq: *seq,
        };
        return i as u8;
    }
    let mut i = 0usize;
    let mut best = leases.first().map(|l| l.seq).unwrap_or(0);
    for (idx, l) in leases.iter().enumerate() {
        if l.seq <= best {
            best = l.seq;
            i = idx;
        }
    }
    *seq = seq.wrapping_add(1);
    leases[i] = Lease {
        mac: *mac,
        seq: *seq,
    };
    i as u8
}

pub fn skip_name(q: &[u8], mut i: usize) -> Option<usize> {
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

/// Build a hijack reply. Copies only the question (drops EDNS/additional).
/// Sets ANCOUNT only when an A record is actually appended.
pub fn build_dns_reply(query: &[u8], n: usize, out: &mut [u8]) -> Option<usize> {
    if n < 12 || n > query.len() {
        return None;
    }
    let q = &query[..n];
    let name_end = skip_name(q, 12)?;
    if name_end + 4 > n {
        return None;
    }
    let qtype = u16::from_be_bytes([q[name_end], q[name_end + 1]]);
    let qend = name_end + 4;
    if out.len() < qend {
        return None;
    }
    out[..qend].copy_from_slice(&q[..qend]);
    out[2] = 0x81;
    out[3] = 0x80;
    out[6] = 0;
    out[8] = 0;
    out[9] = 0;
    out[10] = 0;
    out[11] = 0;

    let mut len = qend;
    if qtype == 1 && len + 16 <= out.len() {
        out[7] = 1;
        out[len] = 0xC0;
        out[len + 1] = 12;
        out[len + 2] = 0;
        out[len + 3] = 1;
        out[len + 4] = 0;
        out[len + 5] = 1;
        out[len + 6] = 0;
        out[len + 7] = 0;
        out[len + 8] = 0;
        out[len + 9] = 30;
        out[len + 10] = 0;
        out[len + 11] = 4;
        out[len + 12..len + 16].copy_from_slice(&SERVER_IP);
        len += 16;
    } else {
        out[7] = 0;
    }
    Some(len)
}

pub fn dns_qtype(query: &[u8], n: usize) -> Option<u16> {
    if n < 12 || n > query.len() {
        return None;
    }
    let name_end = skip_name(&query[..n], 12)?;
    if name_end + 4 > n {
        return None;
    }
    Some(u16::from_be_bytes([query[name_end], query[name_end + 1]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dhcp_min(msg: u8, mac: [u8; 6], opt50: Option<[u8; 4]>) -> [u8; 256] {
        let mut p = [0u8; 256];
        p[0] = 1;
        p[1] = 1;
        p[2] = 6;
        p[28..34].copy_from_slice(&mac);
        p[236..240].copy_from_slice(&DHCP_MAGIC);
        let mut i = 240;
        p[i] = 53;
        p[i + 1] = 1;
        p[i + 2] = msg;
        i += 3;
        if let Some(ip) = opt50 {
            p[i] = 50;
            p[i + 1] = 4;
            p[i + 2..i + 6].copy_from_slice(&ip);
            i += 6;
        }
        p[i] = 255;
        p
    }

    #[test]
    fn option_53_and_50() {
        let p = dhcp_min(DHCP_REQUEST, [1, 2, 3, 4, 5, 6], Some([192, 168, 0, 10]));
        assert_eq!(dhcp_msg_type(&p), Some(DHCP_REQUEST));
        assert_eq!(requested_ip(&p), Some([192, 168, 0, 10]));
    }

    #[test]
    fn lru_does_not_hash_overwrite() {
        let mut leases = [Lease::EMPTY; 8];
        let mut seq = 0u32;
        for i in 0..8u8 {
            let mac = [i, 0, 0, 0, 0, 1];
            assert_eq!(slot_for_mac(&mut leases, &mut seq, &mac), i);
        }
        let ninth = [9, 0, 0, 0, 0, 1];
        let evicted = slot_for_mac(&mut leases, &mut seq, &ninth);
        assert_eq!(evicted, 0);
        assert_eq!(leases[0].mac, ninth);
        let first_again = [0, 0, 0, 0, 0, 1];
        let slot = slot_for_mac(&mut leases, &mut seq, &first_again);
        assert_eq!(slot, 1);
    }

    #[test]
    fn dns_strips_edns_and_sets_ancount() {
        // header + "a.com" + A IN + 11 bytes of trailing junk
        let mut q = [0u8; 40];
        q[0] = 0x12;
        q[1] = 0x34;
        q[2] = 0x01;
        q[5] = 1; // QDCOUNT
        q[11] = 1; // ARCOUNT (EDNS lie)
        q[12] = 1;
        q[13] = b'a';
        q[14] = 3;
        q[15] = b'c';
        q[16] = b'o';
        q[17] = b'm';
        q[18] = 0;
        q[19] = 0;
        q[20] = 1;
        q[21] = 0;
        q[22] = 1;
        q[23..].fill(0xAA);
        let n = 40;
        let mut out = [0u8; 512];
        let len = build_dns_reply(&q, n, &mut out).unwrap();
        assert_eq!(len, 23 + 16);
        assert_eq!(out[7], 1);
        assert_eq!(out[10], 0);
        assert_eq!(out[11], 0);
        assert_eq!(&out[len - 4..len], &SERVER_IP);
        assert_ne!(out[23], 0xAA);
    }

    #[test]
    fn dns_aaaa_is_nodata() {
        let mut q = [0u8; 23];
        q[5] = 1;
        q[12] = 1;
        q[13] = b'a';
        q[14] = 3;
        q[15] = b'c';
        q[16] = b'o';
        q[17] = b'm';
        q[18] = 0;
        q[19] = 0;
        q[20] = 28;
        q[21] = 0;
        q[22] = 1;
        let mut out = [0u8; 512];
        let len = build_dns_reply(&q, 23, &mut out).unwrap();
        assert_eq!(len, 23);
        assert_eq!(out[7], 0);
    }
}
