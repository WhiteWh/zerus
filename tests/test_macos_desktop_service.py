"""Mac installer contract with private files and a modelled launchd/CLI only."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("mac_desktop_installer", ROOT / "scripts/install-macos-desktop-service.py")
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


class Commands:
    def __init__(self, home, binary):
        self.calls = []
        self.gui, self.loaded, self.pid = True, False, 4321
        self.config = "a" * 64
        self.destination = home / "Library/LaunchAgents" / (installer.LABEL + ".plist")
        self.argv = [str(binary), "desktop", "serve", "--config-dir", str(home / "config $literal"),
                     "--state-dir", str(home / "state %literal"), "--launchd"]
        definition = {"Label": installer.LABEL, "ProgramArguments": self.argv, "RunAtLoad": True,
                      "KeepAlive": {"SuccessfulExit": False}, "ThrottleInterval": 10,
                      "AbandonProcessGroup": True, "LimitLoadToSessionType": "Aqua"}
        content = plistlib.dumps(definition).decode().replace("?>", "?>\n<!-- " + installer.MARKER
                    + "; configuration=" + self.config + " -->", 1)
        self.plan = {"schema": 1, "operation": "preview", "configuration": self.config,
                     "label": installer.LABEL, "destination": str(self.destination), "argv": self.argv,
                     "definition": content, "session": {"kind": "aqua", "notification_bundle": "com.hgdev.hgs-tray"},
                     "readiness": {"abi": installer.ABI, "argv": [str(binary), "desktop", "status", "--json"],
                     "environment": {"HGS_CONFIG_DIR": self.argv[4], "HGS_STATE_DIR": self.argv[6]},
                     "expected_configuration": self.config, "configuration_field": "lifecycle.configuration",
                     "ready_field": "lifecycle.ipc_ready", "epoch_field": "lifecycle.epoch",
                     "generation_field": "lifecycle.generation"}}
        self.status = {"lifecycle": {"abi": installer.ABI, "configuration": self.config, "ipc_ready": True,
                        "mode": "passive",
                        "pid": self.pid, "epoch": "fixture-epoch", "generation": 1,
                        "background": {"attention": {"ready": False, "code": "not_started"}}}}
        self.printed_path, self.printed_argv = str(self.destination), list(self.argv)
        self.grant = {"abi": "desktop-ownership-v1", "configuration": self.config, "state": "service",
                      "identity_current": True, "rearm_required": False, "revision": 1,
                      **{key: str(uuid.uuid4()) for key in ("journal", "request_id", "owner_epoch")}}
        self.refuse_caller = False
        self.refuse_revoke = False
        self.revoke_receipts = {}
        self.revoke_authorities = {}
        self.held = False
        self.hold_alive = True
        self.holds = []
        self.before_hold = None
        self.after_revoke_check = None
        self.after_action = None
        self.during_probe = None
        self.release_failure = False
        self.hold_marker = True

    def rearm(self):
        if self.held:
            return False
        self.grant.update(state="service", rearm_required=False,
                          request_id=str(uuid.uuid4()), owner_epoch=str(uuid.uuid4()))
        return True

    def hold(self, hgs, env, configuration, request, receipt, help_text):
        test = self
        installer.require(installer._hold.MARKER in help_text.splitlines()
                          and receipt.get("configuration") == configuration
                          and receipt.get("request_id") == request
                          and "writer_epoch" in receipt
                          and (receipt["writer_epoch"] is None
                               or installer.ModeTransition.valid_uuid(receipt["writer_epoch"])),
                          "Invalid original rollback identity.")
        assert str(hgs) == self.argv[0]
        assert env["HGS_CONFIG_DIR"] == self.argv[4] and env["HGS_STATE_DIR"] == self.argv[6]
        class Held:
            finished = False

            def acquire(self):
                if test.before_hold:
                    test.before_hold()
                installer.require(test.grant["state"] == "revoked"
                                  and test.revoke_authorities[request] == test.grant["owner_epoch"],
                                  "writer_authority_changed")
                assert receipt == test.revoke_receipts[request]
                test.held = True
                test.holds.append(self)
                return self

            def check(self):
                installer.require(test.hold_alive and not self.finished, "original hold exited")

            def close(self, *, validate=True):
                if self.finished:
                    return
                test.held = False
                self.finished = True
                installer.require(not (validate and test.release_failure), "original release unverified")
        return Held()

    def __call__(self, argv, env=None, **kwargs):
        self.calls.append((list(argv), dict(env or {})))
        code, output = 0, ""
        if argv[1:] == ["--help"]:
            output = installer._hold.MARKER if self.hold_marker else "old CLI"
        elif argv[1:3] == ["desktop", "registration"]:
            if argv[3:] == ["mode-check", "--json"]:
                if self.revoke_receipts and self.after_revoke_check:
                    callback, self.after_revoke_check = self.after_revoke_check, None
                    callback()
                code = 1 if self.refuse_caller else 0
                output = json.dumps({"schema": 1, "configuration": self.config, "admitted": True, "source": "interactive_terminal"})
            else:
                value = copy.deepcopy(self.plan)
                if argv[-1] == "--background":
                    value["mode"] = "background"
                    value["argv"].insert(7, "--background")
                    definition = plistlib.loads(value["definition"].encode())
                    definition["ProgramArguments"] = value["argv"]
                    value["definition"] = plistlib.dumps(definition).decode().replace("?>", "?>\n<!-- " + installer.MARKER
                        + "; configuration=" + self.config + " -->", 1)
                output = json.dumps(value)
        elif argv[1:3] == ["desktop", "ownership-status"]:
            output = json.dumps(self.grant)
        elif argv[1:3] == ["desktop", "revoke"]:
            original = argv[argv.index("--request-id") + 1]
            record = json.loads(self.destination.with_name("." + installer.LABEL + ".zerus-registration.json").read_bytes())
            assert record["transition"]["request_id"] == original, "persist original before revoke"
            code = 1 if self.refuse_revoke else 0
            authority = self.revoke_authorities.setdefault(original, self.grant["owner_epoch"])
            old = self.revoke_receipts.get(original)
            if (old and self.grant["state"] == "service"
                    and authority != self.grant["owner_epoch"]):
                return subprocess.CompletedProcess(argv, 1, "", "writer_authority_changed")
            output = json.dumps(self.revoke_receipts.setdefault(original, {"request_id": original,
                "configuration": self.config, "accepted": True, "durable": True, "revoked": True, "checkpointed": True,
                "writer_epoch": self.grant["owner_epoch"]}))
            if not self.refuse_revoke:
                self.grant.update(state="revoked", rearm_required=True)
        elif argv[1:3] == ["desktop", "status"]:
            if self.during_probe:
                self.during_probe()
            output = json.dumps(self.status)
        elif argv[0] == "launchctl":
            if argv[1] == "print":
                if argv[2].count("/") == 1:
                    code = 0 if self.gui else 113
                else:
                    code = 0 if self.loaded else 113
                    output = "\tpath = " + self.printed_path + "\n\tprogram = " + self.printed_argv[0]
                    output += "\n\targuments = {\n" + "".join("\t\t" + arg + "\n" for arg in self.printed_argv)
                    output += "\t}\n\tpid = " + str(self.pid) + "\n"
            elif argv[1] == "bootout":
                self.loaded = False
            elif argv[1] == "bootstrap":
                self.printed_path = argv[3]
                self.printed_argv = plistlib.loads(Path(argv[3]).read_bytes())["ProgramArguments"]
                self.loaded = True
                self.status["lifecycle"]["mode"] = "background" if "--background" in self.printed_argv else "passive"
            if self.after_action is not None and self.after_action[0] == argv[1]:
                _, callback = self.after_action
                self.after_action = None
                callback()
        else:
            raise AssertionError(argv)
        return subprocess.CompletedProcess(argv, code, output, "")

    def mutations(self):
        return [argv for argv, _ in self.calls if argv[0] == "launchctl" and argv[1] != "print"]


class MacDesktopService(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="zerus-mac-desktop-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.home = self.root / "home"; self.home.mkdir()
        self.binary = self.root / "hgs"; self.binary.write_text("synthetic CLI, never executed")
        self.binary.chmod(0o755)
        self.command = Commands(self.home, self.binary)

    def install(self, **kwargs):
        kwargs.setdefault("hold_factory", self.command.hold)
        return installer.install(self.binary, self.home, {"HGS_EXECUTABLE": "/unselected/hgs"},
                                 command=self.command, **kwargs)

    def existing(self, content=None):
        path = self.command.destination
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(self.command.plan["definition"] if content is None else content)
        return path

    def mode_record(self):
        return self.command.destination.with_name("." + installer.LABEL + ".zerus-registration.json")

    def test_explicit_background_requires_native_grant_and_reloads_only_desktop(self):
        self.install()
        self.command.calls.clear()
        result = self.install(mode="background")
        self.assertTrue(result["ok"])
        self.assertEqual((result["selected_mode"], result["running_mode"]), ("background", "background"))
        self.assertIn("--background", plistlib.loads(self.command.destination.read_bytes())["ProgramArguments"])
        self.assertEqual([args[1] for args in self.command.mutations()], ["bootout", "bootstrap"])
        self.command.calls.clear()
        self.install(mode="background")
        self.assertEqual(self.command.mutations(), [])

    def test_mode_preserves_paired_backend_runner_and_cli_refresh_selection(self):
        runner = self.root / "stable-hgs"; runner.write_text("runner"); runner.chmod(0o755)
        self.install(runner_hgs=runner)
        result = self.install(mode="background")
        definition = plistlib.loads(self.command.destination.read_bytes())
        self.assertEqual(definition["ProgramArguments"][0], str(self.binary))
        self.assertEqual(definition["EnvironmentVariables"], {"HGS_EXECUTABLE": str(runner)})
        self.command.calls.clear()
        self.install()
        self.assertEqual(self.command.mutations(), [])
        stable = Commands(self.home, runner)
        def command(argv, **kwargs):
            if argv[:3] == [str(runner), "desktop", "registration"]:
                return stable(argv, **kwargs)
            return self.command(argv, **kwargs)
        installer.install(runner, self.home, {}, refresh_runner=True, command=command)
        definition = plistlib.loads(self.command.destination.read_bytes())
        self.assertIn("--background", definition["ProgramArguments"])
        self.assertEqual(definition["EnvironmentVariables"], {"HGS_EXECUTABLE": str(runner)})
        self.assertEqual(self.command.mutations(), [])

    def test_revoked_missing_stale_or_wrong_config_grant_refuses_before_any_manager_mutation(self):
        self.install()
        old = self.command.destination.read_bytes()
        for changes in ({"state": "passive"}, {"state": "revoked", "rearm_required": True},
                        {"state": "identity_changed", "identity_current": False}, {"configuration": "b" * 64},
                        {"owner_epoch": None}, {"revision": True}):
            grant = dict(self.command.grant)
            self.command.grant.update(changes)
            self.command.calls.clear()
            with self.subTest(changes=changes), self.assertRaisesRegex(installer.Refusal, "Grant/Rearm"):
                self.install(mode="background")
            self.assertEqual(self.command.destination.read_bytes(), old)
            self.assertEqual(self.command.mutations(), [])
            self.command.grant = grant

    def test_agent_markers_and_native_ancestry_refusal_block_both_mode_selections(self):
        self.install()
        original = self.command.destination.read_bytes()
        for mode in ("background", "passive"):
            for name in ("HGS_SESSION", "HGS_RUN_ID"):
                self.command.calls.clear()
                with self.assertRaisesRegex(installer.Refusal, "Agent sessions"):
                    installer.install(self.binary, self.home, {name: ""}, mode=mode, command=self.command)
                self.assertEqual(self.command.destination.read_bytes(), original)
                self.assertEqual(self.command.mutations(), [])
            self.command.refuse_caller = True
            with self.assertRaises(installer.Refusal):
                self.install(mode=mode)
            self.command.refuse_caller = False

    def test_passive_rollback_retains_original_revoke_id_before_bootout_and_never_rearms(self):
        self.install()
        self.install(mode="background")
        self.command.calls.clear()
        old = self.command.destination.read_bytes()
        self.command.refuse_revoke = True
        with self.assertRaises(installer.Refusal):
            self.install(mode="passive")
        pending = json.loads(self.mode_record().read_bytes())
        original = pending["transition"]["request_id"]
        self.assertEqual(self.command.destination.read_bytes(), old)
        self.assertEqual(self.command.mutations(), [])
        with self.assertRaisesRegex(installer.Refusal, "same --mode passive"):
            self.install(mode="background")
        with self.assertRaisesRegex(installer.Refusal, "same --mode passive"):
            self.install()
        self.command.refuse_revoke = False
        result = self.install(mode="passive")
        self.assertEqual(result["last_revoke_request_id"], original)
        requests = [args[args.index("--request-id") + 1] for args, _ in self.command.calls if "revoke" in args]
        self.assertEqual(requests, [original, original])
        self.assertNotIn("--background", plistlib.loads(self.command.destination.read_bytes())["ProgramArguments"])
        self.assertEqual(self.command.grant["state"], "revoked")
        with self.assertRaisesRegex(installer.Refusal, "Grant/Rearm"):
            self.install(mode="background")

    def test_custom_controls_and_no_aqua_refuse_mode_changes_without_overwrite(self):
        self.install()
        original = self.command.destination.read_bytes()
        self.command.gui = False
        with self.assertRaisesRegex(installer.Refusal, "Aqua login"):
            self.install(mode="background")
        self.command.gui = True
        definition = plistlib.loads(original)
        definition["ProcessType"] = "Background"
        self.command.destination.write_bytes(plistlib.dumps(definition).replace(b"?>", b"?>\n<!-- " + installer.MARKER.encode()
            + b"; configuration=" + self.command.config.encode() + b" -->", 1))
        changed = self.command.destination.read_bytes()
        self.command.calls.clear()
        with self.assertRaisesRegex(installer.Refusal, "Custom desktop"):
            self.install(mode="background")
        self.assertEqual(self.command.destination.read_bytes(), changed)
        self.assertEqual(self.command.mutations(), [])

    def test_rearm_after_the_receipt_cannot_win_during_the_held_transition(self):
        self.install()
        self.install(mode="background")
        outcomes = []
        self.command.after_revoke_check = lambda: outcomes.append(self.command.rearm())
        result = self.install(mode="passive")
        self.assertEqual(outcomes, [False])
        self.assertEqual(result["running_mode"], "passive")
        self.assertEqual(self.command.grant["state"], "revoked")
        self.assertFalse(self.command.held)
        self.assertTrue(self.command.holds[0].finished)

    def test_rearm_winning_before_the_hold_has_an_explicit_record_only_exit(self):
        self.install()
        self.install(mode="background")
        original = self.command.destination.read_bytes()
        self.command.before_hold = lambda: self.assertTrue(self.command.rearm())
        self.command.calls.clear()
        with self.assertRaisesRegex(installer.Refusal, "cancel-rollback"):
            self.install(mode="passive")
        request = json.loads(self.mode_record().read_bytes())["transition"]["request_id"]
        grant, receipts = copy.deepcopy(self.command.grant), copy.deepcopy(self.command.revoke_receipts)
        self.assertEqual(self.command.destination.read_bytes(), original)
        self.assertEqual(self.command.mutations(), [])
        with self.assertRaisesRegex(installer.Refusal, "writer_authority_changed"):
            self.install(mode="passive")
        self.command.calls.clear()
        result = self.install(cancel_rollback=request)
        self.install(cancel_rollback=request)
        self.assertEqual(result["operation"], "cancel-rollback")
        self.assertFalse(any(argv[0] == "launchctl" or "revoke" in argv for argv, _ in self.command.calls))
        self.assertEqual(self.command.grant, grant)
        self.assertEqual(self.command.revoke_receipts, receipts)
        self.assertEqual(self.command.destination.read_bytes(), original)
        record = json.loads(self.mode_record().read_bytes())
        self.assertIsNone(record["transition"])
        self.assertEqual(record["selected_mode"], "background")
        self.assertEqual(record["last_cancelled_rollback_request_id"], request)
        self.command.before_hold = None
        result = self.install(mode="passive")
        self.assertNotEqual(result["last_revoke_request_id"], request)
        self.assertEqual(result["last_cancelled_rollback_request_id"], request)

    def test_cancel_before_any_revoke_keeps_the_original_service_grant_and_unit(self):
        self.install()
        self.install(mode="background")
        request = str(uuid.uuid4())
        record = json.loads(self.mode_record().read_bytes())
        record["transition"] = {"target": "passive", "request_id": request}
        self.mode_record().write_text(json.dumps(record))
        unit, grant = self.command.destination.read_bytes(), copy.deepcopy(self.command.grant)
        self.command.calls.clear()
        result = self.install(cancel_rollback=request)
        self.assertEqual(result["last_cancelled_rollback_request_id"], request)
        self.assertEqual(self.command.grant, grant)
        self.assertEqual(self.command.revoke_receipts, {})
        self.assertEqual(self.command.destination.read_bytes(), unit)
        self.assertFalse(any(argv[0] == "launchctl" or "revoke" in argv for argv, _ in self.command.calls))

    def test_cancellation_requires_exact_pending_uuid_current_service_grant_and_native_caller(self):
        self.install()
        self.install(mode="background")
        request = str(uuid.uuid4())
        record = json.loads(self.mode_record().read_bytes())
        record["transition"] = {"target": "passive", "request_id": request}
        self.mode_record().write_text(json.dumps(record))
        original, unit = self.mode_record().read_bytes(), self.command.destination.read_bytes()
        self.command.calls.clear()
        for change in ({"state": "revoked", "rearm_required": True}, {"state": "passive"},
                       {"identity_current": False}, {"configuration": "b" * 64}, {"owner_epoch": None}):
            grant = dict(self.command.grant)
            self.command.grant.update(change)
            with self.subTest(change=change), self.assertRaisesRegex(installer.Refusal, "Grant/Rearm"):
                self.install(cancel_rollback=request)
            self.command.grant = grant
        self.command.refuse_caller = True
        with self.assertRaises(installer.Refusal):
            self.install(cancel_rollback=request)
        self.command.refuse_caller = False
        for wrong in (str(uuid.uuid4()), "invalid"):
            with self.assertRaises(installer.Refusal):
                self.install(cancel_rollback=wrong)
        with self.assertRaises(installer.Refusal):
            self.install(mode="passive", cancel_rollback=request)
        self.assertEqual(self.mode_record().read_bytes(), original)
        self.assertEqual(self.command.destination.read_bytes(), unit)
        self.assertFalse(any(argv[0] == "launchctl" or "revoke" in argv for argv, _ in self.command.calls))

    def test_lost_hold_before_or_after_bootout_admits_no_later_manager_step(self):
        self.install()
        self.install(mode="background")
        original = self.command.destination.read_bytes()
        self.command.after_revoke_check = lambda: setattr(self.command, "hold_alive", False)
        self.command.calls.clear()
        with self.assertRaisesRegex(installer.Refusal, "hold exited"):
            self.install(mode="passive")
        self.assertEqual(self.command.mutations(), [])
        self.command.hold_alive = True
        self.command.after_action = ("bootout", lambda: setattr(self.command, "hold_alive", False))
        self.command.calls.clear()
        with self.assertRaisesRegex(installer.Refusal, "hold exited"):
            self.install(mode="passive")
        self.assertEqual([argv[1] for argv in self.command.mutations()], ["bootout"])
        self.assertEqual(self.command.destination.read_bytes(), original)
        self.assertIsNotNone(json.loads(self.mode_record().read_bytes())["transition"])
        self.assertFalse(self.command.held)

    def test_cancellation_cannot_overwrite_a_record_changed_during_native_checks(self):
        from unittest.mock import patch
        self.install()
        self.install(mode="background")
        record = json.loads(self.mode_record().read_bytes())
        request = str(uuid.uuid4())
        record["transition"] = {"target": "passive", "request_id": request}
        self.mode_record().write_text(json.dumps(record))
        caller = installer.ModeTransition.caller
        replacement = copy.deepcopy(record)
        replacement["transition"]["request_id"] = str(uuid.uuid4())
        def replace_during_check(value):
            caller(value)
            self.mode_record().write_text(json.dumps(replacement))
        self.command.calls.clear()
        with patch.object(installer.ModeTransition, "caller", replace_during_check):
            with self.assertRaisesRegex(installer.Refusal, "changed during cancellation"):
                self.install(cancel_rollback=request)
        self.assertEqual(json.loads(self.mode_record().read_bytes()), replacement)
        self.assertFalse(any(argv[0] == "launchctl" or "revoke" in argv for argv, _ in self.command.calls))

    def test_unverified_original_release_keeps_the_pending_transition(self):
        self.install()
        self.install(mode="background")
        self.command.release_failure = True
        with self.assertRaisesRegex(installer.Refusal, "release unverified"):
            self.install(mode="passive")
        record = json.loads(self.mode_record().read_bytes())
        self.assertIsNotNone(record["transition"])
        self.assertTrue(record["pending_restart"])
        self.assertTrue(self.command.holds[0].finished)

    def test_old_cli_without_the_hold_marker_refuses_before_revoke_or_pending_write(self):
        self.install()
        self.install(mode="background")
        original = self.mode_record().read_bytes()
        self.command.hold_marker = False
        self.command.calls.clear()
        with self.assertRaisesRegex(installer.Refusal, "native rollback hold"):
            self.install(mode="passive")
        self.assertEqual(self.mode_record().read_bytes(), original)
        self.assertEqual(self.command.revoke_receipts, {})
        self.assertEqual(self.command.mutations(), [])

    def test_the_same_original_hold_spans_pending_ready_and_receipt_probes(self):
        self.install()
        self.install(mode="background")
        observed = []
        def probe():
            if self.command.status["lifecycle"]["mode"] != "passive":
                return
            self.assertTrue(self.command.held, "hold released before passive readiness")
            self.assertEqual(len(self.command.holds), 1)
            original = self.command.holds[0]
            self.assertFalse(original.finished)
            self.assertFalse(self.command.rearm())
            observed.append(original)
            self.command.status["lifecycle"]["ipc_ready"] = len(observed) > 1
        self.command.during_probe = probe
        result = self.install(mode="passive")
        self.command.during_probe = None
        self.assertEqual(observed, [self.command.holds[0]] * 3)
        self.assertTrue(observed[0].finished)
        self.assertIsNone(result["transition"])
        self.assertFalse(result["pending_restart"])

    def test_missing_original_writer_uses_the_closed_null_epoch_receipt(self):
        self.install()
        self.install(mode="background")
        self.command.refuse_revoke = True
        with self.assertRaises(installer.Refusal):
            self.install(mode="passive")
        request = json.loads(self.mode_record().read_bytes())["transition"]["request_id"]
        self.command.revoke_receipts[request]["writer_epoch"] = None
        self.command.refuse_revoke = False
        result = self.install(mode="passive")
        self.assertEqual(result["last_revoke_request_id"], request)

    def test_installed_helper_requires_its_shipped_sibling_hold_module(self):
        scripts = self.root / "standalone-setup"
        scripts.mkdir()
        helper = scripts / "install-macos-desktop-service.py"
        helper.write_bytes((ROOT / "scripts" / helper.name).read_bytes())
        module = scripts / "desktop-revoke-hold.py"
        module.write_bytes((ROOT / "scripts" / module.name).read_bytes())
        result = subprocess.run([sys.executable, str(helper), "--help"], cwd=self.root,
                                capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--cancel-rollback", result.stdout)
        module.unlink()
        result = subprocess.run([sys.executable, str(helper), "--help"], cwd=self.root,
                                capture_output=True, text=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)

    def test_custom_environment_is_actionable_and_mode_cannot_change_the_paired_runner(self):
        self.install()
        definition = plistlib.loads(self.command.destination.read_bytes())
        definition["EnvironmentVariables"] = "unverified"
        raw = plistlib.dumps(definition).replace(b"?>", b"?>\n<!-- " + installer.MARKER.encode()
            + b"; configuration=" + self.command.config.encode() + b" -->", 1)
        self.command.destination.write_bytes(raw)
        with self.assertRaisesRegex(installer.Refusal, "Custom desktop environment"):
            self.install(mode="background")
        self.assertEqual(self.command.destination.read_bytes(), raw)
        self.command.destination.write_text(self.command.plan["definition"])
        runner = self.root / "stable-hgs"; runner.write_text("runner"); runner.chmod(0o755)
        other = self.root / "other-hgs"; other.write_text("other"); other.chmod(0o755)
        self.install(runner_hgs=runner)
        raw = self.command.destination.read_bytes()
        self.command.calls.clear()
        with self.assertRaisesRegex(installer.Refusal, "cannot change the independent runner"):
            self.install(mode="background", runner_hgs=other)
        self.assertEqual(self.command.destination.read_bytes(), raw)
        self.assertEqual(self.command.mutations(), [])

    def test_status_is_read_only_and_distinguishes_selection_from_running_mode(self):
        self.install()
        self.install(mode="background")
        original = self.mode_record().read_bytes()
        self.command.calls.clear()
        value = self.install(status_only=True)
        self.assertEqual((value["selected_mode"], value["running_mode"]), ("background", "background"))
        self.assertEqual(self.command.mutations(), [])
        self.assertEqual(self.mode_record().read_bytes(), original)
        self.command.loaded = False
        value = self.install(status_only=True)
        self.assertIsNone(value["running_mode"])
        self.assertFalse(value["ipc_ready"])

    def test_wrong_or_incomplete_revoke_keeps_the_original_unit_and_uuid(self):
        self.install()
        self.install(mode="background")
        original = self.command.destination.read_bytes()
        self.command.refuse_revoke = True
        with self.assertRaises(installer.Refusal):
            self.install(mode="passive")
        request = json.loads(self.mode_record().read_bytes())["transition"]["request_id"]
        self.command.refuse_revoke = False
        for change in ({"configuration": "b" * 64}, {"request_id": str(uuid.uuid4())},
                       {"accepted": False}, {"durable": False}, {"revoked": False}, {"checkpointed": False}):
            self.command.revoke_receipts[request] = {"request_id": request, "configuration": self.command.config,
                "accepted": True, "durable": True, "revoked": True, "checkpointed": True,
                "writer_epoch": self.command.grant["owner_epoch"], **change}
            self.command.calls.clear()
            with self.subTest(change=change), self.assertRaisesRegex(installer.Refusal, "not settled"):
                self.install(mode="passive")
            self.assertEqual(self.command.destination.read_bytes(), original)
            self.assertEqual(json.loads(self.mode_record().read_bytes())["transition"]["request_id"], request)
            self.assertEqual(self.command.mutations(), [])

    def test_concurrent_transition_and_symlinked_metadata_cannot_replace_owned_state(self):
        self.install()
        original = self.command.destination.read_bytes()
        with installer.registration_lock(self.home):
            with self.assertRaises(BlockingIOError):
                self.install(mode="background")
        self.assertEqual(self.command.destination.read_bytes(), original)
        target = self.root / "foreign-mode-record"
        target.write_text("kept")
        self.mode_record().symlink_to(target)
        self.command.calls.clear()
        with self.assertRaises(installer.Refusal):
            self.install(mode="background")
        self.assertEqual(target.read_text(), "kept")
        self.assertEqual(self.command.mutations(), [])

    def test_mode_and_check_remove_or_runner_refresh_are_not_combined(self):
        self.install()
        self.command.calls.clear()
        for flags in ({"remove": True}, {"check_only": True}, {"refresh_runner": True}, {"status_only": True}):
            with self.assertRaisesRegex(installer.Refusal, "cannot be combined"):
                self.install(mode="background", **flags)
        self.assertEqual(self.command.mutations(), [])

    def test_aqua_registration_and_readiness_preserve_the_selected_config_and_cli(self):
        self.assertEqual(self.install(), "ready")
        definition = plistlib.loads(self.command.destination.read_bytes())
        self.assertEqual(definition["ProgramArguments"], self.command.argv)
        self.assertEqual(definition["LimitLoadToSessionType"], "Aqua")
        self.assertIs(definition["AbandonProcessGroup"], True)
        status_env = next(env for argv, env in self.command.calls if argv[1:3] == ["desktop", "status"])
        self.assertEqual(status_env["HGS_EXECUTABLE"], str(self.binary))
        self.assertEqual(status_env["HGS_STATE_DIR"], self.command.argv[6])
        self.assertEqual(status_env["HGS_CONFIG_DIR"], self.command.argv[4])
        self.assertFalse(self.command.status["lifecycle"]["background"]["attention"]["ready"])

    def test_gui_only_install_retains_an_unchanged_active_backend(self):
        self.existing(); self.command.loaded = True
        self.assertEqual(self.install(), "ready")
        self.assertEqual(self.command.mutations(), [])

    def test_separate_runner_never_changes_the_selected_service_registration(self):
        runner = self.root / "stable-hgs"; runner.write_text("independent runner"); runner.chmod(0o755)
        self.assertEqual(self.install(runner_hgs=runner), "ready")
        definition = plistlib.loads(self.command.destination.read_bytes())
        self.assertEqual(definition["ProgramArguments"][0], str(self.binary))
        self.assertEqual(definition["EnvironmentVariables"], {"HGS_EXECUTABLE": str(runner)})
        self.assertTrue(all(env["HGS_EXECUTABLE"] == str(self.binary) for _, env in self.command.calls))
        self.command.calls.clear()
        self.assertEqual(self.install(runner_hgs=runner), "ready")
        self.assertEqual(self.command.mutations(), [])

    def test_cli_only_refresh_retains_the_owned_backend_and_its_unchanged_process(self):
        runner = self.root / "stable-hgs"; runner.write_text("independent runner"); runner.chmod(0o755)
        self.install(runner_hgs=runner)
        self.command.calls.clear()
        # The root installer calls the stable CLI, whose own preview still names
        # itself. The old owned plist retains the original service selection.
        stable = Commands(self.home, runner)
        def command(argv, **kwargs):
            if argv[:3] == [str(runner), "desktop", "registration"]:
                return stable(argv, **kwargs)
            return self.command(argv, **kwargs)
        self.assertEqual(installer.install(runner, self.home, {}, restart=True,
            refresh_runner=True, command=command), "ready")
        definition = plistlib.loads(self.command.destination.read_bytes())
        self.assertEqual(definition["ProgramArguments"][0], str(self.binary))
        self.assertEqual(definition["EnvironmentVariables"]["HGS_EXECUTABLE"], str(runner))
        self.assertEqual(self.command.mutations(), [])

    def test_runner_link_in_bundle_or_missing_backend_is_never_substituted(self):
        runner = self.root / "stable-hgs"
        runner.symlink_to(self.binary)
        with self.assertRaises(installer.Refusal): self.install(runner_hgs=runner)
        runner.unlink(); runner.write_text("independent runner"); runner.chmod(0o755)
        self.install(runner_hgs=runner)
        original = self.command.destination.read_bytes()
        self.binary.unlink()
        with self.assertRaises(installer.Refusal):
            self.install(runner_hgs=runner)
        self.assertEqual(self.command.destination.read_bytes(), original)

    def unpaired_rollback_refresh(self, mode):
        bundled = self.root / "hgs-tray.app/Contents/MacOS/hgs"
        bundled.parent.mkdir(parents=True)
        bundled.write_text("paired CLI"); bundled.chmod(0o755)
        paired = Commands(self.home, bundled)
        self.assertEqual(installer.install(bundled, self.home, {}, runner_hgs=self.binary,
                                          command=paired), "ready")
        if mode == "background":
            installer.install(bundled, self.home, {}, runner_hgs=self.binary, command=paired,
                              mode=mode, hold_factory=paired.hold)
        original_argv = plistlib.loads(paired.destination.read_bytes())["ProgramArguments"]
        self.command.calls.clear()
        # Model update-installed.py restoring the unpaired Qt app at the SAME
        # location; the old registration remains until this CLI-only refresh.
        bundled.unlink()
        qt = bundled.with_name("hgs-tray"); qt.write_text("Qt rollback"); qt.chmod(0o755)
        (bundled.parent.parent / "Info.plist").write_bytes(plistlib.dumps({
            "CFBundleIdentifier": "com.hgdev.hgs-tray", "CFBundleExecutable": "hgs-tray"}))
        self.command.loaded = True
        self.command.printed_argv = original_argv
        self.command.status["lifecycle"]["mode"] = mode
        self.assertEqual(self.install(refresh_runner=True, restart=True), "ready")
        restored = plistlib.loads(self.command.destination.read_bytes())
        expected = list(self.command.argv)
        if mode == "background":
            expected.insert(7, "--background")
        self.assertEqual(restored["ProgramArguments"], expected)
        self.assertNotIn("EnvironmentVariables", restored)
        self.assertEqual([args[1] for args in self.command.mutations()], ["bootout", "bootstrap"])
        self.assertFalse(bundled.exists())
        self.assertEqual(qt.read_text(), "Qt rollback")

    def test_cli_refresh_repairs_a_paired_registration_after_unpaired_qt_rollback(self):
        self.unpaired_rollback_refresh("passive")

    def test_cli_refresh_after_unpaired_rollback_preserves_the_owned_background_selection(self):
        self.unpaired_rollback_refresh("background")
        self.assertEqual(json.loads(self.mode_record().read_bytes())["selected_mode"], "background")

    def test_cli_refresh_never_repairs_a_pair_with_its_helper_missing(self):
        bundled = self.root / "hgs-tray.app/Contents/MacOS/hgs"
        bundled.parent.mkdir(parents=True)
        bundled.write_text("paired CLI"); bundled.chmod(0o755)
        paired = Commands(self.home, bundled)
        installer.install(bundled, self.home, {}, runner_hgs=self.binary, command=paired)
        bundled.unlink()
        qt = bundled.with_name("hgs-tray"); qt.write_text("app"); qt.chmod(0o755)
        contents = bundled.parent.parent
        (contents / "Info.plist").write_bytes(plistlib.dumps({
            "CFBundleIdentifier": "com.hgdev.hgs-tray", "CFBundleExecutable": "hgs-tray"}))
        (contents / "Resources").mkdir()
        record = contents / "Resources/zerus-local-pair.json"; record.write_text("retained pair")
        original = self.command.destination.read_bytes()
        with self.assertRaises((installer.Refusal, OSError)):
            self.install(refresh_runner=True, restart=True)
        self.assertEqual(self.command.destination.read_bytes(), original)
        self.assertEqual(self.command.mutations(), [])

    def test_backend_upgrade_reloads_only_the_owned_desktop_label(self):
        self.existing(); self.command.loaded = True
        self.assertEqual(self.install(restart=True), "ready")
        domain = f"gui/{os.getuid()}"
        self.assertEqual(self.command.mutations(), [
            ["launchctl", "bootout", domain + "/" + installer.LABEL],
            ["launchctl", "bootstrap", domain, str(self.command.destination)]])

    def test_custom_definition_refuses_before_launchd_and_keeps_all_bytes(self):
        original = self.command.plan["definition"]
        for content in (original.replace(installer.MARKER, "Unmanaged definition"),
                        original.replace(self.command.config, "b" * 64),
                        original.replace(self.command.config, self.command.config + "b"),
                        original.replace("<string>" + installer.LABEL, "<string>com.example.foreign")):
            path = self.existing(content)
            with self.subTest(content=content), self.assertRaisesRegex(installer.Refusal, "custom"):
                self.install(restart=True)
            self.assertEqual(path.read_text(), content)
        self.assertFalse(any(argv[0] == "launchctl" for argv, _ in self.command.calls))

    def test_owned_old_cli_path_is_preflighted_and_replaced_using_its_loaded_argv(self):
        old_argv = list(self.command.argv)
        old_argv[0] = str(self.root / "source-install/hgs")  # The old binary need not still exist.
        old = self.command.plan["definition"].replace(self.command.argv[0], old_argv[0])
        path = self.existing(old)
        self.command.loaded = True; self.command.printed_argv = old_argv
        self.assertEqual(self.install(check_only=True), "checked")
        self.assertEqual(path.read_text(), old)
        self.assertEqual(self.command.mutations(), [])
        inode = path.stat().st_ino
        self.assertEqual(self.install(), "ready")
        self.assertEqual(path.read_text(), self.command.plan["definition"])
        self.assertNotEqual(path.stat().st_ino, inode)
        self.assertEqual(self.command.printed_argv, self.command.argv)
        self.assertEqual([args[1] for args in self.command.mutations()], ["bootout", "bootstrap"])

    def test_owned_definition_drift_can_be_upgraded_or_removed(self):
        original = self.command.plan["definition"]
        old = original.replace("<integer>10</integer>", "<integer>30</integer>")
        prefix, suffix = old.rsplit("</dict>", 1)
        old = prefix + "<key>ProcessType</key><string>Background</string></dict>" + suffix
        for remove in (False, True):
            with self.subTest(remove=remove):
                path = self.existing(old); self.command.loaded = True
                self.assertEqual(self.install(remove=remove), "removed" if remove else "ready")
                self.assertEqual(path.exists(), not remove)
                if not remove:
                    self.assertEqual(path.read_text(), original)

    def test_owned_old_cli_path_can_be_removed_after_the_binary_has_moved(self):
        old_argv = list(self.command.argv); old_argv[0] = str(self.root / "removed-source/hgs")
        path = self.existing(self.command.plan["definition"].replace(self.command.argv[0], old_argv[0]))
        self.command.loaded = True; self.command.printed_argv = old_argv
        self.assertEqual(self.install(remove=True), "removed")
        self.assertFalse(path.exists())
        self.assertFalse(Path(old_argv[0]).exists())
        self.assertEqual([args[1] for args in self.command.mutations()], ["bootout"])

    def test_owned_marker_does_not_make_malformed_or_nonservice_arguments_safe(self):
        original = self.command.plan["definition"]
        for argv in ([], ["relative/hgs", "desktop", "serve"], ["/private/example/agent", "run", "session"]):
            definition = plistlib.loads(original.encode() if isinstance(original, str) else original)
            definition["ProgramArguments"] = argv
            content = plistlib.dumps(definition).decode().replace("?>", "?>\n<!-- " + installer.MARKER
                        + "; configuration=" + self.command.config + " -->", 1)
            path = self.existing(content)
            with self.subTest(argv=argv), self.assertRaisesRegex(installer.Refusal, "invalid arguments"):
                self.install(restart=True)
            self.assertEqual(path.read_text(), content)
        self.assertFalse(any(args[0] == "launchctl" for args, _ in self.command.calls))

    def test_loaded_old_definition_must_match_old_not_new_argv(self):
        old = self.command.plan["definition"].replace(self.command.argv[0], str(self.root / "old/hgs"))
        self.existing(old); self.command.loaded = True
        # The loaded argv matches the new CLI but not the owned definition on disk.
        with self.assertRaisesRegex(installer.Refusal, "Loaded"):
            self.install()
        self.assertEqual(self.command.mutations(), [])

    def test_owned_reload_keeps_unknown_process_group_protection(self):
        old = self.command.plan["definition"].replace("<key>AbandonProcessGroup</key>\n\t<true/>",
                                                                  "<key>AbandonProcessGroup</key>\n\t<false/>")
        self.assertNotEqual(old, self.command.plan["definition"])
        path = self.existing(old); self.command.loaded = True
        with self.assertRaisesRegex(installer.Refusal, "process group"):
            self.install(restart=True)
        self.assertEqual(path.read_text(), old)
        self.assertEqual(self.command.mutations(), [])

    def test_symlink_and_actual_fifo_definition_refuse_before_open(self):
        path = self.command.destination; path.parent.mkdir(parents=True)
        foreign = self.root / "foreign"; foreign.write_text("keep")
        path.symlink_to(foreign)
        with self.assertRaises(installer.Refusal): self.install()
        path.unlink(); os.mkfifo(path)
        with self.assertRaises(installer.Refusal): self.install()
        self.assertEqual(foreign.read_text(), "keep")
        self.assertEqual(self.command.mutations(), [])

    def test_linked_directory_spine_refuses_before_cli_or_foreign_reads(self):
        foreign = self.root / "foreign-directory"; foreign.mkdir()
        (self.home / "Library").symlink_to(foreign, target_is_directory=True)
        with self.assertRaises(installer.Refusal): self.install()
        self.assertEqual(self.command.calls, [])
        self.assertEqual(list(foreign.iterdir()), [])

    def test_loaded_foreign_path_or_arguments_are_never_unloaded(self):
        self.existing(); self.command.loaded = True
        for path, argv in [("/private/example/custom.plist", self.command.argv),
                           (str(self.command.destination), [*self.command.argv[:-1], "--custom"])]:
            self.command.printed_path, self.command.printed_argv = path, argv
            with self.assertRaisesRegex(installer.Refusal, "Loaded"):
                self.install(restart=True)
        self.assertEqual(self.command.mutations(), [])

    def test_check_only_creates_no_directories_and_loads_nothing(self):
        self.assertEqual(self.install(check_only=True), "checked")
        self.assertFalse((self.home / "Library").exists())
        self.assertEqual(self.command.mutations(), [])

    def test_replaced_definition_is_kept_even_when_its_bytes_match(self):
        path = self.existing()
        inode = path.stat().st_ino
        def replace(argv, **kwargs):
            result = self.command(argv, **kwargs)
            if argv == ["launchctl", "print", f"gui/{os.getuid()}"]:
                temporary = path.with_suffix(".replacement")
                temporary.write_bytes(path.read_bytes()); os.replace(temporary, path)
            return result
        with self.assertRaisesRegex(installer.Refusal, "changed during preflight"):
            installer.install(self.binary, self.home, {}, command=replace, restart=True)
        self.assertNotEqual(path.stat().st_ino, inode)
        self.assertEqual(self.command.mutations(), [])

    def test_missing_aqua_session_is_deferred_without_a_readiness_claim(self):
        self.command.gui = False
        self.assertEqual(self.install(), "deferred")
        self.assertTrue(self.command.destination.exists())
        self.assertEqual(self.command.mutations(), [])
        self.assertFalse(any(argv[1:3] == ["desktop", "status"] for argv, _ in self.command.calls))

    def test_mismatched_abi_config_or_actor_refuses_readiness(self):
        for field, value in [("abi", "unknown"), ("configuration", "b" * 64), ("pid", 9999)]:
            self.command.status["lifecycle"][field] = value
            with self.subTest(field=field), self.assertRaises(installer.Refusal): self.install()
            self.command.status["lifecycle"][field] = {"abi": installer.ABI, "configuration": self.command.config, "pid": self.command.pid}[field]

    def test_unavailable_ipc_is_bounded_and_never_reported_ready(self):
        self.command.status["lifecycle"]["ipc_ready"] = False
        ticks = iter(range(0, 100, 10))
        with self.assertRaisesRegex(installer.Refusal, "did not become ready"):
            self.install(clock=lambda: next(ticks), sleep=lambda _: None)

    def test_remove_touches_only_its_owned_definition(self):
        self.existing(); self.command.loaded = True
        other = self.command.destination.parent / "com.hgdev.hgs-recovery.plist"; other.write_text("keep")
        self.assertEqual(self.install(remove=True), "removed")
        self.assertFalse(self.command.destination.exists())
        self.assertEqual(other.read_text(), "keep")
        self.assertEqual(len(self.command.mutations()), 1)

    def test_registration_cannot_change_scope_or_probe_command(self):
        original = copy.deepcopy(self.command.plan)
        for field, value in [("destination", "/private/example/foreign.plist"),
                             ("readiness", {"argv": ["unselected", "command"]}),
                             ("session", {"kind": "system"})]:
            self.command.plan = copy.deepcopy(original); self.command.plan[field] = value
            with self.subTest(field=field), self.assertRaises(installer.Refusal): self.install()
        self.assertFalse((self.home / "Library").exists())

    def test_command_output_and_timeout_are_bounded_with_owned_child_cleanup(self):
        env = {"HOME": str(self.home), "PATH": "/usr/bin:/bin"}
        for key in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME",
                    "XDG_RUNTIME_DIR", "TMPDIR", "TMUX_TMPDIR"):
            directory = self.home / key.lower(); directory.mkdir()
            env[key] = str(directory)
        with self.assertRaisesRegex(installer.Refusal, "bound"):
            installer.run([sys.executable, "-c", "import sys; sys.stdout.write('x'*262144)"], env=env)
        with self.assertRaisesRegex(installer.Refusal, "timed out"):
            installer.run([sys.executable, "-c", "import time; time.sleep(10)"], env=env, timeout=0.05)

    def source_install_fixture(self):
        source = self.root / "source"; (source / "scripts").mkdir(parents=True)
        (source / "install.sh").write_bytes((ROOT / "install.sh").read_bytes())
        shim = source / "hgs_state.py"; shim.write_text("first shim")
        calls = self.root / "desktop-install.calls"
        workers = self.root / "worker-install.calls"
        (source / "scripts/install-macos-session-service.py").write_text("pass\n")
        agents = self.home / "Library/LaunchAgents"; agents.mkdir(parents=True)
        for label in ("com.hgdev.hgs-recovery", "com.hgdev.hgs-swarm", "com.hgdev.hgs-voice"):
            (agents / (label + ".plist")).write_text("existing worker definition")
        for name in ("recovery", "swarm", "voice"):
            (source / f"scripts/install-{name}-service.py").write_text(
                "import json, sys\nfrom pathlib import Path\nwith Path(" + repr(str(workers))
                + ").open('a') as log: log.write(json.dumps([" + repr(name) + ",sys.argv[2:]])+'\\n')\n")
        (source / "scripts/install-macos-desktop-service.py").write_text(
            "import sys\nfrom pathlib import Path\nwith Path(" + repr(str(calls))
            + ").open('a') as log: log.write(' '.join(sys.argv[2:])+'\\n')\n")
        tools = self.root / "tools"; tools.mkdir()
        for name, text in {
            "uname": "#!/bin/sh\necho Darwin\n",
            "cargo": "#!/bin/sh\nmkdir -p \"$CARGO_TARGET_DIR/release\"\n"
                     "printf '#!/bin/sh\\necho %s\\n' \"$FIXTURE_REVISION\" > \"$CARGO_TARGET_DIR/release/hgs\"\n"
                     "chmod 755 \"$CARGO_TARGET_DIR/release/hgs\"\n",
        }.items():
            path = tools / name; path.write_text(text); path.chmod(0o755)
        env = {"HOME": str(self.home), "PATH": str(tools) + ":/usr/bin:/bin",
               "CARGO_TARGET_DIR": str(self.root / "target"), "HGS_MOBILE_CONNECTOR": "0",
               "PYTHONDONTWRITEBYTECODE": "1"}
        for key in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME",
                    "XDG_RUNTIME_DIR", "TMPDIR", "TMUX_TMPDIR"):
            directory = self.home / key.lower(); directory.mkdir()
            env[key] = str(directory)
        return source, shim, calls, workers, agents, env

    def test_source_install_restarts_desktop_only_for_changed_cli_bytes(self):
        source, shim, calls, workers, agents, env = self.source_install_fixture()
        for revision, content in [("one", "first shim"), ("one", "first shim"),
                                  ("one", "second shim"), ("two", "second shim")]:
            shim.write_text(content)
            result = subprocess.run(["bash", str(source / "install.sh")],
                env=dict(env, FIXTURE_REVISION=revision), capture_output=True, text=True, timeout=20)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(calls.read_text().splitlines(), ["--refresh-runner --restart", "--refresh-runner", "--refresh-runner", "--refresh-runner --restart"])
        self.assertEqual([json.loads(line) for line in workers.read_text().splitlines()],
                         [[name, args] for args in (["--restart"], [], ["--restart"], ["--restart"])
                          for name in ("recovery", "swarm", "voice")])
        self.assertTrue(all(path.read_text() == "existing worker definition" for path in agents.iterdir()))

    def test_source_install_finishes_worker_updates_before_desktop_success_or_refusal(self):
        source, _, _, workers, _, env = self.source_install_fixture()
        for exit_code in (0, 1):
            with self.subTest(exit_code=exit_code):
                workers.unlink(missing_ok=True)
                (source / "scripts/install-macos-desktop-service.py").write_text(
                    "import json, sys\nfrom pathlib import Path\nwith Path(" + repr(str(workers))
                    + ").open('a') as log: log.write(json.dumps(['desktop',sys.argv[2:]])+'\\n')\n"
                    + f"raise SystemExit({exit_code})\n")
                result = subprocess.run(["bash", str(source / "install.sh")],
                    env=dict(env, FIXTURE_REVISION=f"case-{exit_code}"), capture_output=True, text=True, timeout=20)
                self.assertEqual(result.returncode, exit_code, result.stdout + result.stderr)
                self.assertEqual([json.loads(line) for line in workers.read_text().splitlines()],
                                 [[name, ["--restart"]] for name in ("recovery", "swarm", "voice")]
                                 + [["desktop", ["--refresh-runner", "--restart"]]])


if __name__ == "__main__":
    unittest.main()
