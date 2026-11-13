"""The native key observer preserves direct reads and exact effect forwarding."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest

import native_effort_id as identity


class Fixture:
    def __init__(self, root):
        self.root = root
        self.bin = root / 'bin'
        self.bin.mkdir()
        self.script('tmux', '#!/bin/sh\nprintf \'%s\\0\' "$@" > ' +
                    shlex.quote(str(root / 'argv')) +
                    "\nprintf 'native stderr\\n' >&2\ncat\nexit 17\n")

    def script(self, name, body):
        path = self.bin / name
        path.write_text(body)
        path.chmod(0o700)


@unittest.skipUnless(os.name == 'posix', 'Unix native tmux observer')
class NativeEffortObserver(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='native-observer-')
        self.addCleanup(self.temporary.cleanup)
        self.case = Fixture(Path(self.temporary.name))
        self.attempts = identity.key_log(self.case)
        self.env = dict(PATH='/usr/bin:/bin', HOME=str(self.case.root))

    def call(self, *args):
        return subprocess.run([str(self.case.bin / 'tmux'), *args], input=b'private stdin\x00tail',
                              env=self.env, capture_output=True, timeout=5)

    def test_reads_and_buffer_operations_do_not_start_the_observer(self):
        # A missing observer must be irrelevant to every non-key operation.
        (self.case.root / 'native-effort-tmux.py').unlink()
        for operation in ('capture-pane', 'display-message', 'load-buffer', 'paste-buffer', 'delete-buffer'):
            with self.subTest(operation=operation):
                result = self.call(operation, 'argument with spaces', '-p')
                self.assertEqual(result.returncode, 17, result.stderr)
                self.assertEqual(result.stdout, b'private stdin\x00tail')
                self.assertEqual(result.stderr, b'native stderr\n')
                self.assertEqual(self.attempts(), b'')
                self.assertEqual((self.case.root / 'argv').read_bytes().split(b'\0')[:-1],
                                 [operation.encode(), b'argument with spaces', b'-p'])

    def test_keys_keep_exact_arguments_streams_exit_and_pre_effect_receipt(self):
        receipt = self.case.root / 'input.json'
        expected = dict(status='in_progress', request_id='fixture-id', digest='fixture-digest')
        receipt.write_text(json.dumps(dict(expected, text='not part of the observation')))
        self.env['ZERUS_PENDING_SEND_RECEIPT'] = str(receipt)
        delivered = self.case.root / 'delivered.json'
        original = self.case.bin / 'tmux-native-effort-original'
        original.write_text('#!/bin/sh\nprintf \'%s\\0\' "$@" > ' +
                            shlex.quote(str(self.case.root / 'argv')) + '\ncat ' + shlex.quote(str(
            self.case.root / 'native-pending-outer-receipts.jsonl')) + ' > ' +
                            shlex.quote(str(delivered)) +
                            "\nprintf 'native stderr\\n' >&2\ncat\nexit 17\n")
        args = ['send-keys', '-t', '%0', '-l', "spaces ' quotes $literal"]
        result = self.call(*args)
        self.assertEqual(result.returncode, 17, result.stderr)
        self.assertEqual(result.stdout, b'private stdin\x00tail')
        self.assertEqual(result.stderr, b'native stderr\n')
        self.assertEqual([json.loads(line) for line in self.attempts().splitlines()], [args])
        self.assertEqual(json.loads(delivered.read_text()), dict(args=args, receipt=expected))
        self.assertEqual((self.case.root / 'argv').read_bytes().split(b'\0')[:-1],
                         [arg.encode() for arg in args])


if __name__ == '__main__':
    unittest.main()
