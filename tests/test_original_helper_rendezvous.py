"""Actual CLI dispatch/fast --help and original Popen custody, private data only.

This proves synchronization, never a native code-signature or installed caller
positive. The Mac adapter still performs its real observation before release.
"""
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import time
import unittest

HGS = Path(os.environ.get('HGS_TEST_BIN', Path(__file__).resolve().parents[1] / 'target/debug/hgs')).resolve()
ARG = '--desktop-helper-rendezvous-v1'
RELEASE = b'HGS-ORIGINAL-CHILD-V1\n'


@unittest.skipUnless(sys.platform.startswith(('linux', 'darwin')), 'Unix original helper pipe')
class OriginalHelperRendezvous(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='hgs-rendezvous-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(('HGS_', 'XDG_'))}
        for key, leaf in [('HOME', 'home'), ('HGS_CONFIG_DIR', 'config'), ('HGS_STATE_DIR', 'state'),
                          ('XDG_CONFIG_HOME', 'xdg-config'), ('XDG_STATE_HOME', 'xdg-state'),
                          ('XDG_DATA_HOME', 'xdg-data'), ('XDG_CACHE_HOME', 'cache'), ('TMPDIR', 'tmp')]:
            path = self.root / leaf
            path.mkdir(mode=0o700)
            self.env[key] = str(path)
        self.env['HGS_SELF'] = 'rendezvous-fixture'

    @staticmethod
    def reap(child):
        # Only this actual Popen child, never a discovered PID/process group.
        if child.poll() is None:
            child.terminate()
        try:
            child.communicate(timeout=3)
        except subprocess.TimeoutExpired:
            child.kill()
            child.communicate(timeout=3)

    def start(self, *args, stdout=subprocess.PIPE):
        child = subprocess.Popen([str(HGS), *args], stdin=subprocess.PIPE,
                                 stdout=stdout, stderr=subprocess.PIPE, env=self.env)
        self.addCleanup(self.reap, child)
        return child

    def blocked(self, child):
        # Deliberate slow pre-input observation, beyond a real fast help exit.
        # No readiness boolean grants permission: inspect this ORIGINAL child
        # and original pipes. A gate-removed mutant prints/EOFs and fails here.
        readable, _, _ = select.select([child.stdout, child.stderr], [], [], .15)
        self.assertEqual(readable, [], 'dispatch ran before original observation/release')
        self.assertIsNone(child.poll(), 'the original helper exited before observation')
        self.assertFalse(child.stdin.closed)

    def test_real_fast_help_exits_ungated_but_waits_through_original_observation(self):
        # Keep stdin open through exit, without requiring help to fit in an
        # unread pipe (Linux can give a loaded user's pipes only 8 KiB).
        with tempfile.TemporaryFile() as capture:
            old = self.start('--help', stdout=capture)
            self.assertEqual(old.wait(timeout=5), 0, 'real original --help is fast without stdin')
            _, error = old.communicate(timeout=2)
            capture.seek(0)
            plain = capture.read()
        self.assertIn(b'Desktop termination outcome ABI: desktop-termination-outcome-v1', plain)
        self.assertEqual(error, b'')
        child = self.start(ARG, '--help')
        self.blocked(child)
        actual, error = child.communicate(RELEASE, timeout=5)
        self.assertEqual((child.returncode, actual, error), (0, plain, b''))

    def test_refused_observation_withholds_release_and_never_dispatches(self):
        child = self.start(ARG, '--help')
        self.blocked(child)
        output, error = child.communicate(b'', timeout=3)
        self.assertEqual(child.returncode, 2)
        self.assertEqual(output, b'')
        self.assertEqual(error, b'hgs: original helper rendezvous refused\n')

    def test_prefix_is_consumed_without_losing_or_changing_original_json(self):
        args = ['desktop', 'frontend-request', '--json']
        original = b'{"abi":"private-invalid-fixture"}\n'
        plain = self.start(*args)
        output, error = plain.communicate(original, timeout=5)
        self.assertNotEqual(plain.returncode, 0)
        gated = self.start(ARG, *args)
        self.blocked(gated)
        actual, detail = gated.communicate(RELEASE + original, timeout=5)
        self.assertEqual((gated.returncode, actual, detail), (plain.returncode, output, error))
        self.assertNotIn(b'rendezvous refused', detail)

    def test_wrong_or_partial_prefix_and_nonpipe_cannot_dispatch_help(self):
        for prefix in [b'wrong prefix\n', RELEASE[:3]]:
            with self.subTest(prefix=prefix):
                child = self.start(ARG, '--help')
                output, error = child.communicate(prefix, timeout=3)
                self.assertEqual((child.returncode, output, error),
                                 (2, b'', b'hgs: original helper rendezvous refused\n'))
        # A regular input file cannot substitute for the owned parent pipe.
        path = self.root / 'input'
        path.write_bytes(RELEASE)
        with path.open('rb') as input_file:
            result = subprocess.run([str(HGS), ARG, '--help'], stdin=input_file,
                                    capture_output=True, env=self.env, timeout=3)
        self.assertEqual((result.returncode, result.stdout), (2, b''))

    def test_live_but_unreleased_parent_has_one_bounded_wait(self):
        child = self.start(ARG, '--help')
        before = time.monotonic()
        self.assertEqual(child.wait(timeout=8), 2)
        self.assertLess(time.monotonic() - before, 8)
        output, error = child.communicate(timeout=2)
        self.assertEqual((output, error), (b'', b'hgs: original helper rendezvous refused\n'))
