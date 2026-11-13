"""The key observer must leave ordinary native reads on their original path."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest

import native_effort_id as identity
import test_input


class NativeEffortObserver(unittest.TestCase):
    script = test_input.InputTransport.script

    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='hgs-key-observer-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.original = '#!/bin/sh\nprintf "<%s>\\n" "$@"\nprintf native-stderr >&2\ncat\nexit 7\n'
        self.script('tmux', self.original)
        self.env = dict(PATH=os.defpath, HOME=str(self.root))

    def invoke(self, args):
        return subprocess.run([str(self.bin / 'tmux'), *args], env=self.env,
                              input='literal input\n', capture_output=True, text=True,
                              timeout=5)

    def test_non_key_calls_exec_the_original_without_starting_the_key_observer(self):
        attempts = identity.key_log(self)
        # A read must not start the observer at all. This is independent of
        # interpreter startup speed and pins the actual overhead regression.
        (self.root / 'native-effort-tmux.py').write_text('raise RuntimeError("key observer ran for a read")\n')
        args = ['capture-pane', '-p', '-t', 'pane with spaces']
        result = self.invoke(args)
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertEqual(result.stdout, ''.join('<' + arg + '>\n' for arg in args) + 'literal input\n')
        self.assertEqual(result.stderr, 'native-stderr')
        self.assertEqual(attempts(), b'')

    def test_keys_keep_the_log_and_outer_receipt_proof_before_forwarding(self):
        receipt = self.root / 'receipt.json'
        value = dict(status='in_progress', request_id='fixture-request', digest='fixture-digest')
        receipt.write_text(json.dumps(value))
        self.env['ZERUS_PENDING_SEND_RECEIPT'] = str(receipt)
        outer = self.root / 'native-pending-outer-receipts.jsonl'
        self.script('tmux', '#!/bin/sh\ntest -s ' + shlex.quote(str(outer)) + ' || exit 99\n' + self.original.split('\n', 1)[1])
        attempts = identity.key_log(self)
        args = ['send-keys', '-t', 'pane with spaces', '-l', '/model']
        result = self.invoke(args)
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertEqual(result.stdout, ''.join('<' + arg + '>\n' for arg in args) + 'literal input\n')
        self.assertEqual(result.stderr, 'native-stderr')
        self.assertEqual([json.loads(line) for line in attempts().splitlines()], [args])
        self.assertEqual([json.loads(line) for line in outer.read_text().splitlines()],
                         [dict(args=args, receipt=value)])

    def test_an_existing_observer_is_reused_byte_exactly(self):
        log = self.root / 'existing.jsonl'
        log.write_bytes(b'["original"]\n')
        attempts = identity.key_log(self, existing=log)
        self.assertEqual(attempts(), b'["original"]\n')
        self.assertEqual((self.bin / 'tmux').read_text(), self.original)
        self.assertFalse((self.root / 'native-effort-tmux.py').exists())


if __name__ == '__main__':
    unittest.main()
