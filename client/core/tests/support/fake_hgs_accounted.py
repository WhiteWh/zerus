#!/usr/bin/env python3
"""Private stream-resource fixtures; descendants exit on the release file or deadline."""
import json
import os
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(os.path.abspath(sys.argv[0])).parent
CONFIG = json.loads((ROOT / "accounted.json").read_text())
RELEASE = ROOT / "release"
args = sys.argv[1:]
host = ""
if args and args[0].startswith("@"):
    host = args[0][1:]
    args = args[1:]
CONFIG = CONFIG.get("hosts", {}).get(host, CONFIG)
with (ROOT / "calls.jsonl").open("a") as out:
    out.write(json.dumps(args) + "\n")


def wait_release():
    end = time.monotonic() + 20
    while not RELEASE.exists() and time.monotonic() < end:
        time.sleep(.01)


def holder():
    # Intentionally escape the helper group: cancellation must not chase a
    # detached session. Its pipe remains charged until the explicit release.
    code = "import os,time;end=time.monotonic()+20\nwhile not os.path.exists(%r) and time.monotonic()<end: time.sleep(.01)" % str(RELEASE)
    pipe = CONFIG["pipe"]
    subprocess.Popen([sys.executable, "-c", code],
                     start_new_session=True,
                     stdin=sys.stdin if pipe == "stdin" else subprocess.DEVNULL,
                     stdout=sys.stdout if pipe == "stdout" else subprocess.DEVNULL,
                     stderr=sys.stderr if pipe == "stderr" else subprocess.DEVNULL)


phase = CONFIG["phase"]
if args == ["-h"]:
    if phase == "probe":
        holder()
        (ROOT / "ready").touch()
    if phase != "legacy":
        print(CONFIG["marker"], flush=True)
elif args == ["watch", "--json"]:
    for index, frame in enumerate(CONFIG["frames"], 1):
        print(json.dumps(dict(frame, v=1, seq=index)), flush=True)
    if phase == "watch":
        holder()
        (ROOT / "ready").touch()
    if CONFIG.get("remove_peers"):
        end = time.monotonic() + 20
        while not (ROOT / "remove-peers").exists() and time.monotonic() < end:
            time.sleep(.01)
        print(json.dumps(dict(CONFIG["remove_peers"], v=1,
                              seq=len(CONFIG["frames"]) + 1)), flush=True)
    if CONFIG["pipe"] == "stdin":
        import array
        import fcntl
        import termios
        end = time.monotonic() + 20
        while not RELEASE.exists() and time.monotonic() < end:
            available = array.array("i", [0])
            fcntl.ioctl(sys.stdin.fileno(), termios.FIONREAD, available, True)
            if available[0] > 0:
                (ROOT / "stdin-buffered").touch()
            time.sleep(.01)
    else:
        for line in sys.stdin:
            if phase == "worker" and json.loads(line).get("topic") == "organization":
                for index, frame in enumerate(CONFIG["organization_frames"], len(CONFIG["frames"]) + 1):
                    print(json.dumps(dict(frame, v=1, seq=index)), flush=True)
elif args == ["ls", "--json"]:
    if phase == "legacy":
        holder()
        (ROOT / "ready").touch()
    print(json.dumps({"sessions": []}), flush=True)
elif args == ["swarm", "get"]:
    holder()
    (ROOT / "ready").touch()
    print(json.dumps({"machines": [], "digest": "fixture"}), flush=True)
else:
    sys.exit(2)
