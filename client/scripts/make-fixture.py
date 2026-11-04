#!/usr/bin/env python3
"""Writes the synthetic replay fixtures of the client (stdlib only, deterministic).

fixtures/replay.ndjson       watch-v1 frames of this computer, for ZERUS_CLIENT_REPLAY
fixtures/replay-mac.ndjson   the same for a peer named "mac"
fixtures/replay-batches.json the same fleet as ready batches, for `npm run dev` in a browser
fixtures/replay-organization.json the fleet's shared project organization as `hgs swarm get`
                             prints it, for a fake `hgs` (the organization smoke)

Every name, path and message is synthetic: reserved example domains and
paths under /home/example/ only. The frames follow crates/watch-protocol: a hello, a
catalog snapshot in pages, then deltas separated by heartbeats (the replay
pauses one interval per heartbeat and loops the deltas). Activity frames are
held by the replay and served on follow, as the stream does after a subscribe.
This computer's stream also offers the `organization` topic: one snapshot of
the organization's rows follows `catalog.meta`. Every stream offers the
`processes` topic; this computer's holds one `processes` state, for
claude/sample-project/api-refactor, which the replay serves when the
Processes tab follows that session.
"""
import json
import pathlib
import random

ROOT = pathlib.Path(__file__).resolve().parent.parent / "fixtures"
INTERVAL_MS = 900
BASE_AT = 1_793_000_000.0  # late October 2026, UTC

rng = random.Random(20261028)
seq_counter = [0]


def next_seq():
    seq_counter[0] += 1
    return seq_counter[0]


PROJECTS = {
    "sample-project": ["review", "api-refactor", "pagination", "docs-pass", "flaky-test", "release-notes", "i18n", "perf-budget"],
    "aurora": ["checkout-form", "orders-v3", "image-runner", "migrations", "search-index", "a11y-audit", "theme-tokens"],
    "orders-api": ["rate-limits", "webhooks", "billing-export", "retry-queue", "openapi"],
    "docs-site": ["landing", "changelog", "api-reference", "examples"],
    "infra": ["ci-cache", "runner-images", "backup-check", "dns-move", "metrics"],
}
MAC_PROJECTS = {"ios-app": ["widgets", "push-setup", "testflight"], "aurora": ["safari-fixes", "keyboard-nav"]}
AGENTS = ["claude", "claude", "codex", "claude", "kimi", "codex", "opencode", "deepseek"]
MODELS = {"claude": ("opus", "high"), "codex": ("gpt-5.5-codex", "medium"), "kimi": ("kimi-k3", ""), "opencode": ("qwen3-coder", ""), "deepseek": ("deepseek-v4", "")}
TOOLS = [("Read", "src/routes/orders.ts"), ("Grep", "\"TODO\" src"), ("Bash", "cargo test --workspace"), ("Edit", "src/pagination/cursor.rs"),
         ("Bash", "npm run check"), ("Write", "docs/pagination.md"), ("Bash", "git status --short"), ("Read", "README.md")]
PROMPTS = [
    "Keep the change small and tell me before anything destructive.",
    "Please look at the failing pagination test and fix the cursor round trip.",
    "Review the diff against main and list anything risky.",
    "Add a short section to the docs that explains the new limits.",
    "Run the checks again and summarise what still fails.",
]
REPLIES = [
    "I fixed the cursor round trip in `src/pagination/cursor.rs` and added a test for an empty page.\n\n- `cargo test` passes\n- no API change",
    "The diff touches three files. The only risky part is the retry loop, which now caps at five attempts.",
    "Done. The docs explain the limits with one example request against https://api.example.com/v1/orders.",
    "Two checks still fail on main as well, so they are not caused by this change:\n\n- `lint: unused import`\n- `docs: broken anchor`",
    "I kept the change scoped to the branch and left a note in the task.",
]
# Project colours of the replay organization, in order; the Inbox keeps Qt's grey.
PALETTE = ["#3fb950", "#64b5f6", "#d29922", "#bc8cff", "#f778ba", "#56d4dd", "#e3b341", "#ff7b72"]
INBOX = {"id": "ungrouped", "name": "Inbox", "color": "#8d9baa"}
# Name-derived projects whose rows no project lists, so they show in the Inbox.
UNASSIGNED = {"infra"}
# Sessions per member row of the topic (src/watch/organization.rs CHUNK).
CHUNK = 1000
# The rank step of the Rust grouping (client/core/src/swarm/layout.rs GAP).
RANK_GAP = 1 << 20
NEEDS_APP = "Project changes need the desktop app."
# The session whose processes the replay serves.
PROCESS_SESSION = "claude/sample-project/api-refactor"
# The smoke's driven sessions stay fixed while the rest of the fleet replays deltas.
PHASE4_STABLE = {PROCESS_SESSION, "claude/sample-project/review", "codex/sample-project/pagination",
                 "claude/sample-project/docs-pass", "kimi/sample-project/flaky-test", "codex/sample-project/release-notes"}
THOUGHTS = ["The test builds the cursor from the last row, so an empty page needs its own branch.", "Checking whether the retry cap is configurable before changing it."]


def session_rows(projects, host_seed):
    rows = []
    i = 0
    for project, tags in projects.items():
        for tag in tags:
            agent = AGENTS[(i + host_seed) % len(AGENTS)]
            model, effort = MODELS[agent]
            run = f"{host_seed:02d}{i:04d}-run"
            state = rng.choices(["idle", "working", "tool", "approval", "input", "error", "paused"], [10, 3, 3, 1, 1, 1, 2])[0]
            unread = state == "idle" and rng.random() < 0.3
            tool, target = TOOLS[i % len(TOOLS)]
            reply = f"reply-{run}"
            row = {
                "name": f"{agent}/{project}/{tag}",
                "agent": agent,
                "cmd": "dsh" if agent == "deepseek" else agent,
                "project": project,
                "tag": tag,
                "run_id": run,
                "conversation_id": f"conv-{run}",
                "tracked": True,
                "cwd": f"/home/example/src/{project}",
                "git_branch": tag if i % 3 else "main",
                "model": model,
                "effort": effort,
                "phase": {"idle": "idle", "working": "working", "tool": "tool", "approval": "approval", "input": "input", "error": "error", "paused": "idle"}[state],
                "activity": "busy" if state in ("working", "tool") else "idle",
                "state": "paused" if state == "paused" else "running",
                "runtime_state": "paused" if state == "paused" else "live",
                "process_state": "stopped" if state == "paused" else "running",
                "conversation_state": "active",
                "activity_summary": f"{tool}: {target}" if state in ("working", "tool") else ("Waiting for approval: Bash" if state == "approval" else
                                    "Which layout should I keep?" if state == "input" else "Rate limit reached" if state == "error" else ""),
                "current_tool": tool if state == "tool" else "",
                "reply_id": reply,
                "read": {"reply_id": "" if unread else reply, "at": BASE_AT, "conversation_id": f"conv-{run}", "attention_signature": None},
                "last_event_at": BASE_AT + i * 37,
                "turn_started": BASE_AT + i * 37 - 120 if state in ("working", "tool") else 0,
            }
            rows.append(row)
            i += 1
    return rows


def key_of(row):
    return f"run:{row['run_id']}" if row.get("run_id") else f"name:{row['name']}"


def events_for(row, count):
    """A plausible conversation: prompts, tool runs, thinking and replies, oldest first."""
    events = []
    at = row["last_event_at"] - count * 20
    turn = 0
    while len(events) < count:
        at += 30
        prompt = PROMPTS[turn % len(PROMPTS)]
        events.append({"seq": next_seq(), "type": "UserPromptSubmit", "source": "claude_hooks", "detail": prompt, "at": at})
        if turn % 4 == 1:
            events.append({"seq": next_seq(), "type": "AgentThinking", "source": "claude_transcript", "detail": THOUGHTS[turn % len(THOUGHTS)], "at": at + 2})
        for t in range(1 + turn % 5):
            tool, target = TOOLS[(turn + t) % len(TOOLS)]
            events.append({"seq": next_seq(), "type": "PreToolUse", "source": "claude_hooks", "tool": tool, "detail": target, "at": at + 3 + t})
            failed = tool == "Bash" and turn % 7 == 3
            events.append({"seq": next_seq(), "type": "PostToolUseFailure" if failed else "PostToolUse", "source": "claude_hooks", "tool": tool,
                           "detail": "exit status 1" if failed else f"{target}: ok", "at": at + 4 + t})
            # Other sessions write to the same journal, so one session's seqs have gaps.
            seq_counter[0] += rng.randint(0, 3)
        if turn % 9 == 5:
            events.append({"seq": next_seq(), "type": "UserPromptSubmit", "source": "claude_hooks", "origin": "subagent_report", "sender": "explorer",
                           "detail": "Subagent report: the cursor module has two callers.", "report": "Both callers pass the last row; neither handles an empty page.", "at": at + 9})
        if turn % 11 == 7:
            events.append({"seq": next_seq(), "type": "UserPromptSubmit", "source": "claude_hooks", "origin": "peer_message", "sender": "codex/sample-project/review",
                           "detail": "Please rebase on main before you push; the CI cache moved.", "at": at + 10})
        events.append({"seq": next_seq(), "type": "Stop", "source": "claude_transcript", "detail": REPLIES[turn % len(REPLIES)], "at": at + 12})
        turn += 1
    return events[-count:] if len(events) > count else events


def phase4_children(row):
    if row["name"] == PROCESS_SESSION:
        row["subagents"] = {
            "agent-writable": {"label": "Writable child", "state": "working", "detail": "Ready for a direct message"},
            "agent-readonly": {"label": "Read-only child", "state": "working", "detail": "Managed by the parent conversation"},
        }
        row["subagent_counts_complete"] = True


def phase4_rows(rows):
    """The deterministic sessions the Part A smoke drives, whatever the random fleet does."""
    states = {
        "claude/sample-project/api-refactor": ("idle", "idle", "live", "running"),
        "codex/sample-project/pagination": ("idle", "idle", "live", "running"),
        "claude/sample-project/docs-pass": ("input", "busy", "live", "running"),
        "kimi/sample-project/flaky-test": ("approval", "idle", "live", "running"),
        "codex/sample-project/release-notes": ("working", "busy", "live", "running"),
    }
    for row in rows:
        if row["name"] == "claude/sample-project/review":
            row.update(phase="idle", activity="idle", state="paused", runtime_state="paused", process_state="stopped", resumable=True)
        elif row["name"] in states:
            phase, activity, runtime, process = states[row["name"]]
            row.update(phase=phase, activity=activity, state="running", runtime_state=runtime, process_state=process)
        if row["name"] == PROCESS_SESSION:
            row["process_work"] = processes_for(row)["process_work"]
        phase4_children(row)
    return rows


def phase4_question(row):
    return {"question_id": "tool:fixture-1", "question_hash": "0123456789abcdef" * 4,
            "run_id": row["run_id"], "conversation_id": row["conversation_id"], "source": "hook",
            "answer_transport": "claude_hook", "created_at": BASE_AT, "optional": False,
            "can_answer": True, "can_skip": False,
            "questions": [{"id": "q_0", "question": "Which checks should run?", "header": "Checks",
                           "options": [{"id": "opt_0_0", "label": "Unit"}, {"id": "opt_0_1", "label": "UI"}],
                           "allow_other": True}]}


def phase4_approval(row):
    return {"question_id": "tool:fixture-approval", "question_hash": "fedcba9876543210" * 4,
            "run_id": row["run_id"], "conversation_id": row["conversation_id"], "source": "kimi_tool_approval",
            "created_at": BASE_AT, "optional": False, "can_answer": True, "can_skip": False,
            "approval": True, "approval_choices": False,
            "questions": [{"id": "q_0", "question": "Run rm -rf build?", "body": "rm -rf build",
                           "options": [{"id": "allow", "label": "Allow"}, {"id": "deny", "label": "Deny"}]}]}


def phase4_echoes(row):
    """One readable native image echo and one whose guarded read must fail."""
    if row["name"] != "claude/sample-project/api-refactor":
        return [], []
    at = row["last_event_at"] + 120
    image_path = "/home/example/.local/state/hgs/sessions/attachments/message-echo01/01-pasted-image.png"
    missing_path = "/home/example/.local/state/hgs/sessions/attachments/message-missing/01-missing.png"
    image_raw = f'[Image #1] (Inspect the attached image at "{image_path}")\nReconciled image message'
    missing_raw = f'[Image #1] (Inspect the attached image at "{missing_path}")\nUnreadable image stays raw'
    image = {"name": "pasted-image.png", "request_id": "req-fixture-echo-image", "index": 0,
             "mime": "image/png", "bytes": 68, "reference": "[Image #1]"}
    missing = {"name": "missing.png", "request_id": "req-fixture-missing-image", "index": 0,
               "mime": "image/png", "bytes": 68, "reference": "[Image #1]"}
    events = [
        {"seq": next_seq(), "type": "UserPromptSubmit", "source": "claude_hooks", "detail": image_raw, "at": at},
        {"seq": next_seq(), "type": "UserPromptSubmit", "source": "claude_hooks", "detail": missing_raw, "at": at + 1},
    ]
    receipts = [
        {"type": "UserPromptSubmit", "source": "hgs_delivery", "message_id": image["request_id"],
         "detail": "[Image #1]\nReconciled image message", "submitted_text": image_raw, "attachments": [image]},
        {"type": "UserPromptSubmit", "source": "hgs_delivery", "message_id": missing["request_id"],
         "detail": "[Image #1]\nUnreadable image stays raw", "submitted_text": missing_raw, "attachments": [missing]},
    ]
    return events, receipts


def state_for(row):
    state = {"name": row["name"], "phase": row["phase"], "activity": row["activity"], "current_tool": row["current_tool"],
             "activity_summary": row["activity_summary"], "turn_started": row["turn_started"], "pending_questions": [], "history_truncated": False,
             "tracked": True, "run_id": row["run_id"], "conversation_id": row["conversation_id"],
             "runtime_state": row["runtime_state"], "process_state": row["process_state"]}
    if row["name"] == "claude/sample-project/api-refactor":
        state["session_usage"] = {"prompt_cache": {"status": "warm", "expires_at": BASE_AT + 1800}}
        state["clear_context_supported"] = True
        state["attachment_messages"] = phase4_echoes(row)[1]
        state["process_work"] = processes_for(row)["process_work"]
    elif row["name"] == "codex/sample-project/pagination":
        state.update(cache_hint={"status": "cold", "tokens": 120000}, compact_context_supported=True, clear_context_supported=True)
    elif row["name"] == "claude/sample-project/review":
        state.update(resumable=True, compact_context_supported=True, clear_context_supported=True)
    elif row["name"] == "claude/sample-project/docs-pass":
        state["pending_questions"] = [phase4_question(row)]
    elif row["name"] == "kimi/sample-project/flaky-test":
        state["pending_questions"] = [phase4_approval(row)]
    elif row["name"] == "codex/sample-project/release-notes":
        state["input_queue"] = {"id": "queue-fixture", "text": "↳ also update the changelog", "can_send_now": True,
                                "hint": "Interrupt the current turn and send pending messages now"}
    phase4_children(state)
    return state


CAPTIONS = {"attention": None, "error": "Rate limit reached", "paused": "Paused", "ready": "Ready"}


def processes_for(row):
    """The `processes` topic's value for one session: `processes` and `process_work`
    as `hgs inspect` reports them, without the `run_id` the topic strips
    (src/watch/processes.rs). A live `cargo test` tree (an OS tree's output stays in
    the recorded command or Terminal), a recorded build with no live confirmation
    and a finished fetch, whose output both can be read."""
    at = row["last_event_at"]

    def item(id, **fields):
        return {"id": id, "owner": "main", "provider": row["agent"], "run_id": row["run_id"],
                "conversation_id": row["conversation_id"], "cwd": row["cwd"], **fields}

    items = [
        item("tree-cargo-test", source="process_tree", command="cargo test --workspace", status="running",
             started_at=at - 95, updated_at=at, observed=True, background=True, os_pid=48211, child_count=3,
             description="PID 48211 (3 child processes)", capabilities={"output": False, "stop": True, "stdin": False}),
        item("call-npm-build", call_id="toolu_build", source="hooks", command="npm run build", status="unknown",
             last_status="running", started_at=at - 300, updated_at=at - 240, observed=False, background=True,
             capabilities={"output": True, "stop": False, "stdin": False}),
        item("call-git-fetch", call_id="toolu_fetch", source="hooks", command="git fetch", status="finished",
             started_at=at - 600, updated_at=at - 598, ended_at=at - 598, exit_code=0, output_at=at - 598,
             observed=True, background=False, capabilities={"output": True, "stop": False, "stdin": False}),
    ]
    processes = {"items": items, "active_count": 0, "live_count": 1, "unknown_count": 1, "complete": True,
                 "sampled_at": at, "notes": []}
    return {"processes": processes,
            "process_work": {"count": 1, "started_at": at - 95, "command": "cargo test --workspace", "sampled_at": at}}


def view_row(row, machine):
    """The view row the browser fixture carries, in the shape of client/core view.rs (a simplified projection)."""
    phase = row["phase"]
    unread = bool(row["reply_id"]) and row["read"]["reply_id"] != row["reply_id"]
    status = ("attention" if phase in ("approval", "input") else "error" if phase == "error" else "paused" if row["runtime_state"] == "paused"
              else "working" if row["activity"] == "busy" or phase in ("working", "tool", "compacting") else "ready")
    caption = ("Needs approval" if phase == "approval" else "Needs input" if phase == "input" else "Using a tool" if phase == "tool"
               else "Working" if status == "working" else "New reply" if status == "ready" and unread else CAPTIONS[status])
    return {"name": row["name"], "title": row["tag"], "agent": row["agent"], "status": status, "caption": caption, "phase": phase,
            "attention": "approval" if phase == "approval" else "question" if phase == "input" else None, "unread": unread, "muted": False,
            "machine": machine, "project": row["project"], "folder": row["cwd"].rstrip("/").rsplit("/", 1)[-1], "branch": row["git_branch"] or None,
            "model": row["model"] or None, "effort": row["effort"] or None, "summary": row["activity_summary"], "updated_at": row["last_event_at"],
            "turn_started": row["turn_started"] or None, "run_id": row["run_id"], "conversation_id": row["conversation_id"],
            "background": (row.get("process_work") or {}).get("count", -1)}


def deltas_for(rows):
    """A loop of changes that ends where it starts: tool progress, a finished turn, a replaced run."""
    volatile = [r for r in rows if r["name"] not in PHASE4_STABLE]
    working = [r for r in volatile if r["activity"] == "busy"][:3]
    idle = [r for r in volatile if r["phase"] == "idle" and r["runtime_state"] == "live"][:2]
    steps = []
    for n in range(12):
        upserts = []
        for w in working:
            tool, target = TOOLS[(n + len(w["tag"])) % len(TOOLS)]
            upserts.append(dict(w, phase="tool", current_tool=tool, activity_summary=f"{tool}: {target}", last_event_at=w["last_event_at"] + n))
        if idle:
            flip = idle[0]
            busy = n % 6 < 3
            upserts.append(dict(flip, phase="working" if busy else "idle", activity="busy" if busy else "idle",
                                activity_summary="Bash: ./scripts/smoke-test.sh" if busy else "",
                                read=dict(flip["read"], reply_id="" if not busy and n % 6 == 3 else flip["reply_id"])))
        steps.append({"upsert": upserts, "remove": []})
    if len(idle) > 1:
        old = idle[1]
        new = dict(old, run_id=old["run_id"] + "-2", conversation_id=old["conversation_id"] + "-2")
        steps[4]["remove"].append({"key": key_of(old), "reason": "replaced"})
        steps[4]["upsert"].append(new)
        steps[10]["remove"].append({"key": key_of(new), "reason": "replaced"})
        steps[10]["upsert"].append(old)
    return steps


METRIC_KEYS = ("cpu_percent", "cpu_count", "cpu_state", "cpu_interval_seconds", "load_1", "load_5", "load_15",
               "memory_used_bytes", "memory_total_bytes", "memory_available_bytes", "uptime_seconds", "sampled_at")


def metrics_for(host):
    """A synthetic sample with the keys of src/metrics.rs; the peer "mac" is a Mac."""
    mac = host == "mac"
    total = (32 if mac else 64) * 1024 ** 3
    return {"sampled_at": BASE_AT, "platform": "macos" if mac else "linux", "cpu_percent": 31.0 if mac else 12.0,
            "cpu_interval_seconds": 2.0, "cpu_state": "available", "cpu_count": 10 if mac else 16,
            "load_1": 2.4 if mac else 1.1, "load_5": 2.0 if mac else 0.9, "load_15": 1.8 if mac else 0.8,
            "memory_total_bytes": total, "memory_used_bytes": int(total * (0.74 if mac else 0.38)),
            "memory_available_bytes": int(total * (0.26 if mac else 0.62)), "uptime_seconds": 86400.0}


def machine_card(host):
    """The same sample as the core's MachineMeta projects it, for the browser replay."""
    metrics = metrics_for(host)
    return {"host": host, "hgs_version": "1.56.0", "platform": metrics["platform"], "ok": True,
            "metrics": {key: metrics[key] for key in METRIC_KEYS},
            "presence": {"idle_seconds": None, "locked": None, "locked_seconds": None},
            "local_sessions": None, "local_host": None}


def version_token(project_id):
    """A `versions` key as hgs writes it (src/swarm/catalog.rs Key::token)."""
    return json.dumps({"kind": "project", "id": project_id, "field": "name"}, separators=(",", ":"))


def organization(computers):
    """`hgs swarm get`'s object of the fleet: one project `p-<name>` per name-derived
    project in first-seen order, whose members are its rows (`<host>\n<session name>`),
    and the Inbox last. The rows of UNASSIGNED projects are listed nowhere."""
    projects = {}
    for _computer, host, rows, _long in computers:
        for row in rows:
            name = row["project"]
            if name in UNASSIGNED:
                continue
            if name not in projects:
                projects[name] = {"id": f"p-{name}", "name": name, "color": PALETTE[len(projects) % len(PALETTE)],
                                  "accessible": True, "folders": [], "sessions": []}
            projects[name]["sessions"].append(f"{host}\n{row['name']}")
    listed = [*projects.values(), dict(INBOX, accessible=True, folders=[], sessions=[])]
    ids = [project["id"] for project in listed]
    return {
        "schema": 1, "node_id": "node-workstation", "swarm_id": "swarm-replay", "digest": "replay-1", "initialized": True,
        "organization": {"version": 2, "default_project": INBOX["id"], "project_order": ids, "projects": listed},
        "versions": {version_token(project_id): [f"op-{project_id}-name"] for project_id in ids},
        "conflicts": [], "preferences": {"mobile_delivery": "auto"},
        "machines": [{"id": "node-workstation", "name": "workstation", "connection": None, "local": True},
                     {"id": "node-mac", "name": "mac", "connection": "mac", "local": False}],
        "peers": {"mac": {"last_sync": BASE_AT, "node": "node-mac"}},
        "worker": {"running": True, "current": True},
    }


def organization_rows(full):
    """The `organization` topic's rows of `full` by key, as src/watch/organization.rs
    `rows` builds them: the summary without `versions`, whose `organization.projects`
    are the IDs; `project:<id>` without `sessions`, with `session_chunks`;
    `members:<id>:<n>` of at most CHUNK sessions; `version:<token>`."""
    rows = {}
    for project in full["organization"]["projects"]:
        sessions = project["sessions"]
        chunks = [sessions[start:start + CHUNK] for start in range(0, len(sessions), CHUNK)]
        rows[f"project:{project['id']}"] = dict({k: v for k, v in project.items() if k != "sessions"}, session_chunks=len(chunks))
        for n, chunk in enumerate(chunks):
            rows[f"members:{project['id']}:{n}"] = {"sessions": chunk}
    summary = {k: v for k, v in full.items() if k not in ("versions", "written")}
    summary["organization"] = dict(full["organization"], projects=[p["id"] for p in full["organization"]["projects"]])
    rows["organization"] = summary
    for token, operations in full["versions"].items():
        rows[f"version:{token}"] = operations
    return dict(sorted(rows.items()))


def grouping_view(full):
    """The headers the Rust grouping sends for `full`, with edits disabled: the
    browser replay has no hub to run them."""
    projects = full["organization"]["projects"]
    return {"loaded": True, "default_project": INBOX["id"],
            "headers": [{"id": p["id"], "name": p["name"], "color": p["color"] or None, "accessible": p["accessible"],
                         "collapsed": False, "is_default": p["id"] == INBOX["id"], "palette": at % 8}
                        for at, p in enumerate(projects)],
            "ready": False, "reason": NEEDS_APP, "busy": False, "status": "", "status_error": False, "conflicts": 0}


def placements(full, computers):
    """Each member's project and rank, as the Rust grouping ranks them: the
    catalog's order inside a project, then the rows no project lists, in
    arrival order, in the default project; `(index in its project + 1) << 20`."""
    out = {}
    for project in full["organization"]["projects"]:
        for at, member in enumerate(project["sessions"]):
            out.setdefault(member, (project["id"], (at + 1) * RANK_GAP))
    fallback = 0
    for _computer, host, rows, _long in computers:
        for row in rows:
            member = f"{host}\n{row['name']}"
            if member not in out:
                fallback += 1
                out[member] = (INBOX["id"], fallback * RANK_GAP)
    return out


def group_out(computer, host, row, places):
    group, rank = places[f"{host}\n{row['name']}"]
    return {"computer": computer, "key": key_of(row), "group": group, "rank": rank}


def phase4_attachments(row):
    """A sent file and a real image for Task 9's fetch and convertFileSrc paths."""
    if row["name"] != "claude/sample-project/api-refactor":
        return []
    at = row["last_event_at"] + 60
    return [
        {"seq": next_seq(), "type": "UserPromptSubmit", "source": "claude_hooks", "detail": "The orders are attached.", "at": at,
         "attachments": [{"name": "orders.json", "request_id": "req-fixture-1", "index": 0,
                          "mime": "application/json", "size": 812, "bytes": 812, "reference": "[File #1]"}]},
        {"seq": next_seq(), "type": "UserPromptSubmit", "source": "claude_hooks", "detail": "The tiny image is attached too.", "at": at + 1,
         "attachments": [{"name": "pixel.png", "request_id": "req-fixture-image", "index": 0,
                          "mime": "image/png", "size": 68, "bytes": 68, "reference": "[Image #1]"}]},
        *phase4_echoes(row)[0],
    ]


def stream(host, rows, long_key, topic_rows=None):
    frames = []
    seq = [0]

    def frame(t, **body):
        seq[0] += 1
        frames.append({"v": 1, "seq": seq[0], "t": t, **body})

    frame("hello", machine_id="00000000-0000-4000-8000-0000000000" + ("01" if host == "workstation" else "02"), host=host, hgs_version="1.56.0",
          stream=f"stream-{host}", heartbeat_ms=10000, max_frame_bytes=1 << 20,
          topics=["catalog", "catalog.meta", "activity", "git", "processes"] + (["organization"] if topic_rows is not None else []),
          limits={"activity_subscriptions": 16, "page_bytes": 524288, "queue_bytes": 4194304, "activity_tail": 100, "activity_read": 200,
                  "control_line_bytes": 65536, "git_subscriptions": 64, "processes_subscriptions": 16})
    half = len(rows) // 2
    pages = [rows[:half], rows[half:]]
    for page, chunk in enumerate(pages):
        frame("snapshot", topic="catalog", items=[{"key": key_of(r), "row": r} for r in chunk], page=page, last=page == len(pages) - 1, cursor=1)
    frame("snapshot", topic="catalog.meta", value={"host": host, "ok": True, "hgs_version": "1.56.0", "projects": {}, "peers": [],
                                                   "metrics": metrics_for(host)}, page=0, last=True, cursor=1)
    if topic_rows is not None:
        frame("snapshot", topic="organization", items=[{"key": k, "row": r} for k, r in topic_rows.items()], page=0, last=True, cursor=1)
    activity = {}
    for r in rows:
        count = 1100 if key_of(r) == long_key else rng.randint(8, 40)
        events = events_for(r, count) + phase4_attachments(r)
        activity[key_of(r)] = (events, state_for(r))
    # Activity first, so the replay holds every session's log before the deltas loop.
    for key, (events, state) in activity.items():
        for start in range(0, len(events), 200):
            chunk = events[start:start + 200]
            frame("events", topic="activity", key=key, events=chunk, cursor=chunk[-1]["seq"])
        if state["name"] != "claude/sample-project/review":
            frame("state", topic="activity", key=key, value=state, rev=1)
    # Held by the replay like Activity, and served when the Processes tab follows the session.
    for r in rows:
        if r["name"] == PROCESS_SESSION:
            frame("state", topic="processes", key=key_of(r), value=processes_for(r), rev=1)
    for n, step in enumerate(deltas_for(rows)):
        frame("heartbeat", at=BASE_AT + n)
        frame("delta", topic="catalog", upsert=[{"key": key_of(r), "row": r} for r in step["upsert"]], remove=step["remove"], cursor=2 + n)
    return frames, activity


def main():
    ROOT.mkdir(parents=True, exist_ok=True)
    local_rows = phase4_rows(session_rows(PROJECTS, 0))
    # The long Activity (1 100 events) belongs to the first working session.
    long_key = key_of(next(r for r in local_rows if r["activity"] == "busy"))
    mac_rows = session_rows(MAC_PROJECTS, 7)
    computers = [("", "workstation", local_rows, long_key), ("mac", "mac", mac_rows, None)]
    full = organization(computers)
    places = placements(full, computers)
    batches = {"interval_ms": INTERVAL_MS, "computers": [], "machines": [], "snapshot": [], "deltas": [], "activity": {},
               "grouping": grouping_view(full), "groups": []}
    for computer, host, rows, long in computers:
        frames, activity = stream(host, rows, long, organization_rows(full) if computer == "" else None)
        name = "replay.ndjson" if computer == "" else f"replay-{computer}.ndjson"
        (ROOT / name).write_text("".join(json.dumps(f, separators=(",", ":")) + "\n" for f in frames), encoding="utf-8")
        machine = host
        batches["computers"].append({"computer": computer, "status": "live", "attempt": 0})
        batches["machines"].append({"computer": computer, "machine": machine_card(host)})
        batches["snapshot"].append({"computer": computer, "rows": [{"computer": computer, "key": key_of(r), "row": view_row(r, machine)} for r in rows]})
        batches["groups"] += [group_out(computer, host, r, places) for r in rows]
        present = {key_of(r) for r in rows}
        for n, step in enumerate(deltas_for(rows)):
            if len(batches["deltas"]) <= n:
                batches["deltas"].append({"upserts": [], "removes": []})
            batches["deltas"][n]["upserts"] += [{"computer": computer, "key": key_of(r), "row": view_row(r, machine)} for r in step["upsert"]]
            batches["deltas"][n]["removes"] += [{"computer": computer, "key": k["key"], "reason": k["reason"]} for k in step["remove"]]
            # A row that arrives (a replaced run) gets its place, as the Rust grouping sends it.
            present -= {k["key"] for k in step["remove"]}
            arrived = [r for r in step["upsert"] if key_of(r) not in present]
            present |= {key_of(r) for r in step["upsert"]}
            if arrived:
                batches["deltas"][n].setdefault("groups", []).extend(group_out(computer, host, r, places) for r in arrived)
        for key, (events, state) in activity.items():
            batches["activity"][f"{computer}\n{key}"] = {"events": events, "cursor": events[-1]["seq"], "state": state}
    (ROOT / "replay-batches.json").write_text(json.dumps(batches, separators=(",", ":")) + "\n", encoding="utf-8")
    (ROOT / "replay-organization.json").write_text(json.dumps(full, indent=2) + "\n", encoding="utf-8")
    for path in sorted(ROOT.iterdir()):
        print(f"{path.name}: {path.stat().st_size} bytes")


if __name__ == "__main__":
    main()
