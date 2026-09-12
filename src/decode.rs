//! SK2229R UART clock frames (38400 8N1), captured on UART1 GP21.
//!
//! Repeating 6-byte packet:
//! `60 | chk | unk0 | minutes | seconds | unk1`
//!
//! Minutes and seconds are raw binary (not BCD, not inverted). `chk` changes
//! with the payload (algorithm still unknown). `unk0`/`unk1` have been 0 while
//! only the clock was running.

pub const PACKET_LEN: usize = 6;
pub const PACKET_SOF: u8 = 0x60;
pub const PACKET_MIN_INDEX: usize = 3;
pub const PACKET_SEC_INDEX: usize = 4;

pub const MAX_MINUTES: u8 = 99;
pub const MAX_SECONDS: u8 = 59;

/// Milliseconds the displayed time may sit unchanged before we treat the
/// clock as stopped. Physical start/stop/reset/min/sec on the SK2229R all
/// show up as UART MM:SS changes; GPIO pulses must not own this flag.
pub const STOPPED_AFTER_MS: u64 = 2000;

/// Parse a 6-byte SK2229R frame into `(minutes, seconds)`.
pub fn parse_clock_packet(pkt: &[u8; PACKET_LEN]) -> Option<(u8, u8)> {
    if pkt[0] != PACKET_SOF {
        return None;
    }
    let minutes = pkt[PACKET_MIN_INDEX];
    let seconds = pkt[PACKET_SEC_INDEX];
    if minutes > MAX_MINUTES || seconds > MAX_SECONDS {
        return None;
    }
    Some((minutes, seconds))
}

pub const fn total_seconds(min: u8, sec: u8) -> u16 {
    (min as u16) * 60 + (sec as u16)
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Write `aa bb cc` into `out`. Returns bytes written.
pub fn write_hex(data: &[u8], out: &mut [u8]) -> usize {
    let mut i = 0;
    for (k, &b) in data.iter().enumerate() {
        let need = if k == 0 { 2 } else { 3 };
        if i + need > out.len() {
            break;
        }
        if k > 0 {
            out[i] = b' ';
            i += 1;
        }
        out[i] = HEX[(b >> 4) as usize];
        out[i + 1] = HEX[(b & 0x0f) as usize];
        i += 2;
    }
    i
}

/// Stopped if MM:SS is `00:00` or unchanged for ≥2 s; otherwise running.
pub fn clock_is_running(min: u8, sec: u8, unchanged_ms: u64) -> bool {
    total_seconds(min, sec) != 0 && unchanged_ms < STOPPED_AFTER_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(min: u8, sec: u8) -> [u8; 6] {
        [0x60, 0x00, 0x00, min, sec, 0x00]
    }

    #[test]
    fn parse_captured_countdown_frames() {
        assert_eq!(
            parse_clock_packet(&[0x60, 0x7a, 0x00, 0x02, 0x06, 0x00]),
            Some((2, 6))
        );
        assert_eq!(
            parse_clock_packet(&[0x60, 0x3c, 0x00, 0x01, 0x3b, 0x00]),
            Some((1, 59))
        );
        assert_eq!(parse_clock_packet(&pkt(7, 30)), Some((7, 30)));
    }

    #[test]
    fn parse_rejects_bad_sof_or_range() {
        assert_eq!(
            parse_clock_packet(&[0x00, 0x7a, 0x00, 0x02, 0x06, 0x00]),
            None
        );
        assert_eq!(parse_clock_packet(&pkt(100, 0)), None);
        assert_eq!(parse_clock_packet(&pkt(0, 60)), None);
    }

    #[test]
    fn running_from_time_updates() {
        assert!(!clock_is_running(0, 0, 0));
        assert!(!clock_is_running(0, 0, 500));
        assert!(clock_is_running(7, 30, 0));
        assert!(clock_is_running(7, 29, 500));
        assert!(clock_is_running(7, 30, STOPPED_AFTER_MS - 1));
        assert!(!clock_is_running(7, 30, STOPPED_AFTER_MS));
        assert!(clock_is_running(7, 30, 0));
    }

    #[test]
    fn write_hex_spaces() {
        let mut out = [0u8; 16];
        let n = write_hex(&[0x00, 0xab, 0x3f], &mut out);
        assert_eq!(&out[..n], b"00 ab 3f");
    }
}
