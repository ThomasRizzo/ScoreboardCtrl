#![no_std]
#![no_main]

use core::cell::RefCell;

use cortex_m_rt::{entry, exception};
use defmt_rtt as _;
use embassy_boot_rp::*;
use embassy_sync::blocking_mutex::Mutex;
use embassy_time::Duration;
use panic_probe as _;

const FLASH_SIZE: usize = 2 * 1024 * 1024;
/// RP2040 WATCHDOG.CTRL — ENABLE is bit 30.
const WATCHDOG_CTRL: *mut u32 = 0x4005_8000 as *mut u32;

#[entry]
fn main() -> ! {
    let p = embassy_rp::init(Default::default());

    let flash = WatchdogFlash::<FLASH_SIZE>::start(p.FLASH, p.WATCHDOG, Duration::from_secs(8));
    let flash = Mutex::new(RefCell::new(flash));

    let config = BootLoaderConfig::from_linkerfile_blocking(&flash, &flash, &flash);
    let active_offset = config.active.offset();
    let start = embassy_rp::flash::FLASH_BASE as u32 + active_offset;
    let bl: BootLoader = BootLoader::prepare(config);
    defmt::info!("embassy-boot ready active=0x{:08x}", start);

    // WatchdogFlash leaves the 8s WD running. Stop it so the app can start
    // its own feeder; a leftover WD resets before cyw43/WiFi come up.
    stop_watchdog();

    // Mask leftover IRQs from embassy_rp::init (TIMER, DMA, …) before VTOR
    // switches. Do **not** leave PRIMASK set: cortex-m-rt in the app never
    // cpsie, so the executor would hang on the first await.
    unsafe {
        let nvic = &*cortex_m::peripheral::NVIC::PTR;
        nvic.icer[0].write(0xFFFF_FFFF);
        nvic.icpr[0].write(0xFFFF_FFFF);
        cortex_m::interrupt::enable();
        bl.load(start)
    }
}

fn stop_watchdog() {
    unsafe {
        let v = core::ptr::read_volatile(WATCHDOG_CTRL);
        core::ptr::write_volatile(WATCHDOG_CTRL, v & !(1 << 30));
    }
}

#[no_mangle]
#[link_section = ".HardFault.user"]
unsafe extern "C" fn HardFault() {
    // Halt so probe-rs can inspect. sys_reset here hid jump faults in a loop.
    loop {
        cortex_m::asm::bkpt();
    }
}

#[exception]
unsafe fn DefaultHandler(_: i16) -> ! {
    loop {
        cortex_m::asm::bkpt();
    }
}
