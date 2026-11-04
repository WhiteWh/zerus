"""The Homebrew application's setup must not take over or restart existing installations."""
import contextlib
import importlib.util
import io
import os
from pathlib import Path
import plistlib
import re
import runpy
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
SCRIPT = ROOT / "packaging/macos/zerus-setup"
_spec = importlib.util.spec_from_file_location("package_macos", ROOT / "scripts/package-macos.py")
_package = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_package)
SETUP_SCRIPTS = _package.SETUP_SCRIPTS
CONNECTOR_MARKER = runpy.run_path(str(ROOT / "scripts/install-mobile-connector-service.py"))["AGENT_MARKER"]
LABELS = {
    "session": "com.hgdev.hgs.user-session",
    "desktop": "com.hgdev.hgs-desktop",
    "tray": "com.hgdev.hgs-tray",
    "recovery": "com.hgdev.hgs-recovery",
    "swarm": "com.hgdev.hgs-swarm",
}
FAKE_LAUNCHCTL = """#!/bin/sh
printf '%s\\n' "$*" >> "$ZERUS_TEST_LAUNCHCTL_LOG"
state="$ZERUS_TEST_LAUNCHCTL_STATE"
touch "$state"
case "$1" in
  print)
    case "$2" in
      gui/*/*)
        grep -qx "${2##*/}" "$state" || exit 113
        if [ "${2##*/}" = com.hgdev.hgs-desktop ]; then
          "$HGS_EXECUTABLE" desktop fixture-launchctl-print
        fi ;;
      *) exit 0 ;;
    esac ;;
  bootstrap) basename "$3" .plist >> "$state" ;;
  bootout) grep -vx "${2##*/}" "$state" > "$state.next"; mv "$state.next" "$state" ;;
esac
"""


class MacSetup(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="zerus-mac-setup-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.app = self.root / "Applications/Zerus.app"
        macos = self.app / "Contents/MacOS"
        setup = self.app / "Contents/Resources/setup"
        macos.mkdir(parents=True)
        setup.mkdir(parents=True)
        (self.app / "Contents/Resources/bin").mkdir()
        for name in ("hgs", "hgs-tray", "zerus-mobile-connector"):
            (macos / name).write_text("#!/bin/sh\n# synthetic bundled executable\n")
            (macos / name).chmod(0o755)
        (macos / "hgs").write_text(f'''#!{sys.executable}
import json, os, sys
from pathlib import Path
sys.path.insert(0, {str(ROOT / "tests")!r})
from test_macos_desktop_service import Commands
binary = Path(os.environ.get("HGS_EXECUTABLE", sys.argv[0]))
command = Commands(Path(os.environ["HOME"]), binary)
if sys.argv[1:] == ["desktop", "fixture-launchctl-print"]:
    command.loaded = True
    result = command(["launchctl", "print", "gui/fixture/desktop"])
elif sys.argv[1:3] in (["desktop", "registration"], ["desktop", "status"]):
    result = command([str(binary), *sys.argv[1:]])
else:
    sys.exit(0)
print(result.stdout, end="")
''')
        shutil.copy2(ROOT / "hgs_state.py", setup / "hgs_state.py")
        for name in SETUP_SCRIPTS:
            shutil.copy2(ROOT / "scripts" / name, setup / name)
        shutil.copy2(ROOT / "tray/packaging/com.hgdev.hgs-tray.plist", setup / "com.hgdev.hgs-tray.plist")
        shutil.copy2(SCRIPT, self.app / "Contents/Resources/bin/zerus-setup")
        self.tools = self.root / "tools"
        self.tools.mkdir()
        self.log = self.root / "launchctl.log"
        self.state = self.root / "launchctl.state"
        fake = FAKE_LAUNCHCTL.replace('"$ZERUS_TEST_LAUNCHCTL_LOG"', shlex.quote(str(self.log))).replace(
            '"$ZERUS_TEST_LAUNCHCTL_STATE"', shlex.quote(str(self.state)))
        (self.tools / "launchctl").write_text(fake)
        (self.tools / "launchctl").chmod(0o755)

    def run_setup(self, *arguments, check=True):
        environment = {
            "HOME": str(self.home),
            "PATH": f"{self.tools}:/usr/bin:/bin",
            "ZERUS_TEST_LAUNCHCTL_LOG": str(self.log),
            "ZERUS_TEST_LAUNCHCTL_STATE": str(self.state),
            "PYTHONDONTWRITEBYTECODE": "1",
        }
        for key in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME",
                    "XDG_RUNTIME_DIR", "TMPDIR", "TMUX_TMPDIR"):
            directory = self.root / "private" / key.lower(); directory.mkdir(parents=True, exist_ok=True)
            environment[key] = str(directory)
        result = subprocess.run([sys.executable, str(self.app / "Contents/Resources/bin/zerus-setup"), *arguments],
                                env=environment, capture_output=True, text=True, timeout=60)
        if check:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def agent(self, kind):
        return self.home / "Library/LaunchAgents" / (LABELS[kind] + ".plist")

    def launchctl_calls(self):
        return self.log.read_text().splitlines() if self.log.exists() else []

    def test_tray_agent_template_runs_as_an_interactive_job(self):
        # Without ProcessType launchd starts the GUI as a daemon and clamps all its
        # threads below ordinary CPU load; the whole window then stops responding.
        template = re.sub(r"<!--.*?-->", "", (ROOT / "tray/packaging/com.hgdev.hgs-tray.plist").read_text(), flags=re.S)
        self.assertEqual(plistlib.loads(template.encode()).get("ProcessType"), "Interactive")

    def test_default_setup_creates_links_into_the_bundle(self):
        self.run_setup()
        for name, target in (("hgs", "Contents/MacOS/hgs"), ("hgs_state.py", "Contents/Resources/setup/hgs_state.py")):
            link = self.home / ".local/bin" / name
            self.assertTrue(link.is_symlink(), name)
            self.assertEqual(link.resolve(), (self.app / target).resolve())

    @unittest.skipUnless(sys.platform == "darwin", "LaunchAgents are installed on macOS only")
    def test_default_setup_installs_session_bridge_and_aqua_desktop(self):
        self.run_setup()
        definition = plistlib.loads(self.agent("session").read_bytes())
        self.assertEqual(Path(definition["ProgramArguments"][0]), (self.app / "Contents/MacOS/hgs").resolve())
        desktop = plistlib.loads(self.agent("desktop").read_bytes())
        self.assertEqual(desktop["Label"], LABELS["desktop"])
        self.assertEqual(desktop["LimitLoadToSessionType"], "Aqua")
        self.assertEqual(desktop["ProgramArguments"][-1], "--launchd")
        for kind in ("tray", "recovery", "swarm"):
            self.assertFalse(self.agent(kind).exists(), kind)

    @unittest.skipUnless(sys.platform == "darwin", "LaunchAgents are installed on macOS only")
    def test_second_run_changes_nothing_and_restarts_nothing(self):
        self.run_setup("--autostart", "--workers")
        before = {path: path.stat().st_mtime_ns for path in (self.home / "Library/LaunchAgents").iterdir()}
        calls = len(self.launchctl_calls())
        self.run_setup("--autostart", "--workers")
        after = {path: path.stat().st_mtime_ns for path in (self.home / "Library/LaunchAgents").iterdir()}
        self.assertEqual(before, after)
        repeated = self.launchctl_calls()[calls:]
        self.assertFalse([call for call in repeated if call.split()[0] in {"bootout", "kickstart", "bootstrap"}], repeated)

    def test_mobile_connector_install_stands_alone(self):
        for other in ("--workers", "--autostart", "--remove"):
            with self.subTest(other=other):
                result = self.run_setup("--mobile-connector", "install", other, check=False)
                self.assertEqual(result.returncode, 2)
        self.assertFalse((self.home / "Library/LaunchAgents").exists())

    @unittest.skipUnless(sys.platform == "darwin", "LaunchAgents are installed on macOS only")
    def test_mobile_connector_install_writes_only_its_agent_and_loads_nothing(self):
        result = self.run_setup("--mobile-connector", "install")
        path = self.home / "Library/LaunchAgents/com.hgdev.zerus-mobile-connector.plist"
        definition = plistlib.loads(path.read_bytes())
        self.assertEqual(Path(definition["ProgramArguments"][0]), (self.app / "Contents/MacOS/zerus-mobile-connector").resolve())
        self.assertIs(definition["AbandonProcessGroup"], True)
        self.assertEqual(sorted(p.name for p in (self.home / "Library/LaunchAgents").iterdir()), [path.name])
        self.assertIn("launchctl bootstrap", result.stdout)
        self.assertFalse([call for call in self.launchctl_calls() if call.split()[0] != "print"])
        # --remove unloads and removes it with the other agents of this bundle.
        self.state.write_text("com.hgdev.zerus-mobile-connector\n")
        self.run_setup("--remove")
        self.assertFalse(path.exists())

    def test_remove_keeps_foreign_connector_agents_that_point_into_the_bundle(self):
        label = "com.hgdev.zerus-mobile-connector"
        path = self.home / "Library/LaunchAgents" / (label + ".plist")
        path.parent.mkdir(parents=True)
        cases = ((False, label, False, "zerus-mobile-connector"),
                 (False, label, True, "zerus-mobile-connector"),
                 (True, label, False, "zerus-mobile-connector"),
                 (True, "org.example.custom", True, "zerus-mobile-connector"),
                 (True, label, True, "hgs"))
        for marker, actual_label, abandon, program in cases:
            with self.subTest(marker=marker, label=actual_label, abandon=abandon, program=program):
                definition = {"Label": actual_label,
                              "ProgramArguments": [str(self.app / "Contents/MacOS" / program),
                                                   "--config", str(self.root / "custom-connector.json")],
                              "AbandonProcessGroup": abandon}
                payload = plistlib.dumps(definition)
                if marker:
                    payload = payload.replace(b"\n", b"\n" + CONNECTOR_MARKER + b"\n", 1)
                path.write_bytes(payload)
                self.state.write_text(label + "\n")
                self.log.write_text("")
                result = self.run_setup("--remove")
                self.assertTrue(path.exists(), "foreign connector plist was removed")
                self.assertEqual(path.read_bytes(), payload)
                self.assertIn("Kept", result.stdout)
                self.assertFalse([call for call in self.launchctl_calls() if call.startswith("bootout")])

    def test_remove_owned_connector_unloads_only_its_agent(self):
        label = "com.hgdev.zerus-mobile-connector"
        path = self.home / "Library/LaunchAgents" / (label + ".plist")
        path.parent.mkdir(parents=True)
        definition = {"Label": label,
                      "ProgramArguments": [str(self.app / "Contents/MacOS/zerus-mobile-connector"), "--config",
                                           str(self.home / ".config/hgs/mobile/connector.json")],
                      "AbandonProcessGroup": True}
        payload = plistlib.dumps(definition).replace(b"\n", b"\n" + CONNECTOR_MARKER + b"\n", 1)
        path.write_bytes(payload)
        self.state.write_text(label + "\n")
        self.run_setup("--remove")
        self.assertFalse(path.exists())
        self.assertEqual([call for call in self.launchctl_calls() if call.startswith("bootout")],
                         [f"bootout gui/{os.getuid()}/{label}"])

    def test_existing_executable_aborts_before_any_change(self):
        directory = self.home / ".local/bin"
        directory.mkdir(parents=True)
        (directory / "hgs").write_text("source installation\n")
        result = self.run_setup(check=False)
        self.assertEqual(result.returncode, 2)
        self.assertIn(str(directory / "hgs"), result.stderr)
        self.assertEqual((directory / "hgs").read_text(), "source installation\n")
        self.assertFalse((directory / "hgs_state.py").exists())
        # Apple's /usr/bin/python3 caches bytecode under ~/Library/Caches by itself.
        self.assertFalse((self.home / "Library/LaunchAgents").exists())
        self.assertEqual(self.launchctl_calls(), [])

    def test_foreign_symlink_is_preserved(self):
        directory = self.home / ".local/bin"
        directory.mkdir(parents=True)
        (directory / "hgs_state.py").symlink_to(self.root / "old-install/hgs_state.py")
        result = self.run_setup(check=False)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(os.readlink(directory / "hgs_state.py"), str(self.root / "old-install/hgs_state.py"))
        self.assertFalse((directory / "hgs").exists())

    def test_source_application_is_reported_and_nothing_changes(self):
        (self.home / "Applications/hgs-tray.app/Contents").mkdir(parents=True)
        result = self.run_setup(check=False)
        self.assertEqual(result.returncode, 2)
        self.assertIn("hgs-tray.app", result.stderr)
        self.assertIn("docs/installation.md", result.stderr)
        self.assertFalse((self.home / ".local").exists())
        self.assertFalse((self.home / "Library/LaunchAgents").exists())

    def test_foreign_launch_agent_is_reported_and_preserved(self):
        directory = self.home / "Library/LaunchAgents"
        directory.mkdir(parents=True)
        foreign = plistlib.dumps({"Label": LABELS["session"], "ProgramArguments": [str(self.home / ".local/bin/hgs")]})
        self.agent("session").write_bytes(foreign)
        result = self.run_setup(check=False)
        self.assertEqual(result.returncode, 2)
        self.assertIn(LABELS["session"], result.stderr)
        self.assertEqual(self.agent("session").read_bytes(), foreign)
        self.assertFalse((self.home / ".local").exists())

    @unittest.skipUnless(sys.platform == "darwin", "LaunchAgents are installed on macOS only")
    def test_autostart_writes_the_bundled_tray_agent_without_starting_it(self):
        self.run_setup("--autostart")
        definition = plistlib.loads(self.agent("tray").read_bytes())
        self.assertEqual(definition["ProgramArguments"],
                         [str(self.app.resolve() / "Contents/MacOS/hgs-tray"), "--hgs",
                          str(self.app.resolve() / "Contents/MacOS/hgs")])
        self.assertEqual(definition["StandardErrorPath"], str(self.home / "Library/Logs/hgs-tray.log"))
        self.assertEqual(definition["ProcessType"], "Interactive")
        self.assertFalse([call for call in self.launchctl_calls() if LABELS["tray"] in call and "bootstrap" in call])

    @unittest.skipUnless(sys.platform == "darwin", "LaunchAgents are installed on macOS only")
    def test_autostart_makes_an_earlier_tray_agent_interactive_without_restarting_it(self):
        self.run_setup("--autostart")
        definition = plistlib.loads(self.agent("tray").read_bytes())
        del definition["ProcessType"]
        self.agent("tray").write_bytes(plistlib.dumps(definition))
        with self.state.open("a") as state:   # the earlier agent is loaded
            state.write(LABELS["tray"] + "\n")
        calls = len(self.launchctl_calls())
        result = self.run_setup("--autostart")
        self.assertEqual(plistlib.loads(self.agent("tray").read_bytes())["ProcessType"], "Interactive")
        # bootstrap alone fails for a loaded label: the hint reloads it.
        self.assertIn(f"launchctl bootout gui/{os.getuid()}/{LABELS['tray']}", result.stdout)
        repeated = self.launchctl_calls()[calls:]
        self.assertFalse([call for call in repeated if call.split()[0] in {"bootout", "kickstart", "bootstrap"}], repeated)

    @unittest.skipUnless(sys.platform == "darwin", "LaunchAgents are installed on macOS only")
    def test_workers_are_opt_in(self):
        self.run_setup("--workers")
        for kind in ("recovery", "swarm"):
            definition = plistlib.loads(self.agent(kind).read_bytes())
            self.assertEqual(Path(definition["ProgramArguments"][0]), (self.app / "Contents/MacOS/hgs").resolve())

    @unittest.skipUnless(sys.platform == "darwin", "LaunchAgents are installed on macOS only")
    def test_remove_unloads_only_bundle_integration(self):
        self.run_setup("--autostart", "--workers")
        unrelated = self.home / ".local/bin/other-tool"
        unrelated.symlink_to(self.root / "elsewhere")
        self.run_setup("--remove")
        for name in ("hgs", "hgs_state.py"):
            self.assertFalse(os.path.lexists(self.home / ".local/bin" / name), name)
        self.assertTrue(unrelated.is_symlink())
        for kind in LABELS:
            self.assertFalse(self.agent(kind).exists(), kind)
        boot_outs = [call for call in self.launchctl_calls() if call.startswith("bootout")]
        self.assertEqual({call.rsplit("/", 1)[1] for call in boot_outs} & set(LABELS.values()),
                         {LABELS["session"], LABELS["desktop"], LABELS["recovery"], LABELS["swarm"]})

    def test_remove_keeps_foreign_items(self):
        directory = self.home / ".local/bin"
        directory.mkdir(parents=True)
        (directory / "hgs").write_text("source installation\n")
        self.run_setup("--remove")
        self.assertEqual((directory / "hgs").read_text(), "source installation\n")

    def test_root_is_refused(self):
        entrypoint = runpy.run_path(str(SCRIPT))["main"]
        with patch.dict(os.environ, {"HOME": str(self.home)}), \
                patch("sys.argv", [str(SCRIPT), "--app", str(self.app)]), \
                patch("os.geteuid", return_value=0), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit):
                entrypoint()
        self.assertFalse((self.home / ".local").exists())


if __name__ == "__main__":
    unittest.main()
