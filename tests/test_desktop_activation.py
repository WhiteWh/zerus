"""Real Unix frontend-request admission and idle activation reply, no native SDK.

The private native fixture owns the selected image/canonical socket and original
CLI child. It supplies cached test evidence; Root still checks kernel peers,
parent/birth/image, currentness, ancestry, order and the actual receiver path.
Run positive admission outside an agent ancestry, as required by the product.
"""
import hashlib
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import time
import unittest
import uuid

import test_desktop as kernel


@unittest.skipUnless(sys.platform.startswith(('linux', 'darwin')), 'Unix native frontend')
class Activation(unittest.TestCase):
    cli = kernel.Desktop.cli
    serve = kernel.Desktop.serve
    stop = kernel.Desktop.stop
    def setUp(self):
        kernel.Desktop.setUp(self)
        self.env['HGS_PEERS'] = ''
        self.frontend = self.root / '.local/bin/hgs-tray'
        source = Path(__file__).parent / 'fixtures/desktop_frontend.c'
        compiled = subprocess.run(['cc', '-std=c11', '-Wall', '-Wextra', '-Werror',
                                   str(source), '-o', str(self.frontend)],
                                  capture_output=True, text=True, timeout=30)
        self.assertEqual(compiled.returncode, 0, compiled.stderr)
        self.frontend.chmod(0o700)

    def begin(self, operation='activation_wait', wrong_birth=False, marked=False):
        _, status = self.serve(background=True)
        attempt, request_id = str(uuid.uuid4()), str(uuid.uuid4())
        request = dict(op=operation, request_id=request_id)
        envelope = dict(abi='desktop-ui-request-v1', attempt=attempt, request=request)
        env = dict(self.env)
        if marked:
            env['HGS_RUN_ID'] = 'private-native-frontend-parent'
        frontend = subprocess.Popen([str(self.frontend), str(self.root / 'tmp/hgs-tray'), str(kernel.HGS)],
                                    env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                    stderr=subprocess.PIPE, text=True)
        self.addCleanup(self.finish_child, frontend)
        self.assertTrue(select.select([frontend.stdout], [], [], 5)[0], 'native child setup bound')
        native = json.loads(frontend.stdout.readline(512))
        hello = dict(abi='desktop-delegation-v1', version=1, nonce=attempt,
                     application='com.hgdev.hgs-tray', gui='tauri',
                     build=dict(version='1.0.0', source_commit='a' * 40, source_dirty=False),
                     instance_epoch=str(uuid.uuid4()), writer_epoch=None,
                     configuration=status['configuration'], live=True, replay=False)
        cached = dict(attempt=attempt, request_id=request_id,
                      fingerprint=hashlib.sha256(json.dumps(request, separators=(',', ':')).encode()).hexdigest(),
                      child_pid=native['pid'], child_birth='replaced' if wrong_birth else native['birth'], serial=1)
        for value in (envelope, dict(hello=hello, request=cached)):
            frontend.stdin.write(json.dumps(value, separators=(',', ':')) + '\n')
        frontend.stdin.write(f'desktop-ui-request-v1 {attempt}\n')
        frontend.stdin.flush()
        return frontend, request_id, status

    @staticmethod
    def finish_child(frontend):
        if frontend.poll() is None:
            frontend.terminate()
        if frontend.stdin is not None:
            frontend.stdin.close()
            frontend.stdin = None
        frontend.communicate(timeout=8)

    def result(self, frontend):
        frontend.stdin.close()
        frontend.stdin = None
        output, error = frontend.communicate(timeout=25)
        self.assertEqual(frontend.returncode, 0, output + error)
        self.assertIn('original_reaped=1', error)
        return json.loads(output), error

    def test_authenticated_activation_read_reaches_real_receiver_and_returns_idle_reply(self):
        if sys.platform == 'darwin':
            import macos_ancestry
            topology = macos_ancestry.topology()
            print('ACTIVATION_OBSERVED_CHAIN ' + json.dumps(topology), file=sys.stderr)
            unsupported = macos_ancestry.unsupported_hosted_foreign(topology, os.getuid(), os.environ)
            if unsupported is not None:
                self.skipTest('observed unsupported hosted foreign ancestor: ' + json.dumps(unsupported))
        frontend, request, initial = self.begin()
        until = time.monotonic() + 8
        waiting = False
        while time.monotonic() < until and frontend.poll() is None:
            status = self.cli('desktop', 'status', '--json')
            waiting = status.get('activation_transport', {}).get('waiting', False)
            if waiting:
                break
            time.sleep(.02)
        reply, error = self.result(frontend)
        self.assertTrue(waiting, reply)
        self.assertEqual(reply, dict(abi='desktop-activation-v1', request_id=request,
                                    configuration=initial['configuration'],
                                    service_epoch=initial['epoch'], activation=None))
        self.assertIn('proofs=1 ', error)
        status = self.cli('desktop', 'status', '--json')
        self.assertFalse(status['activation_transport']['waiting'])
        self.assertFalse(status['automatic_approvals'])
        self.assertEqual(status['writers_enabled'], initial['writers_enabled'])
        self.assertEqual(status['desktop_ownership']['revision'], 0)

    def test_wrong_original_child_birth_refuses_before_receiver(self):
        frontend, request, _ = self.begin(wrong_birth=True)
        reply, error = self.result(frontend)
        self.assertEqual(reply, dict(request_id=request, accepted=False, durable=False,
                                    code='frontend_authentication_refused'))
        self.assertIn('proofs=1 ', error)
        self.assertFalse(self.cli('desktop', 'status', '--json')['activation_transport']['waiting'])

    def test_marked_native_frontend_cannot_admit_activation_reader(self):
        frontend, request, _ = self.begin(marked=True)
        reply, _ = self.result(frontend)
        self.assertEqual(reply, dict(request_id=request, accepted=False, durable=False,
                                    code='frontend_authentication_refused'))
        self.assertFalse(self.cli('desktop', 'status', '--json')['activation_transport']['waiting'])

    def test_unknown_operation_refuses_before_native_proof_or_receiver(self):
        frontend, request, _ = self.begin(operation='unknown_operation')
        reply, error = self.result(frontend)
        self.assertEqual(reply, dict(request_id=request, accepted=False, durable=False,
                                    code='frontend_authentication_refused'))
        self.assertIn('proofs=0 ', error)
        self.assertFalse(self.cli('desktop', 'status', '--json')['activation_transport']['waiting'])


if __name__ == '__main__':
    unittest.main()
