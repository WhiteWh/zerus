#!/usr/bin/env python3
"""Private Machines UI fixture. Never opens SSH, files outside its scratch, or services."""
import hashlib
import json
from pathlib import Path
import sys

root = Path(__file__).resolve().parent
config = json.loads((root / "config.json").read_text())
if not config.get("machines_fixture"):
    raise SystemExit("private Machines fixture config required")
args = sys.argv[1:]
with (root / "log.ndjson").open("a") as stream:
    stream.write(json.dumps({"argv": args, "stdin": sys.stdin.read()}) + "\n")
store = root / "machine-profiles.json"
if not store.exists():
    store.write_text(json.dumps({"fixture-peer": {"hostname": "peer.invalid", "user": "fixture", "port": None, "identity_file": "", "enabled": not config.get("disabled_peer", False)}}))
raw = store.read_bytes()
profiles = json.loads(raw)
revision = hashlib.sha256(raw).hexdigest()

def emit(value):
    print(json.dumps(value))

if args == ["--help"]:
    print("Event stream ABI: watch-v1")
    raise SystemExit(0)
if args == ["machine", "ls"]:
    emit({"local": "workstation", "revision": revision, "environment_override": False, "machines": [
        {"alias": alias, "connection": row, "enabled": row["enabled"], "source": "local", "inherited": alias == "fixture-peer"}
        for alias, row in profiles.items()]})
elif args[:2] == ["machine", "resolve"]:
    emit({"alias": args[2], "effective": {"hostname": "peer.invalid", "user": "fixture", "port": "22", "identity_files": ["/home/example/.ssh/id_fixture"]}})
elif args[:2] == ["machine", "setup"] and config.get("setup_fixture"):
    import time
    assert args[args.index("--revision") + 1] == revision and args[2] in profiles
    time.sleep(.15)
    count = sum(json.loads(line)["argv"][:2] == ["machine", "setup"] for line in (root / "log.ndjson").read_text().splitlines())
    print("Setup complete: hgs fixture-installed" if count == 1 else "Original setup output; uncertain fixture result")
    if count > 1: raise SystemExit(75)
elif args == ["session-host", "status", "--json"] and config.get("setup_fixture"):
    emit({"enabled": False})
elif args == ["session-host", "install", "--json"] and config.get("setup_fixture"):
    emit({"enabled": True, "cli": {"path": "C:\\Users\\example\\hgs.exe"}})
elif args[:2] == ["machine", "check"]:
    emit({"ok": True, "ssh_ok": True, "os": "Linux", "arch": "x86_64", "hgs": "hgs fixture", "tmux": "tmux fixture"})
elif args[:1] == ["machine"] and args[1:2] in (["set"], ["reset"], ["remove"]):
    expected = args[args.index("--revision") + 1]
    if expected != revision:
        print("Machine settings changed elsewhere. Reload the page before saving.", file=sys.stderr)
        raise SystemExit(1)
    alias = args[2]
    if args[1] == "set":
        if "--create" in args and alias in profiles:
            print("A machine with this name already exists.", file=sys.stderr)
            raise SystemExit(1)
        profiles[alias] = json.loads(args[args.index("--json") + 1])
    elif args[1] == "reset":
        profiles[alias] = {"hostname": "", "user": "", "port": None, "identity_file": "", "enabled": True}
    else:
        profiles.pop(alias, None)
    stage = store.with_suffix(".tmp")
    stage.write_text(json.dumps(profiles));stage.replace(store)
    emit({"ok": True, "alias": alias, "dry_run": False})
else:
    print("unsupported private Machines fixture call", file=sys.stderr)
    raise SystemExit(2)
