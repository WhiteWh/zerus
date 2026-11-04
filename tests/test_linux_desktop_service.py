"""Installer ownership, configuration and graphical-session ordering."""
import copy
import importlib.util
import contextlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import uuid
import sys
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
installer = None
if sys.platform.startswith("linux"):
    spec = importlib.util.spec_from_file_location("linux_desktop_installer", ROOT / "scripts/install-linux-desktop-service.py")
    installer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(installer)


class Manager:
    def __init__(self, test):
        self.test = test
        self.calls = []
        self.active = False
        self.fragment = ""
        self.dropins = ""
        self.state_override = None
        self.lifecycle = {"abi": installer.ABI, "configuration": "a" * 64, "ipc_ready": True,
                          "mode": "passive",
                          "phase": "serving", "pid": 4321, "epoch": "original-epoch", "generation": 1,
                          "background": {"attention": {"ready": False, "code": "not_started"}}}
        self.grant = {"abi": "desktop-ownership-v1", "configuration": "a" * 64, "state": "service",
                      "identity_current": True, "rearm_required": False, "revision": 1,
                      **{key: str(uuid.uuid4()) for key in ("journal", "request_id", "owner_epoch")}}
        self.caller_refused = False
        self.revoke_failure = False
        self.revoke_receipts = {}
        self.held = False
        self.hold_alive = True
        self.before_hold = None
        self.after_revoke_check = None
        self.holds = []
        self.after_action = None
        self.release_failure = False
        self.during_probe = None

    def rearm(self):
        if self.held:
            return False  # Models Root's separately tested physical exclusion.
        self.grant.update(state="service", rearm_required=False,
                          request_id=str(uuid.uuid4()), owner_epoch=str(uuid.uuid4()))
        return True

    def hold(self, hgs, env, configuration, request, receipt, help_text):
        manager = self
        class Held:
            def acquire(self):
                if manager.before_hold:
                    manager.before_hold()
                if manager.grant["state"] != "revoked":
                    raise installer.Refusal("original writer authority changed")
                manager.test.assertEqual(receipt, manager.revoke_receipts[request])
                manager.held = True
                manager.holds.append(self)
                self.finished = False
                return self

            def check(self):
                if not manager.hold_alive or self.finished:
                    raise installer.Refusal("original rollback hold exited")

            def close(self):
                if self.finished:
                    return
                manager.held = False
                self.finished = True
                if manager.release_failure:
                    raise installer.Refusal("original rollback release unverified")
        return Held()

    def run(self, args, env, timeout=10):
        self.calls.append((list(args), dict(env)))
        if args[0] == str(self.test.hgs):
            if args[1:] == ["--help"]:
                return "hgs desktop registration --json\nDesktop revoke hold ABI: desktop-revoke-hold-v1"
            if args[1:] == ["desktop", "registration", "--json"]:
                return json.dumps(self.test.preview)
            if args[1:] == ["desktop", "registration", "--json", "--background"]:
                value = copy.deepcopy(self.test.preview)
                value["mode"] = "background"
                value["argv"].append("--background")
                value["definition"] = value["definition"].replace("\nKillMode=", ' "--background"\nKillMode=')
                return json.dumps(value)
            if args[1:] == ["desktop", "registration", "mode-check", "--json"]:
                if self.revoke_receipts and self.after_revoke_check:
                    callback, self.after_revoke_check = self.after_revoke_check, None
                    callback()
                if self.caller_refused:
                    raise installer.Refusal("agent-descended frontend request refused")
                return json.dumps({"schema": 1, "configuration": "a" * 64, "admitted": True,
                                   "source": "interactive_terminal"})
            if args[1:] == ["desktop", "ownership-status", "--json"]:
                return json.dumps(self.grant)
            if args[1:4] == ["desktop", "revoke", "--json"]:
                request = args[args.index("--request-id") + 1]
                self.test.assertEqual(args[-2:], ["--reason", "rollback"])
                if self.revoke_failure:
                    raise installer.Refusal("original revoke result unavailable")
                old = self.revoke_receipts.get(request)
                if (old is not None and "writer_epoch" in old and self.grant["state"] == "service"
                        and old["writer_epoch"] != self.grant["owner_epoch"]):
                    raise installer.Refusal("writer_authority_changed")
                result = self.revoke_receipts.setdefault(request, {"request_id": request,
                    "configuration": "a" * 64, "accepted": True, "durable": True,
                    "revoked": True, "checkpointed": True, "writer_epoch": self.grant["owner_epoch"]})
                self.grant.update(state="revoked", rearm_required=True)
                return json.dumps(result)
            self.test.assertEqual(args[1:], ["desktop", "status", "--json"])
            self.test.assertEqual(env["HGS_CONFIG_DIR"], self.test.scope["HGS_CONFIG_DIR"])
            self.test.assertEqual(env["HGS_STATE_DIR"], self.test.scope["HGS_STATE_DIR"])
            if self.during_probe:
                self.during_probe()
            return json.dumps({"lifecycle": self.lifecycle})
        self.test.assertEqual(args[:2], ["systemctl", "--user"])
        action = args[2]
        if action == "show":
            if self.state_override is not None:
                return self.state_override
            fragment = self.fragment or (str(self.test.destination) if self.test.destination.exists() else "")
            return ("LoadState=" + ("loaded" if fragment else "not-found") + "\nActiveState="
                    + ("active" if self.active else "inactive") + "\nMainPID="
                    + ("4321" if self.active else "0") + "\nFragmentPath=" + fragment
                    + "\nDropInPaths=" + self.dropins + "\n")
        if action in ("enable", "start", "restart"):
            self.test.assertEqual(args[3:], [installer.UNIT])
        if action in ("start", "restart"):
            self.active = True
            self.lifecycle["mode"] = "background" if '"--background"' in self.test.destination.read_text() else "passive"
        if self.after_action is not None and self.after_action[0] == action:
            _, callback = self.after_action
            self.after_action = None
            callback()
        return ""

    def actions(self):
        return [args[2] for args, _ in self.calls if args[0] == "systemctl"]


@unittest.skipUnless(sys.platform.startswith("linux"), "Linux service registration")
class LinuxDesktopService(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="zerus-desktop-install-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.runtime = self.root / "runtime"
        self.runtime.mkdir(mode=0o700)
        self.hgs = self.root / "hgs cli%selected"
        self.hgs.write_bytes(b"fixture executable, not launched\n")
        self.hgs.chmod(0o755)
        self.env = {"PATH": "/usr/bin:/bin", "HOME": str(self.home), "XDG_CONFIG_HOME": str(self.home / "config"),
                    "XDG_RUNTIME_DIR": str(self.runtime), "DBUS_SESSION_BUS_ADDRESS": "unix:path=" + str(self.runtime / "bus"),
                    "WAYLAND_DISPLAY": "wayland-0", "XDG_SESSION_TYPE": "wayland", "XDG_SESSION_ID": "session-one",
                    "DISPLAY": ":99", "LD_LIBRARY_PATH": "/private/gui-only-stack"}
        self.scope = {"HGS_CONFIG_DIR": str(self.root / "config $literal%"), "HGS_STATE_DIR": str(self.root / "state")}
        self.destination = self.home / "config/systemd/user/hgs-desktop.service"
        args = [str(self.hgs), "desktop", "serve", "--config-dir", self.scope["HGS_CONFIG_DIR"],
                "--state-dir", self.scope["HGS_STATE_DIR"]]
        definition = ("# Zerus desktop registration v1; configuration=" + "a" * 64
                      + "\n[Unit]\nAfter=graphical-session.target\nPartOf=graphical-session.target\n"
                      + "[Service]\nExecStart=" + " ".join(map(installer.unit_quote, args))
                      + "\nKillMode=process\nRestart=on-failure\nRestartPreventExitStatus=78\n"
                      + "[Install]\nWantedBy=graphical-session.target\n")
        self.preview = {"schema": 1, "configuration": "a" * 64, "label": installer.UNIT,
                        "destination": str(self.destination), "argv": args, "definition": definition,
                        "readiness": {"abi": installer.ABI, "argv": [str(self.hgs), "desktop", "status", "--json"],
                                      "environment": self.scope, "expected_configuration": "a" * 64,
                                      "configuration_field": "lifecycle.configuration", "ready_field": "lifecycle.ipc_ready",
                                      "epoch_field": "lifecycle.epoch", "generation_field": "lifecycle.generation"},
                        "session": {"kind": "graphical_user", "import_from": "current_login_session"}}
        self.manager = Manager(self)

    def instance(self, mode=None):
        return installer.Installer(str(self.hgs), self.env, self.manager.run, mode=mode,
                                   hold_factory=self.manager.hold)

    def test_rearm_after_receipt_cannot_win_during_the_held_manager_transition(self):
        self.instance().install()
        self.instance("background").install()
        outcomes = []
        self.manager.after_revoke_check = lambda: outcomes.append(self.manager.rearm())
        self.instance("passive").install()
        self.assertEqual(outcomes, [False])
        self.assertEqual(self.manager.grant["state"], "revoked")
        self.assertFalse(self.manager.held)
        self.assertTrue(self.manager.holds[0].finished)
        self.assertEqual(self.instance().status()["running_mode"], "passive")

    def test_rearm_winning_before_hold_keeps_original_uuid_and_background_unit(self):
        self.instance().install()
        self.instance("background").install()
        original = self.destination.read_bytes()
        self.manager.calls.clear()
        self.manager.before_hold = lambda: self.assertTrue(self.manager.rearm())
        with self.assertRaisesRegex(installer.Refusal, "authority changed"):
            self.instance("passive").install()
        self.assertEqual(self.destination.read_bytes(), original)
        self.assertEqual(self.manager.actions(), ["show", "show"])
        record = json.loads(self.instance().record_path.read_bytes())
        self.assertEqual(record["transition"]["request_id"], next(iter(self.manager.revoke_receipts)))
        self.assertEqual(self.manager.grant["state"], "service")

    def test_original_hold_survives_both_pending_and_ready_passive_probes(self):
        self.instance().install()
        self.instance("background").install()
        observed = []

        def probe():
            if self.manager.lifecycle["mode"] != "passive":
                return  # The pre-transition check still observes background.
            self.assertTrue(self.manager.held, "rollback hold released before passive readiness")
            self.assertEqual(len(self.manager.holds), 1)
            original = self.manager.holds[0]
            self.assertFalse(original.finished)
            self.assertFalse(self.manager.rearm())
            observed.append(original)
            self.manager.lifecycle["ipc_ready"] = len(observed) > 1

        self.manager.during_probe = probe
        self.instance("passive").install()
        self.manager.during_probe = None
        self.assertEqual(observed, [self.manager.holds[0]] * 2)
        self.assertTrue(observed[0].finished)
        self.assertFalse(self.manager.held)
        status = self.instance().status()
        self.assertEqual(status["running_mode"], "passive")
        self.assertFalse(status["pending_restart"])
        self.assertIsNone(status["transition"])

    def test_lost_original_hold_admits_no_manager_steps_or_transition_completion(self):
        self.instance().install()
        self.instance("background").install()
        original = self.destination.read_bytes()
        self.manager.calls.clear()
        self.manager.after_revoke_check = lambda: setattr(self.manager, "hold_alive", False)
        with self.assertRaisesRegex(installer.Refusal, "hold exited"):
            self.instance("passive").install()
        self.assertEqual(self.destination.read_bytes(), original)
        self.assertEqual(self.manager.actions(), ["show", "show"])
        self.assertIsNotNone(json.loads(self.instance().record_path.read_bytes())["transition"])
        self.assertFalse(self.manager.held)

    def test_explicit_cancellation_of_obsolete_rollback_keeps_native_state_then_new_owner_intent_can_finish(self):
        self.instance().install()
        self.instance("background").install()
        original = self.destination.read_bytes()
        self.manager.before_hold = lambda: self.assertTrue(self.manager.rearm())
        with self.assertRaisesRegex(installer.Refusal, "cancel-rollback"):
            self.instance("passive").install()
        request = next(iter(self.manager.revoke_receipts))
        grant, receipts = copy.deepcopy(self.manager.grant), copy.deepcopy(self.manager.revoke_receipts)
        with self.assertRaisesRegex(installer.Refusal, "writer_authority_changed"):
            self.instance("passive").install()  # Same stale UUID cannot revoke the new runtime.
        self.assertEqual(self.manager.grant, grant)
        self.assertEqual(self.manager.revoke_receipts, receipts)
        self.manager.calls.clear()
        self.instance().cancel_rollback(request)
        self.instance().cancel_rollback(request)  # Same metadata receipt, no native replay.
        self.assertEqual(self.manager.actions(), [])
        self.assertFalse(any("revoke" in argv for argv, _ in self.manager.calls))
        self.assertEqual(self.manager.grant, grant)
        self.assertEqual(self.manager.revoke_receipts, receipts)
        self.assertEqual(self.destination.read_bytes(), original)
        record = json.loads(self.instance().record_path.read_bytes())
        self.assertNotIn("transition", record)
        self.assertEqual(record["last_cancelled_rollback_request_id"], request)
        self.manager.before_hold = None
        self.instance("passive").install()  # A NEW explicit owner request, never an automatic retry.
        self.assertEqual(len(self.manager.revoke_receipts), 2)
        self.assertNotEqual(self.instance().status()["last_revoke_request_id"], request)
        self.assertEqual(self.instance().status()["running_mode"], "passive")
        self.assertEqual(self.instance().status()["last_cancelled_rollback_request_id"], request)

    def test_cancellation_requires_exact_original_uuid_current_service_grant_and_real_caller(self):
        self.instance().install()
        self.instance("background").install()
        value = self.instance()
        record = value.ownership()
        request = str(uuid.uuid4())
        record["transition"] = {"target": "passive", "request_id": request}
        value.save(record)
        raw = value.record_path.read_bytes()
        unit = self.destination.read_bytes()
        for change in ({"state": "revoked", "rearm_required": True},
                       {"state": "passive"}, {"identity_current": False},
                       {"configuration": "b" * 64}, {"owner_epoch": None}):
            with self.subTest(change=change), patch.dict(self.manager.grant, change):
                with self.assertRaisesRegex(installer.Refusal, "Grant/Rearm"):
                    value.cancel_rollback(request)
        self.manager.caller_refused = True
        with self.assertRaisesRegex(installer.Refusal, "agent-descended"):
            value.cancel_rollback(request)
        self.manager.caller_refused = False
        for wrong in (str(uuid.uuid4()), "invalid", None):
            with self.subTest(wrong=wrong), self.assertRaises(installer.Refusal):
                value.cancel_rollback(wrong)
        self.assertEqual(value.record_path.read_bytes(), raw)
        self.assertEqual(self.destination.read_bytes(), unit)
        self.assertFalse(any("revoke" in argv for argv, _ in self.manager.calls))

    def test_cancellation_before_the_original_revoke_keeps_the_existing_service_grant(self):
        self.instance().install()
        self.instance("background").install()
        value = self.instance()
        record = value.ownership()
        request = str(uuid.uuid4())
        record["transition"] = {"target": "passive", "request_id": request}
        value.save(record)  # The helper stopped before offering any revoke.
        unit, grant = self.destination.read_bytes(), copy.deepcopy(self.manager.grant)
        self.manager.calls.clear()
        value.cancel_rollback(request)
        self.assertEqual(self.manager.actions(), [])
        self.assertFalse(any("revoke" in argv for argv, _ in self.manager.calls))
        self.assertEqual(self.manager.revoke_receipts, {})
        self.assertEqual(self.manager.grant, grant)
        self.assertEqual(self.destination.read_bytes(), unit)
        saved = value.ownership()
        self.assertNotIn("transition", saved)
        self.assertEqual(saved["mode"], "background")
        self.assertEqual(saved["last_cancelled_rollback_request_id"], request)

    def test_lost_hold_after_manager_result_stops_before_the_next_manager_step(self):
        self.instance().install()
        self.instance("background").install()
        self.manager.calls.clear()
        self.manager.after_action = ("daemon-reload", lambda: setattr(self.manager, "hold_alive", False))
        with self.assertRaisesRegex(installer.Refusal, "hold exited"):
            self.instance("passive").install()
        self.assertEqual(self.manager.actions(), ["show", "show", "daemon-reload"])
        record = json.loads(self.instance().record_path.read_bytes())
        self.assertEqual(record["transition"]["request_id"], next(iter(self.manager.revoke_receipts)))
        self.assertTrue(record["pending_restart"])
        self.assertFalse(self.manager.held)

    def test_unknown_release_keeps_original_pending_uuid_after_passive_readiness(self):
        self.instance().install()
        self.instance("background").install()
        self.manager.release_failure = True
        with self.assertRaisesRegex(installer.Refusal, "release unverified"):
            self.instance("passive").install()
        record = json.loads(self.instance().record_path.read_bytes())
        original = record["transition"]["request_id"]
        self.assertEqual(original, next(iter(self.manager.revoke_receipts)))
        self.assertTrue(record["pending_restart"])
        self.assertEqual(self.manager.lifecycle["mode"], "passive")
        self.manager.release_failure = False
        self.instance("passive").install()
        self.assertEqual(list(self.manager.revoke_receipts), [original])
        self.assertIsNone(self.instance().status()["transition"])

    def test_old_cli_marker_refuses_before_revoke_or_pending_record_creation(self):
        self.instance().install()
        self.instance("background").install()
        instance = self.instance("passive")
        unit, record = self.destination.read_bytes(), instance.record_path.read_bytes()
        run = instance.run
        instance.run = lambda argv, env, **kwargs: "hgs desktop registration --json" if argv[1:] == ["--help"] else run(argv, env, **kwargs)
        with self.assertRaisesRegex(installer.Refusal, "native rollback hold"):
            instance.install()
        self.assertEqual(self.manager.revoke_receipts, {})
        self.assertEqual(self.destination.read_bytes(), unit)
        self.assertEqual(instance.record_path.read_bytes(), record)

    def test_explicit_background_requires_grant_then_restarts_only_the_owned_service(self):
        self.instance().install()
        self.manager.calls.clear()
        self.instance("background").install()
        self.assertIn('"--background"', self.destination.read_text())
        status = self.instance().status()
        self.assertEqual((status["selected_mode"], status["running_mode"]), ("background", "background"))
        self.assertFalse(status["pending_restart"])
        calls = [args for args, _ in self.manager.calls]
        self.assertLess(next(i for i,a in enumerate(calls) if "mode-check" in a),
                        next(i for i,a in enumerate(calls) if "restart" in a))
        self.assertFalse(any("revoke" in a for a in calls))
        self.assertEqual(self.manager.actions().count("restart"), 1)

    def test_missing_changed_or_revoked_grant_never_changes_unit_or_starts_a_runtime(self):
        self.instance().install()
        original = self.destination.read_bytes()
        for change in ({"state": "passive"}, {"state": "revoked", "rearm_required": True},
                       {"state": "identity_changed", "identity_current": False},
                       {"configuration": "b" * 64}, {"owner_epoch": None}, {"revision": True}):
            with self.subTest(change=change):
                with patch.dict(self.manager.grant, change):
                    self.manager.calls.clear()
                    with self.assertRaisesRegex(installer.Refusal, "Grant/Rearm"):
                        self.instance("background").install()
                self.assertEqual(self.destination.read_bytes(), original)
                self.assertNotIn("restart", self.manager.actions())

    def test_agent_cannot_select_either_mode_even_with_an_existing_grant(self):
        self.instance().install()
        self.manager.caller_refused = True
        original = self.destination.read_bytes()
        for mode in ("passive", "background"):
            with self.assertRaisesRegex(installer.Refusal, "agent-descended"):
                self.instance(mode).install()
            self.assertEqual(self.destination.read_bytes(), original)

    def test_helper_refuses_present_agent_markers_before_accepting_any_cli_reply(self):
        self.instance().install()
        original = self.destination.read_bytes()
        for key in ("HGS_SESSION", "HGS_RUN_ID"):
            for marker in ("", "private-fixture"):
                with patch.dict(self.env, {key: marker}):
                    self.manager.calls.clear()
                    with self.assertRaisesRegex(installer.Refusal, "agent session"):
                        self.instance("background").install()
                self.assertEqual(self.destination.read_bytes(), original)
                self.assertNotIn("restart", self.manager.actions())
                self.assertFalse(any("mode-check" in args for args, _ in self.manager.calls))

    def test_ordinary_upgrade_session_and_refresh_preserve_background_selection(self):
        self.instance().install()
        self.instance("background").install()
        self.manager.calls.clear()
        self.instance().install()
        self.instance().install(session=True)
        self.instance().install(refresh=True)
        self.assertNotIn("restart", self.manager.actions())
        self.hgs.write_bytes(b"new backend\n")
        self.instance().install(refresh=True)
        self.assertEqual(self.manager.actions().count("restart"), 1)
        self.assertEqual(self.instance().status()["selected_mode"], "background")
        self.assertFalse(any("mode-check" in args or "revoke" in args for args, _ in self.manager.calls))

    def test_passive_rollback_waits_for_original_revoke_before_unit_change_and_restart(self):
        self.instance().install()
        self.instance("background").install()
        original = self.destination.read_bytes()
        self.manager.revoke_failure = True
        self.manager.calls.clear()
        with self.assertRaisesRegex(installer.Refusal, "revoke result"):
            self.instance("passive").install()
        self.assertEqual(self.destination.read_bytes(), original)
        self.assertNotIn("restart", self.manager.actions())
        record = json.loads(self.instance().record_path.read_text())
        request = record["transition"]["request_id"]
        with self.assertRaisesRegex(installer.Refusal, "pending rollback"):
            self.instance().install(refresh=True)
        with self.assertRaisesRegex(installer.Refusal, "pending rollback"):
            self.instance("background").install()
        self.manager.revoke_failure = False
        self.instance("passive").install()
        requests = [args[args.index("--request-id") + 1] for args, _ in self.manager.calls if "revoke" in args]
        self.assertEqual(requests, [request, request])
        self.assertNotIn('"--background"', self.destination.read_text())
        status = self.instance().status()
        self.assertEqual((status["selected_mode"], status["running_mode"]), ("passive", "passive"))
        self.assertIsNone(status["transition"])
        self.assertEqual(status["last_revoke_request_id"], request)
        self.assertEqual(self.manager.grant["state"], "revoked")
        with self.assertRaisesRegex(installer.Refusal, "Grant/Rearm"):
            self.instance("background").install()

    def test_wrong_or_undrained_revoke_receipt_never_rewrites_the_background_unit(self):
        self.instance().install()
        self.instance("background").install()
        instance = self.instance("passive")
        original = self.destination.read_bytes()
        request = str(uuid.uuid4())
        record = instance.ownership()
        record["transition"] = {"target": "passive", "request_id": request}
        instance.save(record)
        for change in ({"configuration": "b" * 64}, {"request_id": str(uuid.uuid4())},
                       {"revoked": False}, {"checkpointed": False}, {"accepted": False}):
            self.manager.revoke_receipts[request] = {"request_id": request, "configuration": "a" * 64,
                "accepted": True, "durable": True, "revoked": True, "checkpointed": True, **change}
            with self.assertRaisesRegex(installer.Refusal, "not settled"):
                instance.install()
            self.assertEqual(self.destination.read_bytes(), original)

    def test_explicit_transition_cannot_stage_from_a_headless_or_no_start_context(self):
        self.instance().install()
        original = self.destination.read_bytes()
        for no_start in (True, False):
            with patch.dict(self.env):
                if not no_start:
                    self.env.pop("XDG_SESSION_TYPE")
                with self.assertRaisesRegex(installer.Refusal, "graphical session"):
                    self.instance("background").install(no_start=no_start)
            self.assertEqual(self.destination.read_bytes(), original)

    def test_legacy_records_remain_passive_and_status_does_not_mutate_registration(self):
        self.instance().install()
        instance = self.instance()
        record = instance.ownership()
        record.pop("mode")
        instance.save(record)
        original = instance.record_path.read_bytes()
        self.manager.calls.clear()
        self.assertEqual(instance.status()["selected_mode"], "passive")
        self.assertEqual(instance.record_path.read_bytes(), original)
        self.assertEqual(self.manager.actions(), ["show"])

    def test_a_waiting_upgrade_cannot_flip_a_newly_selected_background_mode(self):
        self.instance().install()
        waiting = self.instance()
        self.instance("background").install()
        original = self.destination.read_bytes()
        self.manager.calls.clear()
        with self.assertRaisesRegex(installer.Refusal, "mode changed"):
            waiting.install(refresh=True)
        self.assertEqual(self.destination.read_bytes(), original)
        self.assertNotIn("restart", self.manager.actions())

    def test_json_receipts_distinguish_selected_running_and_uncertain_original_transition(self):
        self.instance().install()
        self.instance("background").install()
        instance = self.instance("passive")
        self.manager.revoke_failure = True
        self.manager.calls.clear()
        output = io.StringIO()
        with patch.object(sys, "argv", ["helper", "install", "--hgs", str(self.hgs), "--mode", "passive", "--json"]), \
             patch.object(installer, "Installer", return_value=instance), \
             patch.dict(os.environ, self.env, clear=True), contextlib.redirect_stdout(output), \
             contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(installer.main(), 1)
        value = json.loads(output.getvalue())
        self.assertFalse(value["ok"])
        self.assertEqual(value["code"], "registration_refused")
        self.assertEqual(value["selected_mode"], "background")
        self.assertIsNone(value["running_mode"])
        self.assertEqual(value["transition"]["target"], "passive")
        self.assertEqual(value["configuration"], "a" * 64)
        self.assertNotIn("restart", self.manager.actions())

    def test_an_inactive_registration_reports_selection_without_claiming_running_mode(self):
        self.instance().install()
        self.instance("background").install()
        self.manager.active = False
        value = self.instance().status()
        self.assertEqual(value["selected_mode"], "background")
        self.assertIsNone(value["running_mode"])
        self.assertFalse(value["ipc_ready"])

    def test_imports_only_current_session_before_starting_only_desktop(self):
        message = self.instance().install()
        self.assertIn("IPC ready", message)
        self.assertEqual(self.destination.read_text(), self.preview["definition"])
        actions = self.manager.actions()
        self.assertLess(actions.index("import-environment"), actions.index("start"))
        [imported] = [args[3:] for args, _ in self.manager.calls if args[:3] == ["systemctl", "--user", "import-environment"]]
        self.assertEqual(set(imported), {"XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS", "WAYLAND_DISPLAY",
                                         "XDG_SESSION_TYPE", "XDG_SESSION_ID"})
        self.assertNotIn("DISPLAY", imported)
        self.assertNotIn("LD_LIBRARY_PATH", imported)
        self.assertFalse(self.manager.lifecycle["background"]["attention"]["ready"])

    def test_ui_only_update_keeps_unchanged_backend(self):
        self.instance().install()
        before = self.destination.stat().st_mtime_ns
        self.manager.calls.clear()
        self.instance().install()
        self.assertNotIn("start", self.manager.actions())
        self.assertNotIn("restart", self.manager.actions())
        self.assertEqual(self.destination.stat().st_mtime_ns, before)

    def test_backend_binary_upgrade_restarts_only_desktop(self):
        self.instance().install()
        self.hgs.write_bytes(b"changed backend executable\n")
        self.manager.calls.clear()
        self.instance().install()
        actions = self.manager.actions()
        self.assertIn("restart", actions)
        self.assertLess(actions.index("import-environment"), actions.index("restart"))

    def test_refresh_does_not_create_register_enable_or_start_an_absent_service(self):
        self.instance().install(refresh=True)
        self.assertFalse(self.destination.parent.exists())
        self.assertEqual(self.manager.actions(), [])

    def test_refresh_ignores_session_changes_when_backend_unchanged(self):
        self.instance().install()
        self.env.update(WAYLAND_DISPLAY="wayland-1", XDG_SESSION_ID="session-two")
        self.manager.calls.clear()
        self.instance().install(refresh=True)
        self.assertEqual(self.manager.actions(), ["show", "show"])

    def test_refresh_updates_backend_without_enabling_an_inactive_service(self):
        self.instance().install()
        self.manager.active = False
        self.hgs.write_bytes(b"new CLI\n")
        self.manager.calls.clear()
        self.instance().install(refresh=True)
        self.assertIn("daemon-reload", self.manager.actions())
        for action in ("enable", "start", "restart", "import-environment"):
            self.assertNotIn(action, self.manager.actions())

    def test_refresh_restarts_only_changed_active_owned_backend(self):
        self.instance().install()
        self.hgs.write_bytes(b"new CLI\n")
        self.manager.calls.clear()
        self.instance().install(refresh=True)
        actions = self.manager.actions()
        self.assertLess(actions.index("import-environment"), actions.index("restart"))
        self.assertNotIn("enable", actions)

    def test_argv_upgrade_restarts_the_managed_backend(self):
        self.instance().install()
        old = str(self.hgs)
        replacement = self.root / "other hgs"
        replacement.write_bytes(self.hgs.read_bytes())
        replacement.chmod(0o755)
        self.hgs = replacement
        self.preview["argv"][0] = str(replacement)
        self.preview["readiness"]["argv"][0] = str(replacement)
        self.preview["definition"] = self.preview["definition"].replace(installer.unit_quote(old), installer.unit_quote(str(replacement)))
        self.manager.calls.clear()
        self.instance().install(refresh=True)
        self.assertIn("restart", self.manager.actions())

    def test_headless_backend_upgrade_remains_pending_without_restart(self):
        self.instance().install()
        self.hgs.write_bytes(b"new CLI\n")
        self.env.pop("XDG_SESSION_TYPE")
        self.manager.calls.clear()
        with self.assertRaisesRegex(installer.PendingRestart, "pending"):
            self.instance().install(refresh=True)
        record = json.loads(self.destination.with_name(".hgs-desktop.zerus-registration.json").read_text())
        self.assertTrue(record["pending_restart"])
        self.assertNotIn("restart", self.manager.actions())
        # A later deliberate refresh uses the saved pending state, even though
        # the definition and binary now equal the installed record.
        self.env["XDG_SESSION_TYPE"] = "wayland"
        self.instance().install(refresh=True)
        self.assertIn("restart", self.manager.actions())

    def test_pending_refresh_has_a_distinct_cli_status(self):
        error = io.StringIO()
        with patch("sys.argv", ["installer", "refresh", "--hgs", str(self.hgs)]), \
                patch.dict(os.environ, self.env, clear=True), \
                patch.object(installer.os, "geteuid", return_value=1000), \
                patch.object(installer, "read_file", return_value=b"managed"), \
                patch.object(installer, "Installer") as selected, contextlib.redirect_stderr(error):
            selected.return_value.install.side_effect = installer.PendingRestart("backend update is pending")
            self.assertEqual(installer.main(), 3)
        self.assertIn("Desktop service refresh pending", error.getvalue())

    def test_unit_replacement_before_record_save_is_recoverable(self):
        self.instance().install()
        record_path = self.destination.with_name(".hgs-desktop.zerus-registration.json")
        previous = record_path.read_bytes()
        self.preview["definition"] += "# new owned renderer output\n"
        attempt = self.instance()
        self.manager.calls.clear()
        with patch.object(attempt, "save", side_effect=OSError("interrupted record write")):
            with self.assertRaisesRegex(OSError, "interrupted"):
                attempt.install()
        self.assertEqual(record_path.read_bytes(), previous)
        self.assertEqual(self.destination.read_text(), self.preview["definition"])
        self.assertNotIn("restart", self.manager.actions())
        self.instance().install()
        self.assertIn("restart", self.manager.actions())
        self.assertEqual(json.loads(record_path.read_bytes())["definition_sha256"],
                         installer.digest(self.destination.read_bytes()))

    def test_readiness_pid_mismatch_keeps_the_active_service(self):
        self.instance().install()
        self.hgs.write_bytes(b"new CLI\n")
        self.manager.lifecycle["pid"] = 9999
        self.manager.calls.clear()
        with self.assertRaisesRegex(installer.Refusal, "PID/readiness"):
            self.instance().install(refresh=True)
        self.assertNotIn("restart", self.manager.actions())

    def test_transitional_or_masked_manager_state_does_not_change_registration(self):
        for raw in ("LoadState=loaded\nActiveState=activating\n", "LoadState=masked\nActiveState=inactive\n"):
            self.manager.state_override = raw
            with self.assertRaisesRegex(installer.Refusal, "state"):
                self.instance().install()
            self.assertFalse(self.destination.exists())

    def test_unit_directory_writable_by_others_is_kept(self):
        self.destination.parent.mkdir(parents=True)
        self.destination.parent.chmod(0o777)
        with self.assertRaisesRegex(installer.Refusal, "directory"):
            self.instance().install()
        self.assertFalse(self.destination.exists())

    def test_session_replacement_refreshes_the_running_child(self):
        self.instance().install()
        self.env.update(WAYLAND_DISPLAY="wayland-1", XDG_SESSION_ID="session-two")
        self.manager.calls.clear()
        self.instance().install(session=True)
        self.assertIn("restart", self.manager.actions())

    def test_headless_registration_does_not_start_with_stale_manager_data(self):
        self.env.pop("XDG_SESSION_TYPE")
        message = self.instance().install()
        self.assertIn("next graphical login", message)
        self.assertNotIn("start", self.manager.actions())
        self.assertNotIn("import-environment", self.manager.actions())
        self.assertTrue(self.destination.is_file())

    def test_check_is_read_only(self):
        self.instance().install(check=True)
        self.assertFalse(self.destination.parent.exists())
        self.assertEqual(self.manager.actions(), ["show"])

    def test_custom_and_edited_managed_definitions_are_preserved(self):
        self.destination.parent.mkdir(parents=True)
        self.destination.write_text("# custom service\n")
        with self.assertRaisesRegex(installer.Refusal, "custom"):
            self.instance().install()
        self.assertEqual(self.destination.read_text(), "# custom service\n")
        self.destination.unlink()
        self.instance().install()
        self.destination.write_text(self.destination.read_text() + "# local edit\n")
        self.manager.calls.clear()
        with self.assertRaisesRegex(installer.Refusal, "custom"):
            self.instance().install()
        self.assertNotIn("restart", self.manager.actions())
        self.assertTrue(self.destination.read_text().endswith("# local edit\n"))

    def test_symlink_and_global_custom_unit_are_not_adopted(self):
        target = self.root / "custom"
        target.write_text("user data")
        self.destination.parent.mkdir(parents=True)
        self.destination.symlink_to(target)
        with self.assertRaises(OSError):
            self.instance().install()
        self.assertEqual(target.read_text(), "user data")
        self.destination.unlink()
        self.manager.fragment = "/etc/systemd/user/hgs-desktop.service"
        with self.assertRaisesRegex(installer.Refusal, "custom"):
            self.instance().install()
        self.assertFalse(self.destination.exists())

    def test_custom_dropins_are_preserved(self):
        self.manager.dropins = "/etc/systemd/user/hgs-desktop.service.d/custom.conf"
        with self.assertRaisesRegex(installer.Refusal, "drop-ins"):
            self.instance().install()
        self.assertFalse(self.destination.exists())

    def test_another_configuration_and_abi_are_refused(self):
        self.instance().install()
        for change in ({"configuration": "b" * 64}, {"abi": "future"}):
            self.manager.lifecycle.update(change)
            self.manager.calls.clear()
            with self.assertRaisesRegex(installer.Refusal, "mismatch"):
                self.instance().install()
            self.assertNotIn("restart", self.manager.actions())
            self.manager.lifecycle.update(configuration="a" * 64, abi=installer.ABI)

    def test_probe_cannot_substitute_another_command_or_configuration(self):
        for field, value in (("argv", [str(self.hgs), "other-command"]),
                             ("expected_configuration", "b" * 64)):
            original = copy.deepcopy(self.preview)
            self.preview["readiness"][field] = value
            with self.assertRaises(installer.Refusal):
                self.instance()
            self.preview = original

    def test_ready_probe_requires_live_pid_epoch_and_generation(self):
        for field, value in (("pid", True), ("epoch", ""), ("generation", None), ("phase", "unavailable")):
            with self.subTest(field=field):
                before = self.manager.lifecycle[field]
                self.manager.lifecycle[field] = value
                with self.assertRaisesRegex(installer.Refusal, "lifecycle identity"):
                    self.instance().probe()
                self.manager.lifecycle[field] = before

    def test_genuine_x11_clears_wayland_and_imports_its_display(self):
        self.env["XDG_SESSION_TYPE"] = "x11"
        self.instance().install()
        self.assertIn("unset-environment", self.manager.actions())
        [imported] = [args[3:] for args, _ in self.manager.calls if args[:3] == ["systemctl", "--user", "import-environment"]]
        self.assertIn("DISPLAY", imported)
        self.assertNotIn("WAYLAND_DISPLAY", imported)

    def test_incomplete_graphical_environment_never_starts_service(self):
        self.env.pop("DBUS_SESSION_BUS_ADDRESS")
        with self.assertRaisesRegex(installer.Refusal, "incomplete"):
            self.instance().install()
        self.assertNotIn("start", self.manager.actions())

    def test_command_output_and_deadline_are_bounded(self):
        with self.assertRaisesRegex(installer.Refusal, "output exceeded"):
            installer.command([sys.executable, '-c', 'print("x" * 70000)'], self.env)
        with self.assertRaisesRegex(installer.Refusal, "timed out"):
            installer.command([sys.executable, '-c', 'import time; time.sleep(5)'], self.env, timeout=0.05)


@unittest.skipUnless(sys.platform.startswith("linux"), "Linux CLI refresh hook")
class CliRefreshHook(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="zerus-cli-refresh-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.repo = self.root / "repo"
        (self.repo / "scripts").mkdir(parents=True)
        shutil.copyfile(ROOT / "install.sh", self.repo / "install.sh")
        (self.repo / "hgs_state.py").write_text("# fixture shim\n")
        (self.repo / "scripts/install-linux-desktop-service.py").write_text(
            'import json, os, pathlib, sys\n'
            'home = pathlib.Path(os.environ["HOME"])\n'
            '(home / "refresh-call").write_text(repr(sys.argv[1:]))\n'
            'with (home / "install-order").open("a") as log: log.write(json.dumps(["refresh"]) + "\\n")\n'
            'status = int(os.environ.get("FIXTURE_REFRESH_STATUS", "0"))\n'
            'if status == 3:\n'
            '    assert (home / ".local/bin/zerus-mobile-connector").is_file()\n'
            '    print("Desktop service refresh pending: use the current graphical session", file=sys.stderr)\n'
            'sys.exit(status)\n')
        for worker in ("recovery", "swarm", "voice"):
            (self.repo / f"scripts/install-{worker}-service.py").write_text(
                'import json, os, pathlib, sys\n'
                'with pathlib.Path(os.environ["HOME"], "install-order").open("a") as log:\n'
                f'    log.write(json.dumps([{worker!r}, *sys.argv[1:]]) + "\\n")\n')
        fake = self.root / "bin"
        fake.mkdir()
        for name, body in {
                "cargo": 'mkdir -p "$CARGO_TARGET_DIR/release"\n'
                         'case "$*" in *services/mobile/connector/Cargo.toml*) name=zerus-mobile-connector ;; *) name=hgs ;; esac\n'
                         'printf \'#!/bin/sh\\necho %s-fixture\\n\' "$name" > "$CARGO_TARGET_DIR/release/$name"\n',
                "systemctl": 'exit 1\n',
                "uname": 'echo Linux\n', "id": 'echo "${FIXTURE_UID:-1000}"\n'}.items():
            script = fake / name
            script.write_text("#!/bin/sh\n" + body)
            script.chmod(0o755)
        self.home = self.root / "home"
        self.home.mkdir()
        self.env = {"PATH": str(fake) + ":/usr/bin:/bin", "HOME": str(self.home),
                    "CARGO_TARGET_DIR": str(self.root / "build"), "HGS_MOBILE_CONNECTOR": "0",
                    "HGS_RECOVERY_SERVICE": "0", "HGS_SWARM_SERVICE": "0", "HGS_VOICE_SERVICE": "0"}

    def invoke(self, **extra):
        result = subprocess.run(["bash", str(self.repo / "install.sh")], env=dict(self.env, **extra),
                                stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def test_deliberate_per_user_cli_upgrade_uses_the_same_refresh_helper(self):
        self.invoke()
        call = (self.home / "refresh-call").read_text()
        self.assertEqual(call, repr(["refresh", "--hgs", str(self.home / ".local/bin/hgs")]))

    def test_headless_pending_refresh_finishes_connector_and_worker_restarts(self):
        result = self.invoke(HGS_MOBILE_CONNECTOR="1", HGS_RECOVERY_SERVICE="1", HGS_SWARM_SERVICE="1",
                             HGS_VOICE_SERVICE="1", FIXTURE_REFRESH_STATUS="3")
        calls = [json.loads(line) for line in (self.home / "install-order").read_text().splitlines()]
        self.assertEqual(calls, [[worker, str(self.home / ".local/bin/hgs"), "--restart"]
                                for worker in ("recovery", "swarm", "voice")] + [["refresh"]])
        self.assertIn("Desktop backend refresh pending", result.stdout)
        self.assertIn("CLI, connector and worker installation completed", result.stdout)
        self.assertIn("Desktop service refresh pending", result.stderr)
        self.assertTrue((self.home / ".local/bin/zerus-mobile-connector").is_file())

    def test_staging_root_and_foreign_configurations_never_refresh(self):
        for extra in ({"HGS_INSTALL_PREFIX": str(self.root / "stage")}, {"DESTDIR": str(self.root / "package")},
                      {"HGS_CONFIG_DIR": str(self.root / "foreign-config")},
                      {"HGS_STATE_DIR": str(self.root / "foreign-state")}, {"FIXTURE_UID": "0"}):
            with self.subTest(extra=extra):
                self.invoke(**extra)
                self.assertFalse((self.home / "refresh-call").exists())


if __name__ == "__main__":
    unittest.main()
