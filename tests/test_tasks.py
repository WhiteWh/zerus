#!/usr/bin/env python3
"""`hgs task` through the real CLI: an isolated catalog and state; sends go to
a scripted agent on a private tmux socket."""
import datetime
import base64
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest
import uuid

REPO = Path(__file__).resolve().parents[1]
HGS = Path(os.environ.get("HGS_TEST_BIN", REPO / "target/debug/hgs")).resolve()
TMUX = shutil.which("tmux")

# Records every byte the pane receives; never echoes, so the composer stays empty.
AGENT = r'''
import os, pathlib, sys, termios, tty
root = pathlib.Path(os.environ['TASK_FIXTURE'])
fd = sys.stdin.fileno()
tty.setraw(fd)
os.write(1, b'\x1b[?2004h\x1b[2J\x1b[H' + '› '.encode())
(root / 'agent-pid').write_text(str(os.getpid()))
# Runs the commands the test hands this agent, the way an agent's shell runs them.
import json, subprocess, threading, time
def commands():
    inbox, outbox = root / 'commands', root / 'results'
    while True:
        for request in sorted(inbox.glob('*.json')):
            job = json.loads(request.read_text())
            request.unlink()
            done = subprocess.run(job['argv'], input=job['stdin'], env=job['env'], text=True, capture_output=True)
            answer = outbox / (request.stem + '.tmp')
            answer.write_text(json.dumps(dict(code=done.returncode, stdout=done.stdout, stderr=done.stderr)))
            answer.rename(outbox / request.name)
        time.sleep(.02)
threading.Thread(target=commands, daemon=True).start()
while True:
    value = os.read(fd, 65536)
    if not value: break
    with (root / 'received').open('ab') as file: file.write(value)
'''


def agent_record(state, env, name, run_id, pid=None, **fields):
    """A live run of `name` whose agent is process `pid` (this test process by
    default): commands its descendants run act as that session's agent."""
    pid = pid or os.getpid()
    start = subprocess.check_output(["ps", "-p", str(pid), "-o", "lstart="],
                                    env=dict(env, LC_ALL="C", TZ="UTC"), text=True).strip()
    record = dict(version=1, name=name, agent=name.split("/")[0], run_id=run_id,
                  conversation_id=str(uuid.uuid4()), pid=pid, process_start=start, activity="idle",
                  phase="idle", active_tools={}, subagents={}, last_event_at=time.time(), created=time.time())
    record.update(fields)
    path = Path(state) / (hashlib.sha256(name.encode()).hexdigest() + ".json")
    path.write_text(json.dumps(record))
    return path


def join(env, project_id, name):
    """Moves this computer's session `name` into the catalog project `project_id`."""
    def swarm(*args, data=None):
        result = subprocess.run([str(HGS), "swarm", *args], input=json.dumps(data) if data is not None else None,
                                env=env, text=True, capture_output=True, timeout=30)
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout)
    node = swarm("export")["node_id"]
    before = swarm("get")
    desired = json.loads(json.dumps(before["organization"]))
    for project in desired["projects"]:
        project["sessions"] = [s for s in project.get("sessions", []) if s.split("\n", 1)[-1] != name]
        if project["id"] == project_id:
            project["sessions"].append(node + "\n" + name)
    swarm("apply", data=dict(base=before["organization"], desired=desired, versions=before["versions"]))


class Fixture(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="hgs-tasks-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.bin = self.root / ".local/bin"
        self.bin.mkdir(parents=True)
        self.state = self.root / "state"
        self.state.mkdir()
        self.runs = {}
        self.socket = self.root / "socket"
        self.env = dict(os.environ, HOME=str(self.root), TASK_FIXTURE=str(self.root),
                        HGS_CONFIG_DIR=str(self.root / "config"), HGS_STATE_DIR=str(self.state),
                        HGS_SELF="test", HGS_TAB="0", PATH=str(self.bin) + ":" + os.environ["PATH"])
        for variable in ("TMUX", "TMUX_PANE", "HGS_RUN_ID", "HGS_SESSION", "HGS_EXECUTABLE", "HGS_SSH", "HGS_PEERS",
                         "CLAUDE_PID", "CLAUDE_CODE_SESSION_ID", "CLAUDE_CONFIG_DIR"):
            self.env.pop(variable, None)
        # hgs puts $HOME/.local/bin first on PATH: the shim there keeps tmux private.
        # Each test stops its own private server, never another one.
        if TMUX:
            self.addCleanup(lambda: subprocess.run([TMUX, "-S", str(self.socket), "kill-server"], capture_output=True))
            self.script("tmux", '#!/bin/sh\nexec ' + TMUX + ' -S "' + str(self.socket) + '" -f /dev/null "$@"\n')
        else:
            self.script("tmux", "#!/bin/sh\nexit 1\n")
        catalog = {"projects": [{"id": "project-1", "name": "orchard", "color": "#123456", "folders": [], "sessions": []},
                                {"id": "project-2", "name": "team/web", "color": "#654321", "folders": [], "sessions": []}]}
        result = subprocess.run([str(HGS), "swarm", "initialize"], input=json.dumps(catalog), env=self.env,
                                text=True, capture_output=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)

    def script(self, name, value):
        path = self.bin / name
        path.write_text(value)
        path.chmod(0o755)

    def agent(self, name):
        """The run of `name` whose agent is this test process."""
        if name not in self.runs:
            self.runs[name] = "run-" + str(uuid.uuid4())
            agent_record(self.state, self.env, name, self.runs[name])
        return self.runs[name]

    def hgs(self, *args, text=None, ok=True, session=None):
        env = dict(self.env)
        if session:
            # An agent: its session and its run, which this test process owns.
            env.update(HGS_SESSION=session, HGS_RUN_ID=self.agent(session))
        result = subprocess.run([str(HGS), "task", *args], input=text, env=env, text=True,
                                capture_output=True, timeout=30)
        if ok:
            self.assertEqual(result.returncode, 0, result.stderr)
            return json.loads(result.stdout)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        return result.stderr

    def add(self, text, *args, session=None):
        # An agent's add answers in one line; --json prints the task.
        extra = ["--json"] if session else []
        return self.hgs("add", "--project", "orchard", *args, *extra, text=text, session=session)["task"]

    def says(self, *args, text=None, session="codex/api"):
        """The line an agent reads after a change."""
        env = dict(self.env, HGS_SESSION=session, HGS_RUN_ID=self.agent(session))
        result = subprocess.run([str(HGS), "task", *args], input=text, env=env, text=True,
                                capture_output=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(result.stdout.splitlines()), 1, result.stdout)
        return result.stdout.strip()

    def ids(self, stage):
        listing = self.hgs("list", "--project", "orchard", "--stage", stage)
        return [task["id"] for task in listing["tasks"]]


class TaskCommands(Fixture):
    def test_task_identity_survives_rename_and_name_reuse(self):
        launch = str(uuid.uuid4())
        old = agent_record(self.state, self.env, "codex/api", "run-1", launch_id=launch)
        task = self.add("Keep the link", "--target", "codex/api")
        self.assertEqual(task["target"]["session_id"], "launch_id:" + launch)
        record = json.loads(old.read_text()); old.unlink()
        record["name"] = "codex/api-renamed"; record["journal_name"] = "codex/api"
        current = self.state / (hashlib.sha256(record["name"].encode()).hexdigest() + ".json")
        current.write_text(json.dumps(record))
        agent_record(self.state, self.env, "codex/api", "replacement", launch_id=str(uuid.uuid4()))
        shown = self.hgs("get", task["ref"])["task"]
        self.assertEqual(shown["target"]["session"], "codex/api-renamed")
        self.assertEqual(shown["target"]["session_id"], task["target"]["session_id"])
        current.unlink()
        missing = self.hgs("get", task["ref"])["task"]
        self.assertFalse(missing["target"]["resolved"])
        self.assertIn("no longer exists", self.hgs("send", task["ref"], ok=False))

    def test_a_large_saved_record_still_routes_by_identity(self):
        # A live run keeps up to 128 shell jobs with their full heredoc commands.
        launch = str(uuid.uuid4())
        jobs = {f"main:call-{i}": dict(command="python3 - <<'PY'\n" + "x" * 12000 + "\nPY", status="completed")
                for i in range(128)}
        record = agent_record(self.state, self.env, "codex/api", "run-1", launch_id=launch, shell_jobs=jobs)
        self.assertGreater(record.stat().st_size, 1024 * 1024)
        task = self.add("Keep the link", "--target", "codex/api")
        self.assertEqual(task["target"]["session_id"], "launch_id:" + launch)
        self.assertNotIn("resolved", self.hgs("get", task["ref"])["task"]["target"])
        planned = self.hgs("target", task["ref"], "subagent", "--parent", "codex/api")["task"]
        self.assertEqual(planned["target"]["parent"]["session_id"], "launch_id:" + launch)

    def test_task_views_carry_the_projected_execution_of_work_in_progress(self):
        queued = self.add("For later", session="codex/api")
        self.assertNotIn("execution", self.hgs("get", queued["ref"])["task"])
        started = self.hgs("start", queued["ref"], "--json", session="codex/api")["task"]
        # History before attempts: the session holding the task began it, no attempt is invented.
        execution = started["execution"]
        self.assertEqual((execution["requirement"], execution["attempts"]), (1, 0))
        current = execution["current"]
        self.assertEqual((current["legacy"], current["generation"], current["origin"], current["phase"]),
                         (True, 0, "start", "running"))
        self.assertEqual(current["executor"]["session_id"], started["session"]["session_id"])
        self.assertIn("host", current["executor"])
        # Every view of the task agrees: the full document, a history page and list rows.
        page = self.hgs("get", queued["ref"], "--history-limit", "1")["task"]
        self.assertEqual(page["execution"], execution)
        self.assertEqual(self.hgs("list", "--project", "orchard", "--stage", "active")["tasks"][0]["execution"], execution)
        self.assertEqual(self.hgs("list", "--project", "orchard")["tasks"][0]["execution"], execution)
        # A result moves it to review, where history alone shows no current execution.
        self.hgs("report", queued["ref"], "--json", text="Done", session="codex/api")
        self.assertNotIn("execution", self.hgs("get", queued["ref"])["task"])

    def test_an_agent_explicitly_starts_work_without_sending_it(self):
        queued = self.add("For later", session="codex/api")
        self.assertEqual(queued["stage"], "todo")
        started = self.hgs("start", queued["ref"], "--json", session="codex/api")["task"]
        self.assertEqual(started["stage"], "active")
        self.assertEqual(started["session"]["session_id"], started["target"]["session_id"])
        self.assertEqual([e["kind"] for e in started["log"]], ["created", "started"])
        immediate = self.add("Doing this direct request", "--start", session="codex/api")
        self.assertEqual(immediate["stage"], "active")
        other = self.add("Owned elsewhere", session="codex/web")
        self.assertIn("another session", self.hgs("start", other["ref"], session="codex/api", ok=False))
        own = self.add("Already done during this turn", session="codex/api")
        report = self.hgs("report", own["ref"], "--json", text="Checked and finished.", session="codex/api")
        self.assertTrue(report["moved_to_review"])
        self.assertEqual(report["task"]["stage"], "review")
        foreign = self.hgs("report", other["ref"], "--json", text="A review note.", session="codex/api")
        self.assertFalse(foreign["moved_to_review"])
        self.assertEqual(foreign["task"]["stage"], "todo")

    def test_media_is_persisted_but_not_expanded_in_listings(self):
        file = dict(name="screen.png", mime="image/png", reference="[Image #1]",
                    data_base64=base64.b64encode(b"immutable image bytes").decode())
        task = self.add(json.dumps(dict(text="Look here [Image #1]", attachments=[file])), "--content-json")
        self.assertEqual(task["attachments"], [file])
        self.assertEqual(task["schema"], 2)
        listed = self.hgs("list", "--project", "orchard")["tasks"][0]["attachments"][0]
        self.assertNotIn("data_base64", listed)
        self.assertEqual(listed["sha256"], hashlib.sha256(b"immutable image bytes").hexdigest())
        self.hgs("edit", task["ref"], text="Updated text [Image #1]")
        self.assertEqual(self.hgs("get", task["ref"])["task"]["attachments"], [file])
        removed = self.hgs("edit", task["ref"], "--content-json", text=json.dumps(dict(text="Updated text", attachments=[])))["task"]
        self.assertFalse(removed.get("attachments"))
        bad = dict(file, data_base64="%%%")
        self.assertIn("base64", self.hgs("edit", task["ref"], "--content-json", text=json.dumps(dict(text="Bad", attachments=[bad])), ok=False))
        self.assertEqual(self.hgs("get", task["ref"])["task"]["text"], "Updated text")

    def test_comment_media_is_immutable_and_not_expanded_in_listing(self):
        task = self.add("Inspect the screenshot")
        file = dict(name="screen.png", mime="image/png", reference="[Image #1]",
                    data_base64=base64.b64encode(b"comment image bytes").decode())
        noted = self.hgs("comment", task["ref"], "--content-json", text=json.dumps(dict(text="See [Image #1]", attachments=[file])))["task"]
        self.assertEqual(noted["schema"], 3)
        self.assertEqual(noted["log"][-1]["attachments"], [file])
        self.hgs("edit", task["ref"], text="Revised description")
        restored = self.hgs("get", task["ref"])["task"]
        self.assertEqual(restored["schema"], 3)
        self.assertEqual(restored["log"][1]["attachments"], [file])
        self.assertNotIn("log", self.hgs("list", "--project", "orchard")["tasks"][0])
        bad = dict(file, data_base64="%%%")
        self.assertIn("base64", self.hgs("comment", task["ref"], "--content-json", text=json.dumps(dict(text="Bad", attachments=[bad])), ok=False))
        self.assertEqual(self.hgs("get", task["ref"])["task"]["log"], restored["log"])

    def test_executor_plans_are_saved_without_starting_and_keep_parent_identity(self):
        self.agent("codex/api")
        task = self.add("Delegate after explicit start")
        for mode in ("new-session", "subagent"):
            planned = self.hgs("target", task["ref"], mode, "--parent", "codex/api")["task"]
            self.assertEqual(planned["stage"], "todo")
            self.assertIsNone(planned["session"])
            self.assertEqual(planned["target"]["kind"], mode.replace("-", "_"))
            self.assertEqual(planned["target"]["parent"]["session_id"], "run_id:" + self.runs["codex/api"])
            self.assertNotIn("sent", [entry["kind"] for entry in planned["log"]])
        self.assertIn("selected executor plan", self.hgs("start", task["ref"], session="codex/api", ok=False))
        self.assertIn("task assignment changed", self.hgs("send", task["ref"], "--session", "codex/api", "--expect-new-session-parent", "run_id:other", ok=False))
        self.assertEqual(self.hgs("get", task["ref"])["task"]["stage"], "todo")

    def test_add_list_and_get(self):
        task = self.add("Add rate limiting")
        self.assertRegex(task["id"], r"^T-[0-9a-z]{6}$")
        self.assertEqual((task["stage"], task["target"], task["author"]), ("todo", {"kind": "any"}, {"kind": "person"}))
        self.assertEqual(task["ref"], "orchard/" + task["id"])
        backlog = self.add("Later", "--backlog")
        self.assertEqual(self.ids("todo"), [task["id"]])
        self.assertEqual(self.ids("backlog"), [backlog["id"]])
        got = self.hgs("get", task["ref"])["task"]
        self.assertEqual(got["text"], "Add rate limiting")
        self.assertEqual([entry["kind"] for entry in got["log"]], ["created"])
        self.assertEqual(self.hgs("get", task["id"], "--project", "project-1")["task"]["uuid"], got["uuid"])
        self.assertIn("no project", self.hgs("list", "--project", "missing", ok=False))
        self.assertIn("no task", self.hgs("get", "orchard/T-zzzzzz", ok=False))

    def test_named_card_creation_recovery_and_independent_rename(self):
        request = str(uuid.uuid4())
        args = ("add", "--project", "project-1", "--title", "Title only", "--request-id", request)
        first = self.hgs(*args, text="")["task"]
        self.assertEqual((first["uuid"], first["title"], first["text"], first["stage"]), (request, "Title only", "", "todo"))
        self.hgs("edit", first["ref"], text="Detailed task content")
        self.hgs("rename", first["ref"], text="Independent title")
        self.hgs("stage", first["ref"], "backlog")
        before = self.hgs("get", first["ref"])["task"]
        recovered = self.hgs(*args, text="")["task"]
        self.assertEqual(recovered, before)
        self.assertEqual(len(self.hgs("list", "--project", "project-1")["tasks"]), 1)
        self.assertIn("another input", self.hgs("add", "--project", "project-1", "--title", "Changed intent", "--request-id", request, text="", ok=False))
        self.hgs("edit", first["ref"], text="")
        final = self.hgs("get", first["ref"])["task"]
        self.assertEqual((final["title"], final["text"]), ("Independent title", ""))

    def test_order_and_positions(self):
        first, second, third = (self.add(t)["id"] for t in ("one", "two", "three"))
        self.assertEqual(self.ids("todo"), [first, second, third])
        self.hgs("move", "orchard/" + third, "--top")
        self.assertEqual(self.ids("todo"), [third, first, second])
        self.hgs("move", "orchard/" + third, "--after", first)
        self.assertEqual(self.ids("todo"), [first, third, second])
        fourth = self.add("four", "--position", "1")["id"]
        self.assertEqual(self.ids("todo"), [fourth, first, third, second])

    def test_stages_edits_targets_and_comments(self):
        ref = self.add("Draft")["ref"]
        self.hgs("edit", ref, text="Final text")
        self.hgs("stage", ref, "backlog")
        self.hgs("stage", ref, "todo")
        self.assertIn("cannot go from To do to Done", self.hgs("stage", ref, "done", ok=False))
        self.assertIn(ref.split("/")[1] + " is already in To do", self.hgs("stage", ref, "todo", ok=False))
        self.hgs("target", ref, "codex/api")
        task = self.hgs("comment", ref, text="Use Redis")["task"]
        self.assertEqual(task["text"], "Final text")
        self.assertEqual(task["target"]["session"], "codex/api")
        self.assertEqual([e["kind"] for e in task["log"]], ["created", "edited", "stage", "stage", "target", "comment"])
        self.assertIn("longer than 64 KiB", self.hgs("comment", ref, text="x" * (64 * 1024 + 1), ok=False))

    def test_an_agent_files_tasks_for_its_own_session(self):
        task = self.add("Document the setup", session="codex/api")
        self.assertEqual(task["author"]["kind"], "agent")
        self.assertEqual(task["target"]["session"], "codex/api")
        # The author and the history's actors name their computer like sessions do.
        self.assertEqual(task["author"]["host"], "")
        self.assertEqual(task["log"][0]["actor"]["host"], "")
        self.assertIn("pinned to its own session", self.hgs("add", "--project", "orchard", "--target", "any", text="x", session="codex/api", ok=False))

    def test_settings(self):
        settings = self.hgs("settings", "--project", "orchard")["settings"]
        self.assertTrue(settings["task_template"].startswith("[Zerus task {ref}]"))
        settings.update(agents_other=False, agent_limit=4)
        self.assertEqual(self.hgs("settings", "--project", "orchard", "--set", text=json.dumps(settings))["settings"]["agent_limit"], 4)
        settings["task_template"] = "{prompt}"
        self.assertIn("must start with", self.hgs("settings", "--project", "orchard", "--set", text=json.dumps(settings), ok=False))

    def test_settings_change_only_the_fields_given(self):
        self.hgs("settings", "--project", "orchard", "--set", text=json.dumps({"agent_limit": 2}))
        self.hgs("settings", "--project", "orchard", "--set", text=json.dumps({"agents_other": False}))
        settings = self.hgs("settings", "--project", "orchard")["settings"]
        self.assertEqual((settings["agent_limit"], settings["agents_other"]), (2, False))
        self.assertTrue(settings["task_template"].startswith("[Zerus task {ref}]"))
        typo = self.hgs("settings", "--project", "orchard", "--set", text=json.dumps({"agent_limt": 3}), ok=False)
        self.assertIn("agent_limt", typo)

    def test_concurrent_adds_all_land(self):
        self.hgs("list", "--project", "orchard")  # creates the project's lock file
        lock = self.state / "replica" / hashlib.sha256(b"project-1").hexdigest() / ".lock"
        with open(lock, "a") as held:
            # The test holds the project lock while all six start, so they all
            # wait for it at once and race when it is released.
            fcntl.flock(held, fcntl.LOCK_EX)
            processes = [subprocess.Popen([str(HGS), "task", "add", "--project", "orchard"], stdin=subprocess.PIPE,
                                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=self.env, text=True)
                         for _ in range(6)]
            for index, process in enumerate(processes):
                process.stdin.write("task %d" % index)
                process.stdin.close()
            time.sleep(.3)
            self.assertEqual([process.poll() for process in processes], [None] * 6, "an add did not wait for the lock")
            fcntl.flock(held, fcntl.LOCK_UN)
        for process in processes:
            self.assertEqual(process.wait(timeout=30), 0, process.stderr.read())
        tasks = self.hgs("list", "--project", "orchard")["tasks"]
        self.assertEqual(sorted(t["text"] for t in tasks), ["task %d" % i for i in range(6)])
        self.assertEqual(len({t["id"] for t in tasks}), 6)
        self.assertEqual(len({t["rank"] for t in tasks}), 6)

    def test_texts_that_cannot_be_sent_are_refused(self):
        self.assertIn("control characters", self.hgs("add", "--project", "orchard", text="\x1b[31mred", ok=False))
        self.assertIn("control characters", self.hgs("add", "--project", "orchard", text="line one\r\n", ok=False))
        # Fits 64 KiB alone, but not inside the task prompt.
        self.assertIn("rendered prompt", self.hgs("add", "--project", "orchard", text="x" * 65450, ok=False))
        ref = self.add("Short")["ref"]
        self.assertIn("rendered prompt", self.hgs("edit", ref, text="x" * 65450, ok=False))
        self.assertEqual(self.hgs("get", ref)["task"]["text"], "Short")
        self.assertEqual(self.ids("todo"), [ref.split("/")[-1]])

    def test_a_task_that_changes_list_goes_to_its_top(self):
        first, second = self.add("one")["id"], self.add("two")["id"]
        later, other = self.add("later", "--backlog")["id"], self.add("other", "--backlog")["id"]
        self.hgs("stage", "orchard/" + later, "todo")
        self.assertEqual((self.ids("todo"), self.ids("backlog")), ([later, first, second], [other]))
        self.hgs("stage", "orchard/" + second, "backlog")
        self.assertEqual((self.ids("todo"), self.ids("backlog")), ([later, first], [second, other]))

    def test_tasks_name_the_computer_of_their_session(self):
        here = self.add("Local work", "--target", "codex/api")
        self.assertEqual(here["target"]["host"], "")
        refused = self.hgs("add", "--project", "orchard", "--target", "codex/api", "--host", "orchard-mac",
                           text="Remote work", ok=False)
        self.assertIn("orchard-mac is not a verified computer", refused)
        self.assertIn("omit --host for this computer", refused)
        # This computer's own name (HGS_SELF) names this computer.
        named = self.hgs("add", "--project", "orchard", "--target", "codex/api", "--host", "test",
                         text="Local work by name")["task"]
        self.assertEqual((named["target"]["machine"], named["target"]["host"]), (here["target"]["machine"], ""))
        # --host names the computer of a --target session, so it needs one.
        for args in (("--host", "orchard-mac"), ("--target", "any", "--host", "orchard-mac")):
            self.assertIn("--host needs a --target session", self.hgs("add", "--project", "orchard", *args, text="x", ok=False))
        self.assertIn("--host needs a --target session", self.hgs("target", here["ref"], "any", "--host", "orchard-mac", ok=False))
        # This computer verified orchard-mac itself: tasks can be pinned to its sessions.
        catalog = self.root / "config" / "swarm" / "catalog.json"
        data = json.loads(catalog.read_text())
        mac = str(uuid.uuid4())
        data.setdefault("bindings", {})["orchard-mac"] = {"id": mac, "verified": True}
        catalog.write_text(json.dumps(data))
        there = self.hgs("add", "--project", "orchard", "--target", "codex/web", "--host", "orchard-mac",
                         text="Remote work")["task"]
        self.assertEqual((there["target"]["machine"], there["target"]["host"]), (mac, "orchard-mac"))
        moved = self.hgs("target", here["ref"], "codex/web", "--host", "orchard-mac")["task"]
        self.assertEqual(moved["target"]["host"], "orchard-mac")
        listed = {t["id"]: t for t in self.hgs("list", "--project", "orchard")["tasks"]}
        self.assertEqual(listed[there["id"]]["target"]["host"], "orchard-mac")
        # A send for it runs on that computer, which this fixture cannot reach.
        self.assertIn("orchard-mac cannot be reached", self.hgs("send", there["ref"], ok=False))

    def test_only_a_forwarded_command_names_its_agent(self):
        task = self.add("Add rate limiting")
        node = "00000000-0000-4000-8000-000000000001"
        # An agent acts as its own session; it cannot speak for another one.
        self.assertIn("--agent is for commands another computer forwards",
                      self.hgs("cancel", task["ref"], "--agent", node + "/codex/web", session="codex/api", ok=False))
        # It names a computer by its identity, and only send and cancel take it.
        self.assertIn("--agent is NODE/SESSION", self.hgs("cancel", task["ref"], "--agent", "laptop/codex/web", ok=False))
        self.assertIn("only send and cancel take --agent",
                      self.hgs("comment", task["ref"], "--agent", node + "/codex/web", text="x", ok=False))
        self.assertIn("only send and cancel take --agent",
                      self.hgs("add", "--project", "orchard", "--agent", node + "/codex/web", text="x", ok=False))

    def test_an_empty_host_is_this_computer(self):
        task = self.add("Add rate limiting")
        self.assertIn("choose the session with --session", self.hgs("send", task["ref"], "--host", "", ok=False))

    def test_a_project_name_with_a_slash(self):
        task = self.hgs("add", "--project", "team/web", text="Ship it")["task"]
        self.assertEqual(task["ref"], "team/web/" + task["id"])
        self.assertEqual(self.hgs("get", task["ref"])["task"]["uuid"], task["uuid"])

    def test_a_damaged_task_is_reported_not_fatal(self):
        self.add("Fine")
        project = next(p for p in (self.state / "replica").iterdir() if p.is_dir())
        (project / "tasks" / (str(uuid.uuid4()) + ".loro")).write_bytes(b"broken")
        listing = self.hgs("list", "--project", "orchard")
        self.assertEqual(len(listing["tasks"]), 1)
        self.assertEqual(len(listing["damaged"]), 1)
        self.assertNotIn("kept", listing["damaged"][0])
        self.assertEqual(listing["damaged"][0]["recovery"], "depends_on_incoming_copy")

    def test_a_damaged_task_cut_over_here_is_listed_as_kept_for_review(self):
        # zerus-cg5h.4.2: a copy this computer cut over to execution attempts
        # (its mark beside the files) is told apart from damage whose
        # recovery depends on the copy that arrives; torn local updates are
        # kept too. What is shown is only what is known here.
        project = None
        uuids = {}
        for name in ("Kept", "Ordinary", "Torn"):
            task = self.add(name)
            project = project or next(p for p in (self.state / "replica").iterdir() if p.is_dir())
            (project / "tasks" / (task["uuid"] + ".loro")).write_bytes(b"broken")
            uuids[name] = task["uuid"]
        (project / "tasks" / (uuids["Kept"] + ".managed")).write_text('{"snapshot":{}}')
        (project / "tasks" / (uuids["Torn"] + ".updates")).write_bytes(b"\x07")
        expected = {
            uuids["Kept"]: {"kept": True, "kept_reason": "marked"},
            uuids["Ordinary"]: {"recovery": "depends_on_incoming_copy"},
            uuids["Torn"]: {"kept": True, "kept_reason": "unreadable_local_data"},
        }
        for listing in (self.hgs("list", "--project", "orchard"),
                        self.hgs("list", "--project", "orchard", "--stage", "todo", "--limit", "5")):
            rows = {d["uuid"]: {k: v for k, v in d.items() if k not in ("uuid", "error")}
                    for d in listing["damaged"]}
            self.assertEqual(rows, expected)
        # Removing the mark is read at once.
        (project / "tasks" / (uuids["Kept"] + ".managed")).unlink()
        page = self.hgs("list", "--project", "orchard", "--stage", "todo", "--limit", "5")
        row = next(d for d in page["damaged"] if d["uuid"] == uuids["Kept"])
        self.assertEqual(row.get("recovery"), "depends_on_incoming_copy")
        self.assertNotIn("kept", row)


    def test_the_cutover_status_and_a_person_s_hold(self):
        # zerus-cg5h.4 S2b.2 C2: information only; the hold is a person's.
        task = self.add("Add rate limiting")
        status = self.hgs("cutover", "--project", "orchard")
        self.assertEqual((status["phase"], status["cut_over_tasks"], status["members"], status["closed"]),
                         ("not_cut_over", 0, [], None))
        self.assertEqual(status["hold"], {"on": False, "by": None, "at": None})
        self.assertIn("only a person", self.hgs("cutover", "--project", "orchard", "--hold", "on",
                                               ok=False, session="codex/api"))
        held = self.hgs("cutover", "--project", "orchard", "--hold", "on")["hold"]
        self.assertEqual((held["on"], held["by"]["kind"]), (True, "person"))
        self.assertIsInstance(held["at"], int)
        # Clearing it needs an explicit confirmation.
        self.assertIn("--confirm", self.hgs("cutover", "--project", "orchard", "--hold", "off", ok=False))
        self.assertTrue(self.hgs("cutover", "--project", "orchard")["hold"]["on"])
        self.assertFalse(self.hgs("cutover", "--project", "orchard", "--hold", "off", "--confirm")["hold"]["on"])
        self.assertIn("on or off", self.hgs("cutover", "--project", "orchard", "--hold", "maybe", ok=False))
        # A task cut over here, with no other member, is active.
        project = next(p for p in (self.state / "replica").iterdir() if p.is_dir())
        (project / "tasks" / (task["uuid"] + ".managed")).write_text('{"saved":{}}')
        status = self.hgs("cutover", "--project", "orchard")
        self.assertEqual((status["phase"], status["cut_over_tasks"], status["closed"]), ("active", 1, None))
        self.assertIsInstance(status["last_cutover_write"], int)


class AgentIdentity(Fixture):
    def test_an_unproven_session_is_refused(self):
        env = dict(self.env, HGS_SESSION="codex/api", HGS_RUN_ID="run-that-ended")
        result = subprocess.run([str(HGS), "task", "add", "--project", "orchard"], input="Fix it", env=env,
                                text=True, capture_output=True, timeout=30)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("names the agent session codex/api", result.stderr)
        # A person's shell in the session's tmux inherits its variables: say the way out.
        self.assertIn("or unset HGS_SESSION and HGS_RUN_ID to act as yourself", result.stderr)
        self.assertEqual(self.ids("todo"), [])
        # A record without the owned process proves nothing either.
        agent_record(self.state, self.env, "codex/api", "run-stale", process_start="not this process")
        env["HGS_RUN_ID"] = "run-stale"
        result = subprocess.run([str(HGS), "task", "list", "--project", "orchard"], env=env,
                                text=True, capture_output=True, timeout=30)
        self.assertIn("names the agent session codex/api", result.stderr)

    def test_a_background_job_acts_for_the_session_that_moved_it(self):
        # Claude runs the job in a process that inherited another session's run
        # token; Claude's registry names the job, and the session's window holds it.
        window, token = (subprocess.Popen(["sleep", "60"]) for _ in range(2))
        for process in (window, token):
            self.addCleanup(process.wait)
            self.addCleanup(process.kill)
        conversation, job = str(uuid.uuid4()), str(uuid.uuid4())
        agent_record(self.state, self.env, "claude/api", "run-window", pid=window.pid, conversation_id=conversation)
        agent_record(self.state, self.env, "codex/web", "run-token", pid=token.pid)
        registry = self.root / ".claude/sessions"
        registry.mkdir(parents=True)
        held = registry / f"{window.pid}.json"
        held.write_text(json.dumps(dict(pid=window.pid, kind="interactive", parkedJobId=job)))
        (registry / f"{os.getpid()}.json").write_text(json.dumps(dict(pid=os.getpid(), kind="bg", jobId=job, sessionId=conversation)))
        env = dict(self.env, HGS_SESSION="codex/web", HGS_RUN_ID="run-token", CLAUDE_PID=str(os.getpid()),
                   CLAUDE_CODE_SESSION_ID=conversation)
        add = lambda env: subprocess.run([str(HGS), "task", "add", "--project", "orchard", "--json"], input="Document the setup",
                                         env=env, text=True, capture_output=True, timeout=30)
        result = add(env)
        self.assertEqual(result.returncode, 0, result.stderr)
        task = json.loads(result.stdout)["task"]
        self.assertEqual((task["author"]["session"], task["target"]["session"]), ("claude/api", "claude/api"))
        # Another conversation, or a job the window no longer holds, proves nothing.
        self.assertIn("names the agent session codex/web", add(dict(env, CLAUDE_CODE_SESSION_ID=str(uuid.uuid4()))).stderr)
        held.write_text(json.dumps(dict(pid=window.pid, kind="interactive")))
        self.assertIn("names the agent session codex/web", add(env).stderr)
        self.assertEqual(len(self.ids("todo")), 1)

    def test_a_renamed_session_acts_under_its_current_name(self):
        run = self.agent("codex/api")
        old = self.state / (hashlib.sha256(b"codex/api").hexdigest() + ".json")
        old.unlink()
        agent_record(self.state, self.env, "codex/api-renamed", run)
        task = self.add("Document the setup", session="codex/api")
        self.assertEqual((task["target"]["session"], task["author"]["session"]), ("codex/api-renamed", "codex/api-renamed"))

    def test_an_agent_works_on_its_sessions_project(self):
        self.agent("codex/api")
        self.assertIn("codex/api belongs to no project", self.hgs("list", session="codex/api", ok=False))
        join(self.env, "project-2", "codex/api")
        filed = self.hgs("add", "--json", text="Theme the header", session="codex/api")["task"]
        self.assertTrue(filed["ref"].startswith("team/web/"), filed["ref"])
        self.assertEqual(self.hgs("list", session="codex/api")["project"]["name"], "team/web")
        self.assertEqual(self.hgs("get", filed["id"], session="codex/api")["task"]["uuid"], filed["uuid"])
        # A reference or --project still names another project.
        other = self.add("Add rate limiting")
        self.assertEqual(self.hgs("get", other["ref"], session="codex/api")["task"]["uuid"], other["uuid"])
        self.assertEqual(self.hgs("list", "--project", "orchard", session="codex/api")["project"]["name"], "orchard")
        # The person still names the project.
        self.assertIn("usage", self.hgs("list", ok=False))


@unittest.skipUnless(TMUX, "tmux is required for delivery")
class DeliveryFixture(Fixture):
    """A scripted agent of `self.name` on a private tmux socket, which records
    what is pasted and runs the commands handed to it."""
    def setUp(self):
        super().setUp()
        (self.root / "agent.py").write_text(AGENT)
        (self.root / "commands").mkdir()
        (self.root / "results").mkdir()
        self.name = "codex/task-test"
        self.run_id = str(uuid.uuid4())
        self.tmux("new-session", "-d", "-s", self.name, "-x", "100", "-y", "30", "python3", str(self.root / "agent.py"))
        self.addCleanup(self.stop)
        for _ in range(200):
            pid_file = self.root / "agent-pid"
            if pid_file.exists() and pid_file.read_text().strip():
                break
            time.sleep(.02)
        pid = int((self.root / "agent-pid").read_text())
        start = subprocess.check_output(["ps", "-p", str(pid), "-o", "lstart="],
                                        env=dict(self.env, LC_ALL="C", TZ="UTC"), text=True).strip()
        pane = self.tmux("display-message", "-p", "-t", "=" + self.name + ":", "#{pane_id}").strip()
        self.tmux("set-option", "-t", "=" + self.name + ":", "@hgs_run", self.run_id)
        self.record_path = agent_record(self.state, self.env, self.name, self.run_id, pid=pid, agent="codex", pane=pane)
        self.runs[self.name] = self.run_id

    def stop(self):
        # kill-server acknowledges before its panes exit; wait for them before the
        # fixture home is removed, so no pane writes into a deleted directory.
        panes = subprocess.run([str(self.bin / "tmux"), "list-panes", "-a", "-F", "#{pane_pid}"], env=self.env,
                               capture_output=True, text=True, timeout=5).stdout.split()
        pids = {pid for pid in panes if pid.isdigit()}
        subprocess.run([TMUX, "-S", str(self.socket), "kill-server"], capture_output=True)
        deadline = time.monotonic() + 2
        while pids and time.monotonic() < deadline:
            status = subprocess.run(["ps", "-p", ",".join(sorted(pids)), "-o", "pid=,stat="],
                                    capture_output=True, text=True, timeout=5)
            pids = {parts[0] for line in status.stdout.splitlines()
                    if len(parts := line.split()) >= 2 and not parts[1].startswith(("Z", "X"))}
            if pids:
                time.sleep(.02)
        self.assertFalse(pids, "owned fixture panes survived tmux shutdown")

    def as_agent(self, *args, text=None, ok=True):
        """`hgs task ARGS` run by the scripted agent of `self.name`, as its shell would."""
        job = str(uuid.uuid4())
        env = dict(self.env, HGS_SESSION=self.name, HGS_RUN_ID=self.run_id)
        request = self.root / "commands" / (job + ".tmp")
        request.write_text(json.dumps(dict(argv=[str(HGS), "task", *args], stdin=text or "", env=env)))
        request.rename(self.root / "commands" / (job + ".json"))
        result = self.root / "results" / (job + ".json")
        for _ in range(1500):
            if result.exists():
                done = json.loads(result.read_text())
                self.assertEqual(done["code"] == 0, ok, done["stderr"] or done["stdout"])
                return done["stdout"] if ok else done["stderr"]
            time.sleep(.02)
        self.fail("the scripted agent never ran " + " ".join(args))

    def tmux(self, *args):
        result = subprocess.run([str(self.bin / "tmux"), *args], env=self.env, capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def received(self):
        path = self.root / "received"
        return path.read_bytes().decode(errors="replace") if path.exists() else ""

    def wait_received(self, text):
        for _ in range(250):
            if text in self.received():
                return
            time.sleep(.02)
        self.fail("the agent never received " + repr(text) + "; got " + repr(self.received()))

    def confirm(self):
        # The agent's next hook event confirms the pasted input.
        record = json.loads(self.record_path.read_text())
        record["last_event_at"] = record.get("input_pending_at", time.time()) + 1
        # An input-acceptance event, as UserPromptSubmit records it (zerus-iitw8).
        record["input_seen_at"] = record["last_event_at"]
        self.record_path.write_text(json.dumps(record))

    def settings(self, **changes):
        settings = self.hgs("settings", "--project", "orchard")["settings"]
        settings.update(changes)
        self.hgs("settings", "--project", "orchard", "--set", text=json.dumps(settings))

class TaskDelivery(DeliveryFixture):
    def message(self, task, text, mode="send", identity=None, dispatch=None, extra=(), **kw):
        return self.hgs("message", task["ref"], "--content-json", "--mode", mode, *extra,
                        "--request-id", identity or str(uuid.uuid4()), "--expect-dispatch", dispatch or task.get("dispatch_id") or "",
                        text=json.dumps(dict(text=text, attachments=[])), **kw)

    def test_queued_context_is_included_once_in_first_dispatch(self):
        task = self.add("Canonical description", "--target", self.name)
        queued = self.message(task, "Include this clarification", mode="queue")
        self.assertEqual(queued["message"]["delivery"]["state"], "queued")
        self.assertEqual(self.received(), "")
        note = self.message(task, "Private task note", mode="note")
        sent = self.hgs("send", task["ref"])["task"]
        self.wait_received("Include this clarification")
        self.assertNotIn("Private task note", self.received())
        self.assertEqual(sent["text"], "Canonical description")
        messages = [e for e in sent["log"] if e["kind"] == "message"]
        self.assertEqual([e["delivery"]["state"] for e in messages], ["submitted", "note"])
        self.confirm()
        self.as_agent("report-attempt", task["ref"], "--attempt", sent["execution"]["current"]["id"], text="Ready for review")
        self.hgs("stage", task["ref"], "todo")
        self.hgs("send", task["ref"])
        self.assertEqual(self.received().count("Include this clarification"), 1)

    def test_feedback_has_a_durable_receipt_and_explicit_review_reopening(self):
        task = self.add("Work on this", "--target", self.name)
        task = self.hgs("send", task["ref"])["task"]; self.confirm()
        identity = str(uuid.uuid4())
        sent = self.message(task, "Active feedback", identity=identity)
        self.assertEqual(sent["message"]["delivery"]["state"], "submitted")
        self.wait_received("Active feedback"); self.confirm()
        again = self.message(task, "Active feedback", identity=identity)
        self.assertEqual(again["message"]["request_id"], identity)
        self.assertEqual(self.received().count("Active feedback"), 1)
        self.assertIn("different content", self.message(task, "Altered feedback", identity=identity, ok=False))
        self.as_agent("report-attempt", task["ref"], "--attempt", task["execution"]["current"]["id"], text="Result for review")
        reviewed = self.hgs("get", task["ref"])["task"]
        self.assertEqual(self.message(reviewed, "Just a note", mode="note")["task"]["stage"], "review")
        changed = self.message(reviewed, "Please revise", mode="reopen")
        self.assertEqual(changed["task"]["stage"], "active")
        self.assertEqual(changed["task"]["session"]["session_id"], task["session"]["session_id"])
        self.wait_received("Please revise")

    def reviewed(self, text):
        task = self.add(text, "--target", self.name)
        sent = self.hgs("send", task["ref"])["task"]; self.confirm()
        origin = sent.get("execution", {}).get("current", {})
        if origin.get("id") and not origin.get("legacy"):
            self.as_agent("report-attempt", task["ref"], "--attempt", origin["id"], text="Result for review")
        else:
            self.as_agent("report", task["ref"], text="Result for review")
        return self.hgs("get", task["ref"])["task"]

    def notes(self, task, identity):
        log = self.hgs("get", task["ref"])["task"]["log"]
        return [e for e in log if e["kind"] == "message" and e.get("request_id") == identity]

    def test_a_managed_reopen_is_an_attempt_with_its_note(self):
        # desktop-u's (b): the person's text is a note bound to the reopen's
        # ID, saved with the reopen attempt in one admission; the receipt
        # settles it (Active, resumed). Repeating that ID is read only: one
        # note, the original receipt, never another paste; other text under
        # it refuses.
        task = self.reviewed("A managed reopen")
        self.assertEqual(self.execution(task)["attempts"], 1)
        # The send's own attempt ID is not a reopen's: another intent, even
        # with a note of the same text under that ID.
        sent_id = self.execution(task)["current"]["id"]
        self.assertIn("another intent", self.message(task, "Please revise it", mode="reopen", identity=sent_id, ok=False))
        other = self.add("Its note and plan share an ID", "--target", self.name)
        shared = str(uuid.uuid4())
        self.message(other, "Same words", mode="note", identity=shared)
        self.confirm()
        self.assertEqual(self.hgs("start-deliver", other["ref"], text=json.dumps(self.start_plan(other, id=shared)))["state"], "submitted")
        self.confirm()
        self.as_agent("report-attempt", other["ref"], "--attempt", shared, text="Result for review")
        other = self.hgs("get", other["ref"])["task"]
        self.assertIn("another intent", self.message(other, "Same words", mode="reopen", identity=shared, ok=False))
        identity = str(uuid.uuid4())
        changed = self.message(task, "Please revise it", mode="reopen", identity=identity)
        self.assertEqual((changed["attempt"]["id"], changed["attempt"]["state"]), (identity, "submitted"))
        self.assertEqual(changed["task"]["stage"], "active")
        self.assertEqual(changed["message"]["delivery"]["state"], "note")
        self.wait_received("Please revise it"); self.confirm()
        execution = self.execution(task)
        self.assertEqual((execution["attempts"], execution["current"]["origin"]), (2, "reopen"))
        log = self.hgs("get", task["ref"])["task"]["log"]
        self.assertEqual(log[-1]["kind"], "resumed")
        again = self.message(task, "Please revise it", mode="reopen", identity=identity)
        self.assertEqual(again["attempt"]["state"], "submitted")
        self.assertEqual(len(self.notes(task, identity)), 1)
        self.assertEqual(self.received().count("Please revise it"), 1)
        self.assertIn("another intent", self.message(task, "Other text", mode="reopen", identity=identity, ok=False))

    def test_a_managed_reopen_refused_before_input_keeps_its_note_in_review(self):
        # desktop-u's (b) with (a): refused before any input, the reopen is
        # not admitted; its note stays, the task stays in review, and no
        # resumed entry is written. A new reopen sends it.
        task = self.reviewed("Reopened too early")
        other = self.add("Holds the input", "--target", self.name)
        self.hgs("send", other["ref"])
        # The agent has not confirmed that input yet.
        record = json.loads(self.record_path.read_text())
        record["last_event_at"] = record["input_pending_at"] - 1
        record["input_seen_at"] = record["input_pending_at"] - 1
        self.record_path.write_text(json.dumps(record))
        identity = str(uuid.uuid4())
        refused = self.message(task, "Revise after the other", mode="reopen", identity=identity)
        self.assertEqual(refused["attempt"]["state"], "refused")
        self.assertIn("previous input has not been confirmed", refused["attempt"]["error"])
        current = self.hgs("get", task["ref"])["task"]
        self.assertEqual(current["stage"], "review")
        self.assertNotIn("resumed", [e["kind"] for e in current["log"]])
        self.assertEqual(len(self.notes(task, identity)), 1)
        execution = self.execution(task)
        self.assertEqual(execution["attempts"], 2)
        self.assertNotEqual(execution["current"]["id"], identity, "never started, never current")
        self.assertEqual(self.received().count("Revise after the other"), 0)
        self.confirm()
        self.assertEqual(self.message(task, "Revise after the other", mode="reopen", identity=identity)["attempt"]["state"],
                         "refused", "the same ID is never sent again")
        self.assertEqual(self.received().count("Revise after the other"), 0)
        sent = self.message(task, "Revise it now", mode="reopen")
        self.assertEqual((sent["attempt"]["state"], sent["task"]["stage"]), ("submitted", "active"))

    def test_an_unknown_managed_reopen_holds_the_task_and_never_resends(self):
        # An unknown reopen keeps its note but sets neither Active nor
        # resumed, and it holds the task: a later reopen cannot even be
        # prepared, so it is only kept as a comment that was not delivered,
        # with no attempt and no input.
        task = self.reviewed("An unknown reopen")
        identity = str(uuid.uuid4())
        receipts = self.state / "input_receipts"
        receipts.chmod(0o500)
        self.addCleanup(receipts.chmod, 0o700)
        unknown = self.message(task, "Maybe delivered", mode="reopen", identity=identity)
        receipts.chmod(0o700)
        self.assertEqual(unknown["attempt"]["state"], "uncertain")
        current = self.hgs("get", task["ref"])["task"]
        self.assertEqual(current["stage"], "review")
        self.assertNotIn("resumed", [e["kind"] for e in current["log"]])
        self.assertEqual(len(self.notes(task, identity)), 1)
        for extra in ((), ("--recover-only",)):
            again = self.message(task, "Maybe delivered", mode="reopen", identity=identity, extra=extra)
            self.assertEqual(again["attempt"]["state"], "uncertain")
        later = str(uuid.uuid4())
        current = self.hgs("get", task["ref"])["task"]
        refused = self.message(current, "A later revision", mode="reopen", identity=later)
        self.assertEqual(refused["message"]["delivery"]["state"], "failed")
        self.assertNotIn("attempt", refused)
        self.assertEqual((self.execution(task)["attempts"], self.hgs("get", task["ref"])["task"]["stage"]), (2, "review"))
        self.assertEqual(self.received().count("Maybe delivered") + self.received().count("A later revision"), 0)

    def test_unknown_feedback_is_recovered_without_another_paste(self):
        task = self.add("Task for crash recovery", "--target", self.name)
        task = self.hgs("send", task["ref"])["task"]; self.confirm()
        identity = str(uuid.uuid4())
        # The isolated shim lets the original paste through, then kills just
        # its hgs parent before Enter/receipt completion. Never touches live tmux.
        shim = "#!/usr/bin/env python3\nimport os,signal,subprocess,sys\na=sys.argv[1:]\n"
        shim += "r=subprocess.run([" + repr(TMUX) + ",'-S'," + repr(str(self.socket)) + ",'-f','/dev/null',*a])\n"
        shim += "if a and a[0]=='paste-buffer': os.kill(os.getppid(),signal.SIGKILL)\nsys.exit(r.returncode)\n"
        self.script("tmux", shim)
        self.message(task, "Never paste twice", identity=identity, ok=False)
        self.wait_received("Never paste twice")
        self.script("tmux", '#!/bin/sh\nexec ' + TMUX + ' -S "' + str(self.socket) + '" -f /dev/null "$@"\n')
        recovered = self.message(task, "Never paste twice", identity=identity)
        self.assertEqual(recovered["message"]["delivery"]["state"], "uncertain")
        self.assertEqual(self.received().count("Never paste twice"), 1)
        self.message(task, "Never paste twice", identity=identity)
        self.assertEqual(self.received().count("Never paste twice"), 1)

    def test_queued_images_keep_their_message_scoped_references(self):
        image = dict(name="description.png",mime="image/png",reference="[Image #1]",data_base64=base64.b64encode(b"description-image").decode())
        task = self.add(json.dumps(dict(text="Description [Image #1]",attachments=[image])), "--content-json", "--target", self.name)
        queued_image = dict(image,name="clarification.png",data_base64=base64.b64encode(b"clarification-image").decode())
        self.hgs("message",task["ref"],"--content-json","--mode","queue","--request-id",str(uuid.uuid4()),
                 text=json.dumps(dict(text="Clarification [Image #1]",attachments=[queued_image])))
        out = self.hgs("send",task["ref"])
        self.wait_received("Clarification [Image #2]")
        self.assertIn("Description [Image #1]",self.received())
        self.assertEqual([a["reference"] for a in out["receipt"]["attachments"]],["[Image #1]","[Image #2]"])
        self.assertEqual([Path(a["path"]).read_bytes() for a in out["receipt"]["attachments"]],[b"description-image",b"clarification-image"])
        self.assertEqual(out["task"]["text"],"Description [Image #1]")

    def test_feedback_follows_renames_but_refuses_a_changed_conversation(self):
        task = self.add("Owned task", "--target", self.name)
        task = self.hgs("send",task["ref"])["task"]; self.confirm()
        old_name=self.name; self.name="codex/renamed-worker"
        self.tmux("rename-session","-t","="+old_name+":",self.name)
        record=json.loads(self.record_path.read_text()); self.record_path.unlink(); record["name"]=self.name; record["journal_name"]=old_name
        self.record_path=self.state/(hashlib.sha256(self.name.encode()).hexdigest()+".json"); self.record_path.write_text(json.dumps(record))
        sent=self.message(task,"Feedback after rename")
        self.assertEqual(sent["message"]["delivery"]["state"],"submitted"); self.wait_received("Feedback after rename"); self.confirm()
        record=json.loads(self.record_path.read_text()); record["conversation_id"]=str(uuid.uuid4()); self.record_path.write_text(json.dumps(record))
        refused=self.message(task,"Wrong conversation")
        self.assertEqual(refused["message"]["delivery"]["state"],"failed")
        self.assertIn("conversation changed",refused["message"]["delivery"]["error"])
        self.assertEqual(refused["message"]["text"],"Wrong conversation")
        self.assertNotIn("Wrong conversation",self.received())

    def test_a_recovery_check_without_a_reservation_never_sends(self):
        task=self.add("Task", "--target",self.name); task=self.hgs("send",task["ref"])["task"]; self.confirm()
        identity=str(uuid.uuid4())
        recovered=self.hgs("message",task["ref"],"--request-id",identity,"--mode","send","--expect-dispatch",task["dispatch_id"],
                "--recover-only",text="Do not send this check")
        self.assertEqual(recovered["message"]["delivery"]["state"],"uncertain")
        self.assertEqual(recovered["message"]["text"],"Do not send this check")
        self.assertEqual(self.message(task,"Do not send this check",identity=identity)["message"]["delivery"]["state"],"uncertain")
        self.assertNotIn("Do not send this check",self.received())

    def test_stale_execution_comment_is_saved_without_sending_or_reopening(self):
        task=self.add("Owned task", "--target",self.name)
        task=self.hgs("send",task["ref"])["task"]; self.confirm()
        self.hgs("report",task["ref"],text="Ready for review")
        reviewed=self.hgs("get",task["ref"])["task"]
        identity=str(uuid.uuid4())
        saved=self.message(reviewed,"Ordinary review comment",mode="reopen",dispatch=str(uuid.uuid4()),identity=identity)
        self.assertEqual(saved["message"]["delivery"]["state"],"failed")
        self.assertEqual(saved["task"]["stage"],"review")
        self.assertEqual(saved["message"]["text"],"Ordinary review comment")
        again=self.message(reviewed,"Ordinary review comment",mode="reopen",dispatch=saved["message"]["delivery"]["dispatch_id"],identity=identity)
        self.assertEqual(len([e for e in again["task"]["log"] if e.get("request_id")==identity]),1)
        self.assertNotIn("Ordinary review comment",self.received())

    def test_a_renamed_task_is_delivered_to_the_same_native_run(self):
        task = self.add("Follow the immutable run", "--target", self.name)
        old = self.name
        renamed = "codex/task-renamed"
        self.tmux("rename-session", "-t", "=" + old + ":", renamed)
        record = json.loads(self.record_path.read_text())
        record.update(name=renamed, journal_name=old)
        self.record_path.unlink()
        self.record_path = self.state / (hashlib.sha256(renamed.encode()).hexdigest() + ".json")
        self.record_path.write_text(json.dumps(record))
        self.name = renamed; self.runs[renamed] = self.run_id
        sent = self.hgs("send", task["ref"])
        self.assertEqual(sent["task"]["session"]["session"], renamed)
        self.assertEqual(sent["task"]["session"]["session_id"], task["target"]["session_id"])
        self.assertEqual(sent["receipt"]["status"], "submitted")
        self.wait_received("Follow the immutable run")

    def test_handoff_transfers_the_task_pin_and_working_session(self):
        task = self.add("Check the new owner", "--start", session="codex/source")
        before = self.hgs("get", task["ref"])["task"]
        self.hgs("handoff", task["ref"], "--session", "codex/missing", session="codex/source", ok=False)
        self.assertEqual(self.hgs("get", task["ref"])["task"], before)
        handed = self.hgs("handoff", task["ref"], "--session", self.name, "--json", session="codex/source")
        new = handed["task"]
        self.assertEqual(new["stage"], "active")
        self.assertEqual(new["session"]["session"], self.name)
        self.assertEqual(new["target"]["session"], self.name)
        self.assertEqual(new["session"]["session_id"], new["target"]["session_id"])
        self.assertEqual(handed["receipt"]["status"], "submitted")
        entry = new["log"][-1]
        self.assertEqual((entry["kind"], entry["text"], entry["request_id"]), ("sent", "handoff", handed["receipt"]["request_id"]))
        # C3 activation A3: the handoff is the task's attempt, settled by its receipt.
        current = self.execution(task)["current"]
        self.assertEqual((current["id"], current["origin"], current["phase"]),
                         (handed["receipt"]["request_id"], "handoff", "submitted"))
        self.wait_received("Check the new owner")
        self.assertIn("only the task's current session", self.hgs("handoff", task["ref"], "--session", "codex/another", session="codex/source", ok=False))
        report = json.loads(self.as_agent("report-attempt", task["ref"], "--attempt", current["id"], "--json", text="Ownership checked."))
        self.assertTrue(report["moved_to_review"])
        self.assertEqual(report["task"]["stage"], "review")

    def test_only_the_current_executor_moves_a_task_with_its_result(self):
        task = self.add("Check the new owner", "--start", session="codex/source")
        handed = self.hgs("handoff", task["ref"], "--session", self.name, "--json", session="codex/source")
        origin = handed["task"]["execution"]["current"]["id"]
        self.wait_received("Check the new owner")
        # The previous executor's late result is history: it does not move the task.
        late = self.hgs("report-attempt", task["ref"], "--attempt", origin, "--json", text="Finished earlier.", session="codex/source")
        self.assertEqual((late["moved_to_review"], late["report"], late["stale"]), (False, "ready", "other_executor"))
        self.assertEqual((late["task"]["stage"], late["task"]["summary"]), ("active", ""))
        entry = late["task"]["log"][-1]
        self.assertEqual((entry["kind"], entry["text"], entry["stale"]), ("report", "Finished earlier.", "other_executor"))
        self.assertEqual(self.says("report-attempt", task["ref"], "--attempt", origin, text="Same again.", session="codex/source"),
                         task["ref"] + " is In progress; your report was added to its history but cannot move it: "
                         + self.name + " executes it now.")
        self.assertEqual(self.says("report-attempt", task["ref"], "--attempt", origin, "--progress", text="Still here.", session="codex/source"),
                         task["ref"] + " is In progress; your progress was added to its history, outside its current"
                         " execution: " + self.name + " executes it now.")
        # The executor's progress keeps it In progress and records the revision it considered.
        progress = json.loads(self.as_agent("report-attempt", task["ref"], "--attempt", origin, "--progress", "--considered", "1", "--json",
                                            text="Half way."))
        self.assertEqual((progress["moved_to_review"], progress["report"]), (False, "progress"))
        self.assertNotIn("stale", progress)
        entry = progress["task"]["log"][-1]
        self.assertEqual((entry["kind"], entry["text"], entry["considered"]), ("progress", "Half way.", 1))
        self.assertNotIn("stale", entry)
        self.assertEqual(progress["task"]["execution"]["current"]["executor"]["session"], self.name)
        self.assertIn("choose --progress or --ready",
                      self.as_agent("report", task["ref"], "--progress", "--ready", text="Both.", ok=False))
        self.assertIn("--considered is a requirement revision from 1",
                      self.as_agent("report", task["ref"], "--considered", "0", text="None.", ok=False))
        self.assertEqual(self.hgs("get", task["ref"])["task"]["stage"], "active")
        # Its result moves it, with that summary.
        done = json.loads(self.as_agent("report-attempt", task["ref"], "--attempt", origin, "--ready", "--json", text="Ownership checked."))
        self.assertEqual((done["moved_to_review"], done["report"]), (True, "ready"))
        self.assertEqual((done["task"]["stage"], done["task"]["summary"]), ("review", "Ownership checked."))
        self.assertNotIn("stale", done["task"]["log"][-1])

    def test_task_images_reach_native_attachment_storage(self):
        data = b"\x89PNG\r\n\x1a\nsynthetic fixture"
        file = dict(name="screen.png", mime="image/png", reference="[Image #1]", data_base64=base64.b64encode(data).decode())
        task = self.add(json.dumps(dict(text="Inspect [Image #1]", attachments=[file])), "--content-json", "--target", self.name)
        sent = self.hgs("send", task["ref"])
        self.assertEqual(sent["task"]["stage"], "active")
        self.assertEqual(sent["receipt"]["status"], "submitted")
        path = Path(sent["receipt"]["attachments"][0]["path"])
        self.assertEqual(path.read_bytes(), data)
        self.wait_received(str(path))

    def test_send_report_and_a_second_send(self):
        task = self.add("Add rate limiting", "--target", self.name)
        sent = self.hgs("send", task["ref"])
        self.assertEqual(sent["task"]["stage"], "active")
        self.assertEqual(sent["task"]["session"]["session"], self.name)
        # History entries name the computer of their session too.
        entry = next(e for e in sent["task"]["log"] if e["kind"] == "sent")
        self.assertEqual((entry["session"]["session"], entry["session"]["host"]), (self.name, ""))
        self.assertEqual(sent["receipt"]["status"], "submitted")
        self.wait_received("[Zerus task orchard/" + task["id"] + "]")
        self.wait_received("Add rate limiting")
        origin = sent["task"]["execution"]["current"]["id"]
        self.wait_received("`hgs task report-attempt " + task["ref"] + " --attempt " + origin + "`")
        self.assertIn("already In progress in " + self.name, self.hgs("send", task["ref"], ok=False))
        done = json.loads(self.as_agent("report-attempt", task["ref"], "--attempt", origin, "--json", text="Token bucket; 6 tests."))
        self.assertTrue(done["moved_to_review"])
        self.assertEqual((done["task"]["stage"], done["task"]["summary"]), ("review", "Token bucket; 6 tests."))
        again = json.loads(self.as_agent("report", task["ref"], "--json", text="Also updated the README."))
        self.assertFalse(again["moved_to_review"])

    def test_an_any_session_task_needs_a_session(self):
        task = self.add("Anywhere")
        self.assertIn("--session", self.hgs("send", task["ref"], ok=False))
        sent = self.hgs("send", task["ref"], "--session", self.name)["task"]
        self.assertEqual((sent["stage"], sent["target"]), ("active", {"kind": "any"}))
        self.wait_received("Anywhere")

    def test_session_rules(self):
        pinned = self.add("Pinned", "--target", self.name)
        self.assertIn("pinned to " + self.name, self.hgs("send", pinned["ref"], "--session", "codex/other", ok=False))
        anywhere = self.add("Anywhere")
        # An agent that names the session also pins the task there.
        sent = self.hgs("send", anywhere["ref"], "--session", self.name, "--json", session="codex/planner")["task"]
        self.assertEqual(sent["target"]["session"], self.name)

    def test_a_refused_send_changes_nothing(self):
        task = self.add("Nowhere", "--target", "codex/missing")
        self.assertIn("codex/missing", self.hgs("send", task["ref"], ok=False))
        after = self.hgs("get", task["ref"])["task"]
        self.assertEqual((after["stage"], [e["kind"] for e in after["log"]]), ("todo", ["created"]))

    def test_project_settings_bound_agents(self):
        self.settings(agents_other=False)
        other = self.add("Elsewhere", "--target", "codex/other")
        self.assertIn("only in their own session", self.as_agent("send", other["ref"], ok=False))
        self.assertEqual(self.received(), "")
        own = self.add("Mine")
        self.as_agent("send", own["ref"], "--json")
        self.wait_received("Mine")
        self.confirm()
        self.settings(agents_other=True, agent_limit=1)
        another = self.add("One more")
        self.assertIn("1 tasks started by agents per hour", self.as_agent("send", another["ref"], ok=False))
        # People are not limited.
        self.assertEqual(self.hgs("send", another["ref"], "--session", self.name)["task"]["stage"], "active")

    def test_cancel_moves_to_review_and_tells_the_agent(self):
        task = self.add("Refactor the parser", "--target", self.name)
        self.hgs("send", task["ref"])
        self.confirm()
        self.assertIn("a task you were sent", self.as_agent("cancel", task["ref"], ok=False))
        out = self.hgs("cancel", task["ref"], "--reason", "Wrong approach")
        self.assertEqual((out["task"]["stage"], out["task"]["cancel_reason"]), ("review", "Wrong approach"))
        self.assertEqual([(e["kind"], e.get("text", "")) for e in out["task"]["log"][-2:]],
                         [("cancelled", ""), ("comment", "Wrong approach")])
        self.assertTrue(out["cancel_message"]["sent"])
        self.wait_received("[Zerus task orchard/" + task["id"] + " cancelled]")
        self.assertNotIn("Wrong approach", self.received())

    def start_plan(self, task, **overrides):
        current = self.hgs("get", task["ref"])["task"]
        record = json.loads(self.record_path.read_text())
        request = dict(schema=1, id=str(uuid.uuid4()), stage_at=current["stage_at"], target=current["target"],
                       selection=dict(host="", name=self.name, session_id="run_id:" + self.run_id,
                                      run_id=self.run_id, conversation_id=record["conversation_id"]))
        request.update(overrides)
        return self.hgs("start-plan", task["ref"], text=json.dumps(request))["plan"]

    def test_start_plan_is_readonly_and_delivery_uses_one_original_request(self):
        task = self.add("A guarded task start", "--target", self.name)
        plan = self.start_plan(task)
        self.assertEqual(self.received(), "")
        self.assertEqual(self.hgs("get", task["ref"])["task"]["stage"], "todo")
        recovered = self.hgs("start-deliver", task["ref"], "--recover-only", text=json.dumps(plan))
        self.assertEqual(recovered["state"], "uncertain")
        self.assertEqual(self.received(), "")
        sent = self.hgs("start-deliver", task["ref"], text=json.dumps(plan))
        self.assertEqual((sent["state"], sent["id"], sent["task"]["stage"]), ("submitted", plan["id"], "active"))
        self.assertNotIn("log", sent["task"])
        self.wait_received("A guarded task start")
        self.assertEqual(self.hgs("start-deliver", task["ref"], "--recover-only", text=json.dumps(plan))["state"], "submitted")
        self.assertEqual(self.hgs("start-deliver", task["ref"], text=json.dumps(plan))["state"], "submitted")
        self.assertEqual(self.received().count("A guarded task start"), 1)

    def test_start_deliver_refuses_changed_document_or_run_before_input(self):
        task = self.add("Original task", "--target", self.name)
        plan = self.start_plan(task)
        self.hgs("rename", task["ref"], text="Changed title")
        refusal = self.hgs("start-deliver", task["ref"], text=json.dumps(plan))
        self.assertEqual(refusal["state"], "refused")
        self.assertEqual(self.received(), "")
        self.assertEqual(self.hgs("get", task["ref"])["task"]["stage"], "todo")
        plan = self.start_plan(task)
        record = json.loads(self.record_path.read_text()); record["conversation_id"] = str(uuid.uuid4()); self.record_path.write_text(json.dumps(record))
        refusal = self.hgs("start-deliver", task["ref"], text=json.dumps(plan))
        self.assertEqual(refusal["state"], "refused")
        self.assertEqual(self.received(), "")

    def test_new_session_start_requires_exact_launch_and_preserves_parent_target(self):
        self.agent("codex/planner")
        task = self.add("An exact new-session executor")
        self.hgs("target", task["ref"], "new-session", "--parent", "codex/planner")
        record = json.loads(self.record_path.read_text())
        current = self.hgs("get", task["ref"])["task"]
        wrong = dict(host="", name=self.name, session_id="run_id:" + self.run_id,
                     run_id=self.run_id, conversation_id=record["conversation_id"])
        request = dict(schema=1, id=str(uuid.uuid4()), stage_at=current["stage_at"], target=current["target"], selection=wrong)
        self.assertIn("changed", self.hgs("start-plan", task["ref"], text=json.dumps(request), ok=False))
        self.assertEqual(self.received(), "")
        launch = str(uuid.uuid4())
        record["launch_id"] = launch
        self.record_path.write_text(json.dumps(record))
        selected = dict(wrong, session_id="launch_id:" + launch)
        plan = self.start_plan(task, selection=selected)
        self.assertEqual(plan["target"]["kind"], "new_session")
        self.assertEqual(self.received(), "")
        sent = self.hgs("start-deliver", task["ref"], text=json.dumps(plan))
        self.assertEqual(sent["state"], "submitted")
        self.assertEqual(sent["task"]["session"]["session_id"], "launch_id:" + launch)
        self.assertEqual(sent["task"]["target"], current["target"])
        self.wait_received("An exact new-session executor")
        self.assertEqual(self.hgs("start-deliver", task["ref"], "--recover-only", text=json.dumps(plan))["state"], "submitted")
        self.assertEqual(self.received().count("An exact new-session executor"), 1)

    def test_start_recovery_settles_native_receipt_without_pasting_again(self):
        task = self.add("Recover native acknowledgement", "--target", self.name)
        plan = self.start_plan(task)
        # Simulate a process exit after native submission and before task save.
        sent = subprocess.run([str(HGS), "send", self.name, "--json"], input=json.dumps(plan["request"], separators=(",", ":"), ensure_ascii=False), env=self.env, text=True, capture_output=True, timeout=30)
        self.assertEqual(sent.returncode, 0, sent.stderr)
        self.wait_received("Recover native acknowledgement")
        self.assertEqual(self.hgs("get", task["ref"])["task"]["stage"], "todo")
        recovered = self.hgs("start-deliver", task["ref"], "--recover-only", text=json.dumps(plan))
        self.assertEqual((recovered["state"], recovered["task"]["stage"]), ("submitted", "active"))
        self.assertEqual(self.received().count("Recover native acknowledgement"), 1)

    def execution(self, task):
        return self.hgs("get", task["ref"])["task"].get("execution") or {}

    def test_a_start_reserves_one_attempt_and_settles_it_from_its_receipt(self):
        # zerus-cg5h.4 C3 activation A2: a lone computer's bootstrap gate is
        # satisfied, so the Start floors the task, proves the fence and
        # reserves ONE attempt under the plan's own ID before the input; its
        # receipt settles it. A repeat or a recovery only reads that receipt.
        task = self.add("An attempted start", "--target", self.name)
        plan = self.start_plan(task)
        sent = self.hgs("start-deliver", task["ref"], text=json.dumps(plan))
        self.assertEqual((sent["state"], sent["task"]["stage"]), ("submitted", "active"))
        self.wait_received("An attempted start")
        execution = self.execution(task)
        self.assertEqual((execution["attempts"], execution["current"]["id"], execution["current"]["phase"]),
                         (1, plan["id"], "submitted"))
        status = self.hgs("cutover", "--project", "orchard")
        self.assertEqual((status["phase"], status["cut_over_tasks"]), ("active", 1))
        for extra in ((), ("--recover-only",)):
            again = self.hgs("start-deliver", task["ref"], *extra, text=json.dumps(plan))
            self.assertEqual(again["state"], "submitted")
        self.assertEqual(self.received().count("An attempted start"), 1)
        self.assertEqual(self.execution(task)["attempts"], 1)

    def test_a_start_refused_before_any_input_is_not_admitted_and_never_resent(self):
        # desktop-u's (a): the exact submission witnessed that the payload was
        # never offered (the previous input is not confirmed yet), so the
        # attempt is settled as not admitted: refused, nothing pasted. That ID
        # is never sent again; a new plan sends it once the session is ready.
        first = self.add("Holds the input", "--target", self.name)
        second = self.add("Waits behind it", "--target", self.name)
        later = self.start_plan(second)
        self.assertEqual(self.hgs("start-deliver", first["ref"], text=json.dumps(self.start_plan(first)))["state"],
                         "submitted")
        refused = self.hgs("start-deliver", second["ref"], text=json.dumps(later))
        self.assertEqual(refused["state"], "refused")
        self.assertIn("previous input has not been confirmed", refused["error"])
        execution = self.execution(second)
        self.assertEqual((execution["attempts"], execution["current"]["id"], execution["current"]["phase"]),
                         (1, later["id"], "not_started"))
        self.assertEqual(self.hgs("get", second["ref"])["task"]["stage"], "todo")
        self.confirm()
        for extra in ((), ("--recover-only",)):
            again = self.hgs("start-deliver", second["ref"], *extra, text=json.dumps(later))
            self.assertNotEqual(again["state"], "submitted", again)
        self.assertEqual(self.received().count("Waits behind it"), 0)
        sent = self.hgs("start-deliver", second["ref"], text=json.dumps(self.start_plan(second)))
        self.assertEqual(sent["state"], "submitted")
        self.wait_received("Waits behind it")
        self.assertEqual(self.execution(second)["attempts"], 2)

    def test_a_start_whose_receipt_marker_cannot_be_written_stays_unknown(self):
        # desktop-u's (a): a failure while writing the input's receipt marker
        # may have published it, so the delivery is unknown, never "not
        # admitted"; neither a repeat nor --recover-only submits it, even with
        # no receipt at all, and a new plan is refused while it is unknown.
        warm = self.add("Warms the receipts", "--target", self.name)
        self.assertEqual(self.hgs("start-deliver", warm["ref"], text=json.dumps(self.start_plan(warm)))["state"],
                         "submitted")
        self.confirm()
        task = self.add("Its marker fails", "--target", self.name)
        plan = self.start_plan(task)
        receipts = self.state / "input_receipts"
        receipts.chmod(0o500)
        self.addCleanup(receipts.chmod, 0o700)
        unknown = self.hgs("start-deliver", task["ref"], text=json.dumps(plan))
        receipts.chmod(0o700)
        self.assertEqual(unknown["state"], "uncertain", unknown)
        self.assertEqual((self.execution(task)["current"]["id"], self.execution(task)["current"]["phase"]),
                         (plan["id"], "unknown"))
        for extra in ((), ("--recover-only",)):
            again = self.hgs("start-deliver", task["ref"], *extra, text=json.dumps(plan))
            self.assertEqual(again["state"], "uncertain")
        self.assertEqual(self.received().count("Its marker fails"), 0)
        current = self.hgs("get", task["ref"])["task"]
        self.assertIn("unknown", self.hgs("start-plan", task["ref"], text=json.dumps(dict(
            schema=1, id=str(uuid.uuid4()), stage_at=current["stage_at"], target=current["target"],
            selection=plan["selection"])), ok=False))
        # Had that input arrived, its own receipt alone settles the attempt.
        native = subprocess.run([str(HGS), "send", self.name, "--json"], input=json.dumps(plan["request"], separators=(",", ":"), ensure_ascii=False),
                                env=self.env, text=True, capture_output=True, timeout=30)
        self.assertEqual(native.returncode, 0, native.stderr)
        recovered = self.hgs("start-deliver", task["ref"], "--recover-only", text=json.dumps(plan))
        self.assertEqual((recovered["state"], recovered["task"]["stage"]), ("submitted", "active"))
        self.assertEqual(self.execution(task)["current"]["phase"], "submitted")
        self.assertEqual(self.received().count("Its marker fails"), 1)

    def test_a_start_cut_after_its_input_is_unknown_and_never_resent(self):
        # desktop-u's (a): a failure after the receipt marker (here the
        # writer is killed just after the paste) leaves the delivery unknown;
        # a repeat and --recover-only read its receipt and never paste again.
        task = self.add("Cut after the paste", "--target", self.name)
        plan = self.start_plan(task)
        shim = "#!/usr/bin/env python3\nimport os,signal,subprocess,sys\na=sys.argv[1:]\n"
        shim += "r=subprocess.run([" + repr(TMUX) + ",'-S'," + repr(str(self.socket)) + ",'-f','/dev/null',*a])\n"
        shim += "if a and a[0]=='paste-buffer': os.kill(os.getppid(),signal.SIGKILL)\nsys.exit(r.returncode)\n"
        self.script("tmux", shim)
        self.hgs("start-deliver", task["ref"], text=json.dumps(plan), ok=False)
        self.wait_received("Cut after the paste")
        self.script("tmux", '#!/bin/sh\nexec ' + TMUX + ' -S "' + str(self.socket) + '" -f /dev/null "$@"\n')
        for extra in ((), ("--recover-only",)):
            again = self.hgs("start-deliver", task["ref"], *extra, text=json.dumps(plan))
            self.assertEqual(again["state"], "uncertain")
        self.assertEqual(self.received().count("Cut after the paste"), 1)
        self.assertEqual((self.execution(task)["current"]["id"], self.execution(task)["current"]["phase"]),
                         (plan["id"], "unknown"))

    def held(self, *args, text=None, session=None):
        """`hgs task ARGS` held between its freeze and its admission (debug
        builds only: HGS_TEST_ATTEMPT_PAUSE). Returns its release."""
        folder = self.root / ("pause-" + str(uuid.uuid4()))
        folder.mkdir()
        env = dict(self.env, HGS_TEST_ATTEMPT_PAUSE=str(folder))
        if session:
            env.update(HGS_SESSION=session, HGS_RUN_ID=self.agent(session))
        proc = subprocess.Popen([str(HGS), "task", *args], env=env, text=True, stdin=subprocess.PIPE,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        proc.stdin.write(text or "")
        proc.stdin.close()
        self.addCleanup(lambda: proc.poll() is None and ((folder / "release").touch(), proc.wait(timeout=30)))
        until = time.monotonic() + 15
        while not (folder / "ready").exists():
            if proc.poll() is not None:
                self.fail("ended before its admission: " + proc.stderr.read())
            self.assertLess(time.monotonic(), until)
            time.sleep(.005)
        def release():
            (folder / "release").touch()
            out = proc.stdout.read()
            err = proc.stderr.read()
            proc.wait(timeout=30)
            return proc.returncode, out, err
        return release

    def test_a_target_changed_while_frozen_is_never_sent(self):
        # review-win-c and desktop-u on #1519 (zerus-frehz): the task changes
        # between the freeze and the final admission: its target for a send
        # or a Start, its stage for a reopen (Done is reopened too, so only
        # the frozen stage time tells), its stage for a handoff. Every path
        # refuses it there and sends nothing.
        self.agent("codex/other")
        retarget = lambda task: self.hgs("target", task["ref"], "codex/other")
        # CLI send.
        task = self.add("FROZEN_SEND", "--target", self.name)
        release = self.held("send", task["ref"], "--json")
        retarget(task)
        code, _, err = release()
        self.assertNotEqual(code, 0)
        self.assertIn("changed", err)
        self.assertEqual(self.execution(task).get("attempts", 0), 0)
        # CLI send, the stage only: still sendable from Backlog, yet not as frozen.
        task = self.add("FROZEN_STAGE", "--target", self.name)
        release = self.held("send", task["ref"], "--json")
        self.hgs("stage", task["ref"], "backlog")
        code, _, err = release()
        self.assertNotEqual(code, 0)
        self.assertIn("changed", err)
        # Start.
        task = self.add("FROZEN_START", "--target", self.name)
        release = self.held("start-deliver", task["ref"], "--json", text=json.dumps(self.start_plan(task)))
        retarget(task)
        code, out, _ = release()
        self.assertEqual((code, json.loads(out)["state"]), (0, "refused"))
        # Reopen.
        task = self.reviewed("FROZEN_REOPEN")
        identity = str(uuid.uuid4())
        release = self.held("message", task["ref"], "--content-json", "--mode", "reopen", "--request-id", identity,
                            "--expect-dispatch", task["dispatch_id"], "--json",
                            text=json.dumps(dict(text="FROZEN_REOPEN_TEXT", attachments=[])))
        self.hgs("stage", task["ref"], "done")
        code, _, err = release()
        self.assertNotEqual(code, 0)
        self.assertIn("changed", err)
        self.assertEqual(self.notes(task, identity), [])
        # Handoff, by the task's own agent.
        task = self.add("FROZEN_HANDOFF", "--start", session="codex/source")
        release = self.held("handoff", task["ref"], "--session", self.name, "--json", session="codex/source")
        self.hgs("cancel", task["ref"], "--reason", "Stop")
        code, _, err = release()
        self.assertNotEqual(code, 0)
        for text in ("FROZEN_SEND", "FROZEN_STAGE", "FROZEN_START", "FROZEN_REOPEN_TEXT", "FROZEN_HANDOFF"):
            self.assertNotIn(text, self.received())

    def test_the_agent_limit_is_counted_again_in_the_final_admission(self):
        # review-win-c and desktop-u on #1519 (zerus-v3rtj): an agent's
        # start frozen under the limit is refused once another start used it
        # meanwhile; the count is taken in the final admission.
        self.settings(agents_other=True, agent_limit=1)
        first = self.add("LIMIT_FIRST", "--target", self.name)
        second = self.add("LIMIT_SECOND", "--target", self.name)
        release = self.held("send", first["ref"], "--json", session="codex/actor")
        self.hgs("send", second["ref"], "--json", session="codex/actor")
        self.wait_received("LIMIT_SECOND"); self.confirm()
        code, _, err = release()
        self.assertNotEqual(code, 0)
        self.assertIn("per hour", err)
        self.assertNotIn("LIMIT_FIRST", self.received())
        self.assertEqual(self.execution(first).get("attempts", 0), 0)

    def test_a_possibly_delivered_agent_start_holds_the_limit(self):
        # desktop-u on #1519: an agent's reserved start whose delivery is
        # unknown may already have arrived, so it holds the hourly capacity
        # from its reservation; another agent start is refused.
        warm = self.add("WARMS_THE_RECEIPTS", "--target", self.name)
        self.hgs("send", warm["ref"]); self.confirm()
        self.settings(agents_other=True, agent_limit=1)
        unknown = self.add("MAYBE_DELIVERED", "--target", self.name)
        receipts = self.state / "input_receipts"
        receipts.chmod(0o500)
        self.addCleanup(receipts.chmod, 0o700)
        self.assertIn("delivery uncertain", self.hgs("send", unknown["ref"], session="codex/actor", ok=False))
        receipts.chmod(0o700)
        self.assertEqual(self.execution(unknown)["current"]["phase"], "unknown")
        other = self.add("NOT_WITHIN_THE_LIMIT", "--target", self.name)
        self.assertIn("per hour", self.hgs("send", other["ref"], session="codex/actor", ok=False))
        self.assertNotIn("NOT_WITHIN_THE_LIMIT", self.received())

    def test_an_uncountable_task_refuses_an_agent_start(self):
        # review-win-c on #1519 R2: the final admission's count fails closed.
        # A charged task that cannot be read is no evidence of zero starts,
        # and an irregular file at a task's place (a FIFO) never makes the
        # count wait: the agent's start is refused, and nothing is sent. A
        # person's start counts nothing and goes on.
        self.settings(agents_other=True, agent_limit=1)
        tasks = self.state / "replica" / hashlib.sha256(b"project-1").hexdigest() / "tasks"
        first = self.add("UNCOUNTED_FIRST", "--target", self.name)
        charged = self.add("UNCOUNTED_CHARGED", "--target", self.name)
        release = self.held("send", first["ref"], "--json", session="codex/actor")
        self.hgs("send", charged["ref"], "--json", session="codex/actor")
        self.wait_received("UNCOUNTED_CHARGED"); self.confirm()
        snapshot = tasks / (charged["uuid"] + ".loro")
        snapshot.chmod(0)
        try:
            code, _, err = release()
        finally:
            snapshot.chmod(0o600)
        self.assertNotEqual(code, 0)
        self.assertIn("cannot be counted", err)
        self.assertNotIn("UNCOUNTED_FIRST", self.received())
        # A FIFO named as a task's file (other commands that list every
        # task are not this fix's; the task and settings come first).
        self.settings(agents_other=True, agent_limit=5)
        late = self.add("UNCOUNTED_LATE", "--target", self.name)
        fifo = tasks / (str(uuid.uuid4()) + ".loro")
        os.mkfifo(fifo)
        self.addCleanup(lambda: fifo.unlink(missing_ok=True))
        # By its UUID, so only the quota count reads the FIFO here; a
        # short-ID lookup beside one is test_an_irregular_task_file_never_
        # holds_project_reads (zerus-r75ru).
        ref = "orchard/" + late["uuid"]
        began = time.monotonic()
        refused = self.hgs("send", ref, session="codex/actor", ok=False)
        self.assertLess(time.monotonic() - began, 17)
        self.assertIn("cannot be counted", refused)
        self.assertNotIn("UNCOUNTED_LATE", self.received())
        self.assertEqual(self.hgs("send", ref)["task"]["stage"], "active")
        self.wait_received("UNCOUNTED_LATE")

    def test_an_irregular_task_file_never_holds_project_reads(self):
        # zerus-r75ru: a FIFO named as a task's file never holds a command
        # that reads every task. It is listed as damaged; the other tasks
        # are listed, found by short ID and added beside it as before.
        kept = self.add("KEPT_BESIDE_A_FIFO", "--target", self.name)
        tasks = self.state / "replica" / hashlib.sha256(b"project-1").hexdigest() / "tasks"
        fifo = tasks / (str(uuid.uuid4()) + ".loro")
        os.mkfifo(fifo)
        self.addCleanup(lambda: fifo.unlink(missing_ok=True))
        began = time.monotonic()
        listing = self.hgs("list", "--project", "orchard")
        self.assertIn(kept["uuid"], [task["uuid"] for task in listing["tasks"]])
        self.assertIn(fifo.stem, json.dumps(listing["damaged"]))
        self.assertEqual(self.hgs("get", kept["ref"])["task"]["uuid"], kept["uuid"])
        added = self.add("ADDED_BESIDE_A_FIFO")
        self.assertEqual(self.hgs("get", added["ref"])["task"]["uuid"], added["uuid"])
        self.assertLess(time.monotonic() - began, 25)

    def test_the_legacy_route_counts_a_possible_agent_start_too(self):
        # desktop-u on #1519 R2: the legacy route's limit counts the tasks as
        # saved, reservations included, never the index's sent history
        # alone. An agent's unknown reservation holds the capacity, so its
        # legacy send of an unfloored task under the hold is refused.
        warm = self.add("LEGACY_QUOTA_WARM", "--target", self.name)
        self.hgs("send", warm["ref"]); self.confirm()
        self.settings(agents_other=True, agent_limit=1)
        unknown = self.add("LEGACY_QUOTA_UNKNOWN", "--target", self.name)
        receipts = self.state / "input_receipts"
        receipts.chmod(0o500)
        self.addCleanup(receipts.chmod, 0o700)
        self.assertIn("delivery uncertain", self.hgs("send", unknown["ref"], session="codex/actor", ok=False))
        receipts.chmod(0o700)
        self.hgs("cutover", "--project", "orchard", "--hold", "on")
        legacy = self.add("LEGACY_QUOTA_FRESH", "--target", self.name)
        self.assertIn("per hour", self.hgs("send", legacy["ref"], session="codex/actor", ok=False))
        self.assertNotIn("LEGACY_QUOTA_FRESH", self.received())
        self.assertFalse(self.execution(legacy).get("attempts"))

    def test_a_counted_task_with_broken_updates_refuses_an_agent_start(self):
        # review-win-c on #1519 R3: a counted task whose updates log does not
        # frame or import whole is unknown evidence, never fewer starts.
        self.settings(agents_other=True, agent_limit=1)
        self.hgs("cutover", "--project", "orchard", "--hold", "on")
        delivered = self.add("BROKEN_UPDATES_DELIVERED", "--target", self.name)
        later = self.add("BROKEN_UPDATES_LATER", "--target", self.name)
        self.hgs("send", delivered["ref"], "--json", session="codex/actor")
        self.wait_received("BROKEN_UPDATES_DELIVERED"); self.confirm()
        updates = self.state / "replica" / hashlib.sha256(b"project-1").hexdigest() / "tasks" / (delivered["uuid"] + ".updates")
        updates.write_bytes(b"garbage" + updates.read_bytes())
        refused = self.hgs("send", later["ref"], session="codex/actor", ok=False)
        self.assertIn("cannot be counted", refused)
        self.assertNotIn("BROKEN_UPDATES_LATER", self.received())

    def test_an_unreadable_receipt_after_a_start_is_uncertain(self):
        # review-win-c on #1519 R2 (zerus-hhxn5): a receipt that cannot be
        # read after a possible delivery is unknown evidence, never
        # "nothing was sent": a repeat and --recover-only answer uncertain,
        # on the managed and the legacy route, and nothing is pasted again.
        task = self.add("RECEIPT_MANAGED", "--target", self.name)
        plan = self.start_plan(task)
        self.assertEqual(self.hgs("start-deliver", task["ref"], text=json.dumps(plan))["state"], "submitted")
        self.wait_received("RECEIPT_MANAGED"); self.confirm()
        (self.state / "input_receipts" / (plan["id"] + ".json")).write_text("{corrupt")
        for extra in ((), ("--recover-only",)):
            again = self.hgs("start-deliver", task["ref"], *extra, text=json.dumps(plan))
            self.assertEqual(again["state"], "uncertain", again)
        self.hgs("cutover", "--project", "orchard", "--hold", "on")
        legacy = self.add("RECEIPT_LEGACY", "--target", self.name)
        plan = self.start_plan(legacy)
        self.assertEqual(self.hgs("start-deliver", legacy["ref"], text=json.dumps(plan))["state"], "submitted")
        self.wait_received("RECEIPT_LEGACY"); self.confirm()
        self.assertFalse(self.execution(legacy).get("attempts"))
        receipt = self.state / "input_receipts" / (plan["id"] + ".json")
        original = json.loads(receipt.read_text())
        # Unreadable JSON, or JSON that is not a whole receipt (review-win-c
        # on #1519 R3: `{}`, a missing or mistyped field, another schema).
        malformed = ("{corrupt", "{}", "null", "[]", json.dumps(dict(original, digest=None)),
                     json.dumps({k: v for k, v in original.items() if k != "name"}),
                     json.dumps(dict(original, status="lost")), json.dumps(dict(original, first_message="no")))
        for body in malformed:
            receipt.write_text(body)
            for extra in ((), ("--recover-only",)):
                again = self.hgs("start-deliver", legacy["ref"], *extra, text=json.dumps(plan))
                self.assertEqual(again["state"], "uncertain", (body, again))
        # A whole receipt of other content is another request's: refused.
        receipt.write_text(json.dumps(dict(original, digest="0" * 64)))
        self.assertEqual(self.hgs("start-deliver", legacy["ref"], text=json.dumps(plan))["state"], "refused")
        self.assertEqual((self.received().count("RECEIPT_MANAGED"), self.received().count("RECEIPT_LEGACY")), (1, 1))

    def test_the_agent_limit_bounds_a_frozen_handoff_too(self):
        self.settings(agents_other=True, agent_limit=1)
        task = self.add("LIMIT_HANDOFF", "--start", session="codex/source")
        other = self.add("LIMIT_OTHER", "--target", self.name)
        release = self.held("handoff", task["ref"], "--session", self.name, "--json", session="codex/source")
        self.hgs("send", other["ref"], "--json", session="codex/source")
        self.wait_received("LIMIT_OTHER"); self.confirm()
        code, _, err = release()
        self.assertNotEqual(code, 0)
        self.assertIn("per hour", err)
        self.assertNotIn("LIMIT_HANDOFF", self.received())

    def test_a_reopen_obeys_the_project_s_agent_rules(self):
        # desktop-u on #1519 (zerus-wvzdk): a managed reopen runs the
        # project's agent rules in its final admission, as a send does: an
        # agent may not reopen another session's task when the project keeps
        # agents to their own.
        self.settings(agents_other=False)
        task = self.reviewed("OUTSIDER_TASK")
        self.agent("codex/outsider")
        identity = str(uuid.uuid4())
        refused = self.message(task, "OUTSIDER_TEXT", mode="reopen", identity=identity,
                               session="codex/outsider", ok=False)
        self.assertIn("only in their own session", refused)
        current = self.hgs("get", task["ref"])["task"]
        self.assertEqual(current["stage"], "review")
        self.assertNotIn("resumed", [e["kind"] for e in current["log"]])
        self.assertEqual(self.notes(task, identity), [])
        self.assertNotIn("OUTSIDER_TEXT", self.received())

    def test_a_legacy_reopen_obeys_the_project_s_agent_rules(self):
        # zerus-anemc: the legacy route (here, the person's hold over a task
        # with no floor or attempt) applies the same rules: an outsider agent
        # may not reopen another session's task when agents keep to their
        # own, and an agent past the hourly limit may not reopen one. A
        # refusal saves no note and sends nothing; the person still reopens.
        self.hgs("cutover", "--project", "orchard", "--hold", "on")
        self.settings(agents_other=False)
        task = self.reviewed("LEGACY_OUTSIDER_TASK")
        self.assertFalse(self.execution(task).get("attempts"))
        self.agent("codex/outsider")
        identity = str(uuid.uuid4())
        refused = self.message(task, "LEGACY_OUTSIDER_TEXT", mode="reopen", identity=identity,
                               session="codex/outsider", ok=False)
        self.assertIn("only in their own session", refused)
        self.assertEqual(self.hgs("get", task["ref"])["task"]["stage"], "review")
        self.assertEqual(self.notes(task, identity), [])
        self.assertNotIn("LEGACY_OUTSIDER_TEXT", self.received())
        # Past the hourly limit: an agent the rules let reach any session,
        # which just made the hour's one start, is refused too.
        self.settings(agents_other=True, agent_limit=1)
        warm = self.add("LEGACY_REOPEN_WARM", "--target", self.name)
        self.hgs("send", warm["ref"], "--json", session="codex/actor")
        self.wait_received("LEGACY_REOPEN_WARM"); self.confirm()
        limited = str(uuid.uuid4())
        refused = self.message(task, "LEGACY_LIMIT_TEXT", mode="reopen", identity=limited,
                               session="codex/actor", ok=False)
        self.assertIn("per hour", refused)
        self.assertEqual(self.notes(task, limited), [])
        self.assertNotIn("LEGACY_LIMIT_TEXT", self.received())
        # The person's reopen goes on, on the legacy route.
        reopened = self.message(task, "LEGACY_PERSON_TEXT", mode="reopen")
        self.assertEqual(reopened["task"]["stage"], "active")
        self.assertNotIn("attempt", reopened)
        self.wait_received("LEGACY_PERSON_TEXT")

    def test_a_start_id_reused_for_another_body_is_refused(self):
        # desktop-u on #1519 (zerus-f5q49): an ID answers only for its own
        # request. After a managed Start, the same ID with a changed body is
        # refused, repeated or recovered; on the legacy route too, its
        # recorded history answers only when its receipt shows this request.
        def tampered(plan):
            return dict(plan, request=dict(plan["request"], text=plan["request"]["text"] + " CHANGED_BODY"))
        task = self.add("ORIGINAL_BODY", "--target", self.name)
        plan = self.start_plan(task)
        self.assertEqual(self.hgs("start-deliver", task["ref"], text=json.dumps(plan))["state"], "submitted")
        self.wait_received("ORIGINAL_BODY"); self.confirm()
        for extra in ((), ("--recover-only",)):
            out = self.hgs("start-deliver", task["ref"], *extra, text=json.dumps(tampered(plan)))
            self.assertEqual(out["state"], "refused", out)
        # The legacy route: the person's hold keeps this unfloored task on it.
        self.hgs("cutover", "--project", "orchard", "--hold", "on")
        legacy = self.add("LEGACY_BODY", "--target", self.name)
        plan = self.start_plan(legacy)
        self.assertEqual(self.hgs("start-deliver", legacy["ref"], text=json.dumps(plan))["state"], "submitted")
        self.wait_received("LEGACY_BODY")
        self.assertEqual(self.execution(legacy)["attempts"], 0)
        for extra in ((), ("--recover-only",)):
            out = self.hgs("start-deliver", legacy["ref"], *extra, text=json.dumps(tampered(plan)))
            self.assertEqual(out["state"], "refused", out)
        self.assertNotIn("CHANGED_BODY", self.received())

    def test_a_held_lock_refuses_within_the_one_budget(self):
        # review-win-c on #1519 (zerus-9gzdp): every lock wait from the
        # command's entry counts against its one 15-second budget: a held
        # project or catalog lock refuses it as busy, and nothing is sent.
        task = self.add("LOCK_BUDGET", "--target", self.name)
        plan = self.start_plan(task)
        project = self.state / "replica" / hashlib.sha256(b"project-1").hexdigest() / ".lock"
        catalog = self.root / "config" / "swarm" / "catalog.lock"
        cases = (
            (project, ["start-deliver", task["ref"], "--json"], json.dumps(plan)),
            (catalog, ["start-deliver", task["ref"], "--json"], json.dumps(plan)),
            (project, ["send", task["ref"], "--json"], None),
        )
        for path, args, text in cases:
            with path.open("a") as held:
                fcntl.flock(held, fcntl.LOCK_EX)
                began = time.monotonic()
                result = subprocess.run([str(HGS), "task", *args], input=text, env=self.env, text=True,
                                        capture_output=True, timeout=40)
                took = time.monotonic() - began
                fcntl.flock(held, fcntl.LOCK_UN)
            self.assertNotEqual(result.returncode, 0, (args, result.stdout))
            self.assertIn("busy", result.stderr)
            self.assertLess(took, 17, args)
        self.assertNotIn("LOCK_BUDGET", self.received())
        self.assertEqual(self.execution(task).get("attempts", 0), 0)

    def test_a_frozen_start_is_refused_when_its_business_binding_changed(self):
        # desktop-u's condition 2: the reservation re-renders the frozen
        # request against the task as admitted; a new queued message, an
        # edited requirement or another target refuses it, and nothing is
        # reserved or sent. A fresh plan then starts it.
        changes = (
            lambda task: self.message(task, "A late clarification", mode="queue"),
            lambda task: self.hgs("edit", task["ref"], text="A changed requirement"),
            lambda task: self.hgs("target", task["ref"], "any"),
        )
        for index, change in enumerate(changes):
            task = self.add("Frozen start %d" % index, "--target", self.name)
            plan = self.start_plan(task)
            change(task)
            refused = self.hgs("start-deliver", task["ref"], text=json.dumps(plan))
            self.assertEqual(refused["state"], "refused", refused)
            self.assertEqual(self.execution(task).get("attempts", 0), 0)
            self.assertEqual(self.hgs("get", task["ref"])["task"]["stage"], "todo")
        self.assertEqual(self.received(), "")
        task = self.add("Started afresh", "--target", self.name)
        plan = self.start_plan(task)
        self.message(task, "Known before the plan", mode="queue")
        self.assertEqual(self.hgs("start-deliver", task["ref"], text=json.dumps(plan))["state"], "refused")
        sent = self.hgs("start-deliver", task["ref"], text=json.dumps(self.start_plan(task)))
        self.assertEqual(sent["state"], "submitted")
        self.wait_received("Known before the plan")

    def test_a_held_floored_task_is_cutting_over_never_legacy(self):
        # A task that took its floor stays off the legacy route: with the
        # person's hold on, its first attempt is refused as cutting over and
        # nothing is sent; cleared, it starts through its attempt.
        task = self.add("Floored, then held", "--target", self.name)
        plan = self.start_plan(task)
        self.hgs("rename", task["ref"], text="Floored, then held again")
        self.assertEqual(self.hgs("start-deliver", task["ref"], text=json.dumps(plan))["state"], "refused")
        self.assertEqual(self.hgs("cutover", "--project", "orchard")["cut_over_tasks"], 1, "the floor stays")
        self.hgs("cutover", "--project", "orchard", "--hold", "on")
        held = self.hgs("start-deliver", task["ref"], text=json.dumps(self.start_plan(task)))
        self.assertEqual(held["state"], "cutting_over")
        self.assertIn("held", held["error"])
        self.assertEqual(self.received(), "")
        self.hgs("cutover", "--project", "orchard", "--hold", "off", "--confirm")
        sent = self.hgs("start-deliver", task["ref"], text=json.dumps(self.start_plan(task)))
        self.assertEqual(sent["state"], "submitted")
        self.wait_received("Floored, then held again")

    def test_cancel_execution_pins_dispatch_and_executor_and_keeps_reason_literal(self):
        task = self.add("Refactor the parser", "--target", self.name)
        sent = self.hgs("send", task["ref"])["task"]
        self.confirm()
        request = dict(schema=1, dispatch_id=sent["dispatch_id"], stage_at=sent["stage_at"], machine=sent["session"]["machine"],
                       session_id=sent["session"]["session_id"], reason="--agent")
        before = self.received()
        for key, value in (("stage_at", sent["stage_at"] + 1), ("dispatch_id", "later"), ("machine", "other"), ("session_id", "run_id:later"), ("schema", 2)):
            refused = self.hgs("cancel-execution", task["ref"], text=json.dumps(dict(request, **{key: value})), ok=False)
            self.assertTrue("changed" in refused or "invalid" in refused, refused)
            self.assertEqual(self.hgs("get", task["ref"])["task"]["stage"], "active")
            self.assertEqual(self.received(), before)
        out = self.hgs("cancel-execution", task["ref"], text=json.dumps(request))
        self.assertEqual(out["task"]["stage"], "review")
        self.assertNotIn("log", out["task"])
        self.assertTrue(out["cancel_message"]["sent"], out)
        self.wait_received("[Zerus task orchard/" + task["id"] + " cancelled]")
        self.assertNotIn("--agent", self.received())
        self.assertIn("--agent", [e.get("text") for e in self.hgs("get", task["ref"])["task"]["log"]])
        self.assertIn("changed", self.hgs("cancel-execution", task["ref"], text=json.dumps(request), ok=False))
        self.assertEqual(self.received().count(" cancelled]"), 1)

    def test_a_refused_cancel_message_still_cancels(self):
        task = self.add("Refactor the parser", "--target", self.name)
        self.hgs("send", task["ref"])
        # No hook event since the paste: hgs refuses further input.
        out = self.hgs("cancel", task["ref"])
        self.assertEqual(out["task"]["stage"], "review")
        self.assertFalse(out["cancel_message"]["sent"])
        self.assertIn("not been confirmed", out["cancel_message"]["error"])
        self.assertEqual(out["task"]["log"][-1]["kind"], "note")


class AgentAnswers(DeliveryFixture):
    def setUp(self):
        super().setUp()
        self.env["TZ"] = "UTC"
        self.agent("codex/api")
        join(self.env, "project-1", "codex/api")

    def test_changes_answer_in_one_line(self):
        self.add("Earlier work")
        filed = self.says("add", text="Add rate limiting")
        self.assertRegex(filed, r"^Filed orchard/T-\w+ in To do, position 2, for codex/api\.$")
        ref = filed.split()[1]
        self.assertRegex(self.says("add", "--backlog", "--position", "1", text="Measure first"),
                         r"^Filed orchard/T-\w+ in Backlog, position 1, for codex/api\.$")
        self.assertEqual(self.says("move", ref, "--position", "1"), ref + " is position 1 in To do.")
        self.assertEqual(self.says("move", ref, "--position", "9"), ref + " is position 2 in To do.")
        self.assertEqual(self.says("edit", ref, text="Add rate limiting, edited"), ref + " has the new prompt.")
        self.assertEqual(self.says("target", ref, self.name), ref + " now goes to codex/task-test.")
        self.assertEqual(self.says("target", ref, "any"), ref + " now goes to any session.")
        self.assertEqual(self.says("comment", ref, text="Measured: 40 rps."), "Added your comment to " + ref + ".")
        self.assertEqual(self.says("stage", ref, "backlog"), ref + " is in Backlog.")
        # --json prints the task, with the answer beside it.
        out = self.hgs("stage", ref, "todo", "--json", session="codex/api")
        self.assertEqual((out["task"]["stage"], out["answer"]), ("todo", ref + " is in To do."))
        # The person's commands print JSON as before, without an answer.
        self.assertNotIn("answer", self.hgs("stage", ref, "backlog"))

    def test_send_report_and_cancel_answer_in_one_line(self):
        task = self.add("Add rate limiting", "--target", self.name)
        self.assertEqual(self.says("send", task["ref"]), task["ref"] + " sent to codex/task-test.")
        self.wait_received("Add rate limiting")
        origin = self.hgs("get", task["ref"])["task"]["execution"]["current"]["id"]
        self.assertEqual(self.as_agent("report-attempt", task["ref"], "--attempt", origin, text="Token bucket; 6 tests.").strip(),
                         task["ref"] + " is In review.")
        self.assertEqual(self.says("report-attempt", task["ref"], "--attempt", origin, text="Also the README."),
                         task["ref"] + " is In review; your report was added to its history.")
        self.confirm()
        again = self.add("Measure the limits", "--target", self.name)
        sent = self.hgs("send", again["ref"])["task"]
        cancelled_origin = sent["execution"]["current"]["id"]
        self.confirm()
        self.assertEqual(self.says("cancel", again["ref"], "--reason", "Wrong branch"),
                         again["ref"] + " is In review; codex/task-test got the cancel message.")
        cancelled = self.hgs("get", again["ref"])["task"]
        at = datetime.datetime.fromtimestamp(cancelled["cancelled_at"] / 1000, datetime.timezone.utc).strftime("%H:%M")
        self.assertEqual(self.as_agent("report-attempt", again["ref"], "--attempt", cancelled_origin, text="Stopped halfway.").strip(),
                         again["ref"] + " is In review (cancelled at " + at + "); your report was added to its history.")

    def test_a_refused_send_names_its_session(self):
        task = self.add("Add rate limiting", "--target", self.name)
        self.says("send", task["ref"])
        refusal = self.hgs("send", task["ref"], session="codex/api", ok=False)
        self.assertIn("codex/task-test cannot take it now: ", refusal)
        self.assertIn("already In progress in codex/task-test. Nothing was sent.", refusal)

    def test_an_empty_text_says_where_it_goes(self):
        task = self.add("Add rate limiting")
        self.assertIn("the report is empty; give it on stdin, for example: hgs task report REF <<'EOF'",
                      self.hgs("report", task["ref"], text="", session="codex/api", ok=False))
        # The person's tools validate first; they read the plain refusal.
        refusal = self.hgs("comment", task["ref"], text="  ", ok=False)
        self.assertIn("the comment is empty", refusal)
        self.assertNotIn("stdin", refusal)




@unittest.skipUnless(TMUX and HGS.is_file(), "needs the CLI and isolated tmux fixture")
class ReportAttemptCommands(DeliveryFixture):
    def managed(self, text="A managed report execution"):
        task = self.add(text, "--target", self.name)
        sent = self.hgs("send", task["ref"], "--json")
        current = sent["task"]["execution"]["current"]
        self.assertFalse(current.get("legacy", False))
        self.assertGreater(sent["task"]["execution"].get("attempts", 0), 0)
        self.assertEqual(current["phase"], "submitted")
        origin = current["id"]
        command = "hgs task report-attempt " + task["ref"] + " --attempt " + origin
        self.wait_received(command)
        return sent["task"], origin, command

    def test_identical_original_report_arguments_cannot_ready_a_new_generation(self):
        task, first, command = self.managed()
        ack = json.loads(self.as_agent("start", task["ref"], "--json"))
        self.assertEqual((ack["reporting"], ack["report_attempt"], ack["report_command"]),
                         ("attempt", first, command))
        arguments = ("report-attempt", task["ref"], "--attempt", first, "--considered", "1", "--json")
        result = json.loads(self.as_agent(*arguments, text="Exactly the original report arguments"))
        self.assertEqual((result["outcome"], result["moved_to_review"]), ("applied", True))
        after_ready = json.loads(self.as_agent("report-attempt", task["ref"], "--attempt", first,
                                               "--progress", "--json", text="A note after the result"))
        self.assertEqual((after_ready["outcome"], after_ready["task"]["stage"]), ("not_applied", "review"))
        self.assertEqual(after_ready["task"]["execution"]["current"]["phase"], "ready")
        self.confirm()
        self.hgs("stage", task["ref"], "todo")
        sent = self.hgs("send", task["ref"], "--json")
        second = sent["task"]["execution"]["current"]["id"]
        self.assertNotEqual(first, second)
        late = json.loads(self.as_agent(*arguments, text="Exactly the original report arguments"))
        self.assertEqual((late["outcome"], late["history_saved"], late["moved_to_review"], late["stale"]),
                         ("not_applied", True, False, "other_attempt"))
        self.assertNotIn(second, json.dumps(late))
        current = self.hgs("get", task["ref"])["task"]
        self.assertEqual((current["stage"], current["execution"]["current"]["id"]), ("active", second))
        self.assertEqual(current["log"][-1]["reported_attempt"], first)
        self.assertEqual(current["log"][-1]["considered"], 1)
        self.assertNotEqual(current["execution"]["current"]["phase"], "ready")

    def test_plain_managed_reports_save_history_with_exit_zero_without_current_uuid(self):
        task, origin, _ = self.managed()
        before = len(self.hgs("get", task["ref"])["task"]["log"])
        plain = json.loads(self.as_agent("report", task["ref"], "--json", text="Unscoped JSON result"))
        self.assertEqual((plain["outcome"], plain["history_saved"], plain["moved_to_review"]),
                         ("not_applied", True, False))
        self.assertIn("kept as history", plain["report_hint"])
        self.assertIn("original task prompt", plain["report_hint"])
        self.assertNotIn(origin, json.dumps(plain))
        text = self.as_agent("report", task["ref"], text="A distinct unscoped text result")
        self.assertIn("kept as history", text)
        self.assertIn("task did not move", text)
        self.assertNotIn(origin, text)
        saved = self.hgs("get", task["ref"])["task"]
        self.assertEqual(saved["stage"], "active")
        self.assertEqual(len(saved["log"]), before + 2)
        self.assertTrue(all(e["stale"] == "other_attempt" for e in saved["log"][-2:]))

    def test_direct_managed_start_and_active_ack_return_their_original_command(self):
        task, first, _ = self.managed()
        self.as_agent("report-attempt", task["ref"], "--attempt", first, text="First result")
        self.hgs("stage", task["ref"], "todo")
        before = self.received()
        started = json.loads(self.as_agent("start", task["ref"], "--json"))
        origin = started["report_attempt"]
        self.assertNotEqual(origin, first)
        self.assertEqual(started["reporting"], "attempt")
        self.assertIn(" --attempt " + origin, started["report_command"])
        self.assertEqual(self.received(), before, "direct work must not paste a second prompt")
        ack = json.loads(self.as_agent("start", task["ref"], "--json"))
        self.assertEqual((ack["report_attempt"], ack["report_command"]), (origin, started["report_command"]))
        text = self.as_agent("start", task["ref"])
        self.assertIn(started["report_command"], text)

    def test_legacy_add_start_ack_and_unsent_pinned_report_keep_the_legacy_rule(self):
        started = json.loads(self.as_agent("add", "--project", "orchard", "--start", "--json", text="Direct legacy work"))
        self.assertEqual(started["reporting"], "legacy")
        self.assertNotIn("report_attempt", started)
        self.assertEqual(started["report_command"], "hgs task report " + started["task"]["ref"])
        self.assertTrue(started["task"]["execution"]["current"]["legacy"])
        text = self.as_agent("start", started["task"]["ref"])
        self.assertIn("legacy execution", text)
        self.assertIn("no attempt UUID", text)
        self.assertIn(started["report_command"], text)
        ready = json.loads(self.as_agent("report", started["task"]["ref"], "--json", text="Legacy result"))
        self.assertTrue(ready["moved_to_review"])
        queued = json.loads(self.as_agent("add", "--project", "orchard", "--json", text="Never sent, already implemented"))["task"]
        self.assertEqual(queued["stage"], "todo")
        ready = json.loads(self.as_agent("report", queued["ref"], "--json", text="The exact pinned agent reports it"))
        self.assertEqual((ready["moved_to_review"], ready["task"]["stage"]), (True, "review"))
        self.assertEqual(ready["task"].get("execution", {}).get("attempts", 0), 0)

    def test_a_legacy_fallback_acknowledgement_names_its_reporting_boundary(self):
        legacy = self.add("Original legacy work", "--start", session="codex/source")
        held = self.add("Temporarily holds destination input", "--target", self.name)
        self.hgs("send", held["ref"], "--json")
        self.hgs("handoff", legacy["ref"], "--session", self.name, "--json", session="codex/source", ok=False)
        before = self.hgs("get", legacy["ref"])["task"]["execution"]
        current = before["current"]
        self.assertTrue(current["legacy"])
        self.assertEqual(current["executor"]["session"], "codex/source")
        ack = self.hgs("start", legacy["ref"], "--json", session="codex/source")
        self.assertIn("reporting", ack, "successful start needs an explicit reporting/transition outcome")
        self.assertEqual(ack["reporting"], "history_only")
        self.assertNotIn("report_command", ack)
        self.assertNotIn("report_attempt", ack)
        for text in (ack["answer"], ack["report_hint"], self.says("start", legacy["ref"], session="codex/source")):
            self.assertIn("No attempt UUID was created", text)
            self.assertIn("kept as history", text)
            self.assertIn("accept or resend", text)
            self.assertNotIn("hgs task report", text)
        after = self.hgs("get", legacy["ref"])["task"]["execution"]
        self.assertEqual(after["attempts"], before["attempts"], "an acknowledgement must not invent an attempt")
        self.assertTrue(after["current"]["legacy"])
        report = self.hgs("report", legacy["ref"], "--json", text="Original legacy result", session="codex/source")
        self.assertEqual((report["outcome"], report["moved_to_review"], report["stale"]),
                         ("not_applied", False, "other_attempt"))
        self.assertEqual(report["task"]["stage"], "active")
        accepted = self.hgs("report", legacy["ref"], "--json", text="Explicit human acceptance")
        self.assertTrue(accepted["moved_to_review"])

    def test_scope_syntax_refuses_before_history_and_cli_help_has_a_capability_marker(self):
        task, origin, _ = self.managed()
        count = len(self.hgs("get", task["ref"])["task"]["log"])
        for extra in [(), ("--attempt",), ("--attempt", "latest"),
                      ("--attempt", origin, "--attempt", origin), ("--attempt", origin, "--unknown")]:
            self.as_agent("report-attempt", task["ref"], *extra, text="Invalid request", ok=False)
            self.assertEqual(len(self.hgs("get", task["ref"])["task"]["log"]), count)
        help_text = subprocess.check_output([str(HGS), "--help"], env=self.env, text=True)
        self.assertIn("report-attempt-v1", help_text)

    def test_a_reopen_pins_its_new_attempt_before_native_hashing(self):
        task, origin, _ = self.managed()
        self.as_agent("report-attempt", task["ref"], "--attempt", origin, text="Ready for review")
        self.confirm()
        current = self.hgs("get", task["ref"])["task"]
        identity = str(uuid.uuid4())
        reopened = self.hgs("message", task["ref"], "--request-id", identity, "--mode", "reopen",
                            "--expect-dispatch", current["dispatch_id"], "--json", text="A new review request")
        state = reopened["task"]["execution"]["current"]
        self.assertEqual((state["id"], state["phase"]), (identity, "submitted"))
        self.wait_received("hgs task report-attempt " + task["ref"] + " --attempt " + identity)

if __name__ == "__main__":
    unittest.main()
