#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use cyw43::Control;
use cyw43_pio::PioSpi;
use embassy_rp::{
    gpio::{Level, Output},
    peripherals::{DMA_CH0, PIN_0, PIN_1, PIN_2, PIN_23, PIN_25, PIN_3, PIN_4, PIN_5, PIO0},
    pio::Pio,
};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::Duration;
use embassy_time::Timer;
use panic_halt as _;
use picoserve::{
    response::DebugValue,
    routing::{get, parse_path_segment},
};
use rand::Rng;
use static_cell::make_static;

use picoserve::extract::State;

const WIFI_SSID: &str = "Scoreboard";

#[derive(serde::Deserialize)]
struct FormValue {
    a: i32,
    b: heapless::String<32>,
}

embassy_rp::bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => embassy_rp::pio::InterruptHandler<embassy_rp::peripherals::PIO0>;
    USBCTRL_IRQ => embassy_rp::usb::InterruptHandler<embassy_rp::peripherals::USB>;
});

#[embassy_executor::task]
async fn logger_task(usb: embassy_rp::peripherals::USB) {
    let driver = embassy_rp::usb::Driver::new(usb, Irqs);
    embassy_usb_logger::run!(1024, log::LevelFilter::Info, driver);
}

#[embassy_executor::task]
async fn wifi_task(
    runner: cyw43::Runner<
        'static,
        Output<'static, PIN_23>,
        PioSpi<'static, PIN_25, PIO0, 0, DMA_CH0>,
    >,
) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn net_task(stack: &'static embassy_net::Stack<cyw43::NetDriver<'static>>) -> ! {
    stack.run().await
}

struct EmbassyTimer;

impl picoserve::Timer for EmbassyTimer {
    type Duration = embassy_time::Duration;
    type TimeoutError = embassy_time::TimeoutError;

    async fn run_with_timeout<F: core::future::Future>(
        &mut self,
        duration: Self::Duration,
        future: F,
    ) -> Result<F::Output, Self::TimeoutError> {
        embassy_time::with_timeout(duration, future).await
    }
}

#[derive(Clone, Copy)]
struct SharedControl(&'static Mutex<CriticalSectionRawMutex, Control<'static>>);

struct IO {
    start: Output<'static, PIN_0>, //start/stop
    home_inc: Output<'static, PIN_1>,
    home_dec: Output<'static, PIN_2>,
    away_inc: Output<'static, PIN_3>,
    away_dec: Output<'static, PIN_4>,
    reset: Output<'static, PIN_5>,
}

#[derive(Clone, Copy)]
struct SharedIO(&'static Mutex<CriticalSectionRawMutex, IO>);

struct AppState {
    shared_control: SharedControl,
    io: SharedIO,
}

impl picoserve::extract::FromRef<AppState> for SharedControl {
    fn from_ref(state: &AppState) -> Self {
        state.shared_control
    }
}

impl picoserve::extract::FromRef<AppState> for SharedIO {
    fn from_ref(state: &AppState) -> Self {
        state.io
    }
}

type AppRouter = impl picoserve::routing::PathRouter<AppState>;

const WEB_TASK_POOL_SIZE: usize = 8;

#[embassy_executor::task(pool_size = WEB_TASK_POOL_SIZE)]
async fn web_task(
    id: usize,
    stack: &'static embassy_net::Stack<cyw43::NetDriver<'static>>,
    app: &'static picoserve::Router<AppRouter, AppState>,
    config: &'static picoserve::Config<Duration>,
    state: AppState,
) -> ! {
    let mut rx_buffer = [0; 1024];
    let mut tx_buffer = [0; 1024];

    loop {
        let mut socket = embassy_net::tcp::TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);

        log::info!("{id}: Listening on TCP:80...");
        if let Err(e) = socket.accept(80).await {
            log::warn!("{id}: accept error: {:?}", e);
            continue;
        }

        log::info!(
            "{id}: Received connection from {:?}",
            socket.remote_endpoint()
        );

        let (socket_rx, socket_tx) = socket.split();

        match picoserve::serve_with_state(
            app,
            EmbassyTimer,
            config,
            &mut [0; 2048],
            socket_rx,
            socket_tx,
            &state,
        )
        .await
        {
            Ok(handled_requests_count) => {
                log::info!(
                    "{handled_requests_count} requests handled from {:?}",
                    socket.remote_endpoint()
                );
            }
            Err(err) => log::error!("{err:?}"),
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    let p = embassy_rp::init(Default::default());

    spawner.must_spawn(logger_task(p.USB));

    let fw = include_bytes!("../cyw43-firmware/43439A0.bin");
    let clm = include_bytes!("../cyw43-firmware/43439A0_clm.bin");

    let pwr = Output::new(p.PIN_23, Level::Low);
    let cs = Output::new(p.PIN_25, Level::High);
    let mut pio = Pio::new(p.PIO0, Irqs);
    let spi = cyw43_pio::PioSpi::new(
        &mut pio.common,
        pio.sm0,
        pio.irq0,
        cs,
        p.PIN_24,
        p.PIN_29,
        p.DMA_CH0,
    );

    let state = make_static!(cyw43::State::new());
    let (net_device, mut control, runner) = cyw43::new(state, pwr, spi, fw).await;
    spawner.must_spawn(wifi_task(runner));

    control.init(clm).await;

    let stack = &*make_static!(embassy_net::Stack::new(
        net_device,
        embassy_net::Config::ipv4_static(embassy_net::StaticConfigV4 {
            address: embassy_net::Ipv4Cidr::new(embassy_net::Ipv4Address::new(192, 168, 0, 10), 16),
            gateway: None,
            dns_servers: Default::default(),
        }),
        make_static!(embassy_net::StackResources::<WEB_TASK_POOL_SIZE>::new()),
        embassy_rp::clocks::RoscRng.gen(),
    ));

    spawner.must_spawn(net_task(stack));

    //Try to connect forever (AP and PiPicoW powered on at same time, so need to wait a minute for AP to boot)
    loop {
        control.gpio_set(0, true).await;
        match control.join_open(WIFI_SSID).await {
            Ok(_) => break,
            Err(_) => {
                control.gpio_set(0, false).await;
                Timer::after_millis(500).await;
            }
        }
    }

    fn make_app() -> picoserve::Router<AppRouter, AppState> {
        picoserve::Router::new()
            //.route("/", get(|| async move { "Hello World" }))
            .route(
                "/",
                get(|| picoserve::response::File::html(include_str!("index.html"))).post(
                    |picoserve::extract::Form(FormValue { a, b })| {
                        log::info!("Received: {:?} {:?}", a, b);
                        picoserve::response::Redirect::to("/")
                    },
                ),
            )
            .route(
                ("/set", parse_path_segment()),
                get(
                    |led_is_on, State(SharedControl(control)): State<SharedControl>| async move {
                        control.lock().await.gpio_set(0, led_is_on).await;
                        DebugValue(led_is_on)
                    },
                ),
            )
            .route(
                ("/ctrl", parse_path_segment()),
                get(|id, State(SharedIO(io)): State<SharedIO>| async move {
                    match id {
                        //TODO: make id an enum
                        0 => {
                            let mut io = io.lock().await;
                            io.start.set_high();
                            Timer::after_millis(50).await;
                            io.start.set_low();
                            DebugValue("Start/Stop Button")
                        }
                        _ => DebugValue("Unknown function")
                    }

                }),
            )
    }

    let app = make_static!(make_app());

    let config = make_static!(picoserve::Config::new(picoserve::Timeouts {
        start_read_request: Some(Duration::from_secs(60)),
        read_request: Some(Duration::from_secs(60)),
        write: Some(Duration::from_secs(1)),
    })
    .keep_connection_alive());

    let shared_control = SharedControl(make_static!(Mutex::new(control)));
    let io = IO {
        start: Output::new(p.PIN_0, Level::Low),
        home_inc: Output::new(p.PIN_1, Level::Low),
        home_dec: Output::new(p.PIN_2, Level::Low),
        away_inc: Output::new(p.PIN_3, Level::Low),
        away_dec: Output::new(p.PIN_4, Level::Low),
        reset: Output::new(p.PIN_5, Level::Low),

    };
    let io = SharedIO(make_static!(Mutex::new(io)));

    for id in 0..WEB_TASK_POOL_SIZE {
        spawner.must_spawn(web_task(
            id,
            stack,
            app,
            config,
            AppState { shared_control, io },
        ));
    }
}
