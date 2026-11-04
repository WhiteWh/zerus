"""Exercise the real synthetic watch, including its idle protocol/lifetime."""
import json
import os
from pathlib import Path
import select
import shutil
import subprocess
import sys
import tempfile
import unittest


FIXTURE = Path(__file__).resolve().parents[1] / "src-tauri/src/desktop_service/fake_hgs.py"


@unittest.skipUnless(os.name == "posix", "the desktop service fixture is Unix-only")
class ServiceFixture(unittest.TestCase):
    def frame(self, child):
        self.assertTrue(select.select([child.stdout], [], [], 30)[0], "fixture frame deadline")
        line = child.stdout.readline(65537)
        self.assertTrue(line.endswith(b"\n") and len(line) <= 65536)
        return json.loads(line)

    def test_silent_watch_stays_alive_without_any_frame_after_hello(self):
        with tempfile.TemporaryDirectory(prefix="zerus-silent-") as name:
            root = Path(name)
            script = root / "hgs"
            shutil.copyfile(FIXTURE, script)
            (root / "fixture.json").write_text(json.dumps({"silent_watch": True}))
            child = subprocess.Popen(
                [sys.executable, str(script), "desktop", "watch", "--json"],
                env={"PATH": os.defpath, "HOME": name, "TMPDIR": name},
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                bufsize=0,
            )
            try:
                self.assertEqual(self.frame(child)["t"], "hello")
                self.assertFalse(select.select([child.stdout], [], [], 1.2)[0])
                self.assertIsNone(child.poll(), "silence must not be EOF")
                child.stdin.close()
                child.stdin = None
                output, error = child.communicate(timeout=5)
                self.assertEqual(child.returncode, 0, error.decode())
                self.assertEqual(output, b"")
            finally:
                if child.poll() is None:
                    child.kill()
                child.communicate(timeout=5)

    def test_idle_heartbeats_preserve_the_latest_status_and_close_with_stdin(self):
        for config in ({}, {"details": True}, {"counts_degraded": True}):
            with self.subTest(config=config), tempfile.TemporaryDirectory(prefix="zerus-watch-") as name:
                root = Path(name)
                script = root / "hgs"
                shutil.copyfile(FIXTURE, script)
                (root / "fixture.json").write_text(json.dumps(config))
                env = {"PATH": os.defpath, "HOME": name, "TMPDIR": name,
                       "XDG_CONFIG_HOME": name, "XDG_DATA_HOME": name,
                       "XDG_STATE_HOME": name, "XDG_CACHE_HOME": name,
                       "XDG_RUNTIME_DIR": name, "TMUX_TMPDIR": name}
                child = subprocess.Popen([sys.executable, str(script), "desktop", "watch", "--json"],
                                         env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                         stderr=subprocess.PIPE, bufsize=0)
                try:
                    hello = self.frame(child)
                    self.assertEqual(hello["t"], "hello")
                    latest = hello["value"]
                    for _ in range(8):
                        heartbeat = self.frame(child)
                        if heartbeat["t"] == "heartbeat":
                            break
                        self.assertEqual(heartbeat["t"], "status")
                        latest = heartbeat["value"]
                    else:
                        self.fail("bounded fixture frames did not include a heartbeat")
                    self.assertGreater(heartbeat["seq"], hello["seq"])
                    self.assertEqual(heartbeat["epoch"], hello["epoch"])
                    self.assertEqual(heartbeat["value"], latest)
                    child.stdin.close()
                    child.stdin = None
                    output, error = child.communicate(timeout=5)
                    self.assertEqual(child.returncode, 0, error.decode())
                    self.assertLessEqual(len(output), 65536)
                finally:
                    if child.poll() is None:
                        child.kill()  # Only the original private fake Python child.
                    child.communicate(timeout=5)


if __name__ == "__main__":
    unittest.main()
