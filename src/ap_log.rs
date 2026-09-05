//! AP / captive-portal trace on UART1 TX (GP8, 115200 8N1).
//! Also mirrors to USB CDC via `log`.

use core::fmt::{self, Write};

use embassy_rp::uart::{Async, UartTx};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;

const LINE: usize = 160;
const DEPTH: usize = 16;

type Cs = CriticalSectionRawMutex;

#[derive(Clone, Copy)]
struct LogLine {
    buf: [u8; LINE],
    len: u8,
}

impl LogLine {
    const fn empty() -> Self {
        Self {
            buf: [0; LINE],
            len: 0,
        }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len as usize]).unwrap_or("?")
    }
}

impl Write for LogLine {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let pos = self.len as usize;
        let space = LINE.saturating_sub(pos);
        let n = space.min(s.len());
        self.buf[pos..pos + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len = (pos + n) as u8;
        Ok(())
    }
}

static LINES: Channel<Cs, LogLine, DEPTH> = Channel::new();

pub fn emit(args: fmt::Arguments<'_>) {
    let mut line = LogLine::empty();
    let _ = line.write_fmt(args);
    log::info!("{}", line.as_str());
    let _ = LINES.try_send(line);
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

#[embassy_executor::task]
pub async fn uart_log_task(mut uart: UartTx<'static, Async>) -> ! {
    let _ = uart.write(b"\r\nAP log UART1 GP8 115200\r\n").await;
    loop {
        let line = LINES.receive().await;
        let n = line.len as usize;
        let mut out = [0u8; LINE + 2];
        out[..n].copy_from_slice(&line.buf[..n]);
        out[n] = b'\r';
        out[n + 1] = b'\n';
        let _ = uart.write(&out[..n + 2]).await;
    }
}
