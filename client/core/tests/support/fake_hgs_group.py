#!/usr/bin/env python3
"""Owned read-helper group fixtures. No real CLI, SSH, agent or service."""
import json
import os
import subprocess
import sys
import time
from pathlib import Path

root = Path(os.path.abspath(sys.argv[0])).parent
config = json.loads((root / "group.json").read_text())
args = sys.argv[1:]
host = args.pop(0)[1:] if args and args[0].startswith("@") else ""
with (root / "calls.jsonl").open("a") as log:
    log.write(json.dumps({"host": host, "args": args}) + "\n")


def hold():
    child = subprocess.Popen([sys.executable, "-c", "import time;time.sleep(20)"],
                             stdin=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    (root / "pids.json").write_text(json.dumps({"leader": os.getpid(),
                                               "group": os.getpgrp(), "child": child.pid}))
    if config.get("exit_first"):
        return
    # Like hgs waiting for ssh, both retain stdout until the timeout/stop.
    end = time.monotonic() + 20
    while not (root / "release").exists() and time.monotonic() < end:
        time.sleep(.01)
    child.wait(timeout=21)


if args in (["-h"], ["--help"]):
    if config["phase"] == "probe" and not (root / "pids.json").exists():
        hold()
    print("Event stream ABI: watch-v1", flush=True)
elif args == ["watch", "--json"]:
    frames = config.get("frames", [])
    for index, frame in enumerate(frames, 1):
        print(json.dumps(dict(frame, v=1, seq=index)), flush=True)
    if config["phase"] == "watch" and not (root / "pids.json").exists():
        hold()
    else:
        for line in sys.stdin:
            pass
else:
    print("{}")
