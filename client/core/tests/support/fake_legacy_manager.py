#!/usr/bin/env python3
"""Private Windows management transport fixture; records keys, never credentials."""
import json
import os
import sys
from pathlib import Path

config = Path(sys.argv[sys.argv.index("--config") + 1])
root = config.parent
assert root.name.startswith("legacy-manager-")
action = sys.argv[2]
request = json.loads(sys.stdin.read())
assert not any(os.environ.get(k) for k in ["HGS_SESSION", "HGS_RUN_ID", "TMUX", "TMUX_PANE", "HGS_HOST_PANE", "HGS_HOST_SESSION", "HGS_AGENT", "HGS_EXPECTED_ID"])
with (root / "calls").open("a") as log:
    log.write(json.dumps({"action": action, "keys": sorted(request)}) + "\n")
state = json.loads(config.read_text())
mode = (root / "mode").read_text() if (root / "mode").exists() else ""
if action != "status":
    assert request["revision"] == state["revision"]
    if action == "save":
        assert request["node_token"] == "transient-fixture"
        state["server_url"] = request["server_url"]
        state["revision"] = "b" * 64
    if action == "start":
        state["running"] = True
        state["generation"] = "e" * 32
    if action == "stop":
        assert request["generation"] == state["generation"]
        state["running"] = False
        state["generation"] = None
    config.write_text(json.dumps(state))
    if mode == "unknown":
        print("private-output-must-not-surface", file=sys.stderr)
        sys.exit(7)
if mode == "bad-status":
    state["startup_at_login"] = True
print(json.dumps(state))
