//! SK2229R UART time bytes. Encoding is `(0xFF - b) >> 1`.
//!
//! Packet (6 bytes, 38400 8N1): `00 | min | sec | shotclock | 3F | crc`.
//! CRC is not checked here (algorithm not in-tree); the `3F` marker and
//! decoded range are the frame check.

/// Decode one minutes/seconds byte from a scoreboard packet.
pub const fn decode_time_byte(raw: u8) -> u8 {
    (0xFFu8.wrapping_sub(raw)) >> 1
}

pub const PACKET_LEN: usize = 6;
pub const PACKET_SOF: u8 = 0x00;
pub const PACKET_MARK: u8 = 0x3F;
pub const PACKET_MARK_INDEX: usize = 4;

pub const MAX_MINUTES: u8 = 99;
pub const MAX_SECONDS: u8 = 59;

/// Milliseconds the displayed time may sit unchanged before we treat the
/// clock as stopped. The physical start/stop button is independent of GPIO
/// pulses, so this is the source of truth together with 00:00.
pub const STOPPED_AFTER_MS: u64 = 2000;

/// Parse a 6-byte SK2229R frame into `(minutes, seconds)`.
pub fn parse_clock_packet(pkt: &[u8; PACKET_LEN]) -> Option<(u8, u8)> {
    if pkt[0] != PACKET_SOF || pkt[PACKET_MARK_INDEX] != PACKET_MARK {
        return None;
    }
    let minutes = decode_time_byte(pkt[1]);
    let seconds = decode_time_byte(pkt[2]);
    if minutes > MAX_MINUTES || seconds > MAX_SECONDS {
        return None;
    }
    Some((minutes, seconds))
}

pub const fn total_seconds(min: u8, sec: u8) -> u16 {
    (min as u16) * 60 + (sec as u16)
}

/// How to update `running` from successive UART clock readings.
///
/// The SK2229R start/stop button can be pressed on the board itself, so GPIO
/// pulses must not own this flag. Time remaining decreasing means running;
/// 00:00 or a frozen display means stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunUpdate {
    Force(bool),
    Keep,
}

pub fn infer_running(prev_total: u16, min: u8, sec: u8, unchanged_ms: u64) -> RunUpdate {
    let total = total_seconds(min, sec);
    if total == 0 {
        return RunUpdate::Force(false);
    }
    if total < prev_total {
        return RunUpdate::Force(true);
    }
    if total > prev_total {
        // Reset / set jumped the clock up; hardware clock is not counting.
        return RunUpdate::Force(false);
    }
    if unchanged_ms >= STOPPED_AFTER_MS {
        return RunUpdate::Force(false);
    }
    RunUpdate::Keep
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(v: u8) -> u8 {
        0xFFu8.wrapping_sub(v << 1)
    }

    fn pkt(min: u8, sec: u8, shot: u8, crc: u8) -> [u8; 6] {
        [0x00, enc(min), enc(sec), shot, 0x3F, crc]
    }

    #[test]
    fn decode_period_default() {
        assert_eq!(decode_time_byte(enc(7)), 7);
        assert_eq!(decode_time_byte(enc(30)), 30);
        assert_eq!(decode_time_byte(0xFF), 0);
    }

    #[test]
    fn parse_accepts_valid_frame_with_zero_crc() {
        let p = pkt(7, 30, 0x00, 0x00);
        assert_eq!(parse_clock_packet(&p), Some((7, 30)));
    }

    #[test]
    fn parse_rejects_bad_marker_or_range() {
        let mut bad = pkt(7, 30, 0, 0);
        bad[4] = 0x00;
        assert_eq!(parse_clock_packet(&bad), None);
        let mut high = pkt(7, 30, 0, 0);
        high[1] = enc(100);
        assert_eq!(parse_clock_packet(&high), None);
        let mut sec = pkt(7, 30, 0, 0);
        sec[2] = enc(60);
        assert_eq!(parse_clock_packet(&sec), None);
    }

    #[test]
    fn running_from_time_updates() {
        assert_eq!(infer_running(7 * 60 + 30, 0, 0, 0), RunUpdate::Force(false));
        assert_eq!(infer_running(7 * 60 + 30, 7, 29, 0), RunUpdate::Force(true));
        assert_eq!(infer_running(7 * 60 + 30, 7, 30, 500), RunUpdate::Keep);
        assert_eq!(
            infer_running(7 * 60 + 30, 7, 30, 2000),
            RunUpdate::Force(false)
        );
        assert_eq!(infer_running(5 * 60, 7, 30, 0), RunUpdate::Force(false));
    }
}
