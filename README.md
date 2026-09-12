# ScoreboardCtrl

Web UI for a GameCraft [SK2229R](https://www.amazon.com/BSN-Multisport-Indoor-Tabletop-Scoreboard/dp/B003SFP4CI) tabletop scoreboard. The included remote cannot reset the clock; this Pico W firmware can, and it drives start/stop and home/away scores from a phone.

Embassy firmware: the Pico broadcasts an open **Scoreboard** AP and serves the UI itself.

Join **Scoreboard**, then open **`http://192.168.0.1/`**. That IP always stays on the Pico, even if the phone still has mobile data. `http://scoreboard.local/` (mDNS) and `http://scoreboard.com/` (DNS hijack) also work when the phone uses the AP’s DNS.

Default period clock is **7:30** (polo).

## Modes

| Build | What it does |
|---|---|
| **hardware** (`just` default, `--no-default-features`) | 50 ms pulses on GP1–GP11 / GP13 through a CD74HCT4066, time from UART1 GP21 RX at 38400. |
| **simulate** (`just …-sim`, cargo feature `simulate`) | Software timer and scores, onboard LED. Use when the SK2229R is not wired. |

`cargo build` still enables `simulate` unless you pass `--no-default-features`. `just` recipes pass that flag for you.

Do not use GP1 as a blinky: that pin is the start/stop pulse. GP0 is unused.

## Phone UI

- Compact two-column home/away scores, timer, set-clock; **Dev** reveals onboard LED and OTA file upload
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
| Idle TCP | socket timeout 60 s |
| Each HTTP request | `Connection: close`; OTA body reads allowed 30 s |

## Hardware

- [GameCraft SK2229R](https://www.amazon.com/BSN-Multisport-Indoor-Tabletop-Scoreboard/dp/B003SFP4CI) scoreboard
- [Raspberry Pi Pico W](https://www.raspberrypi.com/documentation/microcontrollers/raspberry-pi-pico.html#raspberry-pi-pico-w) — AP, UI, button pulses, UART time
- [CD74HCT4066](https://www.ti.com/lit/ds/symlink/cd74hct4066.pdf) analog switch (scoreboard buttons are 5 V, originally CD4066BE)
- [MAX3232](https://www.ti.com/lit/ds/symlink/max3232.pdf) RS-232 (scoreboard) to Pico TTL UART1 (GP20 TX / GP21 RX)
- Custom PCB: [ScoreBoardPcb](https://github.com/ThomasRizzo/ScoreBoardPcb)

| Pin | Function |
|---|---|
| GP0 | unused |
| GP1 | Start / stop |
| GP2 | Home + |
| GP3 | Home − |
| GP4 | Away + |
| GP5 | Away − |
| GP6 | Reset (time) |
| GP7 | Min + |
| GP8 | Min − |
| GP9 | Sec + |
| GP10 | Sec − |
| GP11 | Clear (score) |
| GP13 | AUX |
| GP20 | UART1 TX → MAX3232 (idle in hardware build; TX used by `just uart-test`) |
| GP21 | UART1 RX ← SK2229R TX, 38400 (hardware build) |
| USB CDC | `--features usb-log` only: VSP logs + `ENTERBOOTLOADER` (`just logs`); uart-test console (`just uart-console`) |
| SWD / Pico Debug Probe | defmt RTT (`just program`, `just attach-ota`) |
| Onboard LED | CYW43 gpio 0 (not GP0); UI under **Dev** |

SK2229R packet (6 bytes, UART 38400): `60 | chk | unk0 | minutes | seconds | unk1`. Minutes and seconds are raw binary. Frames start with `0x60`; minutes ≤ 99 / seconds ≤ 59. `chk` / `unk0` / `unk1` are still being reverse-engineered. Hardware builds dump every UART1 byte as hex on the log facade (defmt RTT, or USB CDC with `--features usb-log`) so unused fields (shot clock, CRC) and any other traffic are visible. The SK2229R’s own buttons can change time and start/stop at any time. Hardware `time` and `running` follow UART only: stopped if MM:SS is `00:00` or unchanged for ≥2 s; otherwise running. Start/stop GPIO is a toggle and is pulsed only when that UART flag disagrees with the request — it does not write `running`. Scores are 0–99 and exist only on the Pico (UART does not carry home/away; a score change on the board cannot be mirrored).

The AP is open (no password). Anyone on **Scoreboard** can change the clock and scores.

## Build and flash

Nightly Rust and `thumbv6m-none-eabi` (`rust-toolchain.toml`). Default firmware is a standalone image (BOOT2 at `0x10000000`). Dev flashes go through a **Pico Debug Probe** (CMSIS-DAP) with **defmt RTT**.

```bash
just setup              # toolchain, RP2040 target, elf2uf2-rs, probe-rs
just test               # check + clippy (hw/sim/ota) + bootloader + host tests
just uart-test          # SK2229R RS-232 probe (USB CDC + UART1 TX/RX; no Wi-Fi)
just uart-console       # type `tx <hex>` / `probe` / `replay` on the Pico VSP
just program            # standalone hardware: probe-rs flash + defmt RTT
just program-ota        # bootloader + hardware ACTIVE, boot through embassy-boot, defmt
just attach-ota         # re-attach defmt (no flash)
just ota                # HTTP POST hardware ACTIVE .bin (AP must be up)
just flash              # hardware standalone UF2 via ENTERBOOTLOADER
just program-sim        # software clock (no SK2229R)
just --list
```

### UART probe (RS-232 TX)

Standalone image in `examples/uart-test.rs`: USB CDC + UART1 only (no AP, OTA, or GPIO). Use it to send bytes on GP20 and watch GP21.

```bash
just uart-test          # probe-rs download + reset (overwrites bootloader)
just uart-console       # another terminal: type tx / blast / hunt / listen / replay
```

| Command | Action |
|---|---|
| `tx 60 7a 00 02 06 00` | Send those bytes on UART1 TX |
| `replay` | Resend the last captured `0x60` clock frame |
| `probe` | Canned bursts (replay + checksum variants, `00`/`ff`/`55`/`aa`/`60`, `AT`, a `07:30` frame) |
| `help` | Print the table |
| `ENTERBOOTLOADER` | UF2 BOOTSEL |

Re-flash production with `just program-ota` when finished.

Pico Debug Probe udev (once): `sudo cp scripts/69-probe-rs.rules /etc/udev/rules.d/` then reload udev. Probe selector is `2e8a:000c`.

embassy-boot HTTP OTA is `--features ota` (ACTIVE image at `0x10009000`). Use `just program-ota`, not a combined UF2, for first bring-up.

Release ELF: `target/thumbv6m-none-eabi/release/scoreboard-ctrl`.  
OTA artifact: `target/thumbv6m-none-eabi/release/scoreboard-ctrl.bin` (`just ota-artifact`).

### Flash map (Pico W 2 MiB)

| Region | Origin | Size | Role |
|---|---|---|---|
| BOOT2 + bootloader | `0x10000000` | 32 KiB | stage2 + embassy-boot |
| STATE | `0x10008000` | 4 KiB | swap / trial-boot state |
| ACTIVE | `0x10009000` | 896 KiB | running app |
| DFU | `0x100E9000` | 900 KiB | staged OTA image (ACTIVE + 4 KiB swap scratch; max POST is 896 KiB) |

Measured release ACTIVE image (cyw43 firmware + UI) is roughly **~446 KiB**; ACTIVE leaves ~450 KiB headroom. Re-check with `just size` / `just ota-artifact` after dependency bumps. Max HTTP OTA body is **896 KiB** (`ACTIVE_CAPACITY`); larger POSTs are `413`.

### First-time OTA flash (probe-rs)

1. `just doctor` — Debug Probe listed and accessible
2. `just program-ota` — chip-erase, bootloader, ACTIVE app, reset, defmt
3. Expect `AP up ssid=Scoreboard` then `embassy-boot: mark_booted ok`
4. Ctrl-C detaches; firmware keeps running. Re-attach with `just attach-ota`

`just program` (no `-ota`) is the standalone image and **overwrites** the bootloader. Use `just program-ota` again before HTTP OTA.

### First-time USB flash (BOOTSEL, no probe)

1. Hold **BOOTSEL**, plug USB, `just flash-bringup` (bootloader + app in one UF2)
2. Prefer probe-rs for OTA bring-up; a combined UF2 has hung this chip before

Bootloader-only (`just flash-bootloader`) does **not** start WiFi.

### OTA (after the app is running)

On a WiFi-only Linux host with NetworkManager (`nmcli`), `just ota` will:

1. Remember the current WiFi connection/SSID
2. Join the open **Scoreboard** AP
3. Wait for `http://192.168.0.1/`
4. `POST` the `.bin` to `/api/ota`
5. Switch back to the previous WiFi (even if the upload fails)

```bash
just ota                 # hardware image + WiFi hop
just ota-sim             # simulator image + WiFi hop
OTA_SKIP_WIFI=1 just ota # already on Scoreboard / skip hopping
```

Phone / UI: stay on **Scoreboard**, open `http://192.168.0.1/`, tap **Dev**, use the OTA file upload (`just ota-artifact`).

Requires `nmcli` and `curl`. Pico must already be running (AP up). Defmt (`just attach-ota`) still works while the laptop is on Scoreboard.

Progress is logged over defmt (or USB CDC with `usb-log`). On success the device `mark_updated`s and soft-resets; embassy-boot swaps DFU→ACTIVE. The new image calls `mark_booted` only after the Scoreboard AP and HTTP workers are up; if it hangs before that, the next watchdog or power-cycle rolls back.

Concurrent OTAs are rejected (`503`). Truncated uploads and bodies larger than ACTIVE (896 KiB) do **not** call `mark_updated`, so the running image stays Booted and will not swap.


### Recovery

- Soft brick / bad trial image: power-cycle; embassy-boot rolls back if `mark_booted` never ran.
- USB BOOTSEL still works: hold BOOTSEL (or `ENTERBOOTLOADER` on CDC if built with `usb-log`), then reflash bootloader and/or app UF2s. Default probe-rs images have no CDC command.
- Do not flash a pre-embassy-boot UF2 without restoring the bootloader first — a whole-flash image can overwrite the boot partitions.

## HTTP API

```
GET  /                         UI
GET  /api/status               JSON: time, running, home, away, led, sim, ver, git, date, age_ms
POST /api/ctrl/start
POST /api/ctrl/stop
POST /api/ctrl/start-stop
POST /api/ctrl/reset
POST /api/ctrl/home-inc
POST /api/ctrl/home-dec
POST /api/ctrl/away-inc
POST /api/ctrl/away-dec
POST /api/ctrl/scores-zero        (hardware: one pulse on GP11 clear)
POST /api/ctrl/aux                 (hardware: pulse GP13)
POST /api/timer/set/{min}/{sec}   (hardware: GP7–GP10 pulses from current UART time; simulate: set software clock)
POST /api/ota                     (--features ota) raw ACTIVE .bin, max 896 KiB
POST /led/on
POST /led/off
```

Unknown GETs (captive-portal probes such as `/generate_204`) 302 to `http://192.168.0.1/`. After the UI has been served, Apple hotspot-detect probes return the Success page so the iOS sheet can show Done.

`GET /api/status` example:

```json
{"time":"07:30","running":false,"home":0,"away":0,"led":false,"sim":false,"ver":"0.4.1","git":"abc1234","date":"2026-09-10","age_ms":120}
```

## Layout

- `src/main.rs` — Embassy tasks, GPIO/LED, HTTP routes
- `src/ota.rs` — embassy-boot `mark_booted` + `POST /api/ota`
- `src/defmt_log.rs` — `log` facade → defmt RTT (default; not used with `usb-log`)
- `bootloader/` — embassy-boot-rp bootloader workspace member
- `memory-app.x` — OTA ACTIVE flash map (copied via `build.rs` when `--features ota`)
- `memory-standalone.x` — whole-flash map for `just program` (no bootloader)
- `src/net_services.rs` — DHCP, DNS hijack, mDNS (`scoreboard.local` / `sb.local`)
- `src/ap_log.rs` — AP/captive trace via `log`
- `src/lib.rs` / `src/decode.rs` / `src/net_proto.rs` — host-tested clock decode and DHCP/DNS helpers
- `index.html` — phone UI (compiled into the firmware)
- `justfile` — setup, flash, OTA, probe-rs
- `scripts/` — probe udev rules, UF2 merge
- `cyw43-firmware/` — CYW43439 firmware + Pico W NVRAM

## Network

Pico is `192.168.0.1/24`. DHCP hands out `.10`–`.17`, DNS and gateway point at the Pico, and A records hijack to `192.168.0.1`. DHCP option 114/160 advertise `http://192.168.0.1/` as the captive portal URL.

## References

- [Embassy](https://embassy.dev/)
- [picoserve](https://github.com/sammhicks/picoserve)
- [Pico W](https://www.raspberrypi.com/documentation/microcontrollers/raspberry-pi-pico.html)
- [cyw43](https://github.com/embassy-rs/embassy/tree/main/cyw43)
- [embassy-boot](https://github.com/embassy-rs/embassy/tree/main/embassy-boot) (RP example bootloader/application)
