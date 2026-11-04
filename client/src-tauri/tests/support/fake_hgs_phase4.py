#!/usr/bin/env python3
"""Fake hgs for the Phase 4 smoke. Reads config.json and writes log.ndjson beside itself."""
import base64
import json
import os
import sys
import time


here = os.path.dirname(os.path.abspath(__file__))
with open(os.path.join(here, "config.json"), encoding="utf-8") as handle:
    config = json.load(handle)
argv = sys.argv[1:]
args = argv[1:] if argv[:1] and argv[0].startswith("@") else argv


def log(stdin=""):
    with open(os.path.join(here, "log.ndjson"), "a", encoding="utf-8") as handle:
        handle.write(json.dumps({"argv": argv, "stdin": stdin}, separators=(",", ":")) + "\n")


def emit(value):
    sys.stdout.write(json.dumps(value, separators=(",", ":")))
    sys.stdout.flush()


def request_receipt(request, status):
    receipt = {"status": status, "request_id": request["request_id"], "name": args[1],
               "run_id": request.get("expected_run_id", ""), "conversation_id": request.get("expected_conversation_id", "")}
    if "agent_id" in request:
        receipt["agent_id"] = request["agent_id"]
    return receipt


if args[:1] == ["a"]:
    log()
    os.execv(sys.executable, [sys.executable, config["attach"], *argv])

if args == ["--help"]:
    log()
    print("Event stream ABI: watch-v1")
    raise SystemExit(0)

stdin = sys.stdin.read()
log(stdin)
time.sleep(config.get("delays", {}).get(args[0] if args else "", 0))

if args[:1] == ["native-ui"] and len(args) >= 2:
    address = config.get("native_ui", {}).get(args[1])
    if not address:
        raise SystemExit(2)
    if args[2:] == ["--json"]:
        emit({"url": address})
    elif args[2:]:
        raise SystemExit(2)
    raise SystemExit(0)

if args[:1] == ["inspect"] and len(args) >= 2:
    if "--subagent" in args:
        agent = args[args.index("--subagent") + 1]
        key = args[1] + "\n" + agent
        if key in config.get("subagent_inspect", {}):
            emit(config["subagent_inspect"][key])
            raise SystemExit(0)
    elif args[1] in config.get("inspect", {}):
        emit(config["inspect"][args[1]])
        raise SystemExit(0)
    raise SystemExit(2)

if args[:1] == ["resume"]:
    if args[1:2] and args[1] in config.get("resume_ready", {}):
        ready = config.get("resume_inspect_ready", config["resume_ready"])
        config.setdefault("inspect", {})[args[1]] = ready[args[1]]
        path = os.path.join(here, "config.json")
        temporary = path + ".tmp"
        with open(temporary, "w", encoding="utf-8") as handle:
            json.dump(config, handle, separators=(",", ":"))
        os.replace(temporary, path)
    raise SystemExit(0)

if len(args) >= 3 and args[0] == "send" and args[2] == "--json":
    emit(request_receipt(json.loads(stdin), "submitted"))
    raise SystemExit(0)

if args[:1] == ["answer"] and args[2:3] == ["--json"]:
    request = json.loads(stdin)
    receipt = request_receipt(request, "answered")
    receipt.update(question_id=request["question_id"], question_hash=request["expected_question_hash"])
    emit(receipt)
    raise SystemExit(0)

if args[:1] == ["send-now"] and args[2:3] == ["--json"]:
    request = json.loads(stdin)
    receipt = request_receipt(request, "submitted")
    receipt["queue_id"] = request["queue_id"]
    emit(receipt)
    raise SystemExit(0)

if args[:1] == ["session-action"] and args[2:3] == ["--json"]:
    request = json.loads(stdin)
    if request.get("action") != "remove_queued":
        raise SystemExit(2)
    emit(request_receipt(request, "completed"))
    raise SystemExit(0)

if args[:1] in (["compact-context"], ["clear-context"]) and args[2:3] == ["--json"]:
    request = json.loads(stdin)
    if args[0] == "compact-context" and config.get("finish_compaction"):
        config["inspect"][args[1]]["compact_context_request"] = {
            "request_id": request["request_id"], "status": "completed"}
        path = os.path.join(here, "config.json")
        with open(path + ".tmp", "w", encoding="utf-8") as handle:
            json.dump(config, handle)
        os.replace(path + ".tmp", path)
    emit(request_receipt(request, "submitted"))
    raise SystemExit(0)

if args[:1] == ["attachment"] and args[2:4] == ["--stage", "--json"]:
    request = json.loads(stdin)
    receipt = request_receipt(request, "staged")
    files = []
    for index, attachment in enumerate(request["attachments"]):
        basename = os.path.basename(attachment["path"]) or f"attachment-{index}"
        files.append({"path": f"/home/example/.hgs/drop/{basename}"})
    receipt["files"] = files
    emit(receipt)
    raise SystemExit(0)

if args[:1] == ["attachment"] and "--request" in args:
    request_id = args[args.index("--request") + 1]
    index = int(args[args.index("--index") + 1])
    attachment = config["attachments"].get(request_id, config.get("attachment"))
    if attachment is None:
        raise SystemExit(2)
    emit({"request_id": request_id, "index": index, "name": attachment["name"], "mime": attachment["mime"],
          "data_base64": base64.b64encode(base64.b64decode(attachment["data_base64"])).decode("ascii")})
    raise SystemExit(0)

if args[:1] == ["processes"] and "--output" in args:
    process_id = args[args.index("--output") + 1]
    run_id = args[args.index("--run") + 1]
    conversation = args[args.index("--conversation") + 1]
    emit({"id": process_id, "run_id": run_id, "conversation_id": conversation,
          "output": "running 42 tests\n", "truncated": False, "status": "finished"})
    raise SystemExit(0)

if args[:1] == ["processes"] and "--stop" in args:
    process_id = args[args.index("--stop") + 1]
    run_id = args[args.index("--run") + 1]
    conversation = args[args.index("--conversation") + 1]
    emit({"id": process_id, "run_id": run_id, "conversation_id": conversation, "status": "requested"})
    raise SystemExit(0)

sys.stderr.write("unsupported fake hgs call\n")
raise SystemExit(2)
