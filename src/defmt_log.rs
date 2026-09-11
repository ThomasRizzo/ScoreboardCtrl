//! `log` facade → defmt RTT (probe-rs).

struct DefmtLogger;

impl log::Log for DefmtLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        match record.level() {
            log::Level::Error => defmt::error!("{}", defmt::Display2Format(record.args())),
            log::Level::Warn => defmt::warn!("{}", defmt::Display2Format(record.args())),
            log::Level::Info => defmt::info!("{}", defmt::Display2Format(record.args())),
            log::Level::Debug => defmt::debug!("{}", defmt::Display2Format(record.args())),
            log::Level::Trace => defmt::trace!("{}", defmt::Display2Format(record.args())),
        }
    }

    fn flush(&self) {}
}

static LOGGER: DefmtLogger = DefmtLogger;

pub fn init() {
    unsafe {
        let _ = log::set_logger_racy(&LOGGER);
        log::set_max_level_racy(log::LevelFilter::Info);
    }
}
