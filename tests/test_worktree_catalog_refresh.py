#!/usr/bin/env python3
"""Forced catalog refresh coalescing against private Git/profile fixtures."""
import copy
try:
    import fcntl
except ImportError:
    fcntl = None
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

REPO = Path(__file__).resolve().parents[1]
HGS = Path(os.environ.get("HGS_TEST_BIN", REPO / "target/debug/hgs")).resolve()
GIT = shutil.which("git")


@unittest.skipUnless(fcntl is not None and GIT, "requires private POSIX flock and Git")
class CatalogRefresh(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="hgs-catalog-refresh-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.repo, self.state = (self.root / n for n in ("repo", "state"))
        for name in ("repo", "state", "home", "config"):
            (self.root / name).mkdir(mode=0o700)
        # hgs searches $HOME/.local/bin, /opt/homebrew/bin and /usr/local/bin
        # before the caller's PATH (platform::prepared_path), so on a Mac with
        # Homebrew Git a wrapper found only through PATH is never run. The
        # private HOME's own bin folder comes first on every platform.
        self.bin = self.root / "home" / ".local" / "bin"
        self.bin.mkdir(parents=True, mode=0o700)
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith(("HGS_", "GIT_")) and k not in ("TMUX", "TMUX_PANE")}
        self.env.update(HOME=str(self.root / "home"), HGS_STATE_DIR=str(self.state),
                        HGS_CONFIG_DIR=str(self.root / "config"), HGS_SELF="fixture", HGS_PEERS="",
                        XDG_CONFIG_HOME=str(self.root / "config"),
                        XDG_DATA_HOME=str(self.root / "data"),
                        XDG_STATE_HOME=str(self.root / "xdg-state"),
                        XDG_CACHE_HOME=str(self.root / "cache"),
                        GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null", LC_ALL="C")
        self.git("init", "-q", "--initial-branch=fixture", str(self.repo))
        self.git("-C", str(self.repo), "-c", "user.name=Fixture", "-c",
                 "user.email=fixture@example.test", "commit", "--allow-empty", "-qm", "initial")
        self.trace = self.root / "trace.jsonl"
        self.trace.touch()
        wrapper = self.bin / "git"
        wrapper.write_text("#!" + sys.executable + "\n" + r'''
import json,os,subprocess,sys,time
from pathlib import Path
args=sys.argv[1:]
action='list' if args[-4:]==['worktree','list','--porcelain','-z'] else 'common' if '--git-common-dir' in args else 'root' if '--show-toplevel' in args else 'branch' if 'symbolic-ref' in args else 'other'
trace=Path(os.environ['CATALOG_TRACE'])
def log(event):
 fd=os.open(trace,os.O_WRONLY|os.O_APPEND)
 os.write(fd,(json.dumps({'case':os.environ['CATALOG_CASE'],'action':action,'event':event})+'\n').encode())
 os.close(fd)
previous=[json.loads(line) for line in trace.read_text().splitlines()]
count=sum(row['case']==os.environ['CATALOG_CASE'] and row['action']==action and row['event']=='begin' for row in previous)
log('begin')
if action=='list':
 time.sleep(float(os.environ.get('CATALOG_LIST_DELAY','0')))
if action==os.environ.get('CATALOG_EXPIRE') and (action!='common' or count>0):
 time.sleep(3.2)
if action=='common' and count>0 and os.environ.get('CATALOG_CHANGED_COMMON'):
 sys.stdout.write(os.environ['CATALOG_CHANGED_COMMON']+'\n');log('end');sys.exit(0)
r=subprocess.run([os.environ['CATALOG_REAL_GIT'],*args],capture_output=True)
log('end')
sys.stdout.buffer.write(r.stdout);sys.stderr.buffer.write(r.stderr);sys.exit(r.returncode)
''')
        wrapper.chmod(0o700)
        self.env.update(PATH=str(self.bin) + ":" + os.environ["PATH"],
                        CATALOG_TRACE=str(self.trace), CATALOG_REAL_GIT=GIT)
        self.children = []
        self.addCleanup(self.settle_children)

    def settle_children(self):
        for process in self.children:
            if process.poll() is None:
                process.terminate()
                try:
                    process.communicate(timeout=1)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.communicate(timeout=1)

    def git(self, *args):
        return subprocess.check_output([GIT, *args], env=self.env, stderr=subprocess.PIPE)

    def events(self, case, action=None, event="begin"):
        return [row for row in map(json.loads, self.trace.read_text().splitlines())
                if row["case"] == case and row["event"] == event
                and (action is None or row["action"] == action)]

    def begin(self, case, path=None, **extra):
        process = subprocess.Popen([str(HGS), "worktrees", "--path", str(path or self.repo),
                                    "--refresh", "--json"],
                                   env={**self.env, "CATALOG_CASE": case, **extra},
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.children.append(process)
        return process, time.monotonic()

    def finish(self, job):
        process, started = job
        stdout, stderr = process.communicate(timeout=6)
        self.assertEqual(process.returncode, 0, stderr)
        return json.loads(stdout), time.monotonic() - started

    def read(self, case, path=None, **extra):
        return self.finish(self.begin(case, path, **extra))[0]

    def wait_for(self, predicate):
        until = time.monotonic() + 2
        while not predicate():
            self.assertLess(time.monotonic(), until, "fixture did not reach its read boundary")
            time.sleep(0.005)

    def cache(self, kind):
        identity = str(self.repo / ".git" if kind == "repo" else self.repo)
        return self.state / "worktree-cache" / (kind + "-" + hashlib.sha256(identity.encode()).hexdigest() + ".json")

    def publish(self, change=lambda value: None, before=None, **extra):
        """A synthetic cache publisher holds the actual private repository lock."""
        self.assertEqual(self.read("warm")["state"], "ok")
        self.assertTrue(self.events("warm", "list"), "hgs did not run the fixture Git")
        self.trace.write_text("")
        cache = self.cache("repo")
        previous = json.loads(cache.read_text())
        previous["sampled_at"] = time.time() - 3600
        if before:
            before(previous)
        cache.write_text(json.dumps(previous))
        with cache.with_suffix(".lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            job = self.begin("waiting", **extra)
            self.wait_for(lambda: self.events("waiting", "common", "end"))
            time.sleep(0.08)  # The reader is now waiting on our private lock.
            current = copy.deepcopy(previous)
            current["sampled_at"] = time.time()
            change(current)
            temporary = cache.with_suffix(".publish")
            temporary.write_text(json.dumps(current))
            temporary.replace(cache)
        return self.finish(job)

    def test_hgs_runs_the_fixture_git_ahead_of_package_manager_git(self):
        self.assertEqual(self.read("selected")["state"], "ok")
        self.assertEqual(len(self.events("selected", "list")), 1, "hgs did not run the fixture Git")

    def test_two_standalone_refreshes_and_one_contended_pair(self):
        for case in ("standalone-a", "standalone-b"):
            value, elapsed = self.finish(self.begin(case))
            self.assertEqual(value["state"], "ok")
            self.assertLess(elapsed, 3)
        for kind in ("path", "repo"):
            path = self.cache(kind)
            value = json.loads(path.read_text())
            value["sampled_at"] = time.time() - 3600
            path.write_text(json.dumps(value))
        # Only the pair needs a held listing to stage contention. Leave room
        # inside the production three-second deadline for wrapped Git startup
        # and the selected-checkout queries, including on slower macOS runners.
        delayed = {"CATALOG_LIST_DELAY": "0.5"}
        first = self.begin("first", **delayed)
        self.wait_for(lambda: self.events("first", "list"))
        second = self.begin("second", **delayed)
        a, _ = self.finish(first)
        b, elapsed = self.finish(second)
        self.assertEqual(a["state"], "ok")
        self.assertEqual(b["state"], "ok", b)
        self.assertFalse(b["stale"])
        self.assertFalse(b["partial"])
        self.assertEqual(b["common_dir"], str(self.repo / ".git"))
        self.assertEqual(b["selected_root"], str(self.repo))
        self.assertEqual(b["selected_branch"], "fixture")
        self.assertEqual(a["sampled_at"], b["sampled_at"])
        self.assertEqual(self.events("second", "list"), [])
        self.assertEqual(len(self.events("second", "common")), 2)
        self.assertTrue(self.events("second", "root", "end"))
        self.assertTrue(self.events("second", "branch", "end"))
        self.assertLess(elapsed, 3)

    def test_a_nonwaiting_forced_reader_refreshes_even_a_current_cache(self):
        self.assertEqual(self.read("warm")["state"], "ok")
        self.assertEqual(self.read("not-waiting")["state"], "ok")
        self.assertEqual(len(self.events("not-waiting", "list")), 1)

    def test_only_a_new_complete_same_repository_snapshot_can_be_adopted(self):
        cases = {
            "pre-request": lambda v: v.update(sampled_at=time.time() - 1),
            "partial": lambda v: v.update(partial=True),
            "foreign": lambda v: v.update(common_dir=str(self.root / "foreign")),
            "non-ok": lambda v: v.update(state="timeout"),
            "malformed": lambda v: v.update(worktrees=[None]),
            "malformed-branch": lambda v: v["worktrees"][0].update(branch={}),
            "oversized": lambda v: v.update(worktrees=v["worktrees"] * 513),
            "future": lambda v: v.update(sampled_at=time.time() + 60),
            "unknown": lambda v: v.pop("sampled_at"),
        }
        for name, change in cases.items():
            with self.subTest(name=name):
                value, _ = self.publish(change)
                self.assertEqual(value["state"], "ok")
                self.assertEqual(len(self.events("waiting", "list")), 1, name)
                self.assertEqual(value["selected_root"], str(self.repo))

        value, _ = self.publish(before=lambda v: v.update(sampled_at=time.time() + 2))
        self.assertEqual(value["state"], "ok")
        self.assertEqual(len(self.events("waiting", "list")), 1,
                         "a post-request sample older than the pre-wait sample cannot coalesce")

    def test_a_coalesced_reader_still_checks_the_current_common_directory(self):
        foreign = self.root / "foreign"
        foreign.mkdir()
        value, _ = self.publish(CATALOG_CHANGED_COMMON=str(foreign))
        self.assertEqual(value["state"], "metadata_error")
        self.assertIn("Repository changed", value["detail"])
        self.assertEqual(self.events("waiting", "list"), [])

    def expired_refresh(self, **extra):
        """A forced refresh staged while its cache lock is held elsewhere.

        The fixture publishes while the reader is expected to be waiting on
        the lock. A loaded machine can deschedule the reader between its
        common query and that attempt: it then loads the publication itself
        or finds the lock already free, takes the valid non-coalesced path
        and lists, so the expired selected query is never issued. Repeat
        within a bound until the reader actually coalesced (a second common
        query); a coalesced attempt that reports success still fails below.
        """
        for _ in range(12):
            value, elapsed = self.publish(**extra)
            if len(self.events("waiting", "common")) >= 2:
                return value, elapsed
        self.fail("the fixture never staged a coalesced refresh: "
                  + json.dumps(self.trace.read_text().splitlines()[-20:]))

    def test_expired_selected_queries_are_not_hidden_as_success(self):
        for action in ("common", "root", "branch"):
            with self.subTest(action=action):
                value, elapsed = self.expired_refresh(CATALOG_EXPIRE=action)
                self.assertEqual(value["state"], "timeout", value)
                self.assertEqual(self.events("waiting", "list"), [])
                self.assertLess(elapsed, 3.7)

    def test_detached_and_bare_catalogs_keep_their_optional_selected_fields(self):
        self.git("-C", str(self.repo), "checkout", "--detach", "-q")
        detached = self.read("detached")
        self.assertEqual(detached["state"], "ok")
        self.assertIsNone(detached["selected_branch"])
        self.assertEqual(detached["selected_root"], str(self.repo))
        bare = self.root / "bare"
        self.git("init", "--bare", "-q", "--initial-branch=fixture", str(bare))
        value = self.read("bare", bare)
        self.assertEqual(value["state"], "ok")
        self.assertIsNone(value["selected_root"])
        self.assertEqual(value["worktrees"][0]["kind"], "bare")


if __name__ == "__main__":
    unittest.main()
