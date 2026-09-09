# ScoreboardCtrl — Raspberry Pi Pico W firmware
#
# Embassy + picoserve AP on 192.168.0.1 with embassy-boot OTA.
# First-time: flash bootloader UF2, then app UF2 (USB BOOTSEL).
# Later updates: just ota (nmcli hops to Scoreboard AP, POSTs, hops back).
#
#   just setup           first time on a machine
#   just test            compile-check + clippy
#   just flash-bootloader
#   just flash           ENTERBOOTLOADER + app UF2 (or hold BOOTSEL)
#   just ota             build + WiFi hop + HTTP OTA + restore WiFi
#   OTA_SKIP_WIFI=1 just ota   # skip hopping if already on Scoreboard

set shell := ["bash", "-euo", "pipefail", "-c"]
set dotenv-load := false

target := "thumbv6m-none-eabi"
bin := "scoreboard-ctrl"
elf_release := "target" / target / "release" / bin
uf2_release := elf_release + ".uf2"
bootloader_bin := "scoreboard-bootloader"
bootloader_elf := "target" / target / "release" / bootloader_bin
bootloader_uf2 := bootloader_elf + ".uf2"
ota_bin := elf_release + ".bin"
serial := "/dev/ttyACM0"
ota_url := "http://192.168.0.1/api/ota"

# List available recipes
default:
    @just --list

# Install nightly toolchain, RP2040 target, and UF2 flasher
setup:
    rustup show
    rustup target add {{ target }}
    rustup component add rust-src rustfmt clippy llvm-tools-preview
    command -v elf2uf2-rs >/dev/null || cargo install elf2uf2-rs
    @echo "optional: cargo install probe-rs-tools   # SWD debug probe"
    @echo "optional: picotool from Raspberry Pi     # load/reboot without BOOTSEL"
    just doctor

# Report toolchain, target, and flash tools
doctor:
    @echo "rustc:     $(rustc -V)"
    @echo "cargo:     $(cargo -V)"
    @echo "host:      $(rustc -vV | awk '/^host:/{print $2}')"
    @rustup target list --installed | grep -F {{ target }} || { echo "missing target {{ target }}"; exit 1; }
    @command -v elf2uf2-rs >/dev/null && echo "elf2uf2:   $(command -v elf2uf2-rs)" || echo "elf2uf2:   NOT INSTALLED (just setup)"
    @command -v probe-rs >/dev/null && echo "probe-rs:  $(command -v probe-rs)" || echo "probe-rs:  (optional)"
    @command -v picotool >/dev/null && echo "picotool:  $(command -v picotool)" || echo "picotool:  (optional)"
    @echo "serial:    {{ serial }} $(if [ -e {{ serial }} ]; then echo present; else echo 'not present'; fi)"
    @echo "ota url:   {{ ota_url }}"
    @echo "workspace: bootloader + scoreboard-ctrl (embassy-boot A/B)"

# Type-check firmware (simulate, default)
check:
    cargo check

# Type-check hardware build (UART + GPIO, no sim clock)
check-hw:
    cargo check --no-default-features

# Type-check embassy-boot bootloader
check-bootloader:
    cargo check -p scoreboard-bootloader --release

# Debug ELF with scoreboard simulator (default)
build:
    cargo build

# Optimized firmware with simulator (Pico not wired to SK2229R)
release:
    cargo build --release

# Optimized firmware for a wired scoreboard (no simulate)
release-hw:
    cargo build --release --no-default-features

# Compile-check + clippy for sim and hardware cfgs, plus host unit tests
test: check clippy check-hw clippy-hw check-bootloader test-host

# Format Rust sources with rustfmt
fmt:
    cargo fmt

# Check rustfmt without writing files
fmt-check:
    cargo fmt -- --check

# Lint firmware with clippy
clippy:
    cargo clippy -- -W clippy::all

# Lint hardware (no simulate) build
clippy-hw:
    cargo clippy --no-default-features -- -W clippy::all

# Host tests for decode + DHCP/DNS helpers
test-host:
    cargo test --lib --target x86_64-unknown-linux-gnu

# Send ENTERBOOTLOADER on USB CDC (ttyACM) so the Pico reboots into UF2 BOOTSEL.
enter-bootloader SERIAL=serial:
    #!/usr/bin/env bash
    bootsel_dir() {
      for p in "/run/media/${USER}/RPI-RP2" "/media/${USER}/RPI-RP2" "/mnt/RPI-RP2"; do
        if [[ -d "$p" ]]; then echo "$p"; return 0; fi
      done
      return 1
    }
    port="{{ SERIAL }}"
    if dest="$(bootsel_dir)"; then
      echo "already in BOOTSEL at $dest"
      exit 0
    fi
    if [[ ! -e "$port" ]]; then
      echo "no serial $port and no RPI-RP2 volume"
      ls -l /dev/ttyACM* /dev/ttyUSB* 2>/dev/null || true
      exit 1
    fi
    echo "sending ENTERBOOTLOADER on $port"
    if command -v python3 >/dev/null && python3 -c "import serial" 2>/dev/null; then
      python3 - "$port" <<'PY'
    import sys, serial, time
    ser = serial.Serial(sys.argv[1], 115200, timeout=1, write_timeout=1)
    ser.dtr = True
    time.sleep(0.05)
    ser.write(b"ENTERBOOTLOADER")
    ser.flush()
    ser.close()
    PY
    else
      stty -F "$port" 115200 raw -echo cs8 -cstopb -parenb || true
      printf 'ENTERBOOTLOADER' > "$port" || true
    fi
    for _ in $(seq 1 25); do
      if dest="$(bootsel_dir)"; then
        echo "BOOTSEL mounted at $dest"
        exit 0
      fi
      sleep 0.4
    done
    echo "timed out waiting for RPI-RP2 after ENTERBOOTLOADER"
    ls -l /dev/ttyACM* /dev/ttyUSB* 2>/dev/null || true
    exit 1

# ENTERBOOTLOADER over USB CDC, then copy the release UF2 (simulator).
reflash: release
    just enter-bootloader
    elf2uf2-rs -d {{ elf_release }}

# ENTERBOOTLOADER over USB CDC, then copy the hardware UF2.
reflash-hw: release-hw
    just enter-bootloader
    elf2uf2-rs -d {{ elf_release }}

# Flash simulator firmware (same as reflash; hold BOOTSEL if CDC is down).
flash: reflash

# Flash hardware firmware (UART 38400 + GPIO pulses, no sim clock).
flash-hw: reflash-hw

# Alias for flash
deploy: flash

# Write a .uf2 next to the release ELF; does not copy to the board
uf2: release
    elf2uf2-rs {{ elf_release }} {{ uf2_release }}
    @ls -lh {{ uf2_release }}

# Copy UF2 onto a mounted RPI-RP2 volume
flash-copy: uf2
    #!/usr/bin/env bash
    dest=""
    for p in "/media/${USER}/RPI-RP2" "/run/media/${USER}/RPI-RP2" "/mnt/RPI-RP2"; do
      if [[ -d "$p" ]]; then dest="$p"; break; fi
    done
    if [[ -z "$dest" ]]; then
      echo "RPI-RP2 volume not mounted. Hold BOOTSEL, plug in the Pico W, then retry."
      exit 1
    fi
    cp {{ uf2_release }} "$dest/"
    echo "copied {{ uf2_release }} -> $dest"

# Flash with a debug probe (CMSIS-DAP / Picoprobe), if probe-rs is installed
flash-probe: release
    probe-rs run --chip RP2040 {{ elf_release }}

# Flash with picotool (Pico already in BOOTSEL, or picotool can reset it)
flash-picotool: uf2
    picotool load -f {{ uf2_release }}
    picotool reboot


# Build embassy-boot bootloader (release)
build-bootloader:
    cargo build -p scoreboard-bootloader --release

# UF2 for the bootloader (flash this once before the app)
uf2-bootloader: build-bootloader
    elf2uf2-rs {{ bootloader_elf }} {{ bootloader_uf2 }}
    @ls -lh {{ bootloader_uf2 }}

# Copy bootloader UF2 to a mounted RPI-RP2 volume (hold BOOTSEL)
flash-bootloader: uf2-bootloader
    #!/usr/bin/env bash
    dest=""
    for p in "/media/${USER}/RPI-RP2" "/run/media/${USER}/RPI-RP2" "/mnt/RPI-RP2"; do
      if [[ -d "$p" ]]; then dest="$p"; break; fi
    done
    if [[ -z "$dest" ]]; then
      echo "RPI-RP2 volume not mounted. Hold BOOTSEL, plug in the Pico W, then retry."
      exit 1
    fi
    cp {{ bootloader_uf2 }} "$dest/"
    echo "copied {{ bootloader_uf2 }} -> $dest"
    echo "Next: just flash   # or just flash-hw"

# Raw ACTIVE-partition image for HTTP OTA (objcopy binary)
ota-artifact: release
    #!/usr/bin/env bash
    sysroot="$(rustc --print sysroot)"
    objcopy="$(find "$sysroot" -name llvm-objcopy | head -1)"
    if [[ -z "$objcopy" || ! -x "$objcopy" ]]; then
      echo "llvm-objcopy not found; run: rustup component add llvm-tools-preview"
      exit 1
    fi
    "$objcopy" -O binary {{ elf_release }} {{ ota_bin }}
    ls -lh {{ ota_bin }}

# Join Scoreboard AP (NetworkManager), POST OTA, then restore prior WiFi.
# Set OTA_SKIP_WIFI=1 to skip hopping (already on AP / Ethernet path).
# Override SSID with: just ota wifi_ssid=Scoreboard
wifi_ssid := "Scoreboard"
ota_host := "192.168.0.1"

# Shared WiFi hop + curl for OTA (expects {{ ota_bin }} already built)
_ota-post BIN=ota_bin:
    #!/usr/bin/env bash
    set -euo pipefail
    bin="{{ BIN }}"
    ap="{{ wifi_ssid }}"
    url="{{ ota_url }}"
    host="{{ ota_host }}"
    skip="${OTA_SKIP_WIFI:-0}"

    if ! command -v nmcli >/dev/null; then
      echo "nmcli not found — install NetworkManager, or run: OTA_SKIP_WIFI=1 just ota"
      exit 1
    fi
    if ! command -v curl >/dev/null; then
      echo "curl not found"
      exit 1
    fi

    prev_conn=""
    prev_ssid=""
    hopped=0

    restore_wifi() {
      if [[ "$hopped" -ne 1 ]]; then
        return 0
      fi
      echo "restoring WiFi..."
      if [[ -n "$prev_conn" ]]; then
        if nmcli connection up id "$prev_conn"; then
          echo "restored connection: $prev_conn"
          return 0
        fi
      fi
      if [[ -n "$prev_ssid" && "$prev_ssid" != "$ap" ]]; then
        if nmcli device wifi connect "$prev_ssid"; then
          echo "restored SSID: $prev_ssid"
          return 0
        fi
      fi
      echo "warning: could not restore prior WiFi; reconnect manually"
      return 0
    }
    trap restore_wifi EXIT

    current_ssid="$(nmcli -t -f ACTIVE,SSID device wifi 2>/dev/null | awk -F: '$1=="yes"{print $2; exit}' || true)"
    prev_conn="$(nmcli -t -f NAME,TYPE connection show --active 2>/dev/null | awk -F: '$2=="802-11-wireless"||$2=="wifi"{print $1; exit}' || true)"
    prev_ssid="$current_ssid"

    if [[ "$skip" == "1" ]]; then
      echo "OTA_SKIP_WIFI=1 — not changing WiFi (current SSID: ${current_ssid:-unknown})"
    elif [[ "$current_ssid" == "$ap" ]]; then
      echo "already on $ap"
    else
      echo "current WiFi: conn=${prev_conn:-none} ssid=${prev_ssid:-none}"
      echo "connecting to open AP '$ap'..."
      # Rescan helps when the Pico just came up
      nmcli device wifi rescan >/dev/null 2>&1 || true
      sleep 1
      if ! nmcli device wifi connect "$ap"; then
        # Retry once after another scan
        nmcli device wifi rescan >/dev/null 2>&1 || true
        sleep 2
        nmcli device wifi connect "$ap"
      fi
      hopped=1
      echo "joined $ap"
    fi

    echo "waiting for http://$host/ ..."
    ok=0
    for _ in $(seq 1 40); do
      if curl -fsS --connect-timeout 1 -o /dev/null "http://$host/" 2>/dev/null; then
        ok=1
        break
      fi
      sleep 0.5
    done
    if [[ "$ok" -ne 1 ]]; then
      echo "Pico not reachable at http://$host/ — is firmware running and AP up?"
      exit 1
    fi

    echo "POST $url <- $bin ($(wc -c < "$bin") bytes)"
    curl -fS --connect-timeout 5 --max-time 180 \
      -X POST \
      -H "Content-Type: application/octet-stream" \
      --data-binary @"$bin" \
      "$url"
    echo
    echo "OTA accepted; device should soft-reset into embassy-boot. Watch: just logs"
    # Give the AP a moment to drop before we hop home (optional)
    sleep 1

# POST simulate image: build, WiFi hop to Scoreboard, upload, restore WiFi
ota: ota-artifact
    just _ota-post {{ ota_bin }}

# POST hardware image: same WiFi hop behavior as ota
ota-hw: release-hw
    #!/usr/bin/env bash
    set -euo pipefail
    sysroot="$(rustc --print sysroot)"
    objcopy="$(find "$sysroot" -name llvm-objcopy | head -1)"
    if [[ -z "$objcopy" || ! -x "$objcopy" ]]; then
      echo "llvm-objcopy not found; run: rustup component add llvm-tools-preview"
      exit 1
    fi
    "$objcopy" -O binary {{ elf_release }} {{ ota_bin }}
    ls -lh {{ ota_bin }}
    just _ota-post {{ ota_bin }}

# Flash / RAM section sizes of the release ELF
size: release
    #!/usr/bin/env bash
    sysroot="$(rustc --print sysroot)"
    host="$(rustc -vV | awk '/^host:/{print $2}')"
    size_bin="${sysroot}/lib/rustlib/${host}/bin/llvm-size"
    if [[ ! -x "$size_bin" ]]; then
      echo "llvm-size not found; run: rustup component add llvm-tools-preview"
      exit 1
    fi
    "$size_bin" -A {{ elf_release }}

# Strip CR so firmware `\r\n` does not become a blank line (TTY ICRNL / miniterm).
_serial_follow port:
    #!/usr/bin/env bash
    port="{{ port }}"
    if command -v python3 >/dev/null && python3 -c "import serial" 2>/dev/null; then
      python3 - "$port" <<'PY'
    import sys
    import serial
    ser = serial.Serial(sys.argv[1], 115200, timeout=0.2)
    buf = b""
    try:
        while True:
            chunk = ser.read(256)
            if not chunk:
                continue
            buf += chunk.replace(b"\r", b"")
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                sys.stdout.buffer.write(line + b"\n")
                sys.stdout.flush()
    except KeyboardInterrupt:
        pass
    PY
    else
      stty -F "$port" 115200 raw -echo -icrnl -inlcr -igncr cs8 -cstopb -parenb || true
      # tr -d '\r' so leftover CR cannot become an extra newline
      tr -d '\r' < "$port"
    fi

# USB CDC log stream (VSP) from embassy-usb-logger
logs SERIAL=serial:
    #!/usr/bin/env bash
    port="{{ SERIAL }}"
    if [[ ! -e "$port" ]]; then
      echo "no serial device at $port (plug in the Pico after flashing, not in BOOTSEL)"
      ls -l /dev/ttyACM* /dev/ttyUSB* 2>/dev/null || true
      exit 1
    fi
    just _serial_follow "$port"

# fmt-check + clippy + host tests + release build (no hardware)
ci: fmt-check clippy clippy-hw test-host release

# Build and open rustdoc, including private items
doc:
    cargo doc --document-private-items --open

# Remove Cargo target artifacts
clean:
    cargo clean
