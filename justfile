# ScoreboardCtrl — Raspberry Pi Pico W firmware
#
# Embassy + picoserve AP on 192.168.0.1. Flash is UF2 over USB BOOTSEL
# (see .cargo/config.toml runner: elf2uf2-rs -sd).
#
#   just setup    first time on a machine
#   just test     compile-check + clippy
#   just flash    hold BOOTSEL, plug in, then run

set shell := ["bash", "-euo", "pipefail", "-c"]
set dotenv-load := false

target := "thumbv6m-none-eabi"
bin := "ScoreboardCtrl"
elf_release := "target" / target / "release" / bin
uf2_release := elf_release + ".uf2"
serial := "/dev/ttyACM0"

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

# Type-check firmware (simulate, default)
check:
    cargo check

# Type-check hardware build (UART + GPIO, no sim clock)
check-hw:
    cargo check --no-default-features

# Debug ELF with scoreboard simulator (default)
build:
    cargo build

# Optimized firmware with simulator (Pico not wired to SK2229R)
release:
    cargo build --release

# Optimized firmware for a wired scoreboard (no simulate)
release-hw:
    cargo build --release --no-default-features

# Compile-check + clippy for sim and hardware cfgs
test: check clippy check-hw

# Format Rust sources with rustfmt
fmt:
    cargo fmt

# Check rustfmt without writing files
fmt-check:
    cargo fmt -- --check

# Lint firmware with clippy
clippy:
    cargo clippy -- -W clippy::all

# Ask running firmware to reboot into USB BOOTSEL (sends ENTERBOOTLOADER)
enter-bootloader SERIAL=serial:
    #!/usr/bin/env bash
    port="{{ SERIAL }}"
    dest=""
    for p in "/run/media/${USER}/RPI-RP2" "/media/${USER}/RPI-RP2"; do
      if [[ -d "$p" ]]; then dest="$p"; break; fi
    done
    if [[ -n "$dest" ]]; then
      echo "already in BOOTSEL at $dest"
      exit 0
    fi
    if [[ ! -e "$port" ]]; then
      echo "no serial $port and no RPI-RP2 volume"
      exit 1
    fi
    printf 'ENTERBOOTLOADER' > "$port" || true
    for _ in $(seq 1 20); do
      for p in "/run/media/${USER}/RPI-RP2" "/media/${USER}/RPI-RP2"; do
        if [[ -d "$p" ]]; then echo "BOOTSEL mounted at $p"; exit 0; fi
      done
      sleep 0.4
    done
    echo "timed out waiting for RPI-RP2 after ENTERBOOTLOADER"
    exit 1

# Flash simulator firmware. Tries ENTERBOOTLOADER, else needs BOOTSEL.
flash: release
    just enter-bootloader || true
    elf2uf2-rs -d {{ elf_release }}

# Flash hardware firmware (UART 38400 + GPIO pulses, no sim clock)
flash-hw:
    cargo run --release --no-default-features

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

# UART1 TX on GP8 (115200 8N1, GND + GP8). USB CDC still on ACM0 via `just logs`.
uart-logs SERIAL="/dev/ttyUSB0":
    #!/usr/bin/env bash
    port="{{ SERIAL }}"
    if [[ ! -e "$port" ]]; then
      echo "no serial $port — adapter RX to Pico GP8, GND to GND, 115200"
      ls -l /dev/ttyACM* /dev/ttyUSB* 2>/dev/null || true
      exit 1
    fi
    just _serial_follow "$port"

# USB CDC log stream from embassy-usb-logger
logs SERIAL=serial:
    #!/usr/bin/env bash
    port="{{ SERIAL }}"
    if [[ ! -e "$port" ]]; then
      echo "no serial device at $port (plug in the Pico after flashing, not in BOOTSEL)"
      ls -l /dev/ttyACM* /dev/ttyUSB* 2>/dev/null || true
      exit 1
    fi
    just _serial_follow "$port"

# fmt-check + clippy + release build (no hardware)
ci: fmt-check clippy release

# Build and open rustdoc, including private items
doc:
    cargo doc --document-private-items --open

# Remove Cargo target artifacts
clean:
    cargo clean
