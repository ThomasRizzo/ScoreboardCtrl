//! SK2229R UART time bytes. `serial.ps1` documents `(0xFF - b) >> 1`.

/// Decode one minutes/seconds byte from a scoreboard packet.
pub const fn decode_time_byte(raw: u8) -> u8 {
    (0xFFu8.wrapping_sub(raw)) >> 1
}
