# SK2229R controller PCB notes (2026-09-12)

Notes from inspecting the GameCraft / MacGregor / BSN SK2229R tabletop scoreboard controller and trying to use its UART as a command path. For Grok Build / later protocol work.

Related in-tree decode: `src/decode.rs`. Live capture already used by the Pico: UART1 GP21 RX @ 38400, 6-byte frames `60 | chk | unk0 | minutes | seconds | unk1`.

## Board identity

- Product: SK2229R (MacGregor / BSN Sports / SSG indoor tabletop multi-sport).
- MCU: Atmel **ATmega168PA-AU** (TQFP-32), silk **IC2**, date code AU 1414.
- Clock: 16.000 MHz crystal next to IC2.
- Companion product: optional **1171525** shot clocks, linked by 900 MHz wireless module **or** a wired data cable. Manual: plugging the data cable disables wireless and the scoreboard becomes master.

Manual scan used for the cable/wireless behavior:
https://sportsfacilitiesgroup.com/store/Content/Images/uploaded/Bison/BSN/SK2229R.pdf

## Headers on the photo

| Header | Pins | Notes |
|---|---|---|
| J9 | 3 pads, no shell | Near C10 / R4 / R5. Not a 6-pin ISP. Likely factory / debug / module pads. |
| J2 | 3 pads, no shell | Also traces to the mega168. Same class as J9. |
| Working UART | off the photo | The RS-232 / data-cable port already used to read MM:SS. Hardware USART is PD0/PD1 only. |

ATmega168 has **one** hardware USART. Two extra 3-pin headers are not a second UART; they are more likely test points, I2C (PC4/PC5), or partial ISP (RESET + two SPI pins).

ISP (if dumping flash) is still the TQFP pins, not J9:

| Signal | Port | TQFP-32 pin |
|---|---|---|
| MOSI | PB3 | 15 |
| MISO | PB4 | 16 |
| SCK | PB5 | 17 |
| RESET | PC6 | 29 |
| VCC / GND | several | |

```bash
avrdude -p m168p -c usbasp -U flash:r:sk2229r.bin:r
```

Lock bits are common on these OEM boards; readout may be refused.

## What the live UART actually is

Observed:

- Easy to parse minutes/seconds from the stream.
- Frame has a checksum / CRC field (in-tree: byte 1 `chk`; bytes 2 and 5 still unknown).
- RX line exists but the board has not responded to injected traffic so far.

Best interpretation: this is the **shot-clock data-cable master stream**, not a documented PC command API. The scoreboard emits status; slaves (1171525) listen. Inbound bytes are dropped unless they look like the same family with a valid CRC.

Expected accepted commands, if any: reset shot, start/stop, set 24/30, horn — i.e. clones of packets the unit already transmits.

Control of game clock / scores on this project remains GPIO pulses through the CD74HCT4066 (`just` hardware build). UART is time *readback*, not the write path, until CRC and command IDs are known.

## Breaking `chk` without a dump

1. Capture raw frames while only the clock ticks, then again on START/STOP, RESET SHOT, HORN, period, rear-panel keys.
2. Diff which bytes move.
3. Brute AVR-typical checksums over the payload:
   - sum / `0x100 - sum`
   - XOR of payload bytes
   - CRC-8 (0x07, 0x31)
   - CRC-16-CCITT / Modbus (0x1021 / 0x8005)
   - Dallas 1-wire CRC-8 (0x31)
4. Keep the algorithm that is stable across every captured frame.
5. Clone a captured control frame, change time/flags, recompute `chk`, send on GP20 (`just uart-test` / `tx` / `replay` / `hunt`).

In-tree helpers already exist: `just uart-test`, `just uart-console` (`tx`, `blast`, `replay`, `probe`, `hunt`, `listen`).

Need ~8–10 hex frames (idle tick + START + RESET SHOT) to name the CRC and the two unknown bytes.

## Firmware dump (optional)

ISP on the TQFP is the clean dump path. If lock bits block read, stay on capture. Disassembly is only worth it if the protocol is stateful or obfuscated; GPIO + UART readback already runs the phone UI.

## Open questions

- Continuity: J2 / J9 pads → which TQFP pins?
- Exact `chk` polynomial and coverage (bytes 0..5).
- Meaning of `unk0` / `unk1` (shot clock? running? period? horn?).
- Whether RX on the data-cable port ever ACKs, or is listen-only on this revision.
