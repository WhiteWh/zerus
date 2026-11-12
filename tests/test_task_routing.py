#!/usr/bin/env python3
"""Send now and Cancel for a session on another computer of the swarm: that
computer gets the project's tasks, pastes and records, and its copy comes back.
Isolated nodes behind a fake SSH client; node b runs a scripted agent on its
own private tmux socket."""
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

from tests import test_tasks

REPO = Path(__file__).resolve().parents[1]
HGS = Path(os.environ.get("HGS_TEST_BIN", REPO / "target/debug/hgs")).resolve()
TMUX = shutil.which("tmux")
SSH = '''#!/usr/bin/env python3
import json, os, pathlib, shlex, subprocess, sys
root = pathlib.Path(os.environ['ROUTING_ROOT'])
alias = sys.argv[-2]
if (root / ('offline-' + alias)).exists():
    print('ssh: connect to host ' + alias + ': Connection timed out', file=sys.stderr); sys.exit(255)
if sys.argv[-1] == 'echo "hgs-remote-v2|%OS%|%SystemRoot%|$env:OS|$env:SystemRoot"':
    print('hgs-remote-v2|%OS%|%SystemRoot%|:OS|:SystemRoot'); sys.exit(0)
node = root / alias
env = dict(os.environ, HOME=str(node / 'profile'), HGS_CONFIG_DIR=str(node / 'config'),
           HGS_STATE_DIR=str(node / 'state'), TASK_FIXTURE=str(node))
for name in ('HGS_SESSION', 'HGS_RUN_ID'):
    env.pop(name, None)  # an SSH login is no agent
args = shlex.split(sys.argv[-1])[1:]
with (root / 'calls').open('a') as file: file.write(alias + ' ' + ' '.join(args) + '\\n')
if (root / ('oldcancel-' + alias)).exists() and args[:2] == ['task', 'cancel-execution']:
    print('hgs: usage: hgs task cancel REF', file=sys.stderr); sys.exit(1)
if (root / ('dropafter-' + alias)).exists() and args[:2] in (['task', 'send'], ['task', 'cancel-execution'], ['task', 'start-deliver']):
    # The connection drops after that computer pasted and answered.
    subprocess.run([os.environ['ROUTING_HGS'], *args], input=sys.stdin.read(), env=env, text=True, capture_output=True)
    print('Connection reset by peer', file=sys.stderr); sys.exit(255)
if (root / ('failexchange-' + alias)).exists() and args[1:2] in (['replica-inventory'], ['replica-exchange']):
    print('Connection reset by peer', file=sys.stderr); sys.exit(255)
if (root / ('noreplica-' + alias)).exists():
    # hgs from before task replication: no replica field in hello.
    result = subprocess.run([os.environ['ROUTING_HGS'], *args], input=sys.stdin.read(), env=env, text=True, capture_output=True)
    sys.stderr.write(result.stderr)
    if result.returncode: sys.exit(result.returncode)
    output = json.loads(result.stdout); output.pop('replica', None); print(json.dumps(output)); sys.exit(0)
os.execve(os.environ['ROUTING_HGS'], [os.environ['ROUTING_HGS'], *args], env)
'''


@unittest.skipUnless(TMUX, "tmux is required for delivery")
class TaskRouting(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="hgs-routing-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        ssh = self.root / "ssh"; ssh.write_text(SSH); ssh.chmod(0o755)
        self.base = dict(os.environ, ROUTING_ROOT=str(self.root), ROUTING_HGS=str(HGS), HGS_SSH=str(ssh), HGS_TAB="0")
        for name in ("TMUX", "TMUX_PANE", "HGS_RUN_ID", "HGS_SESSION", "HGS_EXECUTABLE", "HGS_SELF", "HGS_PEERS"):
            self.base.pop(name, None)
        for node, peer in (("a", "b"), ("b", "a")):
            home = self.root / node / "profile/.local/bin"; home.mkdir(parents=True)
            (self.root / node / "state").mkdir()
            socket = self.root / node / "socket"
            # hgs puts $HOME/.local/bin first on PATH: each node's tmux stays private.
            shim = home / "tmux"; shim.write_text(f'#!/bin/sh\nexec {TMUX} -S "{socket}" -f /dev/null "$@"\n'); shim.chmod(0o755)
            self.addCleanup(lambda socket=socket: subprocess.run([TMUX, "-S", str(socket), "kill-server"], capture_output=True))
            config = self.root / node / "config"; config.mkdir()
            (config / "config").write_text(f'HGS_SELF="{node}"\nHGS_PEERS="{peer}"\n')
            (config / "machines.local.json").write_text(json.dumps({"machines": {peer: dict(hostname="host.invalid", enabled=True)}}))
        projects = [{"id": "project-1", "name": "orchard", "color": "#123456", "folders": [], "sessions": []}]
        self.swarm("a", "initialize", data={"projects": projects})
        self.swarm("b", "initialize", data={"projects": []})
        for node, peer in (("b", "a"), ("a", "b")):
            preview = self.swarm(node, "preview", peer)
            self.swarm(node, "join", peer, data=dict(node_id=preview["node_id"], swarm_id=preview["swarm_id"], project_map={}, prefer_peer_memberships=False))
        self.start_agent()

    def env(self, node, session=None):
        env = dict(self.base, HOME=str(self.root / node / "profile"), HGS_CONFIG_DIR=str(self.root / node / "config"),
                   HGS_STATE_DIR=str(self.root / node / "state"), TASK_FIXTURE=str(self.root / node))
        if session:
            # An agent of this computer: a run this test process owns.
            run = "run-" + session.replace("/", "-")
            test_tasks.agent_record(self.root / node / "state", env, session, run)
            env.update(HGS_SESSION=session, HGS_RUN_ID=run)
        return env

    def swarm(self, node, *args, data=None):
        result = subprocess.run([str(HGS), "swarm", *args], input=json.dumps(data) if data is not None else None,
                                env=self.env(node), text=True, capture_output=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def task(self, node, *args, text=None, ok=True, session=None):
        result = subprocess.run([str(HGS), "task", *args], input=text, env=self.env(node, session), text=True,
                                capture_output=True, timeout=60)
        if ok:
            self.assertEqual(result.returncode, 0, result.stderr)
            return json.loads(result.stdout)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        return result.stderr

    def tmux(self, *args):
        result = subprocess.run([str(self.root / "b/profile/.local/bin/tmux"), *args], env=self.env("b"),
                                capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def start_agent(self):
        """codex/remote on b: a scripted agent that records what it receives."""
        node = self.root / "b"
        (node / "agent.py").write_text(test_tasks.AGENT)
        self.tmux("new-session", "-d", "-s", "codex/remote", "-x", "100", "-y", "30", "python3", str(node / "agent.py"))
        for _ in range(200):
            if (node / "agent-pid").exists() and (node / "agent-pid").read_text().strip():
                break
            time.sleep(.02)
        pid = int((node / "agent-pid").read_text())
        start = subprocess.check_output(["ps", "-p", str(pid), "-o", "lstart="], env=dict(os.environ, LC_ALL="C", TZ="UTC"), text=True).strip()
        pane = self.tmux("display-message", "-p", "-t", "=codex/remote:", "#{pane_id}").strip()
        run_id = str(uuid.uuid4())
        self.tmux("set-option", "-t", "=codex/remote:", "@hgs_run", run_id)
        record = node / "state" / (hashlib.sha256(b"codex/remote").hexdigest() + ".json")
        record.write_text(json.dumps(dict(
            version=1, name="codex/remote", agent="codex", run_id=run_id, conversation_id=str(uuid.uuid4()),
            pane=pane, pid=pid, process_start=start, activity="idle", phase="idle", active_tools={},
            subagents={}, last_event_at=time.time(), created=time.time())))
        self.record = record

    def confirm(self):
        # The agent's next hook event confirms the pasted input.
        record = json.loads(self.record.read_text())
        record["last_event_at"] = record.get("input_pending_at", time.time()) + 1
        # An input-acceptance event, as UserPromptSubmit records it (zerus-iitw8).
        record["input_seen_at"] = record["last_event_at"]
        self.record.write_text(json.dumps(record))

    def received(self):
        path = self.root / "b/received"
        return path.read_bytes().decode(errors="replace") if path.exists() else ""

    def wait_received(self, text):
        for _ in range(250):
            if text in self.received():
                return
            time.sleep(.02)
        self.fail("the agent on b never received " + repr(text) + "; got " + repr(self.received()))

    def add(self, text, *args):
        return self.task("a", "add", "--project", "project-1", *args, text=text)["task"]

    def get(self, node, task):
        return self.task(node, "get", task["id"], "--project", "project-1")["task"]

    def test_send_now_runs_on_the_sessions_computer(self):
        task = self.add("Add rate limiting", "--target", "codex/remote", "--host", "b")
        sent = self.task("a", "send", task["id"], "--project", "project-1")
        self.assertEqual(sent["receipt"]["status"], "submitted")
        self.assertTrue(sent["synced"])
        self.wait_received("[Zerus task orchard/" + task["id"] + "]")
        self.wait_received("Add rate limiting")
        # Both copies show it In progress in b's session, sent by the person.
        here, there = sent["task"], self.get("b", task)
        self.assertEqual((here["stage"], here["session"]["session"], here["session"]["host"]), ("active", "codex/remote", "b"))
        self.assertEqual((there["stage"], there["session"]["host"]), ("active", ""))
        entry = next(e for e in there["log"] if e["kind"] == "sent")
        self.assertEqual(entry["actor"]["kind"], "person")
        # C3 activation: b has not seen a's replica yet, so its bootstrap
        # gate is positively open: the legacy route, no floor, no attempt.
        execution = there["execution"]
        self.assertEqual((execution["attempts"], execution["current"]["legacy"]), (0, True), execution)
        calls = (self.root / "calls").read_text()
        self.assertIn("b task send " + task["uuid"] + " --project project-1", calls)

    def test_a_send_to_a_computer_that_has_the_task_takes_four_calls(self):
        task = self.add("Add rate limiting", "--target", "codex/remote", "--host", "b")
        self.swarm("a", "sync")
        (self.root / "calls").write_text("")
        self.task("a", "send", task["id"], "--project", "project-1")
        self.wait_received("Add rate limiting")
        calls = [line.split()[1:3] for line in (self.root / "calls").read_text().splitlines()]
        self.assertEqual(calls, [["swarm", "hello"], ["task", "send"], ["swarm", "replica-inventory"], ["swarm", "replica-exchange"]])

    def test_this_computers_copy_refuses_first(self):
        task = self.add("Add rate limiting", "--target", "codex/remote", "--host", "b")
        self.task("a", "send", task["id"], "--project", "project-1")
        self.wait_received("Add rate limiting")
        self.confirm()
        self.task("a", "cancel", task["id"], "--project", "project-1")
        # a's copy came back In review: neither a send nor a cancel asks b now.
        (self.root / "calls").write_text("")
        self.assertIn("a task in In review is not sent", self.task("a", "send", task["id"], "--project", "project-1", ok=False))
        self.assertIn("only a task In progress can be cancelled", self.task("a", "cancel", task["id"], "--project", "project-1", ok=False))
        self.assertEqual((self.root / "calls").read_text(), "")

    def test_a_send_whose_copy_cannot_come_back_says_so(self):
        task = self.add("Add rate limiting", "--target", "codex/remote", "--host", "b")
        self.swarm("a", "sync")
        # The connection drops after b pasted and recorded the send.
        (self.root / "failexchange-b").touch()
        sent = self.task("a", "send", task["id"], "--project", "project-1")
        self.wait_received("Add rate limiting")
        self.assertFalse(sent["synced"])
        self.assertEqual(sent["task"]["stage"], "todo")
        # b's record comes with the next sync; a second send is refused here then.
        (self.root / "failexchange-b").unlink()
        self.swarm("a", "sync")
        self.assertEqual(self.get("a", task)["stage"], "active")
        self.assertIn("already In progress in codex/remote", self.task("a", "send", task["id"], "--project", "project-1", ok=False))
        self.assertEqual(self.received().count("Add rate limiting"), 1)

    def test_an_any_session_task_goes_to_the_session_named_there(self):
        task = self.add("Document the setup")
        self.task("a", "send", task["id"], "--project", "project-1", "--session", "codex/remote", "--host", "b")
        self.wait_received("Document the setup")
        view = self.get("a", task)
        self.assertEqual((view["stage"], view["session"]["host"], view["target"]["kind"]), ("active", "b", "any"))

    def test_cancel_runs_on_the_sessions_computer(self):
        task = self.add("Add rate limiting", "--target", "codex/remote", "--host", "b")
        self.task("a", "send", task["id"], "--project", "project-1")
        self.wait_received("Add rate limiting")
        self.confirm()
        cancelled = self.task("a", "cancel", task["id"], "--project", "project-1", "--reason", "Wrong approach")
        self.assertTrue(cancelled["cancel_message"]["sent"], cancelled)
        self.wait_received("[Zerus task orchard/" + task["id"] + " cancelled]")
        for node in "ab":
            view = self.get(node, task)
            self.assertEqual(view["stage"], "review", node)
            self.assertIn("Wrong approach", [e.get("text") for e in view["log"]], node)

    def test_start_plan_and_readonly_recovery_route_the_original_identity(self):
        task = self.add("A routed guarded start", "--target", "codex/remote", "--host", "b")
        current = self.get("a", task)
        record = json.loads(self.record.read_text())
        request = dict(schema=1, id=str(uuid.uuid4()), stage_at=current["stage_at"], target=current["target"],
                       selection=dict(host="b", name="codex/remote", session_id="run_id:" + record["run_id"],
                                      run_id=record["run_id"], conversation_id=record["conversation_id"]))
        plan = self.task("a", "start-plan", task["id"], "--project", "project-1", text=json.dumps(request))["plan"]
        self.assertEqual(plan["selection"]["host"], "")
        self.assertEqual(self.received(), "")
        (self.root / "dropafter-b").touch()
        self.assertIn("Connection reset", self.task("a", "start-deliver", task["id"], "--project", "project-1", text=json.dumps(plan), ok=False))
        self.wait_received("A routed guarded start")
        (self.root / "dropafter-b").unlink()
        settled = self.task("a", "start-deliver", task["id"], "--project", "project-1", "--recover-only", text=json.dumps(plan))
        self.assertEqual((settled["state"], settled["id"]), ("submitted", request["id"]))
        self.assertEqual(self.received().count("A routed guarded start"), 1)
        self.assertEqual(self.get("a", task)["stage"], "active")

    def test_guarded_cancellation_routes_exact_payload_and_refuses_unknown_replay(self):
        task = self.add("Check pinned cancellation", "--target", "codex/remote", "--host", "b")
        self.task("a", "send", task["id"], "--project", "project-1")
        self.wait_received("Check pinned cancellation")
        self.confirm()
        sent = self.get("a", task)
        request = dict(schema=1, dispatch_id=sent["dispatch_id"], stage_at=sent["stage_at"], machine=sent["session"]["machine"],
                       session_id=sent["session"]["session_id"], reason="--agent")
        (self.root / "oldcancel-b").touch()
        self.assertIn("usage", self.task("a", "cancel-execution", task["id"], "--project", "project-1", text=json.dumps(request), ok=False))
        self.assertEqual(self.get("a", task)["stage"], "active")
        self.assertNotIn("cancelled]", self.received())
        (self.root / "oldcancel-b").unlink()
        (self.root / "dropafter-b").touch()
        error = self.task("a", "cancel-execution", task["id"], "--project", "project-1", text=json.dumps(request), ok=False)
        self.assertIn("Connection reset", error)
        self.wait_received(" cancelled]")
        self.assertEqual(self.get("b", task)["stage"], "review")
        (self.root / "dropafter-b").unlink()
        self.assertIn("changed", self.task("a", "cancel-execution", task["id"], "--project", "project-1", text=json.dumps(request), ok=False))
        self.assertEqual(self.received().count(" cancelled]"), 1)
        self.assertNotIn("--agent", self.received())
        self.assertIn("--agent", [e.get("text") for e in self.get("b", task)["log"]])

    def test_guarded_cancellation_offline_fallback_checks_same_execution(self):
        task = self.add("Offline cancellation", "--target", "codex/remote", "--host", "b")
        self.task("a", "send", task["id"], "--project", "project-1")
        self.wait_received("Offline cancellation")
        sent = self.get("a", task)
        request = dict(schema=1, dispatch_id=sent["dispatch_id"], stage_at=sent["stage_at"], machine=sent["session"]["machine"],
                       session_id=sent["session"]["session_id"], reason="No longer needed")
        (self.root / "offline-b").touch()
        out = self.task("a", "cancel-execution", task["id"], "--project", "project-1", text=json.dumps(request))
        self.assertEqual(out["task"]["stage"], "review")
        self.assertNotIn("log", out["task"])
        self.assertFalse(out["cancel_message"]["sent"])
        self.assertNotIn("cancelled]", self.received())

    def test_an_unreachable_computer_refuses_a_send_and_records_nothing(self):
        task = self.add("Add rate limiting", "--target", "codex/remote", "--host", "b")
        (self.root / "offline-b").touch()
        refused = self.task("a", "send", task["id"], "--project", "project-1", ok=False)
        self.assertIn("b cannot be reached", refused)
        self.assertEqual(self.get("a", task)["stage"], "todo")

    def test_a_cancel_goes_through_while_its_computer_is_out_of_reach(self):
        task = self.add("Add rate limiting", "--target", "codex/remote", "--host", "b")
        self.task("a", "send", task["id"], "--project", "project-1")
        self.wait_received("Add rate limiting")
        # b is offline: the task moves to In review here and its session is not told.
        (self.root / "offline-b").touch()
        cancelled = self.task("a", "cancel", task["id"], "--project", "project-1", "--reason", "Wrong approach")
        self.assertFalse(cancelled["cancel_message"]["sent"])
        self.assertIn("b cannot be reached", cancelled["cancel_message"]["error"])
        view = self.get("a", task)
        self.assertEqual(view["stage"], "review")
        texts = [e.get("text", "") for e in view["log"]]
        self.assertIn("Wrong approach", texts)
        self.assertTrue(any(text.startswith("Cancel message not sent: b cannot be reached") for text in texts), texts)
        self.assertNotIn("cancelled]", self.received())
        # Back online, b's copy follows.
        (self.root / "offline-b").unlink()
        self.swarm("a", "sync")
        self.assertEqual(self.get("b", task)["stage"], "review")

    def test_a_cancel_goes_through_for_a_computer_that_left_the_swarm(self):
        task = self.add("Add rate limiting", "--target", "codex/remote", "--host", "b")
        self.task("a", "send", task["id"], "--project", "project-1")
        self.wait_received("Add rate limiting")
        self.swarm("a", "disconnect", "b")
        cancelled = self.task("a", "cancel", task["id"], "--project", "project-1")
        self.assertIn("b is not a member of this swarm", cancelled["cancel_message"]["error"])
        self.assertEqual(self.get("a", task)["stage"], "review")

    def test_a_task_its_computer_shows_in_progress_is_refused(self):
        task = self.add("Add rate limiting", "--target", "codex/remote", "--host", "b")
        self.swarm("b", "sync")
        # b's person sends it there; a's copy has not heard of that yet.
        self.task("b", "send", task["id"], "--project", "project-1")
        self.wait_received("Add rate limiting")
        self.assertEqual(self.get("a", task)["stage"], "todo")
        refused = self.task("a", "send", task["id"], "--project", "project-1", ok=False)
        self.assertIn("already In progress in codex/remote", refused)
        self.assertEqual(self.received().count("Add rate limiting"), 1)

    def test_an_agents_send_that_computer_refuses_says_nothing_was_sent(self):
        task = self.add("Add rate limiting", "--target", "codex/remote", "--host", "b")
        self.swarm("b", "sync")
        self.task("b", "send", task["id"], "--project", "project-1")
        self.wait_received("Add rate limiting")
        refused = self.task("a", "send", task["id"], "--project", "project-1", ok=False, session="codex/local")
        self.assertIn("codex/remote on b cannot take it now: ", refused)
        self.assertIn("already In progress in codex/remote. Nothing was sent.", refused)
        self.assertEqual(refused.count("cannot take it now"), 1, refused)

    def test_a_send_whose_connection_drops_after_the_paste_never_says_nothing_was_sent(self):
        task = self.add("Add rate limiting", "--target", "codex/remote", "--host", "b")
        (self.root / "dropafter-b").touch()
        failed = self.task("a", "send", task["id"], "--project", "project-1", ok=False, session="codex/local")
        self.wait_received("Add rate limiting")
        self.assertIn("Connection reset by peer", failed)
        self.assertNotIn("Nothing was sent", failed)

    def test_an_agent_sends_as_itself(self):
        task = self.add("Add rate limiting", "--target", "codex/remote", "--host", "b")
        self.task("a", "send", task["id"], "--project", "project-1", "--json", session="codex/local")
        self.wait_received("Add rate limiting")
        entry = next(e for e in self.get("b", task)["log"] if e["kind"] == "sent")
        self.assertEqual((entry["actor"]["kind"], entry["actor"]["session"], entry["actor"]["host"]), ("agent", "codex/local", "a"))

    def test_an_agents_send_answers_in_one_line(self):
        task = self.add("Add rate limiting", "--target", "codex/remote", "--host", "b")
        result = subprocess.run([str(HGS), "task", "send", task["id"], "--project", "project-1"],
                                env=self.env("a", "codex/local"), text=True, capture_output=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "orchard/" + task["id"] + " sent to codex/remote on b.")

    def test_an_agents_cancel_whose_copy_cannot_come_back_says_so(self):
        task = self.add("Add rate limiting", "--target", "codex/remote", "--host", "b")
        self.task("a", "send", task["id"], "--project", "project-1")
        self.wait_received("Add rate limiting")
        self.confirm()
        (self.root / "failexchange-b").touch()
        result = subprocess.run([str(HGS), "task", "cancel", task["id"], "--project", "project-1"],
                                env=self.env("a", "codex/local"), text=True, capture_output=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "orchard/" + task["id"] + " is In review; codex/remote on b got the cancel message."
                         " This computer's copy follows with the next sync.")

    def test_a_computer_without_task_replication_is_named(self):
        task = self.add("Add rate limiting", "--target", "codex/remote", "--host", "b")
        (self.root / "noreplica-b").touch()
        refused = self.task("a", "send", task["id"], "--project", "project-1", ok=False)
        self.assertIn("update Zerus on b", refused)
        self.assertEqual(self.get("a", task)["stage"], "todo")


if __name__ == "__main__":
    unittest.main()
