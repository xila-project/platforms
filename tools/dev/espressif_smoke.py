"""Boot a merged ESP32-S3 flash image in Espressif's QEMU and check the boot log.

    espressif_smoke.py QEMU_SYSTEM_XTENSA FLASH_IMAGE RUNS_DIRECTORY

Each run gets its own directory under RUNS_DIRECTORY holding a private copy of the
image (the firmware formats its LittleFS partition), `uart.log`, `qemu.log` and
`screen.ppm` and `screen.png`, the frame that LVGL drew on QEMU's virtual RGB panel.
"""

import json
import os
from pathlib import Path
import re
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import zlib

TIMEOUT = 60
# Each marker must appear in this order: the smoke check fails on the first one
# that is missing, so the failing boot stage is named.
MARKERS = [
    "Xila ESP32 startup",
    "Xila: RNG ",
    "Xila: LittleFS mounted",
    "Xila: LVGL frame flushed",
    "Xila heap: ",
    "Xila: boot smoke ready",
]
WIDTH, HEIGHT = 160, 120


class SmokeError(Exception):
    pass


def check_markers(log):
    lines = log.splitlines()
    position = 0
    for marker in MARKERS:
        try:
            position = next(i for i in range(position, len(lines)) if lines[i].startswith(marker)) + 1
        except StopIteration:
            raise SmokeError(f"missing boot marker {marker!r}") from None
    heap = re.search(r"^Xila heap: (\d+) bytes used, (\d+) free, (\d+) total", log, re.MULTILINE)
    if heap is None:
        raise SmokeError("malformed heap statistics line")
    used, free, total = map(int, heap.groups())
    if used + free != total or used == 0:
        raise SmokeError(f"inconsistent heap statistics: {used} used + {free} free != {total}")
    print(f"Heap after boot: {used} bytes used, {free} free, {total} total", flush=True)


def screendump(directory, deadline):
    """Capture the virtual RGB panel through QMP (this QEMU build writes PPM only)."""
    screen = directory / "screen.ppm"
    with socket.socket(socket.AF_UNIX) as qmp:
        qmp.settimeout(max(1.0, deadline - time.monotonic()))
        qmp.connect("qmp.sock")  # Relative: a Unix socket path holds at most 107 bytes.
        with qmp.makefile("rwb", buffering=0) as stream:
            if "QMP" not in json.loads(stream.readline()):
                raise SmokeError("missing QMP greeting")
            for command in (
                {"execute": "qmp_capabilities"},
                {"execute": "screendump", "arguments": {"filename": str(screen)}},
            ):
                stream.write(json.dumps(command).encode() + b"\n")
                while True:
                    reply = json.loads(stream.readline())
                    if "error" in reply:
                        raise SmokeError(f"QMP {command['execute']}: {reply['error']['desc']}")
                    if "return" in reply:
                        break
    return screen


def png_chunk(kind, payload):
    body = kind + payload
    return struct.pack(">I", len(payload)) + body + struct.pack(">I", zlib.crc32(body))


def check_frame(screen):
    """Reject a blank frame, and keep a PNG copy of the capture for viewers."""
    with screen.open("rb") as ppm:
        magic, size, depth = (ppm.readline().split() for _ in range(3))
        pixels = ppm.read()
    if (magic, size, depth) != ([b"P6"], [b"%d" % WIDTH, b"%d" % HEIGHT], [b"255"]):
        raise SmokeError(f"expected a {WIDTH}x{HEIGHT} P6 frame, got {magic} {size} {depth}")
    if len(pixels) != WIDTH * HEIGHT * 3:
        raise SmokeError(f"truncated frame: {len(pixels)} bytes")
    if len(set(pixels)) < 4:
        raise SmokeError("the virtual panel is blank: LVGL's frame never reached it")
    rows = b"".join(b"\0" + pixels[y * WIDTH * 3:(y + 1) * WIDTH * 3] for y in range(HEIGHT))
    png = (b"\x89PNG\r\n\x1a\n"
           + png_chunk(b"IHDR", struct.pack(">IIBBBBB", WIDTH, HEIGHT, 8, 2, 0, 0, 0))
           + png_chunk(b"IDAT", zlib.compress(rows)) + png_chunk(b"IEND", b""))
    screen.with_suffix(".png").write_bytes(png)
    print(f"Captured the {WIDTH}x{HEIGHT} LVGL frame: {screen.with_suffix('.png')}", flush=True)


def run(qemu, image, runs):
    runs.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="esp32s3.", dir=runs))
    print(f"Emulator artifacts: {directory}", flush=True)
    flash = directory / "flash.bin"
    shutil.copyfile(image, flash)
    uart = directory / "uart.log"
    os.chdir(directory)
    command = [
        qemu, "-M", "esp32s3", "-display", "none", "-monitor", "none", "-nic", "none",
        "-no-reboot", "-serial", f"file:{uart}",
        "-drive", f"file={flash},if=mtd,format=raw",
        "-qmp", "unix:qmp.sock,server=on,wait=off",
    ]
    deadline = time.monotonic() + TIMEOUT
    with (directory / "qemu.log").open("wb") as diagnostics:
        emulator = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=diagnostics,
                                    stderr=subprocess.STDOUT)
        try:
            log = ""
            while "Xila: boot smoke ready" not in log:
                if emulator.poll() is not None:
                    raise SmokeError(f"QEMU exited with {emulator.returncode} before the boot completed")
                if time.monotonic() > deadline:
                    raise SmokeError(f"no boot marker after {TIMEOUT} s")
                time.sleep(0.2)
                log = uart.read_text(errors="replace") if uart.exists() else ""
                if "Xila panic:" in log:
                    raise SmokeError("the firmware panicked")
            check_markers(log)
            check_frame(screendump(directory, deadline))
        except BaseException:
            sys.stdout.write(uart.read_text(errors="replace") if uart.exists() else "")
            print(f"ESP32-S3 smoke failed; see {directory}", file=sys.stderr)
            raise
        finally:
            emulator.terminate()
            try:
                emulator.wait(timeout=5)
            except subprocess.TimeoutExpired:
                emulator.kill()
                emulator.wait()
    sys.stdout.write(uart.read_text(errors="replace"))


if __name__ == "__main__":
    qemu, image, runs = (Path(argument).resolve() for argument in sys.argv[1:])
    try:
        run(str(qemu), image, runs)
    except SmokeError as error:
        print(f"ESP32-S3 smoke check failed: {error}", file=sys.stderr)
        sys.exit(1)
