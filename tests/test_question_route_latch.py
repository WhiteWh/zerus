"""The person's latch on a child's question route (zerus-sa9qn.50) through
the real private desktop service: the request lane an agent can reach never
changes it; it only reads it. No upstream or native calls."""
import json
import os
import time
import unittest
import uuid
from pathlib import Path
import test_desktop as kernel

# A fake connector (zerus-sa9qn.50): `--version`, and the workspace verifier
# echoing the statement exactly. Its behaviour comes from a file beside it,
# since the service runs it with a cleared environment.
FAKE = r"""#!/usr/bin/python3
import json, os, sys, time
here = os.path.dirname(os.path.abspath(__file__))
mode = open(os.path.join(here, 'mode')).read().strip()
if sys.argv[1:] == ['--version']:
    print('zerus-mobile-connector 0.1.0' if mode != 'version' else 'zerus-mobile 0.1.0')
    sys.exit(0)
data = sys.stdin.read()
with open(os.path.join(here, 'calls'), 'a') as log:
    log.write(json.dumps({'argv': sys.argv[1:], 'stdin': json.loads(data)}) + '\n')
statement = json.loads(data)['statement']
if mode == 'slow':
    time.sleep(8)
if mode == 'mutate':
    with open(os.path.abspath(__file__), 'a') as me:
        me.write('# changed\n')
reply = {'ok': True, 'verifier': 'question-route-person-v1', 'verified': statement}
if mode == 'unmarked':
    del reply['verifier']
if mode == 'extra':
    reply['granted'] = True
if mode == 'marker':
    reply['verifier'] = 'question-route-person-v2'

print(json.dumps(reply))
"""


class QuestionRouteLatch(unittest.TestCase):
    setUp = kernel.Desktop.setUp
    cli = kernel.Desktop.cli
    serve = kernel.Desktop.serve
    stop = kernel.Desktop.stop

    def request(self, **fields):
        return self.cli('desktop', 'request', '--json', data=fields)

    def status(self, session):
        request_id = str(uuid.uuid4())
        reply = self.request(op='question_route_child_status', request_id=request_id, session_id=session)
        self.assertEqual(reply, {'request_id': request_id, 'accepted': True, 'durable': False,
                                 'op': 'question_route_child_status', 'session_id': session,
                                 'person_off': False, 'revision': 0})
        return reply

    def test_the_request_lane_reads_the_latch_and_never_changes_it(self):
        self.serve()
        session = str(uuid.uuid4())
        self.status(session)
        # The person's change goes only through the native frontend lane.
        change = dict(op='question_route_child', request_id=str(uuid.uuid4()), session_id=session,
                      run_id='run-1', conversation_id='conversation-1', person_off=False, expected_revision=0)
        for attempt in (change, dict(change, person_off=True)):
            refused = self.request(**attempt)
            self.assertIsNot(refused.get('accepted'), True, refused)
            self.assertNotIn('person_off', refused)
        self.status(session)
        lookup = self.request(op='question_route_child_result', request_id=str(uuid.uuid4()),
                              original_request_id=change['request_id'])
        self.assertEqual((lookup['accepted'], lookup['found'], lookup['result']), (True, False, None))
        for bad in (dict(op='question_route_child_status', request_id=str(uuid.uuid4())),
                    dict(op='question_route_child_status', request_id=str(uuid.uuid4()), session_id='x'),
                    dict(op='question_route_child_status', request_id=str(uuid.uuid4()), session_id=session, extra=1)):
            self.assertFalse(self.request(**bad)['accepted'], bad)
        self.assertFalse((self.root / 'unexpected-tool').exists())


class PhoneStatementVerifier(unittest.TestCase):
    """The service runs its own pinned connector to verify a phone's statement;
    any failure, timeout or unknown output is a refusal."""
    setUp_kernel = kernel.Desktop.setUp
    cli = kernel.Desktop.cli
    serve = kernel.Desktop.serve
    stop = kernel.Desktop.stop

    def setUp(self):
        self.setUp_kernel()
        mobile = self.config / 'mobile'
        mobile.mkdir(mode=0o700)
        config = mobile / 'connector.json'
        config.write_text(json.dumps({'server_url': 'https://relay.example.invalid',
                                      'encrypted_workspace': {'version': 1, 'state_dir': str(mobile / 'workspace-private')}}))
        config.chmod(0o600)
        self.fake_dir = self.root / 'connector'
        self.fake_dir.mkdir(mode=0o700)
        self.fake = self.fake_dir / 'zerus-mobile-connector'
        self.fake.write_text(FAKE)
        self.fake.chmod(0o700)
        self.env['ZERUS_CLIENT_MOBILE_CONNECTOR_PATH'] = str(self.fake)
        self.serve()

    def statement(self):
        return {'workspace_id': 'workspace-fixture', 'device_id': 'phone-1', 'gateway_id': 'gateway-1',
                'session_id': str(uuid.uuid4()), 'run_id': 'run-1', 'conversation_id': 'conversation-1',
                'person_off': True, 'request_id': str(uuid.uuid4()), 'issued_at': 1, 'expires_at': 2}

    def send(self, mode):
        (self.fake_dir / 'mode').write_text(mode)
        request = {'op': 'question_route_person', 'statement': self.statement(), 'signature': 'x' * 86}
        return request, self.cli('desktop', 'request', '--json', data=request)

    def test_an_exact_verified_reply_passes_verification(self):
        request, reply = self.send('good')
        # Verified, then refused only because this computer has no such child.
        self.assertEqual((reply['accepted'], reply['code']), (False, 'not_found'), reply)
        calls = [json.loads(line) for line in (self.fake_dir / 'calls').read_text().splitlines()]
        self.assertEqual(calls, [{'argv': ['--workspace', 'verify-question-route-person', '--config',
                                           str((self.config / 'mobile' / 'connector.json').resolve())],
                                  'stdin': {'statement': request['statement'], 'signature': request['signature']}}])

    def test_any_other_verifier_outcome_is_a_refusal(self):
        for mode in ('unmarked', 'extra', 'marker', 'version', 'mutate'):
            with self.subTest(mode=mode):
                self.fake.write_text(FAKE)
                self.fake.chmod(0o700)
                _, reply = self.send(mode)
                self.assertEqual((reply['accepted'], reply['code']), (False, 'not_verified'), reply)
        # A binary others can write is never run.
        self.fake.write_text(FAKE)
        self.fake.chmod(0o722)
        (self.fake_dir / 'calls').unlink(missing_ok=True)
        _, reply = self.send('good')
        self.assertEqual(reply['code'], 'not_verified')
        self.assertFalse((self.fake_dir / 'calls').exists())

    def test_a_verifier_that_does_not_finish_in_time_is_a_refusal(self):
        started = time.monotonic()
        _, reply = self.send('slow')
        self.assertEqual(reply['code'], 'not_verified')
        # Within the request lane's own 2 s reply bound.
        self.assertLess(time.monotonic() - started, 2.5)


if __name__ == '__main__':
    unittest.main()
