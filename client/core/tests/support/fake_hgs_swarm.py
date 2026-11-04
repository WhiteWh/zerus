#!/usr/bin/env python3
"""A fake `hgs swarm` for the organization tests. Reads `config.json` beside itself:
{"get": [reply, ...], "apply": [reply, ...]} with replies
{"exit": 0, "stdout": <object>, "stderr": "", "sleep": 0, "gate": "<name>"}; a
gate holds the reply until a file of that name appears beside the fake (at
most 20 s), so a test decides when a running call ends. Each call of a
subcommand takes its next reply and the last one repeats, so a list models
another writer changing the catalog between two calls. Appends
{"argv": [...], "stdin": "..."} to `log.ndjson` beside itself."""
import json
import os
import subprocess
import sys
import time

here = os.path.dirname(os.path.abspath(__file__))
args = sys.argv[1:]
data = sys.stdin.read()
with open(os.path.join(here, "log.ndjson"), "a") as log:
    log.write(json.dumps({"argv": args, "stdin": data}) + "\n")
if args in (["swarm", "initialize", "--g3-status", "--json"], ["swarm", "initialize", "--g3", "--json"]):
    action = "g3-status" if args[2] == "--g3-status" else "g3-import"
elif args == ["machine", "ls"]:
    action = "machines"
elif len(args) == 3 and args[0] == "swarm" and args[1] in ("preview", "join"):
    action = args[1]
elif len(args) == 2 and args[0] == "swarm" and args[1] in ("get", "apply", "sync", "resolve"):
    action = args[1]
else:
    sys.stderr.write("fake hgs: unexpected call\n")
    sys.exit(2)
with open(os.path.join(here, "config.json")) as config_file:
    replies = json.load(config_file)[action]
counts_path = os.path.join(here, "counts.json")
counts = {}
if os.path.exists(counts_path):
    with open(counts_path) as counts_file:
        counts = json.load(counts_file)
n = counts.get(action, 0)
counts[action] = n + 1
with open(counts_path, "w") as counts_file:
    json.dump(counts, counts_file)
reply = replies[min(n, len(replies) - 1)]
if reply.get("gate"):
    deadline = time.time() + 20
    while not os.path.exists(os.path.join(here, reply["gate"])) and time.time() < deadline:
        time.sleep(0.02)
time.sleep(reply.get("sleep", 0))
if reply.get("hold_output"):
    code = """import os, sys, time
deadline = time.monotonic() + 20
while not os.path.exists(sys.argv[1]) and time.monotonic() < deadline:
    time.sleep(.02)
"""
    subprocess.Popen([sys.executable, "-c", code, os.path.join(here, reply["hold_output"])],
                     stdin=subprocess.DEVNULL,
                     env={"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "HOME": here, "TMPDIR": here})
if reply.get("stdout") is not None:
    sys.stdout.write(json.dumps(reply["stdout"]))
sys.stderr.write(reply.get("stderr", ""))
sys.exit(reply.get("exit", 0))
