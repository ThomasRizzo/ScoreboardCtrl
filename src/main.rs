#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]

#[cfg_attr(feature = "simulate", allow(dead_code))]
mod ap_log;
mod decode;
mod net_services;

use cyw43::Control;
use cyw43_pio::PioSpi;
use embassy_rp::{
    clocks::RoscRng,
    dma,
    gpio::{Level, Output},
    peripherals::PIO0,
    pio::Pio,
};
use core::cell::Cell;
use embassy_sync::{
    blocking_mutex::{raw::CriticalSectionRawMutex, Mutex as BlockingMutex},
    channel::{Channel, Receiver, Sender},
    mutex::Mutex,
};
use embassy_time::{Duration, Timer};
use embassy_usb_logger::ReceiverHandler;
use panic_persist as _;
use picoserve::{
    io::Read,
    make_static,
    request::{Path, Request},
    response::{IntoResponse, Json, ResponseWriter, StatusCode},
    routing::{get, get_service, parse_path_segment, post, PathRouterService},
    AppBuilder, AppRouter, ResponseSent,
};
use portable_atomic::{AtomicBool, Ordering};

#[cfg(not(feature = "simulate"))]
use decode::decode_time_byte;

embassy_rp::bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => embassy_rp::pio::InterruptHandler<embassy_rp::peripherals::PIO0>;
    USBCTRL_IRQ => embassy_rp::usb::InterruptHandler<embassy_rp::peripherals::USB>;
    UART0_IRQ => embassy_rp::uart::InterruptHandler<embassy_rp::peripherals::UART0>;
    DMA_IRQ_0 => dma::InterruptHandler<embassy_rp::peripherals::DMA_CH0>,
        dma::InterruptHandler<embassy_rp::peripherals::DMA_CH1>,
        dma::InterruptHandler<embassy_rp::peripherals::DMA_CH2>;
});

const WIFI_SSID: &str = "Scoreboard";
/// Simultaneous HTTP/TCP clients. CYW43439 AP itself is max 4 Wi-Fi STAs.
const WEB_TASK_POOL_SIZE: usize = 12;
const NET_SOCKETS: usize = WEB_TASK_POOL_SIZE + 5;
const PULSE_MS: u64 = 50;
const PORTAL_URL: &str = "http://192.168.0.1/";
const UI_HTML: &str = include_str!("../index.html");
const APPLE_SUCCESS: &str =
    "<HTML><HEAD><TITLE>Success</TITLE></HEAD><BODY>Success</BODY></HTML>";

static SERVED_UI: AtomicBool = AtomicBool::new(false);
static HTTP_LIVE: portable_atomic::AtomicU8 = portable_atomic::AtomicU8::new(0);

type Cs = CriticalSectionRawMutex;

#[derive(Clone, Copy)]
enum Command {
    StartStop,
    Start,
    Stop,
    HomeInc,
    HomeDec,
    AwayInc,
    AwayDec,
    ScoresZero,
    Reset,
    LedOn,
    LedOff,
    SetTimer { min: u8, sec: u8 },
}

struct Io {
    start: Output<'static>,
    home_inc: Output<'static>,
    home_dec: Output<'static>,
    away_inc: Output<'static>,
    away_dec: Output<'static>,
    reset: Output<'static>,
}

#[derive(Clone, Copy)]
struct SharedControl(&'static Mutex<Cs, Control<'static>>);

const PERIOD_MIN: u8 = 7;
const PERIOD_SEC: u8 = 30;

#[derive(Clone, Copy, serde::Serialize)]
struct ScoreboardState {
    minutes: u8,
    seconds: u8,
    running: bool,
    home: u16,
    away: u16,
    led: bool,
}

impl Default for ScoreboardState {
    fn default() -> Self {
        Self {
            minutes: PERIOD_MIN,
            seconds: PERIOD_SEC,
            running: false,
            home: 0,
            away: 0,
            led: false,
        }
    }
}

#[derive(Clone, Copy)]
struct SharedScoreboard(&'static Mutex<Cs, ScoreboardState>);

struct AppProps {
    cmd: Sender<'static, Cs, Command, 8>,
    scoreboard: SharedScoreboard,
}

struct MarkUi;

fn hdr<'a>(parts: &picoserve::request::RequestParts<'a>, name: &str) -> &'a str {
    parts
        .headers()
        .get(name)
        .and_then(|v| v.as_str().ok())
        .unwrap_or("-")
}

fn ua_tag(ua: &str) -> &str {
    if ua.contains("CaptiveNetworkSupport") {
        "CNA"
    } else if ua.contains("Android") {
        "Android"
    } else if ua.contains("iPhone") || ua.contains("iPad") || ua.contains("iPod") {
        "iOS"
    } else if ua.contains("Macintosh") {
        "macOS"
    } else if ua.len() > 28 {
        &ua[..28]
    } else {
        ua
    }
}

fn log_http(parts: &picoserve::request::RequestParts<'_>, action: &str) {
    let path = parts.path().encoded();
    if path.starts_with("/api/status") {
        return;
    }
    ap_log::emit(format_args!(
        "HTTP {} {} host={} ua={} served={} -> {}",
        parts.method(),
        path,
        hdr(parts, "Host"),
        ua_tag(hdr(parts, "User-Agent")),
        SERVED_UI.load(Ordering::Relaxed) as u8,
        action
    ));
}

impl picoserve::routing::RequestHandlerService for MarkUi {
    async fn call_request_handler_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        state: &(),
        path_parameters: (),
        request: Request<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        SERVED_UI.store(true, Ordering::Relaxed);
        log_http(&request.parts, "UI");
        picoserve::response::File::html(UI_HTML)
            .call_request_handler_service(state, path_parameters, request, response_writer)
            .await
    }
}

fn ui_page() -> impl picoserve::routing::MethodHandler {
    get_service(MarkUi)
}

/// 302 to the Pico IP. Android treats this as captive and opens that URL
/// (so Chrome/mobile-data DNS never sees scoreboard.com).
enum ProbeReply {
    Redirect,
    AppleDone,
}

impl IntoResponse for ProbeReply {
    async fn write_to<R: Read, W: ResponseWriter<Error = R::Error>>(
        self,
        connection: picoserve::response::Connection<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        match self {
            Self::Redirect => {
                (
                    StatusCode::FOUND,
                    ("Location", PORTAL_URL),
                    ("Cache-Control", "no-store"),
                    PORTAL_URL,
                )
                    .write_to(connection, response_writer)
                    .await
            }
            Self::AppleDone => {
                (
                    ("Content-Type", "text/html"),
                    ("Cache-Control", "no-store"),
                    APPLE_SUCCESS,
                )
                    .write_to(connection, response_writer)
                    .await
            }
        }
    }
}

struct AppleProbe;

impl picoserve::routing::RequestHandlerService for AppleProbe {
    async fn call_request_handler_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        _state: &(),
        _path_parameters: (),
        request: Request<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        let done = SERVED_UI.load(Ordering::Relaxed);
        log_http(
            &request.parts,
            if done { "apple-success" } else { "302" },
        );
        let reply = if done {
            ProbeReply::AppleDone
        } else {
            ProbeReply::Redirect
        };
        reply
            .write_to(request.body_connection.finalize().await?, response_writer)
            .await
    }
}

fn apple_probe() -> impl picoserve::routing::MethodHandler {
    get_service(AppleProbe)
}

/// Unknown paths (OS captive probes we didn't list) 302 to the scoreboard UI.
struct PortalFallback;

impl PathRouterService for PortalFallback {
    async fn call_path_router_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        _state: &(),
        _current_path_parameters: (),
        _path: Path<'_>,
        request: Request<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        log_http(&request.parts, "302");
        ProbeReply::Redirect
            .write_to(request.body_connection.finalize().await?, response_writer)
            .await
    }
}

impl AppBuilder for AppProps {
    type PathRouter = impl picoserve::routing::PathRouter;

    fn build_app(self) -> picoserve::Router<Self::PathRouter> {
        let cmd = self.cmd;
        let scoreboard = self.scoreboard;
        picoserve::Router::from_service(PortalFallback)
            .route("/", ui_page())
            .route("/index.html", ui_page())
            .route("/hotspot-detect.html", apple_probe())
            .route("/hotspotdetect.html", apple_probe())
            .route("/library/test/success.html", apple_probe())
            .route(
                "/led/on",
                post(move || async move {
                    let _ = cmd.send(Command::LedOn).await;
                    "OK"
                }),
            )
            .route(
                "/led/off",
                post(move || async move {
                    let _ = cmd.send(Command::LedOff).await;
                    "OK"
                }),
            )
            .route(
                "/api/ctrl/start-stop",
                post(move || async move {
                    let _ = cmd.send(Command::StartStop).await;
                    "OK"
                }),
            )
            .route(
                "/api/ctrl/start",
                post(move || async move {
                    let _ = cmd.send(Command::Start).await;
                    "OK"
                }),
            )
            .route(
                "/api/ctrl/stop",
                post(move || async move {
                    let _ = cmd.send(Command::Stop).await;
                    "OK"
                }),
            )
            .route(
                "/api/ctrl/reset",
                post(move || async move {
                    let _ = cmd.send(Command::Reset).await;
                    "OK"
                }),
            )
            .route(
                "/api/ctrl/home-inc",
                post(move || async move {
                    let _ = cmd.send(Command::HomeInc).await;
                    "OK"
                }),
            )
            .route(
                "/api/ctrl/home-dec",
                post(move || async move {
                    let _ = cmd.send(Command::HomeDec).await;
                    "OK"
                }),
            )
            .route(
                "/api/ctrl/away-inc",
                post(move || async move {
                    let _ = cmd.send(Command::AwayInc).await;
                    "OK"
                }),
            )
            .route(
                "/api/ctrl/away-dec",
                post(move || async move {
                    let _ = cmd.send(Command::AwayDec).await;
                    "OK"
                }),
            )
            .route(
                "/api/ctrl/scores-zero",
                post(move || async move {
                    let _ = cmd.send(Command::ScoresZero).await;
                    "OK"
                }),
            )
            .route(
                (
                    "/api/timer/set",
                    parse_path_segment::<u8>(),
                    parse_path_segment::<u8>(),
                ),
                post(move |(min, sec)| async move {
                    let _ = cmd.send(Command::SetTimer { min, sec }).await;
                    "OK"
                }),
            )
            .route(
                "/api/status",
                get(move || async move {
                    let s = *scoreboard.0.lock().await;
                    Json(StatusJson {
                        time: TimeStr::from_parts(s.minutes, s.seconds),
                        running: s.running,
                        home: s.home,
                        away: s.away,
                        led: s.led,
                        sim: cfg!(feature = "simulate"),
                    })
                }),
            )
    }
}

/// MM:SS as a serde string without alloc.
struct TimeStr {
    buf: [u8; 5],
}

impl TimeStr {
    fn from_parts(min: u8, sec: u8) -> Self {
        Self {
            buf: [
                b'0' + (min / 10) % 10,
                b'0' + min % 10,
                b':',
                b'0' + (sec / 10) % 10,
                b'0' + sec % 10,
            ],
        }
    }
}

impl serde::Serialize for TimeStr {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let s = core::str::from_utf8(&self.buf).unwrap_or("00:00");
        serializer.serialize_str(s)
    }
}

#[derive(serde::Serialize)]
struct StatusJson {
    time: TimeStr,
    running: bool,
    home: u16,
    away: u16,
    led: bool,
    sim: bool,
}

async fn pulse(pin: &mut Output<'static>) {
    pin.set_high();
    Timer::after_millis(PULSE_MS).await;
    pin.set_low();
}

async fn blink_onboard(control: SharedControl, hold_led: bool) {
    if hold_led {
        return;
    }
    control.0.lock().await.gpio_set(0, true).await;
    Timer::after_millis(PULSE_MS).await;
    control.0.lock().await.gpio_set(0, false).await;
}

/// Pico W onboard LED (CYW43 GPIO 0). Solid if the UI turned it on; otherwise a 1 Hz heartbeat.
#[embassy_executor::task]
async fn heartbeat_task(control: SharedControl, scoreboard: SharedScoreboard) -> ! {
    loop {
        let hold = scoreboard.0.lock().await.led;
        if hold {
            control.0.lock().await.gpio_set(0, true).await;
            Timer::after_millis(200).await;
            continue;
        }
        control.0.lock().await.gpio_set(0, true).await;
        Timer::after_millis(80).await;
        if !scoreboard.0.lock().await.led {
            control.0.lock().await.gpio_set(0, false).await;
        }
        Timer::after_millis(920).await;
    }
}

#[embassy_executor::task]
async fn board_task(
    mut io: Io,
    control: SharedControl,
    scoreboard: SharedScoreboard,
    receiver: Receiver<'static, Cs, Command, 8>,
) -> ! {
    loop {
        let cmd = receiver.receive().await;
        match cmd {
            Command::LedOn => {
                control.0.lock().await.gpio_set(0, true).await;
                scoreboard.0.lock().await.led = true;
                log::info!("LED on");
            }
            Command::LedOff => {
                control.0.lock().await.gpio_set(0, false).await;
                scoreboard.0.lock().await.led = false;
                log::info!("LED off");
            }
            Command::StartStop | Command::Start | Command::Stop => {
                let led = scoreboard.0.lock().await.led;
                pulse(&mut io.start).await;
                blink_onboard(control, led).await;
                #[cfg(feature = "simulate")]
                {
                    let mut s = scoreboard.0.lock().await;
                    s.running = match cmd {
                        Command::Start => true,
                        Command::Stop => false,
                        _ => !s.running,
                    };
                    log::info!("sim running={}", s.running);
                }
                #[cfg(not(feature = "simulate"))]
                log::info!("pulse start/stop");
            }
            Command::HomeInc => {
                {
                    let mut s = scoreboard.0.lock().await;
                    s.home = s.home.saturating_add(1);
                }
                let led = scoreboard.0.lock().await.led;
                pulse(&mut io.home_inc).await;
                blink_onboard(control, led).await;
                log::info!("home +");
            }
            Command::HomeDec => {
                {
                    let mut s = scoreboard.0.lock().await;
                    s.home = s.home.saturating_sub(1);
                }
                let led = scoreboard.0.lock().await.led;
                pulse(&mut io.home_dec).await;
                blink_onboard(control, led).await;
                log::info!("home -");
            }
            Command::AwayInc => {
                {
                    let mut s = scoreboard.0.lock().await;
                    s.away = s.away.saturating_add(1);
                }
                let led = scoreboard.0.lock().await.led;
                pulse(&mut io.away_inc).await;
                blink_onboard(control, led).await;
                log::info!("away +");
            }
            Command::AwayDec => {
                {
                    let mut s = scoreboard.0.lock().await;
                    s.away = s.away.saturating_sub(1);
                }
                let led = scoreboard.0.lock().await.led;
                pulse(&mut io.away_dec).await;
                blink_onboard(control, led).await;
                log::info!("away -");
            }
            Command::ScoresZero => {
                let (home, away, led) = {
                    let mut s = scoreboard.0.lock().await;
                    let home = s.home;
                    let away = s.away;
                    s.home = 0;
                    s.away = 0;
                    (home, away, s.led)
                };
                #[cfg(not(feature = "simulate"))]
                {
                    for _ in 0..home {
                        pulse(&mut io.home_dec).await;
                    }
                    for _ in 0..away {
                        pulse(&mut io.away_dec).await;
                    }
                }
                blink_onboard(control, led).await;
                log::info!("scores 0 (was {}-{})", home, away);
            }
            Command::Reset => {
                #[cfg(feature = "simulate")]
                {
                    let mut s = scoreboard.0.lock().await;
                    s.running = false;
                    log::info!("sim reset stop {:02}:{:02}", s.minutes, s.seconds);
                }
                let led = scoreboard.0.lock().await.led;
                pulse(&mut io.reset).await;
                blink_onboard(control, led).await;
                #[cfg(not(feature = "simulate"))]
                log::info!("pulse reset");
            }
            Command::SetTimer { min, sec } => {
                #[cfg(feature = "simulate")]
                {
                    let mut s = scoreboard.0.lock().await;
                    s.minutes = min.min(99);
                    s.seconds = sec.min(59);
                    s.running = false;
                    log::info!("sim set {:02}:{:02}", s.minutes, s.seconds);
                }
                #[cfg(not(feature = "simulate"))]
                log::info!("set-timer ignored (hardware mode)");
                let _ = (min, sec);
            }
        }
    }
}

#[cfg(feature = "simulate")]
#[embassy_executor::task]
async fn timer_task(scoreboard: SharedScoreboard) -> ! {
    loop {
        Timer::after_secs(1).await;
        let mut s = scoreboard.0.lock().await;
        if s.running {
            if s.seconds > 0 {
                s.seconds -= 1;
            } else if s.minutes > 0 {
                s.minutes -= 1;
                s.seconds = 59;
            } else {
                s.running = false;
                log::info!("sim timer done");
            }
        }
    }
}

#[cfg(not(feature = "simulate"))]
#[embassy_executor::task]
async fn read_serial(
    mut rx: embassy_rp::uart::UartRx<'static, embassy_rp::uart::Async>,
    scoreboard: SharedScoreboard,
) -> ! {
    let mut byte_buf = [0; 1];
    let mut packet_buf = [0u8; 6];
    let mut buf_idx = 0;
    log::info!("UART 38400 GP17");
    loop {
        match rx.read(&mut byte_buf).await {
            Ok(_) => {
                let byte = byte_buf[0];
                if byte == 0x00 {
                    packet_buf[0] = byte;
                    buf_idx = 1;
                } else if buf_idx > 0 {
                    packet_buf[buf_idx] = byte;
                    buf_idx += 1;
                    if buf_idx == 6 {
                        let raw_min = packet_buf[1];
                        let raw_sec = packet_buf[2];
                        let minutes = decode_time_byte(raw_min);
                        let seconds = decode_time_byte(raw_sec);
                        let mut s = scoreboard.0.lock().await;
                        if s.minutes != minutes || s.seconds != seconds {
                            s.minutes = minutes;
                            s.seconds = seconds;
                            log::info!(
                                "UART {:02x}:{:02x} -> {}:{:02}",
                                raw_min,
                                raw_sec,
                                minutes,
                                seconds
                            );
                        }
                        buf_idx = 0;
                    }
                }
            }
            Err(e) => {
                log::warn!("UART {:?}", e);
                buf_idx = 0;
            }
        }
    }
}

/// USB CDC is read/write: logs out, `ENTERBOOTLOADER` in (reboots to UF2 BOOTSEL).
struct BootCmd;

impl embassy_usb_logger::ReceiverHandler for BootCmd {
    fn new() -> Self {
        Self
    }

    async fn handle_data(&self, data: &[u8]) {
        const CMD: &[u8] = b"ENTERBOOTLOADER";
        static MATCH: BlockingMutex<CriticalSectionRawMutex, Cell<usize>> =
            BlockingMutex::new(Cell::new(0));
        let hit = MATCH.lock(|c| {
            for &b in data {
                let n = c.get();
                if n < CMD.len() && b == CMD[n] {
                    c.set(n + 1);
                    if n + 1 == CMD.len() {
                        c.set(0);
                        return true;
                    }
                } else if b == CMD[0] {
                    c.set(1);
                } else {
                    c.set(0);
                }
            }
            false
        });
        if hit {
            log::info!("ENTERBOOTLOADER -> USB BOOTSEL");
            embassy_rp::rom_data::reset_to_usb_boot(0, 0);
        }
    }
}

#[embassy_executor::task]
async fn logger_task(usb: embassy_rp::Peri<'static, embassy_rp::peripherals::USB>) {
    let driver = embassy_rp::usb::Driver::new(usb, Irqs);
    embassy_usb_logger::run!(1024, log::LevelFilter::Info, driver, BootCmd);
}

#[embassy_executor::task]
async fn wifi_task(
    runner: cyw43::Runner<'static, cyw43::SpiBus<Output<'static>, PioSpi<'static, PIO0, 0>>>,
) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn net_task(mut stack: embassy_net::Runner<'static, cyw43::NetDriver<'static>>) -> ! {
    stack.run().await
}

#[embassy_executor::task(pool_size = WEB_TASK_POOL_SIZE)]
async fn web_task(
    task_id: usize,
    stack: embassy_net::Stack<'static>,
    app: &'static AppRouter<AppProps>,
    config: &'static picoserve::Config,
) -> ! {
    let mut tcp_rx_buffer = [0; 1024];
    let mut tcp_tx_buffer = [0; 1024];
    let mut http_buffer = [0; 2048];
    loop {
        let mut socket =
            embassy_net::tcp::TcpSocket::new(stack, &mut tcp_rx_buffer, &mut tcp_tx_buffer);
        // picoserve's listen_and_serve hardcodes 45s idle. A closed laptop tab
        // that misses RST would pin a worker that long and block reconnects.
        socket.set_timeout(Some(Duration::from_secs(5)));
        socket.set_keep_alive(None);
        if let Err(e) = socket.accept(80).await {
            ap_log::emit(format_args!("tcp{} accept {:?}", task_id, e));
            continue;
        }
        let remote = socket.remote_endpoint();
        let live = HTTP_LIVE.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        if live >= WEB_TASK_POOL_SIZE as u8 {
            ap_log::emit(format_args!("tcp full {:?} live={}/{}", remote, live, WEB_TASK_POOL_SIZE));
        } else {
            ap_log::emit(format_args!(
                "tcp+ {:?} live={}/{}",
                remote, live, WEB_TASK_POOL_SIZE
            ));
        }
        let _ = picoserve::Server::new(app, config, &mut http_buffer)
            .serve(socket)
            .await;
        let live = HTTP_LIVE.fetch_sub(1, Ordering::Relaxed).saturating_sub(1);
        ap_log::emit(format_args!("tcp- live={}/{}", live, WEB_TASK_POOL_SIZE));
    }
}

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    let p = embassy_rp::init(Default::default());

    // USB logging must start before cyw43 init so a hang/panic is still visible.
    spawner.spawn(logger_task(p.USB).unwrap());
    {
        let mut cfg = embassy_rp::uart::Config::default();
        cfg.baudrate = 115_200;
        let uart = embassy_rp::uart::UartTx::new(p.UART1, p.PIN_8, p.DMA_CH2, Irqs, cfg);
        spawner.spawn(ap_log::uart_log_task(uart).unwrap());
    }
    Timer::after_millis(200).await;
    ap_log::emit(format_args!(
        "boot sim={} portal={}",
        cfg!(feature = "simulate"),
        PORTAL_URL
    ));

    if let Some(panic_message) = panic_persist::get_panic_message_utf8() {
        loop {
            log::error!("{panic_message}");
            Timer::after_secs(5).await;
        }
    }

    let fw = cyw43::aligned_bytes!("../cyw43-firmware/43439A0.bin");
    let clm = include_bytes!("../cyw43-firmware/43439A0_clm.bin");
    let nvram = cyw43::aligned_bytes!("../cyw43-firmware/nvram_rp2040.bin");

    let pwr = Output::new(p.PIN_23, Level::Low);
    let cs = Output::new(p.PIN_25, Level::High);
    let mut pio = Pio::new(p.PIO0, Irqs);
    let spi = cyw43_pio::PioSpi::new(
        &mut pio.common,
        pio.sm0,
        cyw43_pio::DEFAULT_CLOCK_DIVIDER,
        pio.irq0,
        cs,
        p.PIN_24,
        p.PIN_29,
        dma::Channel::new(p.DMA_CH0, Irqs),
    );

    let state = make_static!(cyw43::State, cyw43::State::new());
    let (net_device, mut control, wifi_runner) = cyw43::new(state, pwr, spi, fw, nvram).await;
    // Runner must be polling before any Control ioctl (init, AP, LED).
    spawner.spawn(wifi_task(wifi_runner).unwrap());
    ap_log::emit(format_args!("cyw43 runner up, loading CLM"));
    control.init(clm).await;
    ap_log::emit(format_args!("cyw43 init done"));

    let scoreboard = SharedScoreboard(make_static!(
        Mutex<Cs, ScoreboardState>,
        Mutex::new(ScoreboardState::default())
    ));
    let control = SharedControl(make_static!(
        Mutex<Cs, Control<'static>>,
        Mutex::new(control)
    ));
    spawner.spawn(heartbeat_task(control, scoreboard).unwrap());

    let (stack, net_runner) = embassy_net::new(
        net_device,
        embassy_net::Config::ipv4_static(embassy_net::StaticConfigV4 {
            address: embassy_net::Ipv4Cidr::new(embassy_net::Ipv4Address::new(192, 168, 0, 1), 24),
            gateway: None,
            dns_servers: Default::default(),
        }),
        make_static!(
            embassy_net::StackResources<NET_SOCKETS>,
            embassy_net::StackResources::new()
        ),
        RoscRng.next_u64(),
    );

    spawner.spawn(net_task(net_runner).unwrap());

    ap_log::emit(format_args!("starting AP '{}'", WIFI_SSID));
    control.0.lock().await.start_ap_open(WIFI_SSID, 6).await;
    ap_log::emit(format_args!(
        "AP up ssid={} ip=192.168.0.1 ch=6 sim={}",
        WIFI_SSID,
        cfg!(feature = "simulate")
    ));

    spawner.spawn(net_services::dhcp_task(stack).unwrap());
    spawner.spawn(net_services::dns_task(stack).unwrap());
    spawner.spawn(net_services::mdns_task(stack).unwrap());

    let io = Io {
        start: Output::new(p.PIN_0, Level::Low),
        home_inc: Output::new(p.PIN_1, Level::Low),
        home_dec: Output::new(p.PIN_2, Level::Low),
        away_inc: Output::new(p.PIN_3, Level::Low),
        away_dec: Output::new(p.PIN_4, Level::Low),
        reset: Output::new(p.PIN_5, Level::Low),
    };

    let channel = make_static!(Channel<Cs, Command, 8>, Channel::new());

    spawner.spawn(board_task(io, control, scoreboard, channel.receiver()).unwrap());

    #[cfg(feature = "simulate")]
    spawner.spawn(timer_task(scoreboard).unwrap());

    #[cfg(not(feature = "simulate"))]
    {
        let mut uart_config = embassy_rp::uart::Config::default();
        uart_config.baudrate = 38400;
        let rx = embassy_rp::uart::UartRx::new(p.UART0, p.PIN_17, Irqs, p.DMA_CH1, uart_config);
        spawner.spawn(read_serial(rx, scoreboard).unwrap());
    }

    let app = make_static!(
        AppRouter<AppProps>,
        AppProps {
            cmd: channel.sender(),
            scoreboard,
        }
        .build_app()
    );

    let config = make_static!(
        picoserve::Config,
        picoserve::Config::new(picoserve::Timeouts {
            start_read_request: Duration::from_secs(2),
            persistent_start_read_request: Duration::from_secs(1),
            read_request: Duration::from_secs(1),
            write: Duration::from_secs(1),
        })
        .close_connection_after_response()
    );

    for task_id in 0..WEB_TASK_POOL_SIZE {
        spawner.spawn(web_task(task_id, stack, app, config).unwrap());
    }

    loop {
        Timer::after_secs(60).await;
    }
}
