//! AP / captive-portal trace on USB CDC (VSP) via `log`.

use core::fmt;

pub fn emit(args: fmt::Arguments<'_>) {
    log::info!("{}", args);
}

/// Decode a DNS QNAME at offset 12 into `out`. Returns bytes written.
pub fn format_qname(pkt: &[u8], out: &mut [u8]) -> usize {
    let mut i = 12usize;
    let mut o = 0usize;
    let mut hops = 0u8;
    loop {
        hops = hops.saturating_add(1);
        if hops > 10 || i >= pkt.len() || o >= out.len() {
            break;
        }
        let len = pkt[i] as usize;
        if len == 0 {
            break;
        }
        if len & 0xC0 == 0xC0 {
            if i + 1 >= pkt.len() {
                break;
            }
            i = ((len & 0x3F) << 8) | (pkt[i + 1] as usize);
            continue;
        }
        if i + 1 + len > pkt.len() {
            break;
        }
        if o > 0 {
            out[o] = b'.';
            o += 1;
            if o >= out.len() {
                break;
            }
        }
        let n = len.min(out.len() - o);
        out[o..o + n].copy_from_slice(&pkt[i + 1..i + 1 + n]);
        o += n;
        i += 1 + len;
    }
    o
}

pub fn qtype_name(qtype: u16) -> &'static str {
    match qtype {
        1 => "A",
        12 => "PTR",
        28 => "AAAA",
        255 => "ANY",
        _ => "?",
    }
}
