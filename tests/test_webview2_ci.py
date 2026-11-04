"""CI bootstrapper guards, bounded downloads and execution ordering, without Windows/network."""
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("webview2_ci", ROOT / "scripts/ci/ensure-webview2.py")
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


class Response(io.BytesIO):
    def __init__(self, data, declared=None, url="https://download.microsoft.com/bootstrapper.exe"):
        super().__init__(data)
        self.headers = {} if declared is None else {"Content-Length": str(declared)}
        self.url = url

    def geturl(self):
        return self.url


class WebView2CI(unittest.TestCase):
    def test_owner_machines_and_non_windows_jobs_refuse_before_any_probe_or_write(self):
        for platform, env in (("posix", {}), ("nt", {}), ("nt", {"GITHUB_ACTIONS": "true"}),
                              ("nt", {"GITHUB_ACTIONS": "false", "RUNNER_OS": "Windows"})):
            with self.subTest(platform=platform, env=env), self.assertRaises(ValueError):
                helper.ci_only(platform, env)
        helper.ci_only("nt", {"GITHUB_ACTIONS": "true", "RUNNER_OS": "Windows"})
        with patch.object(helper, "ci_only", side_effect=ValueError("outside CI")), \
                patch.object(helper, "runtime_version") as probe, \
                patch.object(helper, "download") as download, self.assertRaises(ValueError):
            helper.ensure_runtime()
        probe.assert_not_called()
        download.assert_not_called()

    def test_runtime_registration_requires_a_nonzero_four_part_version(self):
        for value in (None, "", "0.0.0.0", "1.2.3", "1.2.3.4beta", "x.2.3.4", "1." * 100):
            self.assertFalse(helper.valid_version(value), value)
        self.assertTrue(helper.valid_version("123.0.6312.86"))

    def test_streaming_limits_and_truncation_are_enforced_without_executing_any_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "bootstrapper.exe"
            with patch.object(helper, "urlopen", return_value=Response(b"exact", 5)):
                helper.download(path)
            self.assertEqual(path.read_bytes(), b"exact")
            for response in (Response(b"", 0), Response(b"short", 10), Response(b"x", helper.LIMIT + 1),
                             Response(b"x" * (helper.LIMIT + 1)),
                             Response(b"x", url="http://download.microsoft.com/bootstrapper.exe"),
                             Response(b"x", url="https://microsoft.com.example/bootstrapper.exe")):
                path.unlink(missing_ok=True)
                with patch.object(helper, "urlopen", return_value=response), self.assertRaises(ValueError):
                    helper.download(path)
            path.unlink(missing_ok=True)
            with patch.object(helper, "urlopen", return_value=Response(b"x")), \
                    patch.object(helper.time, "monotonic", side_effect=[0, 121]), self.assertRaises(ValueError):
                helper.download(path)

    def test_only_valid_microsoft_authenticode_allows_execution(self):
        for status, subject, valid in (("Valid", "CN=Microsoft Corporation, O=Microsoft Corporation, C=US", True),
                                      ("NotSigned", None, False), ("HashMismatch", "O=Microsoft Corporation", False),
                                      ("Valid", "CN=Microsoft Corporation, O=Example Publisher", False),
                                      ("Valid", "O=Microsoft Corporation Example", False)):
            result = Mock(stdout=json.dumps(dict(status=status, subject=subject)).encode())
            with self.subTest(status=status, subject=subject), patch.dict(os.environ, {"SystemRoot": "C:/Windows"}), \
                    patch.object(helper.subprocess, "run", return_value=result) as run:
                if valid:
                    helper.verify_signature(Path("private/bootstrapper.exe"))
                else:
                    with self.assertRaises(ValueError):
                        helper.verify_signature(Path("private/bootstrapper.exe"))
                self.assertIn("WindowsPowerShell", run.call_args.args[0][0])
                self.assertEqual(run.call_args.kwargs["timeout"], 60)
                self.assertNotIn("-ExecutionPolicy", run.call_args.args[0])

    def test_existing_runtime_stays_untouched_and_unknown_setup_never_retries(self):
        with patch.object(helper, "ci_only"), patch.object(helper, "runtime_version", return_value="1.2.3.4"), \
                patch.object(helper, "download") as download, patch.object(helper.subprocess, "run") as run:
            helper.ensure_runtime()
        download.assert_not_called()
        run.assert_not_called()
        with tempfile.TemporaryDirectory() as temporary:
            for outcome, observed, succeeds in ((0, "1.2.3.4", True), (3010, "1.2.3.4", True),
                                                (1, None, False), (0, None, False),
                                                (subprocess.TimeoutExpired("bootstrapper", 120), None, False)):
                calls = []
                def execute(command, **kwargs):
                    calls.append("execute")
                    self.assertEqual(command[1:], ["/silent", "/install"])
                    self.assertEqual(kwargs["timeout"], 120)
                    if isinstance(outcome, Exception):
                        raise outcome
                    return Mock(returncode=outcome)
                with patch.object(helper, "ci_only"), patch.dict(os.environ, {"RUNNER_TEMP": temporary}), \
                        patch.object(helper, "runtime_version", side_effect=[None, observed]), \
                        patch.object(helper, "download", side_effect=lambda _: calls.append("download")), \
                        patch.object(helper, "verify_signature", side_effect=lambda _: calls.append("verify")), \
                        patch.object(helper.subprocess, "run", side_effect=execute):
                    if succeeds:
                        helper.ensure_runtime()
                    else:
                        with self.assertRaises((ValueError, subprocess.TimeoutExpired)):
                            helper.ensure_runtime()
                self.assertEqual(calls, ["download", "verify", "execute"])
                self.assertEqual(list(Path(temporary).iterdir()), [])
            with patch.object(helper, "ci_only"), patch.dict(os.environ, {"RUNNER_TEMP": temporary}), \
                    patch.object(helper, "runtime_version", return_value=None), \
                    patch.object(helper, "download"), \
                    patch.object(helper, "verify_signature", side_effect=ValueError("wrong signer")), \
                    patch.object(helper.subprocess, "run") as run, self.assertRaises(ValueError):
                helper.ensure_runtime()
            run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
