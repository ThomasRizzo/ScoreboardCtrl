# ScoreboardCtrl

Web UI for a GameCraft [SK2229R](https://www.amazon.com/BSN-Multisport-Indoor-Tabletop-Scoreboard/dp/B003SFP4CI) tabletop scoreboard. The included remote cannot reset the clock; this Pico W firmware can, and it drives start/stop and home/away scores from a phone.

Embassy firmware: the Pico broadcasts an open **Scoreboard** AP and serves the UI itself.

Join **Scoreboard**, then open **`http://192.168.0.1/`**. That IP always stays on the Pico, even if the phone still has mobile data. `http://scoreboard.local/` (mDNS) and `http://scoreboard.com/` (DNS hijack) also work when the phone uses the AP’s DNS.

Default period clock is **7:30** (polo).

## Modes

| Build | What it does |
|---|---|
| **simulate** (default) | Software timer and scores, onboard LED. Use when the SK2229R is not wired. |
| **hardware** (`--no-default-features`) | 50 ms pulses on GP0–GP5 through a CD74HCT4066, time from UART0/GP17 at 38400. |

Do not use GP0 as a blinky: that pin is the start/stop pulse.

## Phone UI

- Compact two-column home/away scores, timer, set-clock, LED below the fold
- Optimistic +/- so taps feel instant; 280 ms lockout so one tap is +1
- iOS captive-portal sheet usually opens the UI on join
- Android often will not pop a sign-in sheet; type `http://192.168.0.1` (include `http://`)
- **Refresh** reloads `http://192.168.0.1/`

## Limits

| Layer | Limit |
|---|---|
| Wi-Fi stations (CYW43439 AP) | 4 |
| DHCP pool | 192.168.0.10–.17 (8) |
| HTTP at once | 12 TCP workers |
| Idle TCP | dropped after 5 s |
| Each HTTP request | `Connection: close` |

## Hardware

- [GameCraft SK2229R](https://www.amazon.com/BSN-Multisport-Indoor-Tabletop-Scoreboard/dp/B003SFP4CI) scoreboard
- [Raspberry Pi Pico W](https://www.raspberrypi.com/documentation/microcontrollers/raspberry-pi-pico.html#raspberry-pi-pico-w) — AP, UI, button pulses, UART time
- [CD74HCT4066](https://www.ti.com/lit/ds/symlink/cd74hct4066.pdf) analog switch (scoreboard buttons are 5 V, originally CD4066BE)
- [MAX3232](https://www.ti.com/lit/ds/symlink/max3232.pdf) RS-232 (scoreboard) to Pico TTL on GP17
- Custom PCB: [ScoreBoardPcb](https://github.com/ThomasRizzo/ScoreBoardPcb)

| Pin | Function |
|---|---|
| GP0 | Start / stop pulse |
| GP1 | Home + |
| GP2 | Home − |
| GP3 | Away + |
| GP4 | Away − |
| GP5 | Hardware reset pulse |
| GP17 | UART0 RX ← SK2229R TX, 38400 (hardware build, RX only) |
| USB CDC | VSP: all logs out, `ENTERBOOTLOADER` in (`just logs`, typically `/dev/ttyACM0`) |
| Onboard LED | CYW43 gpio 0 (not GP0) |

SK2229R packet (6 bytes, UART 38400): `00 | min | sec | shotclock | 3F | crc`. Time bytes decode as `(0xFF - b) >> 1`. Frames are accepted only with the `3F` marker and minutes ≤ 99 / seconds ≤ 59; a `0x00` CRC or shot-clock byte does not resync the parser. Hardware builds dump every UART0 byte as hex on USB CDC (`just logs`) so unused fields (shot clock, CRC) and any other traffic are visible. Hardware `running` follows the clock: time remaining decreasing means running, `00:00` or a frozen display means stopped (the board’s own start/stop button is independent of GP0). Scores are 0–99.

The AP is open (no password). Anyone on **Scoreboard** can change the clock and scores.

## Build and flash

Nightly Rust and `thumbv6m-none-eabi` (`rust-toolchain.toml`). The firmware uses **embassy-boot** (A/B): a small bootloader plus ACTIVE/DFU/STATE partitions. First-time bring-up is still UF2 over USB BOOTSEL (`elf2uf2-rs`); later updates can use HTTP OTA on the Scoreboard AP.

```bash
just setup              # toolchain, RP2040 target, elf2uf2-rs
just test               # check + clippy (sim/hw) + bootloader + host tests
just flash-bootloader   # once: hold BOOTSEL, copy bootloader UF2
just flash              # simulate app via ENTERBOOTLOADER + UF2
just flash-hw           # hardware app
just ota                # POST release .bin to http://192.168.0.1/api/ota
just logs               # USB CDC (VSP)
just --list
```

Release ELF: `target/thumbv6m-none-eabi/release/scoreboard-ctrl`.  
OTA artifact: `target/thumbv6m-none-eabi/release/scoreboard-ctrl.bin` (`just ota-artifact`).

### Flash map (Pico W 2 MiB)

| Region | Origin | Size | Role |
|---|---|---|---|
| BOOT2 + bootloader | `0x10000000` | 32 KiB | stage2 + embassy-boot |
| STATE | `0x10008000` | 4 KiB | swap / trial-boot state |
| ACTIVE | `0x10009000` | 896 KiB | running app |
| DFU | `0x100E9000` | 900 KiB | staged OTA image (ACTIVE + 4 KiB) |

Measured release ACTIVE image (simulate, with cyw43 firmware + UI) is roughly **~460 KiB**; ACTIVE leaves ~400 KiB headroom. Re-check with `just size` / `just ota-artifact` after dependency bumps.

### First-time USB flash (boxer-86)

1. `just doctor`
2. Hold **BOOTSEL**, plug USB, `just flash-bootloader`
3. Hold **BOOTSEL** again (or wait for remount), `just flash` (or `just flash-hw`)
4. `just logs` — expect `mark_booted ok` and the Scoreboard AP

### OTA (after the app is running)

On a WiFi-only Linux host with NetworkManager (`nmcli`), `just ota` will:

1. Remember the current WiFi connection/SSID
2. Join the open **Scoreboard** AP
3. Wait for `http://192.168.0.1/`
4. `POST` the `.bin` to `/api/ota`
5. Switch back to the previous WiFi (even if the upload fails)

```bash
just ota                 # simulate image + WiFi hop
just ota-hw              # hardware image + WiFi hop
OTA_SKIP_WIFI=1 just ota # already on Scoreboard / skip hopping
```

Phone / UI: stay on **Scoreboard**, open `http://192.168.0.1/`, use the OTA file upload.

Requires `nmcli` and `curl`. Pico must already be running (AP up). USB CDC (`just logs`) still works while the laptop is on Scoreboard.

Progress is logged on USB CDC. On success the device `mark_updated`s and soft-resets; embassy-boot swaps DFU→ACTIVE and the new image must call `mark_booted` (it does on startup) or the next reset rolls back.

Concurrent OTAs are rejected (`503`). Truncated uploads do **not** call `mark_updated`, so the running image stays Booted and will not swap.


### Recovery

- Soft brick / bad trial image: power-cycle; embassy-boot rolls back if `mark_booted` never ran.
- USB BOOTSEL still works: `ENTERBOOTLOADER` on CDC or hold BOOTSEL, then reflash bootloader and/or app UF2s.
- Do not flash a pre-embassy-boot UF2 without restoring the bootloader first — a whole-flash image can overwrite the boot partitions.

## HTTP API

```
GET  /                         UI
GET  /api/status               JSON: time, running, home, away, led, sim, ver, git, date
POST /api/ctrl/start
POST /api/ctrl/stop
POST /api/ctrl/start-stop
POST /api/ctrl/reset
POST /api/ctrl/home-inc
POST /api/ctrl/home-dec
POST /api/ctrl/away-inc
POST /api/ctrl/away-dec
POST /api/ctrl/scores-zero
POST /api/timer/set/{min}/{sec}   (simulate only; hardware Reset pulses GP5)
POST /led/on
POST /led/off
```

Unknown GETs (captive-portal probes such as `/generate_204`) 302 to `http://192.168.0.1/`. After the UI has been served, Apple hotspot-detect probes return the Success page so the iOS sheet can show Done.

`GET /api/status` example:

```json
{"time":"07:30","running":false,"home":0,"away":0,"led":false,"sim":true,"ver":"0.2.1","git":"abc1234","date":"2026-09-05"}
```

## Layout

- `src/main.rs` — Embassy tasks, GPIO/LED, HTTP routes, USB BOOTSEL helper
- `src/ota.rs` — embassy-boot `mark_booted` + `POST /api/ota`
- `bootloader/` — embassy-boot-rp bootloader workspace member
- `memory-app.x` — app flash map (copied to linker via `build.rs`; kept out of repo root name `memory.x` so it cannot shadow `bootloader/memory.x`)
- `src/net_services.rs` — DHCP, DNS hijack, mDNS (`scoreboard.local` / `sb.local`)
- `src/ap_log.rs` — AP/captive trace on USB CDC (VSP)
- `src/lib.rs` / `src/decode.rs` / `src/net_proto.rs` — host-tested clock decode and DHCP/DNS helpers
- `index.html` — phone UI (compiled into the firmware)
- `justfile` — setup, flash, logs
- `cyw43-firmware/` — CYW43439 firmware + Pico W NVRAM

## Network

Pico is `192.168.0.1/24`. DHCP hands out `.10`–`.17`, DNS and gateway point at the Pico, and A records hijack to `192.168.0.1`. DHCP option 114/160 advertise `http://192.168.0.1/` as the captive portal URL.

## References

- [Embassy](https://embassy.dev/)
- [picoserve](https://github.com/sammhicks/picoserve)
- [Pico W](https://www.raspberrypi.com/documentation/microcontrollers/raspberry-pi-pico.html)
- [cyw43](https://github.com/embassy-rs/embassy/tree/main/cyw43)
- [embassy-boot](https://github.com/embassy-rs/embassy/tree/main/embassy-boot) (RP example bootloader/application)
