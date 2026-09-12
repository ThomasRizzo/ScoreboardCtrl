//! HTTP OTA via embassy-boot DFU partition.
//!
//! `POST /api/ota` with `Content-Type: application/octet-stream` (or raw body)
//! and `Content-Length` set to the ACTIVE-partition firmware image size.
//! On success the device marks the DFU image and soft-resets so the bootloader
//! can swap. Truncated or failed uploads leave state as Booted (no swap).
//! The new image must call `mark_booted` only after AP + HTTP are up, or the
//! next reset reverts the swap.

use core::cell::{Cell, RefCell};

use embassy_boot_rp::{AlignedBuffer, BlockingFirmwareUpdater, FirmwareUpdaterConfig};
use embassy_rp::{
    flash::{Blocking, Flash, ERASE_SIZE, WRITE_SIZE},
    peripherals::FLASH,
    watchdog::Watchdog,
};
use embassy_sync::{
    blocking_mutex::{
        raw::{CriticalSectionRawMutex, NoopRawMutex},
        Mutex as BlockingMutex,
    },
    mutex::Mutex,
};
use embassy_time::{Duration, Timer};
use picoserve::{
    io::Read,
    request::Request,
    response::{IntoResponse, ResponseWriter, StatusCode},
    routing::RequestHandlerService,
    ResponseSent,
};
use portable_atomic::{AtomicBool, Ordering};

use crate::ap_log;

pub const FLASH_SIZE: usize = 2 * 1024 * 1024;
/// ACTIVE partition from memory.x — max OTA payload.
pub const ACTIVE_CAPACITY: usize = 896 * 1024;
/// DFU partition size (ACTIVE + one 4 KiB erase page of swap scratch).
/// The extra page is not firmware capacity; do not accept uploads that large.
pub const DFU_CAPACITY: usize = ACTIVE_CAPACITY + 4 * 1024;
const _: () = assert!(DFU_CAPACITY == 900 * 1024);

pub type SharedFlash =
    &'static BlockingMutex<NoopRawMutex, RefCell<Flash<'static, FLASH, Blocking, FLASH_SIZE>>>;
pub type SharedWatchdog = &'static Mutex<CriticalSectionRawMutex, Watchdog>;

static OTA_BUSY: AtomicBool = AtomicBool::new(false);
static OTA_RESET: AtomicBool = AtomicBool::new(false);

/// True after a successful OTA; main loop soft-resets once the HTTP response can flush.
pub fn reset_pending() -> bool {
    OTA_RESET.load(Ordering::Relaxed)
}

pub fn mark_booted(flash: SharedFlash) {
    let mut aligned = AlignedBuffer([0; WRITE_SIZE]);
    let config = FirmwareUpdaterConfig::from_linkerfile_blocking(flash, flash);
    let mut updater = BlockingFirmwareUpdater::new(config, &mut aligned.0);
    match updater.mark_booted() {
        Ok(()) => {
            log::info!("embassy-boot: mark_booted ok");
            ap_log::emit(format_args!("boot: mark_booted ok"));
        }
        Err(e) => {
            log::warn!("embassy-boot: mark_booted {:?}", e);
            ap_log::emit(format_args!("boot: mark_booted err"));
            let _ = e;
        }
    }
}

#[derive(Clone, Copy)]
pub struct OtaService {
    pub flash: SharedFlash,
    pub watchdog: SharedWatchdog,
}

enum OtaReply {
    Ok,
    Busy,
    BadRequest(&'static str),
    PayloadTooLarge,
    Failed(&'static str),
}

impl IntoResponse for OtaReply {
    async fn write_to<R: Read, W: ResponseWriter<Error = R::Error>>(
        self,
        connection: picoserve::response::Connection<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        match self {
            Self::Ok => {
                (StatusCode::OK, "OK\n")
                    .write_to(connection, response_writer)
                    .await
            }
            Self::Busy => {
                (StatusCode::SERVICE_UNAVAILABLE, "OTA in progress\n")
                    .write_to(connection, response_writer)
                    .await
            }
            Self::BadRequest(msg) => {
                (StatusCode::BAD_REQUEST, msg)
                    .write_to(connection, response_writer)
                    .await
            }
            Self::PayloadTooLarge => {
                (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "image too large for ACTIVE\n",
                )
                    .write_to(connection, response_writer)
                    .await
            }
            Self::Failed(msg) => {
                (StatusCode::INTERNAL_SERVER_ERROR, msg)
                    .write_to(connection, response_writer)
                    .await
            }
        }
    }
}

impl RequestHandlerService for OtaService {
    async fn call_request_handler_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        _state: &(),
        _path_parameters: (),
        mut request: Request<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        if OTA_BUSY
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            ap_log::emit(format_args!("OTA rejected: busy"));
            return OtaReply::Busy
                .write_to(request.body_connection.finalize().await?, response_writer)
                .await;
        }

        let result = self.run_ota(&mut request).await;
        OTA_BUSY.store(false, Ordering::Release);

        let reply = match result {
            Ok(()) => {
                OTA_RESET.store(true, Ordering::Release);
                OtaReply::Ok
            }
            Err(OtaError::BadRequest(m)) => OtaReply::BadRequest(m),
            Err(OtaError::TooLarge) => OtaReply::PayloadTooLarge,
            Err(OtaError::Truncated) => OtaReply::BadRequest("truncated upload\n"),
            Err(OtaError::Flash) => OtaReply::Failed("flash write failed\n"),
            Err(OtaError::Io) => OtaReply::Failed("read failed\n"),
        };

        reply
            .write_to(request.body_connection.finalize().await?, response_writer)
            .await
    }
}

enum OtaError {
    BadRequest(&'static str),
    TooLarge,
    Truncated,
    Flash,
    Io,
}

impl OtaService {
    async fn run_ota<R: Read>(&self, request: &mut Request<'_, R>) -> Result<(), OtaError> {
        let content_len = request.body_connection.content_length();
        if content_len == 0 {
            return Err(OtaError::BadRequest("empty body; set Content-Length\n"));
        }
        if content_len > ACTIVE_CAPACITY {
            return Err(OtaError::TooLarge);
        }

        ap_log::emit(format_args!("OTA start {} bytes", content_len));
        log::info!("OTA start {} bytes", content_len);

        let mut aligned = AlignedBuffer([0; WRITE_SIZE]);
        let config = FirmwareUpdaterConfig::from_linkerfile_blocking(self.flash, self.flash);
        let mut updater = BlockingFirmwareUpdater::new(config, &mut aligned.0);

        let mut offset = 0usize;
        let mut chunk = [0u8; ERASE_SIZE];
        let mut last_log = 0usize;
        // Progress counter for CDC without floating point.
        let progress = Cell::new(0usize);

        {
            let mut body = request.body_connection.body().reader();
            while offset < content_len {
                let want = core::cmp::min(chunk.len(), content_len - offset);
                match read_exact_or_eof(&mut body, &mut chunk[..want]).await {
                    Ok(0) => {
                        ap_log::emit(format_args!("OTA truncated at {}/{}", offset, content_len));
                        log::warn!("OTA truncated at {}/{}", offset, content_len);
                        return Err(OtaError::Truncated);
                    }
                    Ok(n) => {
                        // Feed watchdog before blocking flash work (single-threaded executor).
                        self.watchdog.lock().await.feed(Duration::from_secs(8));

                        if let Err(_e) = updater.write_firmware(offset, &chunk[..n]) {
                            log_fw_err("write");
                            return Err(OtaError::Flash);
                        }

                        offset += n;
                        progress.set(offset);

                        if offset - last_log >= 32 * 1024 || offset == content_len {
                            last_log = offset;
                            let pct = (offset as u64 * 100 / content_len as u64) as u32;
                            ap_log::emit(format_args!("OTA {}/{} ({}%)", offset, content_len, pct));
                            log::info!("OTA {}/{} ({}%)", offset, content_len, pct);
                        }

                        // Let other tasks (watchdog feeder, net) run.
                        Timer::after_millis(1).await;
                    }
                    Err(()) => {
                        ap_log::emit(format_args!("OTA read error at {}", offset));
                        return Err(OtaError::Io);
                    }
                }
            }
        }

        if offset != content_len {
            return Err(OtaError::Truncated);
        }

        self.watchdog.lock().await.feed(Duration::from_secs(8));

        if let Err(_e) = updater.mark_updated() {
            log_fw_err("mark_updated");
            return Err(OtaError::Flash);
        }

        ap_log::emit(format_args!("OTA mark_updated ok; resetting"));
        log::info!("OTA complete, soft reset pending");
        let _ = progress;
        Ok(())
    }
}

async fn read_exact_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<usize, ()> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]).await {
            Ok(0) => return Ok(filled),
            Ok(n) => filled += n,
            Err(_) => return Err(()),
        }
    }
    Ok(filled)
}

fn log_fw_err(what: &str) {
    log::error!("OTA {} failed", what);
    ap_log::emit(format_args!("OTA {} failed", what));
}
