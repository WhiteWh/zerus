#!/usr/bin/env python3
"""Trusted wheel input for phase4's own Xvfb only; never a desktop display."""
import ctypes as c
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import threading


def private_display():
    match = re.fullmatch(r":([0-9]+)(?:\.[0-9]+)?", os.environ.get("DISPLAY", ""))
    if not match or os.environ.get("ZERUS_CLIENT_HEADLESS") != "1" or os.environ.get("GDK_BACKEND") != "x11":
        raise RuntimeError("native wheel driver requires the fixture's private Xvfb")
    parent = os.getppid()
    if not any(
        Path(os.fsdecode(arg)).name == "xvfb-run" for arg in Path(f"/proc/{parent}/cmdline").read_bytes().split(b"\0") if arg
    ):
        raise RuntimeError("wheel driver is not a child of xvfb-run")
    fd = os.open(f"/tmp/.X{match[1]}-lock", os.O_RDONLY | os.O_NOFOLLOW)
    try:
        info = os.fstat(fd)
        if info.st_uid != os.geteuid() or not stat.S_ISREG(info.st_mode):
            raise RuntimeError("unexpected Xvfb lock owner/type")
        pid = int(os.read(fd, 64).strip())
    finally:
        os.close(fd)
    server = Path(f"/proc/{pid}")
    status = dict(line.split(":", 1) for line in (server / "status").read_text().splitlines() if ":" in line)
    if (server / "exe").resolve().name != "Xvfb" or int(status["PPid"]) != parent:
        raise RuntimeError("display server is not the original sibling Xvfb")
    if f":{match[1]}".encode() not in (server / "cmdline").read_bytes().split(b"\0"):
        raise RuntimeError("Xvfb display identity changed")


def main():
    private_display()
    x = c.CDLL("libX11.so.6")
    xt = c.CDLL("libXtst.so.6")
    if not x.XInitThreads():
        raise RuntimeError("private Xlib thread initialization failed")
    x.XOpenDisplay.argtypes = [c.c_char_p]
    x.XOpenDisplay.restype = c.c_void_p
    x.XFlush.argtypes = [c.c_void_p]
    x.XCloseDisplay.argtypes = [c.c_void_p]
    x.XKeysymToKeycode.argtypes = [c.c_void_p, c.c_ulong]
    x.XKeysymToKeycode.restype = c.c_uint
    xt.XTestFakeKeyEvent.argtypes = [c.c_void_p, c.c_uint, c.c_int, c.c_ulong]
    xt.XTestFakeMotionEvent.argtypes = [c.c_void_p, c.c_int, c.c_int, c.c_int, c.c_ulong]
    xt.XTestFakeButtonEvent.argtypes = [c.c_void_p, c.c_uint, c.c_int, c.c_ulong]
    display = x.XOpenDisplay(None)
    if not display:
        raise RuntimeError("private Xvfb connection failed")
    stopped = threading.Event()
    app = subprocess.Popen(sys.argv[1:], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
    worker = None
    wheel_done = threading.Event()

    def wheel(point):
        xt.XTestFakeMotionEvent(display, -1, point["x"], point["y"], 0)
        x.XFlush(display)
        for index in range(1000 if point.get("bottomReach") else 64 if point.get("staticWheel") else 160):
            if stopped.is_set() or wheel_done.is_set():
                return
            xt.XTestFakeButtonEvent(display, 5, 1, 0)
            xt.XTestFakeButtonEvent(display, 5, 0, 0)
            x.XFlush(display)
            delay = .9 if point.get("staticWheel") and index % 4 == 3 else .04
            if point.get("bottomReach"):
                delay = .02
            elif not point.get("staticWheel"):
                delay = .04 + index % 3 * .005 if index % 7 else .27 + index % 9 * .008
            stopped.wait(delay)

    try:
        for line in app.stdout:
            print(line, end="", flush=True)
            if "phase4-native-end-ready" in line:
                key = x.XKeysymToKeycode(display, 0xff57)  # XK_End, original private display only.
                xt.XTestFakeKeyEvent(display, key, 1, 0)
                xt.XTestFakeKeyEvent(display, key, 0, 0)
                x.XFlush(display)
            if "phase4-native-wheel-complete" in line:
                wheel_done.set()
            marker = "phase4-native-wheel-ready "
            if marker in line and worker is None:
                point = json.loads(line.split(marker, 1)[1])
                worker = threading.Thread(target=wheel, args=(point,), daemon=True)
                worker.start()
        return app.wait()
    finally:
        stopped.set()
        if worker:
            worker.join(timeout=2)
        if app.poll() is None:
            app.terminate()  # This fixture's original GUI only, never an agent.
            app.wait(timeout=5)
        x.XCloseDisplay(display)


if __name__ == "__main__":
    sys.exit(main())
