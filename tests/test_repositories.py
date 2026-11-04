"""Repository acquisition contracts: real Git, private profiles, no agent state."""
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
import uuid

REPO = Path(__file__).resolve().parents[1]
HGS = Path(os.environ.get("HGS_TEST_BIN", REPO / "hgs")).resolve()


def bounded_run(argv, **kwargs):
    """Retain and reap the original subtree even when the CLI/test fails."""
    seconds = kwargs.pop('timeout', 30)
    if sys.platform.startswith('linux'):
        argv = [sys.executable, str(Path(__file__).with_name('bounded_process.py')),
                '--seconds', str(seconds), '--max-processes', '32', '--', *map(str, argv)]
        seconds += 5
    return subprocess.run(argv, timeout=seconds, **kwargs)


def image_proof(executable):
    stat = executable.stat()
    seconds, nanos = divmod(stat.st_mtime_ns, 1_000_000_000)
    return f"{stat.st_dev & ((1 << 64) - 1)}:{stat.st_ino}:{seconds}:{nanos}"


class Repositories(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="zerus-repository-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(("HGS_", "GIT_"))}
        self.env.update(HOME=str(self.root), USERPROFILE=str(self.root),
                        XDG_CONFIG_HOME=str(self.root / "config"), XDG_STATE_HOME=str(self.root / "state"),
                        XDG_DATA_HOME=str(self.root / "data"), GIT_CONFIG_NOSYSTEM="1",
                        GIT_CONFIG_GLOBAL=str(self.root / "gitconfig"),
                        GIT_AUTHOR_NAME="Fixture", GIT_AUTHOR_EMAIL="fixture@example.invalid",
                        GIT_COMMITTER_NAME="Fixture", GIT_COMMITTER_EMAIL="fixture@example.invalid")
        help_result = bounded_run([str(HGS), '--help'], env=self.env, cwd=self.root,
                                  capture_output=True, text=True, timeout=5)
        self.assertEqual(help_result.returncode, 0, help_result.stderr)
        self.assertIn('repository-clone-v1', help_result.stdout,
                      'repository fixtures require the actual CLI, never a libtest image')
        self.source = self.root / "source"
        self.source.mkdir()
        self.git("init", "-b", "main")
        (self.source / "content").write_text("preserved\n")
        self.git("add", "content")
        self.git("commit", "-m", "fixture")
        self.url = "https://example.invalid/team/source.git"
        subprocess.run(["git", "config", "--global", "url." + self.source.as_uri() + ".insteadOf", self.url],
                       env=self.env, check=True, capture_output=True)

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.source), *args], env=self.env,
                              capture_output=True, text=True, check=True, timeout=10)

    def hgs(self, *args, rc=0):
        result = bounded_run([str(HGS), *args], env=self.env, cwd=self.root,
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, rc, result.stdout + result.stderr)
        return json.loads(result.stdout) if rc == 0 else result

    def operation(self, action, name="checkout", token=None, parent=None, url=None, rc=0):
        args = ["repository", action, "--url", url or self.url, "--parent", str(parent or self.root),
                "--name", name, "--json"]
        if token is not None:
            args += ["--request-id", token]
        return self.hgs(*args, rc=rc)

    def mobile(self, argv, machine, rc=0):
        envelope = {"schema": 1, "target_machine_id": machine, "argv": argv, "payload": None}
        result = bounded_run([str(HGS), "swarm", "__mobile-peer-local", "--json"],
                                input=json.dumps(envelope), env=self.env, cwd=self.root,
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, rc, result.stdout + result.stderr)
        return json.loads(result.stdout) if rc == 0 else result

    @unittest.skipIf(os.name == "nt", "Unix mobile one-hop acquisition; Windows gateway refuses before dispatch")
    def test_mobile_guarded_acquisition_uses_actual_git_and_original_receipt(self):
        machine = self.hgs("swarm", "export", "--json")["node_id"]
        token = str(uuid.uuid4())
        argv = ["repository", "check", "--url", self.url, "--parent", str(self.root), "--name", "mobile checkout", "--json"]
        self.assertEqual(self.mobile(argv, machine)["status"], "ready")
        argv[1] = "clone"
        argv += ["--request-id", token]
        cloned = self.mobile(argv, machine)
        self.assertEqual(cloned["status"], "cloned")
        self.assertEqual((self.root / "mobile checkout" / "content").read_text(), "preserved\n")
        self.assertEqual(self.mobile(argv, machine), cloned)
        self.assertEqual(self.mobile(["repository", "status", "--request-id", token, "--json"], machine), cloned)

    @unittest.skipIf(os.name == "nt", "Unix mobile one-hop identity guard; Windows gateway refuses before dispatch")
    def test_mobile_wrong_machine_refuses_before_creating_any_destination(self):
        self.hgs("swarm", "export", "--json")
        self.mobile(["repository", "clone", "--url", self.url, "--parent", str(self.root), "--name", "foreign", "--request-id", str(uuid.uuid4()), "--json"], str(uuid.uuid4()), rc=1)
        self.assertFalse((self.root / "foreign").exists())
        self.assertEqual((self.source / "content").read_text(), "preserved\n")

    @unittest.skipUnless(os.name == "nt", "Windows gateway one-hop repository refusal")
    def test_windows_mobile_repository_routes_refuse_without_effects(self):
        machine = self.hgs("swarm", "export", "--json")["node_id"]
        token = str(uuid.uuid4())
        destination = self.root / "windows mobile checkout"
        receipts = self.root / "state" / "hgs" / "sessions" / "repository-clones"
        trace = self.root / "git-trace.log"
        self.env["GIT_TRACE"] = trace.as_posix()

        def snapshot():
            # This bounded private fixture includes Git metadata and native identity/journal files.
            return {str(path.relative_to(self.root)): (path.stat().st_mtime_ns,
                                                       path.read_bytes() if path.is_file() else None)
                    for path in self.root.rglob("*")}

        self.assertFalse(destination.exists())
        self.assertFalse(receipts.exists())
        self.assertFalse(trace.exists())
        before = snapshot()
        arguments = ["--url", self.url, "--parent", str(self.root), "--name", destination.name]
        clone = ["repository", "clone", *arguments, "--request-id", token, "--json"]
        cases = [("check", ["repository", "check", *arguments, "--json"], machine),
                 ("clone", clone, machine),
                 ("wrong-machine clone", clone, str(uuid.uuid4())),
                 ("status", ["repository", "status", "--request-id", token, "--json"], machine)]
        for operation, argv, target in cases:
            with self.subTest(operation=operation):
                result = self.mobile(argv, target, rc=2)
                self.assertEqual(result.stdout, "")
                self.assertIn("One-hop mobile peer routing is unavailable on a Windows gateway; "
                              "local Windows sessions are supported.", result.stderr)
                self.assertFalse(destination.exists())
                self.assertFalse(receipts.exists())
                self.assertFalse(trace.exists(), "Refused routing must not invoke Git")
                self.assertEqual(snapshot(), before, "Refused routing changed the private fixture")

    def synthetic_git(self, body):
        """Override only hgs's private, prepended Git, leaving fixture Git real."""
        real_git = shutil.which("git")
        executable = self.root / ".local" / "bin" / "git"
        executable.parent.mkdir(parents=True, exist_ok=True)
        executable.write_text(f"#!{sys.executable}\nimport os, pathlib, subprocess, sys, time\n"
                              f"real_git = {real_git!r}\nroot = pathlib.Path({str(self.root)!r})\n"
                              + body + "\nos.execv(real_git, [real_git, *sys.argv[1:]])\n")
        executable.chmod(0o700)

    def test_check_has_no_destination_or_receipt_effect(self):
        receipt = self.operation("check")
        self.assertEqual(receipt["status"], "ready")
        self.assertEqual(receipt["path"], str(self.root / "checkout"))
        self.assertFalse((self.root / "checkout").exists())
        self.assertFalse((self.root / "state" / "hgs" / "sessions" / "repository-clones").exists())

    def test_clone_checkout_and_exact_receipt(self):
        token = str(uuid.uuid4())
        receipt = self.operation("clone", token=token)
        self.assertEqual(receipt["status"], "cloned")
        self.assertEqual(receipt["request_id"], token)
        self.assertEqual(receipt["parent"], str(self.root))
        self.assertEqual((self.root / "checkout" / "content").read_text(), "preserved\n")
        self.assertEqual(self.hgs("repository", "status", "--request-id", token, "--json"), receipt)

    def test_duplicate_does_not_restore_or_modify_checkout(self):
        token = str(uuid.uuid4())
        receipt = self.operation("clone", token=token)
        target = self.root / "checkout" / "content"
        target.write_text("local changes")
        self.assertEqual(self.operation("clone", token=token), receipt)
        self.assertEqual(target.read_text(), "local changes")
        self.operation("clone", name="different", token=token, rc=1)
        self.assertFalse((self.root / "different").exists())

    def test_unconfirmed_receipt_never_replays_git(self):
        token = str(uuid.uuid4())
        receipt = self.operation("clone", token=token)
        receipt["status"] = "unconfirmed"
        journal = self.root / "state" / "hgs" / "sessions" / "repository-clones" / (token + ".json")
        journal.write_text(json.dumps(receipt))
        target = self.root / "checkout" / "content"
        target.write_text("retained after uncertain outcome")
        self.assertEqual(self.operation("clone", token=token), receipt)
        self.assertEqual(target.read_text(), "retained after uncertain outcome")

    def test_failed_clone_is_durable_and_never_replayed(self):
        token = str(uuid.uuid4())
        # The URL maps to a missing local repository: deterministic, no network.
        self.git("config", "--global", "url." + (self.root / "missing").as_uri() + ".insteadOf", "https://missing.invalid/repo")
        receipt = self.operation("clone", token=token, url="https://missing.invalid/repo")
        self.assertEqual(receipt["status"], "failed")
        self.assertIn("detail", receipt)
        self.assertEqual(self.operation("clone", token=token, url="https://missing.invalid/repo"), receipt)

    def test_existing_directory_file_and_symlink_are_preserved(self):
        (self.root / "existing").mkdir()
        sentinel = self.root / "existing" / "local"
        sentinel.write_text("retained")
        (self.root / "file").write_text("retained")
        names = ["existing", "file"]
        if os.name != "nt":
            (self.root / "link").symlink_to(self.root / "existing", target_is_directory=True)
            names.append("link")
        for name in names:
            self.operation("check", name=name, rc=1)
            self.operation("clone", name=name, token=str(uuid.uuid4()), rc=1)
        self.assertEqual(sentinel.read_text(), "retained")
        self.assertEqual((self.root / "file").read_text(), "retained")

    def test_literal_folder_name_and_canonical_parent(self):
        name = "checkout ' $(touch injected) `touch another`"
        receipt = self.operation("clone", name=name, token=str(uuid.uuid4()))
        self.assertEqual(receipt["status"], "cloned")
        self.assertEqual((self.root / name / "content").read_text(), "preserved\n")
        self.assertFalse((self.root / "injected").exists())
        self.assertFalse((self.root / "another").exists())

    def test_invalid_urls_names_and_request_ids_have_no_checkout_effect(self):
        for url in ("ext::bad", "--upload-pack=bad", "file:///tmp/repo", "https://token@example.invalid/repo",
                    "C:/source", "C:relative", r"C:\source", r"\\server\share\repo",
                    r"\\?\C:\repo", r"\\.\C:\repo", "//server/share/repo", "./repo", "../repo"):
            self.operation("clone", url=url, token=str(uuid.uuid4()), rc=1)
        for name in ("..", "/tmp/repo", "a/b", "a\\b", "bad\nname", "trailing."):
            self.operation("clone", name=name, token=str(uuid.uuid4()), rc=1)
        for token in ("bad", str(uuid.uuid4()).upper(), "00000000-0000-0000-0000-000000000000"):
            self.operation("clone", token=token, rc=1)
        self.assertFalse((self.root / "checkout").exists())

    def test_reserved_device_names_refuse_before_receipts_or_destinations(self):
        names = ("CON", "prn", "AuX", "NUL", "CONIN$", "conout$.git", "con.txt",
                 "NUL.tar.gz", "com9.git", "LPT1", "lpt9.txt", "CON  .git",
                 "COM1 .tar.gz", "COM¹", "com².git", "LPT³  .git")
        receipts = self.root / "state" / "hgs" / "sessions" / "repository-clones"
        self.assertFalse(receipts.exists())
        for name in names:
            for action in ("check", "clone"):
                with self.subTest(name=name, action=action):
                    result = self.operation(action, name=name,
                                            token=str(uuid.uuid4()) if action == "clone" else None,
                                            rc=1)
                    self.assertIn("reserved Windows device name", result.stderr)
                    self.assertFalse(receipts.exists(), "Refusal must precede receipt or request-lock creation")
                    # NUL.exists() describes a Win32 device, not an actual
                    # directory entry. Enumerate the known parent instead.
                    entries = {entry.name.casefold() for entry in self.root.iterdir()}
                    self.assertNotIn(name.casefold(), entries)
                    self.assertEqual((self.source / "content").read_text(), "preserved\n")

    @unittest.skipIf(os.name == "nt", "Unix synthetic executable")
    def test_large_escaped_error_keeps_status_and_duplicate_readable(self):
        self.synthetic_git("if 'clone' in sys.argv:\n"
                           "    os.write(2, (b'\\x01\\\\\\\"' * 20000))\n"
                           "    sys.exit(1)\n")
        token = str(uuid.uuid4())
        receipt = self.operation("clone", token=token)
        self.assertEqual(receipt["status"], "failed")
        journal = self.root / "state" / "hgs" / "sessions" / "repository-clones" / (token + ".json")
        self.assertLessEqual(journal.stat().st_size, 16384)
        self.assertTrue(receipt["detail"].endswith(" [truncated]"))
        self.assertEqual(self.hgs("repository", "status", "--request-id", token, "--json"), receipt)
        self.assertEqual(self.operation("clone", token=token), receipt)

    @unittest.skipIf(os.name == "nt", "Unix directory replacement; Windows denies rename while pinned")
    def test_destination_replacement_never_receives_git_writes(self):
        for kind in ("directory", "symlink"):
            with self.subTest(kind=kind):
                name = "checkout-" + kind
                foreign = self.root / ("foreign-" + kind)
                foreign.mkdir()
                sentinel = self.root / ("foreign-sentinel-" + kind)
                sentinel.write_text("retained")
                self.synthetic_git("if 'clone' in sys.argv:\n"
                                   f"    target = root / {name!r}\n"
                                   f"    target.rename(root / {'owned-' + kind!r})\n"
                                   + ("    target.mkdir()\n" if kind == "directory" else
                                      f"    target.symlink_to(root / {'foreign-' + kind!r}, target_is_directory=True)\n"))
                receipt = self.operation("clone", name=name, token=str(uuid.uuid4()))
                self.assertEqual(receipt["status"], "failed")
                self.assertFalse((self.root / name / ".git").exists(), "Git wrote into a replacement")
                self.assertEqual(list(foreign.iterdir()), [])
                self.assertEqual(sentinel.read_text(), "retained")
                self.assertEqual((self.root / ("owned-" + kind) / "content").read_text(), "preserved\n")

    @unittest.skipIf(os.name == "nt", "Unix directory replacement; Windows denies rename while pinned")
    def test_parent_replacement_retains_owned_checkout_and_foreign_tree(self):
        parent = self.root / "parent"
        parent.mkdir()
        self.synthetic_git("if 'clone' in sys.argv:\n"
                           "    (root / 'parent').rename(root / 'owned-parent')\n"
                           "    (root / 'parent').mkdir()\n"
                           "    (root / 'parent' / 'foreign').write_text('retained')\n")
        receipt = self.operation("clone", parent=parent, token=str(uuid.uuid4()))
        self.assertEqual(receipt["status"], "failed")
        self.assertEqual([p.name for p in parent.iterdir()], ["foreign"])
        self.assertEqual((parent / "foreign").read_text(), "retained")
        self.assertEqual((self.root / "owned-parent" / "checkout" / "content").read_text(), "preserved\n")

    @unittest.skipIf(os.name == "nt", "Unix wrapper SIGKILL / lifeline regression")
    def test_wrapper_shutdown_cancels_only_owned_git_group(self):
        self.synthetic_git("if 'clone' in sys.argv:\n"
                           "    subprocess.Popen([sys.executable, '-c', \"import pathlib,time; time.sleep(2); pathlib.Path(\" + repr(str(root / 'escaped-grandchild')) + \").touch()\"])\n"
                           "    (root / 'started').write_text(str(os.getpid()))\n"
                           "    time.sleep(2)\n"
                           "    (root / 'escaped-git').touch()\n"
                           "    sys.exit(0)\n")
        unrelated = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"],
                                     env=self.env, stdin=subprocess.DEVNULL,
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.addCleanup(unrelated.wait)
        self.addCleanup(unrelated.terminate)
        token = str(uuid.uuid4())
        process = subprocess.Popen([str(HGS), "repository", "clone", "--url", self.url,
                                    "--parent", str(self.root), "--name", "checkout",
                                    "--request-id", token, "--json"], env=self.env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.addCleanup(process.wait)
        self.addCleanup(lambda: process.kill() if process.poll() is None else None)
        end = time.monotonic() + 10
        while not (self.root / "started").exists() and time.monotonic() < end:
            time.sleep(0.01)
        self.assertTrue((self.root / "started").exists())
        process.send_signal(signal.SIGKILL)
        process.communicate(timeout=5)
        time.sleep(2.2)  # Self-expiring counterexample children, no global process cleanup.
        self.assertFalse((self.root / "escaped-git").exists(), "Git outlived its wrapper/deadline owner")
        self.assertFalse((self.root / "escaped-grandchild").exists(), "Git descendant escaped cleanup")
        self.assertIsNone(unrelated.poll(), "Cleanup affected an unrelated process")
        receipt = self.hgs("repository", "status", "--request-id", token, "--json")
        self.assertEqual(receipt["status"], "unconfirmed")
        self.assertEqual(self.operation("clone", token=token), receipt)

    @unittest.skipIf(os.name == "nt", "Unix worker's independent deadline")
    def test_git_cannot_resolve_to_the_running_hgs_image(self):
        git = self.root / '.local/bin/git'
        git.parent.mkdir(parents=True, exist_ok=True)
        git.symlink_to(HGS)
        result = bounded_run([str(HGS), '__state', 'repository-git-worker', '5',
                              image_proof(HGS), '--version'], env=self.env, cwd=self.root,
                             input='', capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('Git resolves to the running hgs image', json.loads(result.stdout)['Err'])

    @unittest.skipIf(os.name == "nt", "Unix worker context guard")
    @unittest.skipUnless(shutil.which('cc'), 'native argv[0] shim fixture requires cc')
    def test_git_symlink_keeps_its_dispatch_name(self):
        manager = self.root / 'tool-manager'
        source = ('#include <stdio.h>\n#include <string.h>\n'
                  'int main(int argc, char **argv) { (void)argc; '
                  'const char *name = strrchr(argv[0], \'/\'); '
                  'if (strcmp(name ? name + 1 : argv[0], "git")) return 64; '
                  'puts("git shim"); return 0; }\n')
        subprocess.run(['cc', '-x', 'c', '-o', str(manager), '-'], input=source,
                       capture_output=True, text=True, check=True, timeout=10)
        git = self.root / '.local/bin/git'
        git.parent.mkdir(parents=True, exist_ok=True)
        git.symlink_to(manager)
        result = self.worker_with_lifeline()
        self.assertEqual(json.loads(result)['Ok'], list(b'git shim\n'))

    @unittest.skipIf(os.name == "nt", "Unix worker context guard")
    def test_git_descendant_cannot_reenter_the_repository_worker(self):
        self.synthetic_git("(root / 'git-called').touch()\n")
        self.env['HGS_REPOSITORY_GIT_WORKER'] = '1'
        result = bounded_run([str(HGS), '__state', 'repository-git-worker', '5',
                              image_proof(HGS), '--version'], env=self.env, cwd=self.root,
                             input='', capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn('Nested repository Git workers are refused', result.stderr)
        self.assertFalse((self.root / 'git-called').exists())

    @unittest.skipIf(os.name == "nt", "Unix worker's independent deadline")
    def test_actual_git_wrapper_cannot_reinvoke_the_worker(self):
        # Even a missing production guard remains bounded: the wrapper refuses
        # its own second visit instead of recursively spawning another worker.
        self.synthetic_git(
            "import json\n"
            "visits = root / 'worker-visits'\n"
            "if visits.exists():\n"
            "    visits.write_text('2')\n"
            "    sys.exit(0)\n"
            "visits.write_text('1')\n"
            f"nested = subprocess.run([{str(HGS)!r}, '__state', 'repository-git-worker', '1', {image_proof(HGS)!r}, '--version'], "
            "input='', capture_output=True, text=True, timeout=3)\n"
            "(root / 'nested-result').write_text(json.dumps([nested.returncode,nested.stderr]))\n"
            "sys.exit(1)\n"
        )
        self.worker_with_lifeline()
        code, error = json.loads((self.root / 'nested-result').read_text())
        self.assertEqual(code, 1, error)
        self.assertIn('Nested repository Git workers are refused', error)
        self.assertEqual((self.root / 'worker-visits').read_text(), '1')

    def worker_with_lifeline(self):
        command = [str(HGS), '__state', 'repository-git-worker', '5', image_proof(HGS), '--version']
        if sys.platform.startswith('linux'):
            command = [sys.executable, str(Path(__file__).with_name('bounded_process.py')),
                       '--seconds', '6', '--max-processes', '16', '--', *command]
        process = subprocess.Popen(command, env=self.env, cwd=self.root, stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            # Keep the original lifeline open while the real Git wrapper runs.
            process.wait(timeout=11)
            output, error = process.stdout.read(), process.stderr.read()
            self.assertEqual(process.returncode, 0, output + error)
            return output
        finally:
            if process.poll() is None:
                process.kill()
            process.communicate(timeout=3)

    @unittest.skipIf(os.name == "nt", "Unix worker's independent deadline")
    def test_worker_deadline_stops_git_without_wrapper_cancellation(self):
        self.synthetic_git("if 'clone' in sys.argv:\n"
                           "    time.sleep(2)\n"
                           "    (root / 'escaped-deadline').touch()\n"
                           "    sys.exit(0)\n")
        process = subprocess.Popen([str(HGS), "__state", "repository-git-worker", "1", image_proof(HGS), "clone"],
                                   cwd=self.root, env=self.env, stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.addCleanup(process.wait)
        self.addCleanup(lambda: process.kill() if process.poll() is None else None)
        # Keep the lifeline open: expiry, rather than parent EOF, must cancel Git.
        process.wait(timeout=5)
        self.assertEqual(process.returncode, 0, process.stderr.read())
        result = json.loads(process.stdout.read())
        process.stdin.close()
        process.stdout.close()
        process.stderr.close()
        self.assertIn("timed out", result["Err"])
        time.sleep(1.2)
        self.assertFalse((self.root / "escaped-deadline").exists())

    @unittest.skipIf(os.name == "nt", "Unix worker image admission across executable replacement")
    def test_worker_rejects_executable_replaced_after_parent_check(self):
        candidate = self.root / "worker-executable"
        replacement = self.root / "replacement"
        shutil.copy2(HGS, candidate)
        shutil.copy2(HGS, replacement)
        expected = image_proof(candidate)  # The proof admitted before pathname replacement.
        os.replace(replacement, candidate)
        self.assertNotEqual(expected, image_proof(candidate))
        self.synthetic_git("(root / 'git-called').touch()\n")

        def worker(proof):
            process = subprocess.Popen([str(candidate), "__state", "repository-git-worker", "5", proof, "--version"],
                                       cwd=self.root, env=self.env, stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                # Keep the lifeline open until this worker exits, for both controls.
                process.wait(timeout=10)
                return process.returncode, process.stdout.read(), process.stderr.read()
            finally:
                if process.poll() is None:
                    process.kill()
                process.wait()
                process.stdin.close()
                process.stdout.close()
                process.stderr.close()

        code, out, error = worker(expected)
        self.assertEqual(code, 1, (out, error))
        self.assertIn(b"image", error)
        self.assertFalse((self.root / "git-called").exists(), "Replaced executable started Git")
        # Causal positive control: that same actual image works with a current proof.
        code, out, error = worker(image_proof(candidate))
        self.assertEqual(code, 0, error)
        self.assertTrue((self.root / "git-called").exists())
        self.assertIn(b"git version", bytes(json.loads(out)["Ok"]))

    def test_missing_parent_and_dry_run_never_clone(self):
        self.operation("clone", parent=self.root / "missing", token=str(uuid.uuid4()), rc=1)
        self.hgs("--dry-run", "repository", "clone", "--url", self.url, "--parent", str(self.root),
                 "--name", "checkout", "--request-id", str(uuid.uuid4()), rc=1)
        self.assertFalse((self.root / "checkout").exists())

    def test_inherited_git_directory_cannot_redirect_the_clone(self):
        self.env.update(GIT_DIR=str(self.source / ".git"), GIT_WORK_TREE=str(self.source),
                        GIT_COMMON_DIR=str(self.source / ".git"))
        receipt = self.operation("clone", token=str(uuid.uuid4()))
        self.assertEqual(receipt["status"], "cloned")
        self.assertEqual((self.root / "checkout" / "content").read_text(), "preserved\n")
        self.assertEqual((self.source / "content").read_text(), "preserved\n")

    @unittest.skipIf(os.name == "nt", "symlink permission is not assumed")
    def test_parent_alias_is_resolved_on_the_computer(self):
        alias = self.root / "alias"
        alias.symlink_to(self.root, target_is_directory=True)
        receipt = self.operation("check", parent=alias)
        self.assertEqual(receipt["parent"], str(self.root))
        self.assertEqual(receipt["path"], str(self.root / "checkout"))
        self.assertFalse((self.root / "checkout").exists())

    def test_remote_dry_run_keeps_capability_and_noninteractive_guards(self):
        result = subprocess.run([str(HGS), "--dry-run", "@remote.invalid", "repository", "clone",
                                 "--url", self.url, "--parent", "/workspace", "--name", "example", "--request-id", str(uuid.uuid4())],
                                env=self.env, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("BatchMode=yes", result.stdout)
        self.assertIn("repository-clone-v1", result.stdout)
        self.assertFalse((self.root / "checkout").exists())

    def test_empty_repository_is_a_valid_checkout(self):
        self.git("checkout", "--orphan", "empty")
        empty = self.root / "empty-source"
        subprocess.run(["git", "init", "--bare", str(empty)], env=self.env, check=True, capture_output=True)
        self.git("config", "--global", "url." + empty.as_uri() + ".insteadOf", "https://empty.invalid/repo")
        receipt = self.operation("clone", url="https://empty.invalid/repo", token=str(uuid.uuid4()))
        self.assertEqual(receipt["status"], "cloned")
        self.assertTrue((self.root / "checkout" / ".git").is_dir())
