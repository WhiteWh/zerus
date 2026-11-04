#!/usr/bin/env python3
"""The Windows session host (spec D1-D6, D9, D13) through `hgs.exe __host`.

Windows only. HGS_TEST_BIN names the hgs.exe under test (default
target/debug/hgs.exe); the fake agent cases also need
target/debug/examples/fake_agent.exe (`cargo build --locked --example
fake_agent`; HGS_TEST_FAKE_AGENT names another build).

Every case has its own HGS_STATE_DIR, USERPROFILE and HOME, so it reaches only
a host it started itself and stages nothing into the real profile. A host runs
only once local sessions are enabled and only from the staged image (spec D2,
src/host/gate.rs); the cases run the build under test instead, as an isolated
test host: HGS_HOST_IMAGE names it and HGS_HOST_ALLOW_UNSTAGED=1 lets it serve.
`Staged` drops both and enables local sessions in its temporary profile with
`hgs session-host install`, as a user would. Every case stops what it started: its sessions are killed,
the host is asked to stop, every host process the case saw is waited for (its
pipe goes before the process does, and the process holds files in the state
directory) and, as a last resort, ended through a handle opened while it was
known to run (the host's kill-on-close job then ends anything left). Consoles
stay hidden.
"""
import ctypes
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest

REPO = Path(__file__).resolve().parents[1]
HGS = Path(os.environ.get("HGS_TEST_BIN", REPO / "target/debug/hgs.exe")).resolve()
FAKE_AGENT = Path(os.environ.get("HGS_TEST_FAKE_AGENT",
                                 REPO / "target/debug/examples/fake_agent.exe")).resolve()
HIDDEN = getattr(subprocess, "CREATE_NO_WINDOW", 0)
# The host's linger (registry::LINGER) plus margin.
IDLE_EXIT = 30
LIMIT = 30

if os.name == "nt":
    from ctypes import wintypes

    KERNEL32 = ctypes.WinDLL("kernel32", use_last_error=True)
    KERNEL32.OpenProcess.restype = wintypes.HANDLE
    KERNEL32.OpenProcess.argtypes = (wintypes.DWORD, wintypes.BOOL, wintypes.DWORD)
    KERNEL32.GetProcessTimes.argtypes = (wintypes.HANDLE,) + (ctypes.POINTER(wintypes.FILETIME),) * 4
    KERNEL32.WaitForSingleObject.argtypes = (wintypes.HANDLE, wintypes.DWORD)
    KERNEL32.TerminateProcess.argtypes = (wintypes.HANDLE, wintypes.UINT)
    KERNEL32.CloseHandle.argtypes = (wintypes.HANDLE,)
    SYNCHRONIZE = 0x00100000
    PROCESS_TERMINATE = 0x0001
    PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
    WAIT_OBJECT_0 = 0


def open_host(pid, created):
    """A handle on host process `pid` when its creation time is `created` (100 ns
    ticks, as the hello reports it), else None. The handle pins the process ID."""
    handle = KERNEL32.OpenProcess(SYNCHRONIZE | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not handle:
        return None
    times = [wintypes.FILETIME() for _ in range(4)]
    if KERNEL32.GetProcessTimes(handle, *(ctypes.byref(time) for time in times)):
        ticks = (times[0].dwHighDateTime << 32) | times[0].dwLowDateTime
        if str(ticks) == created:
            return handle
    KERNEL32.CloseHandle(handle)
    return None


@unittest.skipUnless(os.name == "nt", "the session host runs on Windows only")
class HostCase(unittest.TestCase):
    # The temporary profile's folder name, below the case's temporary folder.
    PROFILE = "home"

    def setUp(self):
        if not HGS.is_file():
            raise AssertionError(f"{HGS} is missing; run cargo build --locked")
        self.temp = tempfile.TemporaryDirectory(prefix="hgs-host-")
        self.addCleanup(self.cleanup_temp)
        self.root = Path(self.temp.name)
        self.state = self.root / "state"
        self.home = self.root / self.PROFILE
        self.work = self.root / "work"
        for path in (self.state, self.home, self.work):
            path.mkdir()
        self.env = {key: value for key, value in os.environ.items()
                    if not key.upper().startswith(("HGS_", "FAKE_AGENT_", "TMUX", "XDG_"))}
        self.env.update(HGS_STATE_DIR=str(self.state), USERPROFILE=str(self.home), HOME=str(self.home),
                        # An isolated test host of the build under test (src/host/gate.rs).
                        HGS_HOST_IMAGE=str(HGS), HGS_HOST_ALLOW_UNSTAGED="1")
        self.unique = f"hgs-host-{os.getpid()}-{time.monotonic_ns()}"
        self.launches = 0
        # Host processes this case saw running, by process ID.
        self.hosts = {}
        self.addCleanup(self.stop_host)

    def cleanup_temp(self):
        self.temp.cleanup()

    # The host and its tools.

    def run_hgs(self, *args, timeout=60):
        return subprocess.run([str(HGS), "__host", *args], env=self.env, capture_output=True, text=True,
                              timeout=timeout, creationflags=HIDDEN)

    def call(self, request, start=False):
        """The exit code and the printed response of one request."""
        args = ["call", "--start"] if start else ["call"]
        result = self.run_hgs(*args, json.dumps(request))
        response = json.loads(result.stdout) if result.stdout.strip() else None
        return result.returncode, response, result.stderr

    def ok(self, request, start=False):
        code, response, stderr = self.call(request, start)
        self.assertEqual(code, 0, f"{request}: {response} {stderr}")
        return response["ok"]

    def error(self, request):
        code, response, stderr = self.call(request)
        self.assertEqual(code, 1, f"{request}: {response} {stderr}")
        return response["error"]

    def status(self):
        result = self.run_hgs("status")
        self.assertEqual(result.returncode, 0, result.stderr)
        status = json.loads(result.stdout)
        if status.get("running"):
            host = status["host"]
            if host["pid"] not in self.hosts:
                handle = open_host(host["pid"], host["created"])
                if handle:
                    self.hosts[host["pid"]] = handle
        return status

    def wait_for(self, what, predicate, timeout=LIMIT):
        deadline = time.monotonic() + timeout
        while True:
            value = predicate()
            if value:
                return value
            if time.monotonic() > deadline:
                self.fail(f"timed out waiting for {what}")
            time.sleep(0.1)

    def stop_host(self):
        try:
            self.stop_sessions()
        finally:
            self.wait_for_hosts()

    def stop_sessions(self):
        status = self.status()
        if not status.get("running"):
            return
        for pane in self.ok({"op": "list"})["panes"]:
            self.call({"op": "kill", "target": {"pane": pane["pane"]}})
        deadline = time.monotonic() + LIMIT
        while time.monotonic() < deadline and self.ok({"op": "list"})["sessions"]:
            time.sleep(0.2)
        self.call({"op": "stop_host"})
        deadline = time.monotonic() + LIMIT
        while time.monotonic() < deadline and self.status().get("running"):
            time.sleep(0.2)

    def wait_for_hosts(self):
        """Wait until every host process this case saw has exited; end those
        that do not."""
        for handle in self.hosts.values():
            if KERNEL32.WaitForSingleObject(handle, LIMIT * 1000) != WAIT_OBJECT_0:
                KERNEL32.TerminateProcess(handle, 1)
                KERNEL32.WaitForSingleObject(handle, LIMIT * 1000)
            KERNEL32.CloseHandle(handle)
        self.hosts.clear()

    # Sessions.

    def create_request(self, name, argv, launch_id=None, size=(100, 30)):
        if launch_id is None:
            self.launches += 1
            launch_id = f"{self.unique}-{self.launches}"
        return {
            "op": "create_session",
            "launch_id": launch_id,
            "name": name,
            "cwd": str(self.work),
            "env": [f"{key}={value}" for key, value in self.env.items()],
            "argv": [str(word) for word in argv],
            "size": {"cols": size[0], "rows": size[1]},
        }

    def create(self, name, argv, launch_id=None, size=(100, 30), start=True):
        return self.ok(self.create_request(name, argv, launch_id, size), start=start)

    def cmd_argv(self, prompt):
        comspec = Path(os.environ["SystemRoot"]) / "System32" / "cmd.exe"
        return [comspec, "/d", "/q", "/k", f"prompt {prompt}$G"]

    def cmd(self, name):
        """A cmd.exe session with a prompt of its own."""
        prompt = f"{self.unique}-{name}"
        created = self.create(name, self.cmd_argv(prompt))
        self.wait_for("the prompt", lambda: prompt + ">" in self.capture(created["pane"]))
        return created, prompt

    def capture(self, pane, styled=False):
        return self.ok({"op": "capture", "target": {"pane": pane}, "styled": styled})["text"]

    def info(self, pane):
        return self.ok({"op": "pane_info", "target": {"pane": pane}})

    def keys(self, pane, *keys):
        self.ok({"op": "send_keys", "target": {"pane": pane}, "keys": list(keys)})

    def attach(self, request, steps):
        result = self.run_hgs("attach", json.dumps(request), json.dumps(steps))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return json.loads(result.stdout)


class Lifecycle(HostCase):
    def test_a_started_host_runs_cmd_sessions_and_exits_once_idle(self):
        # A listing never starts a host.
        code, response, stderr = self.call({"op": "list"})
        self.assertEqual((code, response), (3, None))
        self.assertIn("no session host runs", stderr)
        self.assertEqual(self.status(), {"running": False})

        created, prompt = self.cmd("cmd/a")
        self.assertEqual((created["session"][0], created["pane"][0], created["existing"]), ("$", "%", False))
        status = self.status()
        host = status["host"]
        self.assertTrue(status["running"])
        self.assertFalse(host["in_job"])
        self.assertIn(status["status"]["conpty"], ("bundled", "inbox"))
        self.assertEqual(status["status"]["sessions"], 1)
        pane = created["pane"]

        marker = f"{self.unique}-echo"
        self.keys(pane, {"literal": f"echo {marker}"}, {"named": "Enter"})
        self.wait_for("the echo", lambda: marker in self.capture(pane).splitlines())
        pasted = f"{self.unique}-paste"
        # cmd.exe never enables bracketed paste, so the text arrives plain.
        self.ok({"op": "paste", "target": {"pane": pane}, "text": f"echo {pasted}", "bracketed": True})
        self.keys(pane, {"named": "Enter"})
        self.wait_for("the paste", lambda: pasted in self.capture(pane).splitlines())
        self.assertIn(prompt, self.capture(pane, styled=True))

        details = self.info(pane)
        self.assertEqual((details["width"], details["height"]), (100, 30))
        self.assertEqual(details["pane_pid"], created["pid"])
        self.assertEqual(details["server_pid"], host["pid"])
        self.assertEqual(details["session_name"], "cmd/a")
        self.assertFalse(details["dead"] or details["in_mode"] or details["bracketed_paste"])
        self.assertEqual(details["current_command"], "cmd")

        self.ok({"op": "set_environment", "target": {"name": "cmd/a"}, "name": "@hgs_run", "value": "run-1"})
        listed = self.ok({"op": "list"})
        self.assertEqual([session["name"] for session in listed["sessions"]], ["cmd/a"])
        self.assertEqual(listed["panes"][0]["run_marker"], "run-1")
        self.assertEqual(listed["panes"][0]["pid"], created["pid"])
        self.assertEqual(listed["panes"][0]["current_path"], str(self.work))
        for target in ({"name": "cmd/a"}, {"session": created["session"]}, {"pane": pane}):
            resolved = self.ok({"op": "resolve_target", "target": target})
            self.assertEqual(resolved, {"session": created["session"], "name": "cmd/a", "pane": pane})

        self.ok({"op": "kill", "target": {"pane": pane}})
        self.wait_for("the end", lambda: not self.ok({"op": "list"})["sessions"])
        # The idle host lingers, closes its pipe and exits; nothing starts it again.
        self.wait_for("the idle exit", lambda: not self.status()["running"], timeout=IDLE_EXIT)
        code, _, stderr = self.call({"op": "list"})
        self.assertEqual(code, 3, stderr)

    def test_a_retried_create_starts_nothing_and_names_stay_unique(self):
        created, _ = self.cmd("cmd/once")
        launch_id = f"{self.unique}-1"
        comspec = Path(os.environ["SystemRoot"]) / "System32" / "cmd.exe"
        again = self.create("cmd/once", [comspec, "/d", "/q", "/k"], launch_id=launch_id)
        self.assertEqual((again["session"], again["pane"], again["pid"], again["existing"]),
                         (created["session"], created["pane"], created["pid"], True))
        self.assertEqual(len(self.ok({"op": "list"})["sessions"]), 1)
        code, response, _ = self.call({
            "op": "create_session", "launch_id": f"{self.unique}-other", "name": "cmd/once",
            "cwd": str(self.work), "env": [f"{key}={value}" for key, value in self.env.items()],
            "argv": [str(comspec)], "size": {"cols": 80, "rows": 24}})
        self.assertEqual((code, response["error"]["code"]), (1, "exists"))

    def test_a_launch_reports_how_its_session_ended(self):
        # What a launch reads to report a program that could not start
        # (zerus-ziq3.10): the exit and the last lines of the pane, a long line
        # whole although it wrapped.
        launch_id = f"{self.unique}-ends"
        created, _ = self.cmd("cmd/ends")
        self.assertEqual(self.ok({"op": "launch_status", "launch_id": f"{self.unique}-1"}),
                         {"session": created["session"], "pane": created["pane"], "name": "cmd/ends",
                          "dead": False, "ended": False, "code": None, "output": ""})
        said = "cannot start: " + "x" * 120
        comspec = Path(os.environ["SystemRoot"]) / "System32" / "cmd.exe"
        quick = self.create("cmd/quick", [comspec, "/d", "/q", "/c", f"echo {said}& exit 3"],
                            launch_id=launch_id)
        state = self.wait_for("the end", lambda: (lambda state: state["ended"] and state)(
            self.ok({"op": "launch_status", "launch_id": launch_id})))
        self.assertEqual({key: state[key] for key in ("session", "pane", "name", "dead", "code")},
                         {"session": quick["session"], "pane": quick["pane"], "name": "cmd/quick", "dead": True,
                          "code": 3})
        self.assertEqual(state["output"].splitlines()[-1], said, state)
        self.assertIn("ended: exit 3 ", (self.state / "host.log").read_text(errors="replace"))
        self.assertEqual(self.error({"op": "launch_status", "launch_id": f"{self.unique}-none"})["code"],
                         "not_found")

    def test_a_second_host_is_refused_by_the_lock(self):
        self.cmd("cmd/lock")
        first = self.status()["host"]["pid"]
        result = self.run_hgs("serve", timeout=LIMIT)
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertEqual(self.status()["host"]["pid"], first)
        log = (self.state / "host.log").read_text(errors="replace")
        self.assertIn("another session host holds the lock", log)

    def test_attach_repaints_follows_the_client_size_and_detaches(self):
        created, prompt = self.cmd("cmd/attach")
        pane = created["pane"]
        attached = self.attach(
            {"target": {"pane": pane}, "size": {"cols": 90, "rows": 25}, "run_id": None},
            [{"wait_ms": 500}, {"resize": {"cols": 120, "rows": 40}}, {"wait_ms": 500},
             {"send": "echo attached-input\r"}, {"wait_ms": 500}, {"detach": True}])
        self.assertEqual(attached["attached"]["pane"], pane)
        self.assertTrue(attached["sent"])
        output = bytes.fromhex(attached["output_hex"]).decode("utf-8", "replace")
        self.assertIn(prompt, output, "the repaint shows the screen")
        self.wait_for("the input", lambda: "attached-input" in self.capture(pane).splitlines())
        # With no client left the last size stays.
        details = self.info(pane)
        self.assertEqual((details["width"], details["height"]), (120, 40))
        self.assertEqual(self.ok({"op": "list_clients", "target": None})["clients"], [])

    def test_only_an_unviewed_pane_grows_until_a_client_attaches(self):
        created, prompt = self.cmd("cmd/grow")
        pane = created["pane"]
        grow = lambda rows: self.ok({"op": "grow_unviewed", "target": {"pane": pane}, "rows": rows})
        self.assertEqual(grow(60), {"grown": True})
        details = self.info(pane)
        self.assertEqual((details["width"], details["height"]), (100, 60))
        # It never shrinks, and the console still runs at the new height.
        self.assertEqual(grow(40), {"grown": False})
        self.keys(pane, {"literal": "echo grown-pane"}, {"named": "Enter"})
        self.wait_for("the echo", lambda: "grown-pane" in self.capture(pane).splitlines())
        # The next client sets the size again, and it stays once it leaves.
        self.attach({"target": {"pane": pane}, "size": {"cols": 90, "rows": 25}, "run_id": None},
                    [{"wait_ms": 500}, {"detach": True}])
        details = self.info(pane)
        self.assertEqual((details["width"], details["height"]), (90, 25))
        self.assertEqual(grow(50), {"grown": True})
        self.assertEqual((self.info(pane)["width"], self.info(pane)["height"]), (90, 50))


class Staged(HostCase):
    """`hgs session-host` and hosts started from the staged image (spec D2), in
    the case's temporary profile, without the test host variables."""

    def setUp(self):
        super().setUp()
        for key in ("HGS_HOST_IMAGE", "HGS_HOST_ALLOW_UNSTAGED"):
            del self.env[key]
        self.cli = self.home / ".local" / "bin" / "hgs.exe"
        self.images = self.home / ".local" / "share" / "hgs" / "host"
        self.marker = self.home / ".config" / "hgs" / "local-sessions"

    def session_host(self, *args, hgs=HGS, env=None):
        """The exit code, the printed JSON and the error output of one command."""
        result = subprocess.run([str(hgs), "session-host", *args, "--json"], env=env or self.env,
                                capture_output=True, text=True, timeout=LIMIT, creationflags=HIDDEN)
        report = json.loads(result.stdout) if result.stdout.strip() else None
        return result.returncode, report, result.stderr

    def host_state(self):
        code, status, stderr = self.session_host("status")
        self.assertEqual(code, 0, stderr)
        return status["host"]["state"]

    def test_install_starts_the_staged_image_until_uninstall(self):
        # Not enabled: nothing starts a host, and no host runs.
        code, status, stderr = self.session_host("status")
        self.assertEqual(code, 0, stderr)
        self.assertEqual((status["enabled"], status["host"]), (False, {"state": "not-installed"}))
        code, response, stderr = self.call(self.create_request("cmd/early", self.cmd_argv("early")), start=True)
        self.assertEqual((code, response), (4, None), stderr)
        self.assertIn("not enabled", stderr)
        self.assertEqual(self.status(), {"running": False})

        code, installed, stderr = self.session_host("install")
        self.assertEqual(code, 0, stderr)
        self.assertTrue(installed["enabled"] and installed["marker_created"], installed)
        self.assertTrue(self.marker.is_file() and self.cli.is_file())
        image = Path(installed["image"]["path"])
        self.assertTrue(os.path.samefile(image.parent, self.images), image)
        self.assertEqual(image.name, installed["image"]["name"])
        self.assertEqual((self.images / "current").read_text().strip(), image.name)

        # Enabled and stopped, as the stable CLI reports it too.
        code, status, stderr = self.session_host("status", hgs=self.cli)
        self.assertEqual(code, 0, stderr)
        self.assertTrue(status["enabled"])
        self.assertEqual(status["host"], {"state": "stopped"})
        self.assertEqual(status["image"]["name"], image.name)
        self.assertEqual(status["cli"]["sha256"], installed["cli"]["sha256"])

        # A start runs the staged image, not the build under test.
        created, _ = self.cmd("cmd/staged")
        hello = self.status()["host"]
        self.assertTrue(os.path.samefile(hello["image"], image / "hgs.exe"), hello["image"])
        self.assertEqual(hello["image_hash"], installed["image"]["sha256"])
        code, status, stderr = self.session_host("status")
        self.assertEqual(code, 0, stderr)
        host = status["host"]
        self.assertEqual((host["state"], host["pid"], host["sessions"], host["in_job"]),
                         ("running", hello["pid"], 1, False))

        # Neither stop nor uninstall ends a running session.
        code, report, stderr = self.session_host("stop")
        self.assertEqual((code, report), (1, None), stderr)
        self.assertIn("sessions are running", stderr)
        code, report, stderr = self.session_host("uninstall")
        self.assertEqual((code, report), (1, None), stderr)
        self.assertIn("1 session runs", stderr)
        self.assertTrue(self.marker.is_file())
        self.assertEqual(self.status()["host"]["pid"], hello["pid"])

        # Once its session has ended, the host lingers and exits by itself.
        self.ok({"op": "kill", "target": {"pane": created["pane"]}})
        self.wait_for("the end", lambda: not self.ok({"op": "list"})["sessions"])
        self.wait_for("the idle exit", lambda: self.host_state() == "stopped", timeout=IDLE_EXIT)
        self.wait_for_hosts()

        code, removed, stderr = self.session_host("uninstall")
        self.assertEqual(code, 0, stderr)
        self.assertEqual((removed["enabled"], removed["marker_removed"], removed["host"]), (False, True, "stopped"))
        self.assertFalse(self.marker.exists())
        # The staged files stay for a later install; the computer is not enabled.
        self.assertTrue((image / "hgs.exe").is_file() and self.cli.is_file())
        self.assertEqual(self.host_state(), "not-installed")
        code, _, stderr = self.call(self.create_request("cmd/late", self.cmd_argv("late")), start=True)
        self.assertEqual(code, 4, stderr)
        self.assertIn("not enabled", stderr)
        self.assertEqual(self.status(), {"running": False})

    def test_serve_refuses_unless_enabled_and_staged_or_isolated(self):
        # By hand from the build tree, with nothing enabled: it exits at once.
        result = self.run_hgs("serve", timeout=LIMIT)
        self.assertEqual(result.returncode, 4, result.stderr)
        self.assertIn("not enabled", result.stderr)
        self.assertFalse((self.state / "host.log").exists(), "a refused host writes nothing")

        # The test variables count only for an isolated state directory. Every
        # hgs process exports HGS_STATE_DIR, so naming the default one is not that.
        default = dict(self.env, HGS_STATE_DIR=str(self.home / ".local" / "state" / "hgs" / "sessions"),
                       HGS_HOST_ALLOW_UNSTAGED="1", HGS_HOST_IMAGE=str(HGS))
        result = subprocess.run([str(HGS), "__host", "serve"], env=default, capture_output=True, text=True,
                                timeout=LIMIT, creationflags=HIDDEN)
        self.assertEqual(result.returncode, 4, result.stderr)
        result = subprocess.run([str(HGS), "__host", "call", "--start", '{"op": "list"}'], env=default,
                                capture_output=True, text=True, timeout=LIMIT, creationflags=HIDDEN)
        self.assertEqual(result.returncode, 4, result.stderr)
        self.assertIn("isolated", result.stderr)

        code, _, stderr = self.session_host("install")
        self.assertEqual(code, 0, stderr)
        # Enabled, but this executable is not the staged image.
        result = self.run_hgs("serve", timeout=LIMIT)
        self.assertEqual(result.returncode, 5, result.stderr)
        self.assertIn("is not a staged session host image", result.stderr)
        result = subprocess.run([str(HGS), "__host", "serve"], env=default, capture_output=True, text=True,
                                timeout=LIMIT, creationflags=HIDDEN)
        self.assertEqual(result.returncode, 5, result.stderr)
        self.assertEqual(self.status(), {"running": False})


@unittest.skipUnless(FAKE_AGENT.is_file(), "build the fake agent: cargo build --locked --example fake_agent")
class FakeAgent(HostCase):
    def setUp(self):
        super().setUp()
        self.record = self.root / "record.jsonl"
        self.ready = self.root / "ready.json"
        self.control = self.root / "control"

    def events(self, kind=None):
        lines = self.record.read_text(errors="replace").split("\n")[:-1] if self.record.exists() else []
        events = [json.loads(line) for line in lines]
        return [event for event in events if kind is None or event["kind"] == kind]

    def send_control(self, *lines):
        staged = self.root / "control.tmp"
        staged.write_text("\n".join(lines) + "\n")
        os.replace(staged, self.control)
        self.wait_for("the control file", lambda: not self.control.exists())

    def replies(self):
        return [(event.get("query"), bool(event.get("unsolicited"))) for event in self.events("reply")]

    def test_modes_pastes_and_query_answers_reach_the_agent_once(self):
        created = self.create("fake/a", [
            FAKE_AGENT, "--record", self.record, "--ready", self.ready, "--control", self.control,
            "--modes", "paste", "--query", "dsr,da1,da2,status,osc10,osc11,xtversion",
            "--query-wait", "3000", "--prompt", "fake> "])
        pane = created["pane"]
        self.wait_for("the agent", self.ready.exists)
        conpty = self.status()["status"]["conpty"]
        # Every query gets at most one answer: from the host's screen model for
        # what reaches it (the bundled ConPTY forwards DA1 and the cursor
        # position), from the inbox ConPTY for what it answers itself. Colours
        # stay unanswered, as tmux leaves them without a client terminal. The
        # terminal version stays unanswered too, although tmux answers it with
        # its own (`ESC P >|tmux 3.8 ESC \`): the screen model leaves it to the
        # host, and the host does not answer it yet.
        answered = self.replies()
        self.assertFalse([query for query, unsolicited in answered if unsolicited], answered)
        queries = [query for query, _ in answered]
        for query in set(queries):
            self.assertEqual(queries.count(query), 1, answered)
        self.assertTrue({"dsr", "da1"} <= set(queries), answered)
        unanswered = {"osc10", "osc11", "xtversion"} if conpty == "bundled" else {"osc10", "osc11"}
        self.assertFalse(unanswered & set(queries), answered)

        # Bracketed paste is on: the model sees DECSET 2004 through ConPTY.
        self.wait_for("bracketed paste", lambda: self.info(pane)["bracketed_paste"])
        self.ok({"op": "paste", "target": {"pane": pane}, "text": "line one\nline two", "bracketed": True})
        self.wait_for("the paste", lambda: [event["text"] for event in self.events("paste")] == ["line one\nline two"])

        # An attached client's terminal answers too; only its typing arrives.
        replies_before = len(self.events("reply"))
        attached = self.attach(
            {"target": {"pane": pane}, "size": {"cols": 100, "rows": 30}, "run_id": None},
            [{"wait_ms": 300}, {"send_hex": b"\x1b[?1;2c\x1b[5;1R\x1b]11;rgb:0/0/0\x07".hex()},
             {"send": "typed"}, {"wait_ms": 500}, {"detach": True}])
        self.assertTrue(attached["sent"])
        output = bytes.fromhex(attached["output_hex"])
        for query in (b"\x1b[6n", b"\x1b[c", b"\x1b[>c", b"\x1b]10;?", b"\x1b]11;?"):
            self.assertNotIn(query, output)
        self.wait_for("the typing", lambda: "typed" in "".join(event.get("text", "") for event in self.events("text")))
        self.assertEqual(len(self.events("reply")), replies_before, self.events("reply"))

        # Mode toggles reach the model.
        self.send_control("modes no-paste,cursor")
        self.wait_for("paste off", lambda: not self.info(pane)["bracketed_paste"])
        self.keys(pane, {"named": "Up"})
        self.wait_for("the cursor key", lambda: any(event.get("hex") == "1b4f41" for event in self.events("key")))
        self.ok({"op": "paste", "target": {"pane": pane}, "text": "plain", "bracketed": True})
        self.wait_for("the plain paste", lambda: "plain" in "".join(event.get("text", "") for event in self.events("text")))
        self.assertEqual(len(self.events("paste")), 1)

        self.send_control("exit 0")
        self.wait_for("the end", lambda: not self.ok({"op": "list"})["sessions"])
        self.assertEqual(self.events("exit")[-1]["code"], 0)


if __name__ == "__main__":
    unittest.main()
