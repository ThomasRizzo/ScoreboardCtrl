#!/usr/bin/env python3
"""Merge two UF2 files into one (single BOOTSEL copy).

The RP2040 ROM reboots after a complete UF2 (block_no / num_blocks). Concatenating
two images without rewriting those fields would flash only the first.

Optional --state-addr/--state-size writes an erased embassy-boot STATE partition
(BOOT_MAGIC 0xD0 in the first byte, 0xFF elsewhere) so leftover flash cannot look
like SWAP_MAGIC and swap DFU garbage over a freshly programmed ACTIVE image.
"""
from __future__ import annotations

import argparse
import struct
import sys

MAGIC0 = 0x0A324655
MAGIC1 = 0x9E5D5157
MAGIC_END = 0x0AB16F30
# RP2040 family ID; FLAG_FAMILY_ID_PRESENT
FLAGS = 0x2000
FAMILY_RP2040 = 0xE48BFF56
PAYLOAD = 256
BOOT_MAGIC = 0xD0


def blocks(path: str) -> list[tuple[int, bytearray]]:
    data = open(path, "rb").read()
    if len(data) % 512:
        raise SystemExit(f"{path}: length {len(data)} not multiple of 512")
    out: list[tuple[int, bytearray]] = []
    for i in range(0, len(data), 512):
        b = bytearray(data[i : i + 512])
        m0, m1, _flags, addr, _payload, _no, _num, _fam = struct.unpack_from(
            "<IIIIIIII", b, 0
        )
        mend = struct.unpack_from("<I", b, 508)[0]
        if m0 != MAGIC0 or m1 != MAGIC1 or mend != MAGIC_END:
            raise SystemExit(f"{path}: bad UF2 magic at block {i // 512}")
        out.append((addr, b))
    return out


def uf2_block(addr: int, payload: bytes) -> bytearray:
    if len(payload) != PAYLOAD:
        raise SystemExit(f"UF2 payload must be {PAYLOAD} bytes, got {len(payload)}")
    blk = bytearray(512)
    struct.pack_into(
        "<IIIIIIII",
        blk,
        0,
        MAGIC0,
        MAGIC1,
        FLAGS,
        addr,
        PAYLOAD,
        0,
        0,
        FAMILY_RP2040,
    )
    blk[32 : 32 + PAYLOAD] = payload
    struct.pack_into("<I", blk, 508, MAGIC_END)
    return blk


def state_blocks(addr: int, size: int) -> list[tuple[int, bytearray]]:
    if size % PAYLOAD:
        raise SystemExit(f"state size {size} is not a multiple of {PAYLOAD}")
    data = bytearray([0xFF] * size)
    data[0] = BOOT_MAGIC
    out: list[tuple[int, bytearray]] = []
    for off in range(0, size, PAYLOAD):
        a = addr + off
        out.append((a, uf2_block(a, bytes(data[off : off + PAYLOAD]))))
    return out


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("bootloader_uf2")
    p.add_argument("app_uf2")
    p.add_argument("out_uf2")
    p.add_argument(
        "--state-addr",
        default=None,
        help="absolute flash address of embassy-boot STATE (e.g. 0x10008000)",
    )
    p.add_argument("--state-size", type=int, default=4096)
    args = p.parse_args()

    merged = blocks(args.bootloader_uf2)
    if args.state_addr:
        addr = int(args.state_addr, 0)
        merged.extend(state_blocks(addr, args.state_size))
        print(f"injected STATE at {addr:#x} ({args.state_size} bytes, BOOT_MAGIC)")
    merged.extend(blocks(args.app_uf2))
    n = len(merged)
    with open(args.out_uf2, "wb") as f:
        for i, (_addr, blk) in enumerate(merged):
            struct.pack_into("<II", blk, 20, i, n)
            f.write(blk)
    first, last = merged[0][0], merged[-1][0]
    print(f"merged {n} UF2 blocks -> {args.out_uf2} (addrs {first:#x} .. {last:#x})")


if __name__ == "__main__":
    main()
