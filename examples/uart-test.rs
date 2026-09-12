//! SK2229R RS-232 probe: UART1 (GP20 TX / GP21 RX @ 38400) + USB CDC console.
//!
//! No Wi-Fi, OTA, or GPIO pulses. Flash with `just uart-test`, then
//! `just uart-console`.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]

use core::cell::{Cell, RefCell};
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_rp::{
    uart::{
        BufferedInterruptHandler, BufferedUart, BufferedUartRx, BufferedUartTx,
        Config as UartConfig,
    },
    usb::Driver,
    watchdog::Watchdog,
};
use embassy_sync::{
    blocking_mutex::{raw::CriticalSectionRawMutex, Mutex as BlockingMutex},
    channel::Channel,
    mutex::Mutex,
};
use embassy_time::{Duration, Instant, Timer};
use embassy_usb_logger::ReceiverHandler;
use embedded_io_async::{Read, Write};
use panic_probe as _;
use portable_atomic::{AtomicU32, Ordering};
use scoreboard_ctrl::decode::{parse_clock_packet, write_hex, PACKET_LEN, PACKET_SOF};
use static_cell::StaticCell;

type Cs = CriticalSectionRawMutex;

embassy_rp::bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => embassy_rp::usb::InterruptHandler<embassy_rp::peripherals::USB>;
    UART1_IRQ => BufferedInterruptHandler<embassy_rp::peripherals::UART1>;
});

const VERSION: &str = env!("CARGO_PKG_VERSION");
const TX_MAX: usize = 64;
const LINE_MAX: usize = 128;
const RX_LINE: usize = 16;

#[derive(Clone, Copy)]
struct TxBurst {
    buf: [u8; TX_MAX],
    len: u8,
}

impl TxBurst {
    fn from_slice(data: &[u8]) -> Option<Self> {
        if data.is_empty() || data.len() > TX_MAX {
            return None;
        }
        let mut buf = [0u8; TX_MAX];
        buf[..data.len()].copy_from_slice(data);
        Some(Self {
            buf,
            len: data.len() as u8,
        })
    }

    fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len as usize]
    }
}

#[derive(Clone, Copy)]
enum HostCmd {
    Tx(TxBurst),
    Blast(TxBurst),
    Replay,
    Probe,
    Hunt,
    Listen,
    Help,
}

static TX_CH: Channel<Cs, HostCmd, 4> = Channel::new();
static LAST_FRAME: Mutex<Cs, Option<[u8; PACKET_LEN]>> = Mutex::new(None);
static RX_PKTS: AtomicU32 = AtomicU32::new(0);
static RX_ERRS: AtomicU32 = AtomicU32::new(0);
static RX_STRAY: AtomicU32 = AtomicU32::new(0);
static LAST_RX_MS: AtomicU32 = AtomicU32::new(0);
static TX_BUF: StaticCell<[u8; 64]> = StaticCell::new();
static RX_BUF: StaticCell<[u8; 256]> = StaticCell::new();
static WDG: StaticCell<Mutex<Cs, Watchdog>> = StaticCell::new();

struct CdcCmd;

impl ReceiverHandler for CdcCmd {
    fn new() -> Self {
        Self
    }

    async fn handle_data(&self, data: &[u8]) {
        const BOOT: &[u8] = b"ENTERBOOTLOADER";
        static BOOT_N: BlockingMutex<Cs, Cell<usize>> = BlockingMutex::new(Cell::new(0));
        static LINE: BlockingMutex<Cs, RefCell<LineBuf>> =
            BlockingMutex::new(RefCell::new(LineBuf {
                data: [0; LINE_MAX],
                len: 0,
            }));

        let hit_boot = BOOT_N.lock(|c| {
            for &b in data {
                let n = c.get();
                if n < BOOT.len() && b == BOOT[n] {
                    c.set(n + 1);
                    if n + 1 == BOOT.len() {
                        c.set(0);
                        return true;
                    }
                } else if b == BOOT[0] {
                    c.set(1);
                } else {
                    c.set(0);
                }
            }
            false
        });
        if hit_boot {
            log::info!("ENTERBOOTLOADER -> USB BOOTSEL");
            embassy_rp::rom_data::reset_to_usb_boot(0, 0);
        }

        let mut raw: [[u8; LINE_MAX]; 4] = [[0; LINE_MAX]; 4];
        let mut raw_len = [0usize; 4];
        let mut n_raw = 0usize;
        LINE.lock(|line| {
            let mut line = line.borrow_mut();
            for &b in data {
                if b == b'\n' || b == b'\r' {
                    if line.len == 0 {
                        continue;
                    }
                    if n_raw < raw.len() {
                        raw[n_raw][..line.len].copy_from_slice(&line.data[..line.len]);
                        raw_len[n_raw] = line.len;
                        n_raw += 1;
                    }
                    line.len = 0;
                    continue;
                }
                if line.len < LINE_MAX {
                    let n = line.len;
                    line.data[n] = b;
                    line.len = n + 1;
                } else {
                    line.len = 0;
                }
            }
        });
        for i in 0..n_raw {
            if let Some(cmd) = parse_line(&raw[i][..raw_len[i]]) {
                TX_CH.send(cmd).await;
            }
        }
    }
}

struct LineBuf {
    data: [u8; LINE_MAX],
    len: usize,
}

fn trim(s: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = s.len();
    while start < end && s[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && s[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &s[start..end]
}

fn cmd_eq(line: &[u8], name: &[u8]) -> bool {
    line.eq_ignore_ascii_case(name)
        || (line.len() > name.len() && line[line.len() - name.len()..].eq_ignore_ascii_case(name))
}

fn parse_line(line: &[u8]) -> Option<HostCmd> {
    let line = trim(line);
    if line.is_empty() {
        return None;
    }
    if cmd_eq(line, b"help") {
        return Some(HostCmd::Help);
    }
    if cmd_eq(line, b"replay") {
        return Some(HostCmd::Replay);
    }
    if cmd_eq(line, b"probe") {
        return Some(HostCmd::Probe);
    }
    if cmd_eq(line, b"hunt") {
        return Some(HostCmd::Hunt);
    }
    if cmd_eq(line, b"listen") {
        return Some(HostCmd::Listen);
    }
    if line.len() >= 2 && line[..2].eq_ignore_ascii_case(b"tx") {
        return parse_tx_cmd(&line[2..], false);
    }
    if line.len() >= 5 && line[..5].eq_ignore_ascii_case(b"blast") {
        return parse_tx_cmd(&line[5..], true);
    }
    log::warn!("unknown cmd (type help)");
    None
}

fn parse_tx_cmd(rest: &[u8], blast: bool) -> Option<HostCmd> {
    let rest = trim(rest);
    let mut buf = [0u8; TX_MAX];
    match parse_hex_bytes(rest, &mut buf) {
        Some(n) => TxBurst::from_slice(&buf[..n]).map(|b| {
            if blast {
                HostCmd::Blast(b)
            } else {
                HostCmd::Tx(b)
            }
        }),
        None => {
            log::warn!("tx: bad hex");
            None
        }
    }
}

fn parse_hex_bytes(s: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut hi: Option<u8> = None;
    let mut n = 0usize;
    for &c in s {
        if c.is_ascii_whitespace() {
            continue;
        }
        let d = unhex(c)?;
        if let Some(h) = hi {
            if n >= out.len() {
                return None;
            }
            out[n] = (h << 4) | d;
            n += 1;
            hi = None;
        } else {
            hi = Some(d);
        }
    }
    if hi.is_some() || n == 0 {
        None
    } else {
        Some(n)
    }
}

fn unhex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn xor_chk(pkt: &[u8; PACKET_LEN]) -> u8 {
    pkt[0] ^ pkt[2] ^ pkt[3] ^ pkt[4] ^ pkt[5]
}

fn sum_chk(pkt: &[u8; PACKET_LEN]) -> u8 {
    pkt[0]
        .wrapping_add(pkt[2])
        .wrapping_add(pkt[3])
        .wrapping_add(pkt[4])
        .wrapping_add(pkt[5])
}

fn log_hex(prefix: &str, data: &[u8]) {
    let mut hex = [0u8; 192];
    let n = write_hex(data, &mut hex);
    log::info!(
        "{} {}",
        prefix,
        core::str::from_utf8(&hex[..n]).unwrap_or("?")
    );
}

fn take_stray(buf: &mut [u8], n: &mut usize) {
    if *n == 0 {
        return;
    }
    RX_STRAY.fetch_add(*n as u32, Ordering::Relaxed);
    log_hex("UART stray", &buf[..*n]);
    *n = 0;
}

/// Repeat-clock streams would flood CDC; log when the window changes or every 2 s.
fn emit_rx_hex(data: &[u8], last: &mut [u8; RX_LINE], last_n: &mut usize, last_at: &mut Instant) {
    let now = Instant::now();
    let same = *last_n == data.len() && last[..data.len()] == data[..];
    if same && now.saturating_duration_since(*last_at).as_millis() < 2000 {
        return;
    }
    last[..data.len()].copy_from_slice(data);
    *last_n = data.len();
    *last_at = now;
    log_hex("UART rx", data);
}

fn log_help() {
    log::info!("uart-test v{VERSION} 38400 8N1 GP20 TX / GP21 RX");
    log::info!("tx <hex>   send bytes on UART1 TX  (tx 60 7a 00 02 06 00)");
    log::info!("blast <hex>  repeat that payload for 500 ms");
    log::info!("replay     resend last 0x60 clock frame");
    log::info!("probe      canned TX bursts, 300 ms gap");
    log::info!("hunt       framed cmds in RX gaps; watch UART stray + digits");
    log::info!("listen     20 s stray watch (press wireless remote / buttons)");
    log::info!("help       this text");
    log::info!("ENTERBOOTLOADER  UF2 BOOTSEL");
}

fn now_ms() -> u32 {
    Instant::now().as_millis() as u32
}

#[embassy_executor::task]
async fn watchdog_task(wdg: &'static Mutex<Cs, Watchdog>) -> ! {
    loop {
        wdg.lock().await.feed(Duration::from_secs(8));
        Timer::after_secs(1).await;
    }
}

#[embassy_executor::task]
async fn logger_task(usb: embassy_rp::Peri<'static, embassy_rp::peripherals::USB>) {
    let driver = Driver::new(usb, Irqs);
    embassy_usb_logger::run!(1024, log::LevelFilter::Info, driver, CdcCmd);
}

#[embassy_executor::task]
async fn uart_rx_task(mut rx: BufferedUartRx) -> ! {
    let mut dma_tmp = [0u8; RX_LINE];
    let mut line = [0u8; RX_LINE];
    let mut line_n = 0usize;
    let mut packet = [0u8; PACKET_LEN];
    let mut pkt_n = 0usize;
    let mut last_logged = [0u8; PACKET_LEN];
    let mut have_logged = false;
    let mut last_raw = [0u8; RX_LINE];
    let mut last_raw_n = 0usize;
    let mut last_raw_at = Instant::from_ticks(0);
    let mut stats_at = Instant::now();
    let mut stats_pkts = 0u32;
    let mut stray_buf = [0u8; RX_LINE];
    let mut stray_n = 0usize;
    let mut last_pkt_at = Instant::now();
    log::info!("UART RX up");
    loop {
        let now = Instant::now();
        if now.saturating_duration_since(stats_at).as_millis() >= 1000 {
            let pkts = RX_PKTS.load(Ordering::Relaxed);
            let errs = RX_ERRS.load(Ordering::Relaxed);
            let stray = RX_STRAY.load(Ordering::Relaxed);
            let delta = pkts.wrapping_sub(stats_pkts);
            stats_pkts = pkts;
            stats_at = now;
            log::info!("UART stats pkts/s={delta} total={pkts} err={errs} stray={stray}");
        }
        match select(rx.read(&mut dma_tmp), Timer::after_millis(50)).await {
            Either::First(Ok(n)) if n > 0 => {
                LAST_RX_MS.store(now_ms(), Ordering::Relaxed);
                for &byte in &dma_tmp[..n] {
                    if line_n == line.len() {
                        emit_rx_hex(&line, &mut last_raw, &mut last_raw_n, &mut last_raw_at);
                        line_n = 0;
                    }
                    line[line_n] = byte;
                    line_n += 1;

                    if pkt_n == 0 {
                        if byte != PACKET_SOF {
                            // Pre-sync clock tails are not replies; only count
                            // extras after we have locked onto 0x60 frames.
                            if have_logged {
                                if stray_n == stray_buf.len() {
                                    take_stray(&mut stray_buf, &mut stray_n);
                                }
                                stray_buf[stray_n] = byte;
                                stray_n += 1;
                            }
                            continue;
                        }
                        packet[0] = byte;
                        pkt_n = 1;
                        last_pkt_at = Instant::now();
                        continue;
                    }
                    packet[pkt_n] = byte;
                    pkt_n += 1;
                    last_pkt_at = Instant::now();
                    if pkt_n < PACKET_LEN {
                        continue;
                    }
                    pkt_n = 0;
                    if let Some((min, sec)) = parse_clock_packet(&packet) {
                        RX_PKTS.fetch_add(1, Ordering::Relaxed);
                        *LAST_FRAME.lock().await = Some(packet);
                        if !have_logged || packet != last_logged {
                            last_logged = packet;
                            have_logged = true;
                            log::info!(
                                "UART frame {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} -> {:02}:{:02}",
                                packet[0],
                                packet[1],
                                packet[2],
                                packet[3],
                                packet[4],
                                packet[5],
                                min,
                                sec
                            );
                        }
                    } else {
                        RX_STRAY.fetch_add(PACKET_LEN as u32, Ordering::Relaxed);
                        log_hex("UART nonclock", &packet);
                    }
                }
            }
            Either::First(Ok(_)) => {}
            Either::First(Err(e)) => {
                if line_n > 0 {
                    emit_rx_hex(
                        &line[..line_n],
                        &mut last_raw,
                        &mut last_raw_n,
                        &mut last_raw_at,
                    );
                    line_n = 0;
                }
                RX_ERRS.fetch_add(1, Ordering::Relaxed);
                log::warn!("UART rx err {:?}", e);
                pkt_n = 0;
            }
            Either::Second(_) => {
                // Keep assembling across the ~90 ms inter-frame gap. Only
                // drop a partial 0x60 packet if it stalls for a long time.
                if pkt_n > 0
                    && Instant::now()
                        .saturating_duration_since(last_pkt_at)
                        .as_millis()
                        >= 250
                {
                    RX_STRAY.fetch_add(pkt_n as u32, Ordering::Relaxed);
                    log_hex("UART incomplete", &packet[..pkt_n]);
                    pkt_n = 0;
                }
                take_stray(&mut stray_buf, &mut stray_n);
                if line_n > 0 {
                    emit_rx_hex(
                        &line[..line_n],
                        &mut last_raw,
                        &mut last_raw_n,
                        &mut last_raw_at,
                    );
                    line_n = 0;
                }
            }
        }
    }
}

async fn send_blast(tx: &mut BufferedUartTx, data: &[u8]) {
    log_hex("UART blast 500ms", data);
    let end = Instant::now() + Duration::from_millis(500);
    let mut n = 0u32;
    while Instant::now() < end {
        if let Err(e) = tx.write_all(data).await {
            log::warn!("UART tx err {:?}", e);
            break;
        }
        n = n.saturating_add(1);
    }
    let _ = tx.flush().await;
    log::info!("UART blast end n={n}");
}

async fn send_burst(tx: &mut BufferedUartTx, data: &[u8]) {
    log_hex("UART tx", data);
    if let Err(e) = tx.write_all(data).await {
        log::warn!("UART tx err {:?}", e);
        return;
    }
    let _ = tx.flush().await;
    Timer::after_millis(300).await;
    log::info!("UART window end");
}

async fn wait_rx_gap() {
    // Clock frames are ~1.6 ms; ~90 ms of silence between them.
    for _ in 0..40 {
        let t = LAST_RX_MS.load(Ordering::Relaxed);
        Timer::after_millis(12).await;
        if LAST_RX_MS.load(Ordering::Relaxed) == t {
            return;
        }
    }
}

async fn send_gap(tx: &mut BufferedUartTx, data: &[u8]) {
    wait_rx_gap().await;
    let stray0 = RX_STRAY.load(Ordering::Relaxed);
    log_hex("UART hunt", data);
    if let Err(e) = tx.write_all(data).await {
        log::warn!("UART tx err {:?}", e);
        return;
    }
    let _ = tx.flush().await;
    Timer::after_millis(80).await;
    let stray1 = RX_STRAY.load(Ordering::Relaxed);
    if stray1 != stray0 {
        log::info!("UART hunt stray +{}", stray1.wrapping_sub(stray0));
    }
}

fn xor8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |a, b| a ^ b)
}

async fn hunt(tx: &mut BufferedUartTx) {
    log::info!("hunt start — watch digits/horn AND UART stray lines");
    let stray0 = RX_STRAY.load(Ordering::Relaxed);

    log::info!("hunt 1: key-sized bytes in RX gap");
    for b in 0x01u8..=0x20 {
        send_gap(tx, &[b]).await;
    }

    log::info!("hunt 2: cmd + complement");
    for b in 0x01u8..=0x10 {
        send_gap(tx, &[b, !b]).await;
    }

    log::info!("hunt 3: STX cmd ETX / A5 cmd chk");
    for b in [0x01u8, 0x02, 0x03, 0x10, 0x11, 0x20, 0x30, 0x80, 0xaa] {
        send_gap(tx, &[0x02, b, 0x03]).await;
        send_gap(tx, &[0xa5, b, xor8(&[0xa5, b])]).await;
        send_gap(tx, &[0x55, 0xaa, b]).await;
    }

    log::info!("hunt 4: 6-byte clock layout, unk0=cmd");
    let last = *LAST_FRAME.lock().await;
    let (min, sec) = last
        .and_then(|f| parse_clock_packet(&f))
        .unwrap_or((7, 30));
    for cmd in [0x00u8, 0x01, 0x02, 0x03, 0x10, 0x80, 0xff] {
        let mut pkt = [PACKET_SOF, 0, cmd, min, sec, sec];
        pkt[1] = xor8(&[pkt[0], pkt[2], pkt[3], pkt[4], pkt[5]]);
        send_gap(tx, &pkt).await;
        pkt[1] = pkt[0]
            .wrapping_add(pkt[2])
            .wrapping_add(pkt[3])
            .wrapping_add(pkt[4])
            .wrapping_add(pkt[5]);
        send_gap(tx, &pkt).await;
    }

    log::info!("hunt 5: 6-byte alt SOF (same payload as last clock)");
    let payload = last.unwrap_or([PACKET_SOF, 0, 0, min, sec, 0]);
    for sof in [
        0x00u8, 0x01, 0x02, 0x10, 0x20, 0x40, 0x55, 0x61, 0x7e, 0x80, 0xa5, 0xaa, 0xc0, 0xe0,
        0xf0, 0xff,
    ] {
        let mut pkt = payload;
        pkt[0] = sof;
        send_gap(tx, &pkt).await;
    }

    log::info!("hunt 6: ASCII");
    let ascii: &[&[u8]] = &[
        b"ST\r",
        b"GO\r",
        b"SP\r",
        b"RST\r",
        b"RESET\r",
        b"SET\r",
        b"TIME\r",
        b"START\r",
        b"STOP\r",
        b"?\r",
        b"AT\r",
        b"\x01\x02\x03\x04\r",
    ];
    for s in ascii {
        send_gap(tx, s).await;
    }

    let stray1 = RX_STRAY.load(Ordering::Relaxed);
    log::info!(
        "hunt done stray {} -> {} (delta {})",
        stray0,
        stray1,
        stray1.wrapping_sub(stray0)
    );
}

async fn listen_stray() {
    let stray0 = RX_STRAY.load(Ordering::Relaxed);
    log::info!("listen 20s: press wireless remote and board buttons now");
    Timer::after_secs(20).await;
    let stray1 = RX_STRAY.load(Ordering::Relaxed);
    log::info!(
        "listen done stray {} -> {} (delta {})",
        stray0,
        stray1,
        stray1.wrapping_sub(stray0)
    );
}

async fn send_chk_variants(tx: &mut BufferedUartTx, mut pkt: [u8; PACKET_LEN]) {
    send_burst(tx, &pkt).await;
    pkt[1] = xor_chk(&pkt);
    log::info!("chk xor={:02x}", pkt[1]);
    send_burst(tx, &pkt).await;
    pkt[1] = sum_chk(&pkt);
    log::info!("chk sum={:02x}", pkt[1]);
    send_burst(tx, &pkt).await;
    pkt[1] = 0;
    log::info!("chk 00");
    send_burst(tx, &pkt).await;
}

#[embassy_executor::task]
async fn uart_tx_task(mut tx: BufferedUartTx) -> ! {
    log_help();
    loop {
        match TX_CH.receive().await {
            HostCmd::Help => log_help(),
            HostCmd::Tx(burst) => send_burst(&mut tx, burst.as_slice()).await,
            HostCmd::Blast(burst) => send_blast(&mut tx, burst.as_slice()).await,
            HostCmd::Replay => match *LAST_FRAME.lock().await {
                Some(frame) => send_burst(&mut tx, &frame).await,
                None => log::info!("replay: no 0x60 frame yet"),
            },
            HostCmd::Probe => {
                log::info!("probe start");
                let last = *LAST_FRAME.lock().await;
                if let Some(frame) = last {
                    log::info!("probe replay + chk variants");
                    send_chk_variants(&mut tx, frame).await;
                } else {
                    log::info!("probe: no captured frame, skipping replay");
                }
                for b in [0x00u8, 0xff, 0x55, 0xaa, 0x60] {
                    send_burst(&mut tx, &[b]).await;
                }
                send_burst(&mut tx, b"?\r\n").await;
                send_burst(&mut tx, b"AT\r\n").await;
                send_burst(&mut tx, b"\r\n").await;
                log::info!("probe clock-shaped 07:30");
                send_chk_variants(&mut tx, [PACKET_SOF, 0, 0, 7, 30, 0]).await;
                log::info!("probe done");
            }
            HostCmd::Hunt => hunt(&mut tx).await,
            HostCmd::Listen => listen_stray().await,
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    let mut wdg = Watchdog::new(p.WATCHDOG);
    wdg.pause_on_debug(true);
    wdg.start(Duration::from_secs(8));
    let watchdog = WDG.init(Mutex::new(wdg));
    spawner.spawn(watchdog_task(watchdog).unwrap());
    spawner.spawn(logger_task(p.USB).unwrap());
    Timer::after_millis(500).await;

    let mut uart_config = UartConfig::default();
    uart_config.baudrate = 38400;
    let tx_buf = TX_BUF.init([0; 64]);
    let rx_buf = RX_BUF.init([0; 256]);
    let uart = BufferedUart::new(
        p.UART1,
        p.PIN_20,
        p.PIN_21,
        Irqs,
        tx_buf,
        rx_buf,
        uart_config,
    );
    let (tx, rx) = uart.split();
    spawner.spawn(uart_rx_task(rx).unwrap());
    spawner.spawn(uart_tx_task(tx).unwrap());

    loop {
        Timer::after_secs(8).await;
    }
}
