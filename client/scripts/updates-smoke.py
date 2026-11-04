#!/usr/bin/env python3
"""Capture Updates and the Sessions status bar from bounded replay views, without network."""
import argparse
import copy
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess


CLIENT = Path(__file__).resolve().parent.parent
STATES = ("unchecked", "available", "cached-error", "restart-required", "statusbar")
THEMES = ("dark", "light")

def text(source):
    """A core Text (client/src/text.ts) the webview translates."""
    return {"source": source}


def verbatim(value):
    return {"source": "%1", "args": [{"kind": "verbatim", "value": value}]}


BASE = {
    "busy": False,
    "available": False,
    "heading": text("Updates have not been checked"),
    "installed": verbatim("0.46.2 (abcdef0)"),
    "latest": text("Not checked"),
    "comparison": None,
    "last_checked": "Never",
    "cached": False,
    "error": None,
    "channel": "stable",
    "automatic": True,
    "release_label": text("View release"),
    "release_enabled": False,
    "download_label": None,
    "details": {
        "ownership": text("Local source checkout. Update it from the exact verified source."),
        "command": verbatim("yay -S zerus"),
        "copy_name": text("Copy the update command"),
        "instructions": text("Review the installation instructions before updating."),
    },
    "restart_required": False,
    "login_item_deprioritized": False,
    "version": "",
    "quiet": False,
}


def view(state):
    result = copy.deepcopy(BASE)
    if state == "checking":
        result.update(busy=True, heading=text("Checking for updates…"), comparison=text("A previous comparison"))
    elif state in ("available", "cached-error", "restart-required", "statusbar"):
        result.update(
            available=True,
            heading=text("Update available"),
            latest=verbatim("0.47.0"),
            comparison=text("Update available for this local build."),
            last_checked="2026-10-30T00:00:00Z",
            release_enabled=True,
            download_label=text("Download dev source"),
            version="0.47.0",
        )
        if state == "cached-error":
            result.update(cached=True, error=text("Check failed. Try again later."))
        elif state in ("restart-required", "statusbar"):
            result["restart_required"] = True
        if state == "statusbar":
            result["login_item_deprioritized"] = True
    elif state != "unchecked":
        raise ValueError(f"unknown update replay state: {state}")
    return result


def run_case(app, replay, scratch, state, theme):
    case = scratch / f"{state}-{theme}"
    for name in ("home", "data", "cache", "tmp", "runtime"):
        (case / name).mkdir(parents=True)
    (case / "runtime").chmod(0o700)
    fixture = case / "view.json"
    fixture.write_text(json.dumps(view(state), separators=(",", ":")), encoding="utf-8")
    image = scratch / f"{state}-{theme}.png"
    log = case / "app.log"
    page = "sessions" if state == "statusbar" else "settings"
    query = f"page={page}&section=updates&skin=zerus-{theme}&updates-smoke={state}"
    script = case / "run.sh"
    script.write_text(f"""set -eu
{shlex.quote(str(app))} 2> {shlex.quote(str(log))} &
pid=$!
trap 'kill "$pid" 2>/dev/null || true' EXIT
n=0
until grep -q {shlex.quote(f"updates-smoke {state}: ready")} {shlex.quote(str(log))} 2>/dev/null; do
  if grep -q {shlex.quote(f"updates-smoke {state}: failed")} {shlex.quote(str(log))} 2>/dev/null; then
    cat {shlex.quote(str(log))}
    exit 1
  fi
  if ! kill -0 "$pid" 2>/dev/null; then
    cat {shlex.quote(str(log))}
    exit 1
  fi
  n=$((n + 1))
  if [ "$n" -ge 300 ]; then
    cat {shlex.quote(str(log))}
    exit 1
  fi
  sleep 0.1
done
sleep 1
import -window root {shlex.quote(str(image))}
wait "$pid"
trap - EXIT
""", encoding="utf-8")
    env = os.environ.copy()
    env.pop("DISPLAY", None)
    env.pop("ZERUS_CLIENT_HGS", None)
    env.update(
        HOME=str(case / "home"),
        XDG_DATA_HOME=str(case / "data"),
        XDG_CACHE_HOME=str(case / "cache"),
        TMPDIR=str(case / "tmp"),
        XDG_RUNTIME_DIR=str(case / "runtime"),
        WAYLAND_DISPLAY="zerus-none",
        GDK_BACKEND="x11",
        WEBKIT_DISABLE_DMABUF_RENDERER="1",
        ZERUS_CLIENT_HEADLESS="1",
        ZERUS_CLIENT_REPLAY=str(replay),
        ZERUS_CLIENT_UPDATES_FIXTURE=str(fixture),
        ZERUS_CLIENT_QUERY=query,
        ZERUS_CLIENT_PERF="smoke",
    )
    command = ["xvfb-run", "-a", "-s", "-screen 0 1440x900x24", "sh", str(script)]
    result = subprocess.run(command, cwd=CLIENT, env=env, timeout=60, check=False)
    if result.returncode or not image.is_file() or image.stat().st_size == 0:
        raise RuntimeError(f"updates smoke failed for {state} {theme}; see {log}")
    print(f"updates-smoke {state} {theme}: {image}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, default=CLIENT / "src-tauri/target/release/zerus-client")
    parser.add_argument("--replay", type=Path, default=CLIENT / "fixtures/replay.ndjson")
    parser.add_argument("--scratch", type=Path, required=True, help="a new directory for logs and screenshots")
    args = parser.parse_args()
    app = args.app.resolve()
    replay = args.replay.resolve()
    scratch = args.scratch.resolve()
    if not app.is_file():
        parser.error(f"the app does not exist: {app}")
    if not replay.is_file():
        parser.error(f"the replay does not exist: {replay}")
    for tool in ("xvfb-run", "import"):
        if not shutil.which(tool):
            parser.error(f"required tool is unavailable: {tool}")
    scratch.mkdir(parents=True, exist_ok=False)
    for state in STATES:
        for theme in THEMES:
            run_case(app, replay, scratch, state, theme)
    print(f"ten offline WebKitGTK screenshots are in {scratch}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
