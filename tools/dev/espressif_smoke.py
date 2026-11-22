"""Boot a merged ESP32 flash image in Espressif's QEMU and check the boot log.

    espressif_smoke.py [--chip esp32s3|esp32c6] QEMU FLASH_IMAGE RUNS_DIRECTORY

Each run gets its own directory under RUNS_DIRECTORY holding a private copy of the
image (the firmware formats its LittleFS partition), `uart.log` and `qemu.log`.
The S3 run also keeps the LVGL frame in `screen.ppm` and `screen.png`.
"""

import argparse
import asyncio
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import zlib

TIMEOUT = 60
RNG_MARKER = "Xila: RNG "
LVGL_MARKER = "Xila: LVGL frame flushed"
SHELL_PROMPT_MARKER = "User name:"
ANSI_ESCAPE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
# Each marker must appear in this order: the smoke check fails on the first one
# that is missing, so the failing boot stage is named.
MARKERS = [
    "Xila ESP32 startup",
    "Xila: LittleFS mounted",
    RNG_MARKER,
    LVGL_MARKER,
    "Xila heap: ",
    SHELL_PROMPT_MARKER,
]
WIDTH, HEIGHT = 160, 120


class SmokeError(Exception):
    pass


def check_markers(log, chip):
    plain = ANSI_ESCAPE.sub("", log)
    lines = plain.splitlines()
    position = 0
    for marker in MARKERS:
        # The C6 machine lacks the RNG register and virtual RGB panel.
        if chip == "esp32c6" and marker in (RNG_MARKER, LVGL_MARKER):
            continue
        try:
            position = next(i for i in range(position, len(lines)) if marker in lines[i]) + 1
        except StopIteration:
            raise SmokeError(f"missing boot marker {marker!r}") from None
    heap = re.search(r"Xila heap: (\d+) bytes used, (\d+) free, (\d+) total", plain)
    if heap is None:
        raise SmokeError("malformed heap statistics line")
    used, free, total = map(int, heap.groups())
    if used + free != total or used == 0:
        raise SmokeError(f"inconsistent heap statistics: {used} used + {free} free != {total}")
    print(f"Heap after boot: {used} bytes used, {free} free, {total} total", flush=True)


async def capture_screen(screen):
    # A relative path keeps the Unix socket name within its length limit.
    reader, writer = await asyncio.open_unix_connection("qmp.sock")
    try:
        if "QMP" not in json.loads(await reader.readline()):
            raise SmokeError("missing QEMU monitor greeting")
        for command in (
            {"execute": "qmp_capabilities"},
            {"execute": "screendump", "arguments": {"filename": str(screen)}},
        ):
            writer.write(json.dumps(command).encode() + b"\n")
            await writer.drain()
            while True:
                line = await reader.readline()
                if not line:
                    raise SmokeError(f"QEMU monitor closed during {command['execute']}")
                reply = json.loads(line)
                if "error" in reply:
                    raise SmokeError(
                        f"QEMU monitor {command['execute']}: {reply['error']['desc']}"
                    )
                if "return" in reply:
                    break
    finally:
        writer.close()


def screendump(directory, deadline):
    """Capture the virtual panel through the QEMU monitor within the boot deadline."""
    screen = directory / "screen.ppm"
    try:
        remaining = deadline - time.monotonic()
        asyncio.run(asyncio.wait_for(capture_screen(screen), timeout=remaining))
    except TimeoutError:
        raise SmokeError("timed out capturing the frame from the QEMU monitor") from None
    except (OSError, ValueError) as error:
        raise SmokeError(f"could not capture the frame from the QEMU monitor: {error}") from error
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


def run(chip, qemu, image, runs):
    # Espressif QEMU exposes the virtual RGB panel on the S3 machine.
    check_display = chip == "esp32s3"
    runs.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix=f"{chip}.", dir=runs))
    print(f"Emulator artifacts: {directory}", flush=True)
    flash = directory / "flash.bin"
    shutil.copyfile(image, flash)
    uart = directory / "uart.log"
    os.chdir(directory)
    command = [
        qemu, "-M", chip, "-display", "none", "-monitor", "none", "-nic", "none",
        "-no-reboot", "-serial", f"file:{uart}",
        "-drive", f"file={flash},if=mtd,format=raw",
    ]
    if check_display:
        command += ["-qmp", "unix:qmp.sock,server=on,wait=off"]
    deadline = time.monotonic() + TIMEOUT
    with (directory / "qemu.log").open("wb") as diagnostics:
        emulator = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=diagnostics,
                                    stderr=subprocess.STDOUT)
        try:
            log = ""
            while SHELL_PROMPT_MARKER not in log:
                if emulator.poll() is not None:
                    raise SmokeError(f"QEMU exited with {emulator.returncode} before the shell prompt appeared")
                if time.monotonic() > deadline:
                    raise SmokeError(f"no {SHELL_PROMPT_MARKER!r} shell prompt after {TIMEOUT} s")
                time.sleep(0.2)
                log = uart.read_text(errors="replace") if uart.exists() else ""
                if "Xila panic:" in log:
                    raise SmokeError("the firmware panicked")
            check_markers(log, chip)
            if check_display:
                check_frame(screendump(directory, deadline))
        except BaseException:
            sys.stdout.write(uart.read_text(errors="replace") if uart.exists() else "")
            print(f"{chip} smoke failed; see {directory}", file=sys.stderr)
            raise
        finally:
            emulator.terminate()
            try:
                emulator.wait(timeout=5)
            except subprocess.TimeoutExpired:
                emulator.kill()
                try:
                    emulator.wait(timeout=5)
                except subprocess.TimeoutExpired as error:
                    raise SmokeError("QEMU did not exit after being killed") from error
    sys.stdout.write(uart.read_text(errors="replace"))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--chip", choices=("esp32s3", "esp32c6"), default="esp32s3")
    parser.add_argument("qemu", type=Path)
    parser.add_argument("image", type=Path)
    parser.add_argument("runs", type=Path)
    args = parser.parse_args()
    try:
        run(args.chip, str(args.qemu.resolve()), args.image.resolve(), args.runs.resolve())
    except SmokeError as error:
        print(f"{args.chip} smoke check failed: {error}", file=sys.stderr)
        sys.exit(1)
