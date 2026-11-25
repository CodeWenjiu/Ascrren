#!/usr/bin/env python3
"""Minimal reference client copied from the vendor Python demo.

Warning: the streaming LCD path can lock up V1.0 firmware and require a
physical USB replug. Only run this for protocol experiments.

Usage:
    python3 scripts/msu2_ref_stream.py 240
    python3 scripts/msu2_ref_stream.py 32
"""

import sys
import time
from pathlib import Path

import serial
import serial.tools.list_ports

ROOT = Path(__file__).resolve().parents[1]
IMAGE_240 = (
    ROOT
    / "docs/MSU2可编程USB副屏资料/MSU2 演示Demo及固件V1.0/Python源码/1.bin"
)


def find_port() -> str:
    ports = [
        port
        for port in serial.tools.list_ports.comports()
        if port.vid == 0x1A86 and port.pid == 0xFE0C
    ]
    if not ports:
        raise SystemExit("MSU2 (1a86:fe0c) not found")
    return ports[0].device


def wait_for(ser: serial.Serial, needle: bytes, timeout: float) -> bytes:
    buf = b""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        time.sleep(0.05)
        buf += ser.read(ser.in_waiting)
        if needle in buf:
            return buf
    return buf


def lcd_set_xy(ser: serial.Serial, x: int, y: int) -> None:
    ser.write(bytes([2, 0, x >> 8, x & 0xFF, y >> 8, y & 0xFF]))


def lcd_set_size(ser: serial.Serial, width: int, height: int) -> None:
    ser.write(bytes([2, 1, width >> 8, width & 0xFF, height >> 8, height & 0xFF]))


def lcd_add(ser: serial.Serial, x: int, y: int, width: int, height: int) -> None:
    lcd_set_xy(ser, x, y)
    lcd_set_size(ser, width, height)
    ser.write(bytes([2, 3, 7, 0, 0, 0]))

    deadline = time.monotonic() + 2.0
    while time.monotonic() < deadline:
        time.sleep(0.001)
        data = ser.read(ser.in_waiting)
        if data:
            print(f"lcd_add_rx={data.hex()}")
            return
    print("lcd_add_rx=<timeout>")


def lcd_photo(
    ser: serial.Serial, x: int, y: int, width: int, height: int, page: int
) -> None:
    lcd_set_xy(ser, x, y)
    lcd_set_size(ser, width, height)
    ser.write(bytes([2, 3, 0, page >> 8, page & 0xFF, 0]))

    deadline = time.monotonic() + 2.0
    while time.monotonic() < deadline:
        time.sleep(0.001)
        data = ser.read(ser.in_waiting)
        if data:
            print(f"lcd_photo_rx={data.hex()}")
            return
    print("lcd_photo_rx=<timeout>")


def write_lcd_photo_fast1(
    ser: serial.Serial,
    x: int,
    y: int,
    width: int,
    height: int,
    data: bytes,
    chunked: bool = False,
) -> None:
    lcd_add(ser, x, y, width, height)

    packet = bytearray()
    full_pages = len(data) // 256
    for page in range(full_pages):
        chunk = data[page * 256 : (page + 1) * 256]
        for index in range(64):
            offset = index * 4
            packet += bytes(
                [
                    4,
                    index,
                    chunk[offset],
                    chunk[offset + 1],
                    chunk[offset + 2],
                    chunk[offset + 3],
                ]
            )
        packet += bytes([2, 3, 8, 1, 0, 0])

    remainder = len(data) % 256
    if remainder:
        chunk = data[full_pages * 256 :]
        chunk += b"\xff" * (256 - remainder)
        for index in range(64):
            offset = index * 4
            packet += bytes(
                [
                    4,
                    index,
                    chunk[offset],
                    chunk[offset + 1],
                    chunk[offset + 2],
                    chunk[offset + 3],
                ]
            )
        packet += bytes([2, 3, 8, 0, remainder, 0])

    packet += bytes([2, 3, 9, 0, 0, 0])
    print(f"tx_len={len(packet)} head={packet[:12].hex()} tail={packet[-12:].hex()}")
    if chunked:
        # 6-byte command chunks with a small delay, mimicking a slow UART/driver.
        for offset in range(0, len(packet), 6):
            ser.write(packet[offset : offset + 6])
            ser.flush()
            time.sleep(0.001)
    else:
        ser.write(packet)
        ser.flush()


def main() -> None:
    mode = sys.argv[1] if len(sys.argv) > 1 else "240"
    chunked = "--chunked" in sys.argv
    if mode == "240":
        width = height = 240
        data = IMAGE_240.read_bytes()
        assert len(data) == width * height * 2, len(data)
    elif mode == "32":
        width = height = 32
        data = bytes([0xF8, 0x00]) * (width * height)
    elif mode == "photo":
        width = height = 240
        data = None
    else:
        raise SystemExit(f"unknown mode: {mode}")

    port = find_port()
    print(f"port={port} mode={mode}")
    ser = serial.Serial(port, 19200, timeout=2, write_timeout=10)
    try:
        boot = wait_for(ser, b"MSN", 8)
        print(f"boot={boot.hex()}")

        ser.write(b"\x00MSNCN")
        handshake = wait_for(ser, b"MSNCN", 3)
        print(f"handshake={handshake.hex()}")

        # The vendor demo does SFR/UID reads and starts threads before LCD use.
        time.sleep(1.5)

        if mode == "photo":
            # Display stock image 1 from external Flash page 0.
            lcd_photo(ser, 0, 0, width, height, 0)
        else:
            write_lcd_photo_fast1(ser, 0, 0, width, height, data, chunked=chunked)

        time.sleep(3)
        after = ser.read(ser.in_waiting)
        print(f"after={after.hex()}")
        if b"MSN" in after:
            print("device_alive=yes")
        else:
            print("device_alive=no")
    finally:
        ser.close()


if __name__ == "__main__":
    main()