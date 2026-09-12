# ScoreboardCtrl — Raspberry Pi Pico W firmware
#
# Embassy + picoserve AP on 192.168.0.1.
# Dev: Pico Debug Probe (CMSIS-DAP) + probe-rs + defmt RTT.
# Recipes default to a wired SK2229R (no simulate). Add -sim for the software clock.
#   just program         standalone hardware: build, flash, defmt
#   just program-ota     bootloader + ACTIVE app, reset through embassy-boot, defmt
#   just ota             POST hardware ACTIVE .bin (needs program-ota image + AP up)
#   just program-sim / just ota-sim / just flash-sim   software scoreboard
#
#   just setup           first time on a machine
#   just test            compile-check + clippy
#   just flash           ENTERBOOTLOADER + standalone UF2 (or hold BOOTSEL)

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
probe := "2e8a:000c"
chip := "RP2040"

# List available recipes
default:
    @just --list

# Install nightly toolchain, RP2040 target, UF2 flasher, and probe-rs
setup:
    rustup show
    rustup target add {{ target }}
    rustup component add rust-src rustfmt clippy llvm-tools-preview
    command -v elf2uf2-rs >/dev/null || cargo install elf2uf2-rs
    command -v probe-rs >/dev/null || cargo install probe-rs-tools --locked
    @echo "udev for Pico Debug Probe: just probe-udev"
    just doctor

# Allow the current user to use the Pico Debug Probe (needs sudo)
probe-udev:
    sudo cp scripts/69-probe-rs.rules /etc/udev/rules.d/69-probe-rs.rules
    sudo udevadm control --reload-rules
    sudo udevadm trigger
    @echo "unplug/replug the Debug Probe if probe-rs list still says inaccessible"

# Report toolchain, target, and flash tools
doctor:
    @echo "rustc:     $(rustc -V)"
    @echo "cargo:     $(cargo -V)"
    @echo "host:      $(rustc -vV | awk '/^host:/{print $2}')"
    @rustup target list --installed | grep -F {{ target }} || { echo "missing target {{ target }}"; exit 1; }
    @command -v elf2uf2-rs >/dev/null && echo "elf2uf2:   $(command -v elf2uf2-rs)" || echo "elf2uf2:   NOT INSTALLED (just setup)"
    @command -v probe-rs >/dev/null && echo "probe-rs:  $(command -v probe-rs)" || echo "probe-rs:  NOT INSTALLED (just setup)"
    @command -v picotool >/dev/null && echo "picotool:  $(command -v picotool)" || echo "picotool:  (optional)"
    @if command -v probe-rs >/dev/null; then probe-rs list || true; fi
    @echo "serial:    {{ serial }} $(if [ -e {{ serial }} ]; then echo present; else echo 'not present'; fi)"
    @echo "ota url:   {{ ota_url }}"
    @echo "workspace: just recipes are hardware (no simulate); add -sim. just program-ota / --features ota for embassy-boot"

# Type-check hardware firmware (UART + GPIO, no sim clock)
check:
    cargo check --no-default-features

# Type-check simulator build (software clock + scores)
check-sim:
    cargo check

# Type-check embassy-boot bootloader
check-bootloader:
    cargo check -p scoreboard-bootloader --release

# Type-check ACTIVE (embassy-boot) hardware image
check-ota:
    cargo check --no-default-features --features ota

# Type-check ACTIVE simulator image
check-ota-sim:
    cargo check --features ota

# Debug ELF, wired scoreboard
build:
    cargo build --no-default-features

# Debug ELF with software clock (Pico not wired to SK2229R)
build-sim:
    cargo build

# Optimized firmware for a wired scoreboard
release:
    cargo build --release --no-default-features

# Optimized firmware with simulator
release-sim:
    cargo build --release

# Compile-check + clippy for hardware and sim cfgs, plus host unit tests
test: check clippy check-sim clippy-sim check-bootloader check-ota clippy-ota check-ota-sim clippy-ota-sim test-host

# Format Rust sources with rustfmt
fmt:
    cargo fmt

# Check rustfmt without writing files
fmt-check:
    cargo fmt -- --check

# Lint hardware firmware with clippy
clippy:
    cargo clippy --no-default-features -- -W clippy::all

# Lint simulator build
clippy-sim:
    cargo clippy -- -W clippy::all

# Lint embassy-boot ACTIVE hardware image
clippy-ota:
    cargo clippy --no-default-features --features ota -- -W clippy::all

# Lint embassy-boot ACTIVE simulator image
clippy-ota-sim:
    cargo clippy --features ota -- -W clippy::all

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

# ENTERBOOTLOADER over USB CDC, then copy the hardware UF2.
reflash: release
    just enter-bootloader
    elf2uf2-rs -d {{ elf_release }}

# ENTERBOOTLOADER over USB CDC, then copy the simulator UF2.
reflash-sim: release-sim
    just enter-bootloader
    elf2uf2-rs -d {{ elf_release }}

# Flash hardware firmware (UART 38400 + GPIO pulses, no sim clock).
flash: reflash

# Flash simulator firmware (same as reflash-sim; hold BOOTSEL if CDC is down).
flash-sim: reflash-sim

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

# Build hardware release, flash via Pico Debug Probe, follow defmt RTT until Ctrl-C
program:
    #!/usr/bin/env bash
    export PATH="${HOME}/.cargo/bin:${PATH}"
    export DEFMT_LOG="${DEFMT_LOG:-info}"
    cargo build --release --no-default-features
    probe-rs run --chip {{ chip }} --probe {{ probe }} {{ elf_release }}

# Same as program, software clock (no SK2229R)
program-sim:
    #!/usr/bin/env bash
    export PATH="${HOME}/.cargo/bin:${PATH}"
    export DEFMT_LOG="${DEFMT_LOG:-info}"
    cargo build --release
    probe-rs run --chip {{ chip }} --probe {{ probe }} {{ elf_release }}

# embassy-boot: erase, flash bootloader + hardware ACTIVE app, reset through the BL, follow app defmt
program-ota:
    #!/usr/bin/env bash
    export PATH="${HOME}/.cargo/bin:${PATH}"
    export DEFMT_LOG="${DEFMT_LOG:-info}"
    cargo build -p scoreboard-bootloader --release
    cargo build --release --no-default-features --features ota
    echo "chip-erase + bootloader (clears STATE so leftover SWAP_MAGIC cannot clobber ACTIVE)"
    probe-rs download --chip {{ chip }} --probe {{ probe }} --chip-erase {{ bootloader_elf }}
    echo "ACTIVE app @ 0x10009000"
    probe-rs download --chip {{ chip }} --probe {{ probe }} {{ elf_release }}
    probe-rs reset --chip {{ chip }} --probe {{ probe }}
    # Bootloader has its own RTT block; wait for the app to jump and init defmt.
    sleep 2
    echo "attaching app defmt RTT (Ctrl-C detaches; firmware keeps running)"
    probe-rs attach --chip {{ chip }} --probe {{ probe }} --no-catch-reset --no-catch-hardfault {{ elf_release }}

# Same as program-ota, simulator ACTIVE image
program-ota-sim:
    #!/usr/bin/env bash
    export PATH="${HOME}/.cargo/bin:${PATH}"
    export DEFMT_LOG="${DEFMT_LOG:-info}"
    cargo build -p scoreboard-bootloader --release
    cargo build --release --features ota
    probe-rs download --chip {{ chip }} --probe {{ probe }} --chip-erase {{ bootloader_elf }}
    probe-rs download --chip {{ chip }} --probe {{ probe }} {{ elf_release }}
    probe-rs reset --chip {{ chip }} --probe {{ probe }}
    sleep 2
    probe-rs attach --chip {{ chip }} --probe {{ probe }} --no-catch-reset --no-catch-hardfault {{ elf_release }}

# Attach defmt to a running ACTIVE image (no flash). Use after program-ota or HTTP OTA.
# RTT is not a history buffer — idle firmware is silent. Reset first to recapture boot:
#   probe-rs reset --chip RP2040 --probe 2e8a:000c && just attach-ota
attach-ota:
    #!/usr/bin/env bash
    export PATH="${HOME}/.cargo/bin:${PATH}"
    export DEFMT_LOG="${DEFMT_LOG:-info}"
    probe-rs attach --chip {{ chip }} --probe {{ probe }} --no-catch-reset --no-catch-hardfault {{ elf_release }}

# Attach defmt using the bootloader ELF (debug a failed jump)
attach-bootloader:
    #!/usr/bin/env bash
    export PATH="${HOME}/.cargo/bin:${PATH}"
    export DEFMT_LOG="${DEFMT_LOG:-info}"
    probe-rs attach --chip {{ chip }} --probe {{ probe }} --no-catch-reset --no-catch-hardfault {{ bootloader_elf }}

# Flash with a debug probe and attach (alias for program)
flash-probe: program

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
    sync
    echo "copied {{ bootloader_uf2 }} -> $dest"
    echo "Bootloader-only flash has no app/AP. Next (BOOTSEL again): just flash"
    echo "Or one-shot: just flash-bringup"

# Merge UF2 images into one file (single BOOTSEL copy; RP2 reboots after a complete UF2)
_merge-uf2 A B OUT:
    python3 scripts/merge-uf2.py "{{ A }}" "{{ B }}" "{{ OUT }}" --state-addr 0x10008000 --state-size 4096

# Copy a UF2 onto the mounted RPI-RP2 volume and sync
_copy-uf2 FILE:
    #!/usr/bin/env bash
    dest=""
    for p in "/media/${USER}/RPI-RP2" "/run/media/${USER}/RPI-RP2" "/mnt/RPI-RP2"; do
      if [[ -d "$p" ]]; then dest="$p"; break; fi
    done
    if [[ -z "$dest" ]]; then
      echo "RPI-RP2 volume not mounted. Hold BOOTSEL, plug in the Pico W, then retry."
      exit 1
    fi
    src="{{ FILE }}"
    echo "copying $src -> $dest/"
    cp "$src" "$dest/"
    sync
    echo "copied $(basename "$src") ($(wc -c < "$src") bytes)"

bringup_uf2 := "target" / target / "release" / "scoreboard-bringup.uf2"

# First-time USB flash: bootloader + app in one UF2 (one BOOTSEL)
flash-bringup: uf2-bootloader uf2
    just _merge-uf2 {{ bootloader_uf2 }} {{ uf2_release }} {{ bringup_uf2 }}
    just _copy-uf2 {{ bringup_uf2 }}
    @echo "Pico should leave BOOTSEL and boot the app. Watch: just logs"

# Optimized ACTIVE firmware (embassy-boot, wired scoreboard)
# DEFMT_LOG is compile-time: unset = error-only, which strips UART info logs.
release-ota:
    #!/usr/bin/env bash
    export DEFMT_LOG="${DEFMT_LOG:-info}"
    cargo build --release --no-default-features --features ota

# Optimized ACTIVE firmware with simulator
release-ota-sim:
    #!/usr/bin/env bash
    export DEFMT_LOG="${DEFMT_LOG:-info}"
    cargo build --release --features ota

# Raw ACTIVE-partition image for HTTP OTA (hardware; must be --features ota)
ota-artifact: release-ota
    #!/usr/bin/env bash
    sysroot="$(rustc --print sysroot)"
    objcopy="$(find "$sysroot" -name llvm-objcopy | head -1)"
    if [[ -z "$objcopy" || ! -x "$objcopy" ]]; then
      echo "llvm-objcopy not found; run: rustup component add llvm-tools-preview"
      exit 1
    fi
    "$objcopy" -O binary {{ elf_release }} {{ ota_bin }}
    ls -lh {{ ota_bin }}

# Simulator ACTIVE .bin (same path as ota-artifact; overwrites)
ota-artifact-sim: release-ota-sim
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
    echo "OTA accepted; embassy-boot will swap ACTIVE (~tens of seconds) then trial-boot."
    echo "AP returns when mark_booted succeeds. Recapture boot logs:"
    echo "  probe-rs reset --chip RP2040 --probe 2e8a:000c && just attach-ota"
    # Give the AP a moment to drop before we hop home (optional)
    sleep 1

# POST hardware ACTIVE image: build, WiFi hop to Scoreboard, upload, restore WiFi
ota: ota-artifact
    just _ota-post {{ ota_bin }}

# POST simulator ACTIVE image: same WiFi hop behavior as ota
ota-sim: ota-artifact-sim
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

# fmt-check + clippy (hw + sim) + host tests + hardware release
ci: fmt-check clippy clippy-sim test-host release

# Build and open rustdoc, including private items
doc:
    cargo doc --document-private-items --open

# Remove Cargo target artifacts
clean:
    cargo clean
