#!/usr/bin/env python3
"""Run the Phase 4 Part A smoke against a fresh synthetic scratch directory."""
import argparse
import base64
import copy
import io
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import wave


CLIENT = Path(__file__).resolve().parent.parent
PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="


def fixture_config(replay_path=None):
    catalog = {}
    states = {}
    events = {}
    for line in (replay_path or CLIENT / "fixtures/replay.ndjson").read_text(encoding="utf-8").splitlines():
        frame = json.loads(line)
        if frame.get("t") == "snapshot" and frame.get("topic") == "catalog":
            for item in frame["items"]:
                catalog[item["key"]] = item["row"]
        elif frame.get("t") == "events" and frame.get("topic") == "activity":
            events.setdefault(frame["key"], []).extend(frame["events"])
        elif frame.get("t") == "state" and frame.get("topic") == "activity":
            states[frame["key"]] = frame["value"]
    inspect = {}
    for key, state in states.items():
        row = catalog[key]
        detail = copy.deepcopy(state)
        detail["tracked"] = True
        detail["state"] = row["state"]
        detail["events"] = events.get(key, [])
        detail["cursor"] = detail["events"][-1]["seq"] if detail["events"] else 0
        inspect[row["name"]] = detail
    review_key = next(key for key, row in catalog.items() if row["name"] == "claude/sample-project/review")
    row = catalog[review_key]
    review_events = events.get(review_key, [])
    inspect[row["name"]] = {
        "name": row["name"], "phase": row["phase"], "activity": row["activity"], "current_tool": row["current_tool"],
        "activity_summary": row["activity_summary"], "turn_started": row["turn_started"], "pending_questions": [],
        "history_truncated": False, "tracked": True, "run_id": row["run_id"], "conversation_id": row["conversation_id"],
        "state": row["state"], "runtime_state": row["runtime_state"], "process_state": row["process_state"],
        "resumable": True, "compact_context_supported": True, "clear_context_supported": True,
        "events": review_events, "cursor": review_events[-1]["seq"] if review_events else 0,
    }
    resumed = copy.deepcopy(inspect["claude/sample-project/review"])
    resumed.update(run_id=resumed["run_id"] + "-resumed", state="running", runtime_state="live", process_state="running",
                   phase="idle", activity="idle")
    api = catalog[next(key for key, row in catalog.items() if row["name"] == "claude/sample-project/api-refactor")]
    children = {}
    for agent, supported, scope, seq in (
        ("agent-writable", True, "Native subagent history", 900001),
        ("agent-readonly", False, "Recorded subagent activity", 900002),
    ):
        children[api["name"] + "\n" + agent] = {
            "name": api["name"], "agent_id": agent, "parent_conversation_id": api["conversation_id"],
            "conversation_id": api["conversation_id"] + "/" + agent, "run_id": api["run_id"],
            "state": "running", "runtime_state": "live", "process_state": "running", "phase": "idle", "activity": "idle",
            "tracked": True, "send_supported": supported, "history_scope": scope, "history_truncated": False,
            "attachments_supported": True, "events": [{"seq": seq, "type": "Stop", "source": "subagent_summary", "detail": scope, "at": 1793000000}],
            "cursor": seq,
        }
    orders = base64.b64encode(b'{"orders":[1,2,3]}\n').decode("ascii")
    return {
        "attach": str(CLIENT / "core/tests/support/fake_attach.py"),
        "inspect": inspect,
        "subagent_inspect": children,
        "resume_ready": {"claude/sample-project/review": resumed},
        "delays": {"clear-context": 0.5, "resume": 0.3},
        "attachments": {
            "req-fixture-1": {"name": "orders.json", "mime": "application/json", "data_base64": orders},
            "req-fixture-image": {"name": "pixel.png", "mime": "image/png", "data_base64": PNG},
            "req-fixture-echo-image": {"name": "pasted-image.png", "mime": "image/png", "data_base64": PNG},
        },
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, default=CLIENT / "src-tauri/target/release/zerus-client")
    parser.add_argument("--scratch", type=Path, required=True, help="a new directory; logs are kept here")
    parser.add_argument("--only", help="run one named phase4 step")
    parser.add_argument("--native-wheel", action="store_true", help="drive variable-scroll with trusted wheel events in its private Xvfb")
    parser.add_argument("--static-wheel", action="store_true", help="no appends; separate wheel gestures with long idle pauses")
    parser.add_argument("--renderer", choices=("shm", "dmabuf"), default="shm")
    parser.add_argument("--static-shape", choices=("messages", "mixed", "expanded"), default="messages")
    args = parser.parse_args()
    if args.static_wheel and not args.native_wheel:
        parser.error("--static-wheel requires --native-wheel")
    if args.static_wheel and args.only != "variable-scroll":
        parser.error("--static-wheel requires --only variable-scroll")
    if args.native_wheel and args.only not in ("variable-scroll", "activity-bottom"):
        parser.error("--native-wheel requires --only variable-scroll or activity-bottom")
    app = args.app.resolve()
    scratch = args.scratch.resolve()
    if not app.is_file():
        parser.error(f"the app does not exist: {app}")
    scratch.mkdir(parents=True, exist_ok=False)
    (scratch / "home").mkdir()
    (scratch / "data").mkdir()
    (scratch / "cache").mkdir()
    for part in ("config", "state", "tmux"):
        (scratch / part).mkdir(mode=0o700)
    (scratch / "runtime").mkdir(mode=0o700)
    (scratch / "tmp").mkdir(mode=0o700)
    hgs = scratch / "hgs"
    shutil.copy2(CLIENT / "src-tauri/tests/support/fake_hgs_phase4.py", hgs)
    hgs.chmod(0o755)
    frames = [json.loads(line) for line in (CLIENT / "fixtures/replay.ndjson").read_text(encoding="utf-8").splitlines()]
    if args.only == "resume-history":
        frames.append({"v": 1, "seq": max(frame["seq"] for frame in frames) + 1,
                       "t": "events", "topic": "activity", "key": "run:000000-run", "cursor": 900002,
                       "events": [
                           {"seq": 900001, "type": "UserPromptSubmit", "source": "claude_hooks", "detail": "Earlier private conversation", "at": 1793000001},
                           {"seq": 900002, "type": "Stop", "source": "claude_hooks", "detail": "Existing Activity reply before resume", "at": 1793000002}]})
    # Exercise both exact own-receipt rendering and an unmatched native entry.
    for frame in frames:
        value = frame.get("value", {})
        if frame.get("t") == "state" and frame.get("topic") == "activity" and value.get("name") == "codex/sample-project/release-notes":
            original = "**Queued review** [Image #1]"
            submitted = original + ' (Inspect the attached image at "/synthetic/attachments/message-queue/01-pasted-image.png")'
            value["input_queue"]["text"] = "• Queued follow-up inputs\n  ↳ " + submitted + "\n  ↳ also update the changelog"
            value["attachment_messages"] = [{"source": "hgs_delivery", "type": "UserPromptSubmit", "agent_id": "",
                "message_id": "req-fixture-echo-image", "detail": original, "submitted_text": submitted,
                "attachments": [{"request_id": "req-fixture-echo-image", "index": 0, "reference": "[Image #1]",
                                 "name": "pasted-image.png", "mime": "image/png", "bytes": 68}]}]
    activity_events = [event for frame in frames if frame.get("topic") == "activity" for event in frame.get("events", [])]
    media = {"seq": max(event["seq"] for event in activity_events) + 1, "type": "UserPromptSubmit", "source": "hgs_voice",
             "message_id": "m-media-smoke", "voice": {"id": "m-media-smoke", "state": "submitted",
             "audio": {"duration_ms": 60000, "waveform": [128] * 64, "request_id": "req-media-smoke", "index": 0},
             "transcript": "media smoke transcript"}, "at": max(event.get("at", 0) for event in activity_events) + 1}
    frames.append({"v": 1, "seq": max(frame["seq"] for frame in frames) + 1,
                   "t": "events", "topic": "activity", "key": "run:000001-run", "events": [media], "cursor": media["seq"]})
    replay = scratch / "replay.ndjson"
    replay.write_text("".join(json.dumps(frame, separators=(",", ":")) + "\n" for frame in frames), encoding="utf-8")
    config = fixture_config(replay)
    if args.only == "start-poll":
        config["resume_inspect_ready"] = copy.deepcopy(config["resume_ready"])
        for value in config["resume_ready"].values():
            value["phase"] = "starting"
        config["finish_compaction"] = True
    if args.only == "resume-status":
        # Only the followed stream knows readiness; a fallback inspection
        # cannot accidentally conceal a missed first frame in this scenario.
        config["resume_inspect_ready"] = copy.deepcopy(config["resume_ready"])
        for value in config["resume_inspect_ready"].values():
            value["phase"] = "starting"
    for index in range(150):
        config["attachments"][f"req-bottom-{index}"] = {"name": "late-image.png", "mime": "image/png", "data_base64": PNG}
    if args.only == "activity-bottom":
        config.setdefault("delays", {})["attachment"] = .4
    audio = io.BytesIO()
    with wave.open(audio, "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(8000)
        wav.writeframes(bytes(16000))  # Silent fixture: never play a tone on the host.
    config["attachments"]["req-media-smoke"] = {"name": "media-smoke.wav", "mime": "audio/wav",
                                                "data_base64": base64.b64encode(audio.getvalue()).decode("ascii")}
    (scratch / "config.json").write_text(json.dumps(config, separators=(",", ":")), encoding="utf-8")
    smoke_file = scratch / "smoke-notes.txt"
    smoke_file.write_text('{"orders":[1,2,3]}\n', encoding="utf-8")

    env = {
        "PATH": "/usr/bin:/bin", "LANG": "C.UTF-8",
        "HOME": str(scratch / "home"), "XDG_DATA_HOME": str(scratch / "data"), "XDG_CACHE_HOME": str(scratch / "cache"),
        "XDG_RUNTIME_DIR": str(scratch / "runtime"), "TMPDIR": str(scratch / "tmp"),
        "XDG_CONFIG_HOME": str(scratch / "config"), "XDG_STATE_HOME": str(scratch / "state"),
        "TMUX_TMPDIR": str(scratch / "tmux"), "HGS_CONFIG_DIR": str(scratch / "config"), "HGS_STATE_DIR": str(scratch / "state"),
        "WAYLAND_DISPLAY": "zerus-none", "GDK_BACKEND": "x11", "WEBKIT_DISABLE_DMABUF_RENDERER": "1" if args.renderer == "shm" else "0", "ZERUS_CLIENT_RENDERER": args.renderer, "ZERUS_CLIENT_HEADLESS": "1",
        "ZERUS_CLIENT_REPLAY": str(replay), "ZERUS_CLIENT_HGS": str(hgs),
        "ZERUS_CLIENT_QUERY": "phase4-smoke=1" + ("&phase4-only=" + args.only if args.only else "") + ("&phase4-native-wheel=1" + ("&phase4-static-wheel=1&phase4-static-shape=" + args.static_shape if args.static_wheel else "&phase4-provider-window=1") if args.native_wheel else ""), "ZERUS_CLIENT_PERF": "smoke", "ZERUS_CLIENT_SMOKE_FILE": str(smoke_file),
        "ZERUS_CLIENT_SMOKE_NOW": "1793000100",
    }
    child = [str(app), "--sessions"]
    if args.native_wheel:
        child = [sys.executable, str(CLIENT / "scripts/phase4-wheel-driver.py"), *child]
    command = ["dbus-run-session", "--", "xvfb-run", "-a", "-s", "-screen 0 1440x900x24", *child]
    app_log = scratch / "app.log"
    print("GDK_BACKEND=x11 WAYLAND_DISPLAY=zerus-none DISPLAY=<unset>", flush=True)
    with app_log.open("w", encoding="utf-8") as output:
        output.write("GDK_BACKEND=x11 WAYLAND_DISPLAY=zerus-none DISPLAY=<unset>\n")
        output.write("command: " + " ".join(command) + "\n")
        output.flush()
        process = subprocess.Popen(command, cwd=CLIENT, env=env, stdout=output, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            code = process.wait(timeout=180)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
            code = 124
    print(app_log.read_text(encoding="utf-8"), end="")
    print(f"phase4 smoke exit {code}; logs kept in {scratch}")
    return code


if __name__ == "__main__":
    raise SystemExit(main())
