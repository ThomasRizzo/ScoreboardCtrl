# ScoreboardCtrl

Embassy firmware for a Raspberry Pi Pico W that drives a GameCraft SK2229R tabletop scoreboard. The Pico runs an open Wi-Fi AP named **Scoreboard** (`192.168.0.1`) and serves the phone UI itself (`index.html` is compiled in). Default period clock is **7:30**.

`just` recipes are the hardware build (`--no-default-features`: GPIO pulses + UART time). `cargo` without that flag builds **simulate** (software clock and scores). Do not treat a default `cargo build` as the wired image.

## Do not

- Pulse or reuse **GP1** as a blinky. It is start/stop. Onboard LED is CYW43 gpio 0.
- Flash, OTA, or `probe-rs` unless asked. `just program` overwrites the bootloader. `just uart-test` does too. Restore with `just program-ota`.
- Grow the ACTIVE image past **896 KiB**. Check with `just size` after dependency changes.
- Mirror scores from UART. Home/away live only on the Pico. Hardware `time` and `running` follow UART only.

## Verify

```bash
just test    # check + clippy (hw/sim/ota/uart-test) + bootloader + host tests
```

Host tests cover `src/lib.rs`, `src/decode.rs`, and `src/net_proto.rs`. The phone UI is the firmware binary, not a local web app.

## Map

| Path | Role |
|---|---|
| `src/main.rs` | Embassy tasks, GPIO, HTTP routes |
| `src/net_services.rs` | DHCP, DNS hijack, mDNS |
| `src/ota.rs` | embassy-boot + `POST /api/ota` |
| `bootloader/` | embassy-boot-rp |
| `index.html` | Phone UI |
| `docs/architecture.html` | Tasks and the clock/event flow |
| `justfile` | setup, flash, OTA, probe-rs |
| `README.md` | Pins, API, flash map, OTA |

Dev logs are defmt RTT via a Pico Debug Probe (`2e8a:000c`). USB CDC exists only with `--features usb-log`.
