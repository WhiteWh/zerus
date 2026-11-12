"""The person's latch on a child's question route (zerus-sa9qn.50) through
the real private desktop service: the request lane an agent can reach never
changes it; it only reads it. No upstream or native calls."""
import unittest
import uuid
import test_desktop as kernel


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


if __name__ == '__main__':
    unittest.main()
