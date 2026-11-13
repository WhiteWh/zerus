"""The read-only Activity lines of submitted question routes (zerus-sa9qn.33)
through the real private desktop request lane; no upstream or native calls."""
from contextlib import closing
import sqlite3
import unittest
import uuid
import test_desktop as kernel


class QuestionRouteActivity(unittest.TestCase):
    setUp = kernel.Desktop.setUp
    cli = kernel.Desktop.cli
    serve = kernel.Desktop.serve
    stop = kernel.Desktop.stop

    def request(self, ok=True, **fields):
        return self.cli('desktop', 'request', '--json', data=dict(op='question_route_activity', **fields), ok=ok)

    def test_the_service_answers_exactly_and_changes_nothing(self):
        session, request_id = str(uuid.uuid4()), str(uuid.uuid4())
        # No service: no reply is made up.
        self.request(ok=False, request_id=request_id, session_id=session)
        self.serve()
        reply = self.request(request_id=request_id, session_id=session)
        self.assertEqual(reply, {'request_id': request_id, 'accepted': True, 'durable': False,
                                 'op': 'question_route_activity', 'session_id': session, 'routes': []})
        for bad in (dict(request_id=request_id),
                    dict(request_id=request_id, session_id=session, limit=100),
                    dict(request_id=request_id, session_id='0000000A-0000-4000-8000-000000000001'),
                    dict(request_id=request_id, session_id='00000000-0000-0000-0000-000000000000'),
                    dict(request_id='x', session_id=session)):
            refused = self.request(**bad)
            self.assertEqual((refused['accepted'], refused['code']), (False, 'invalid_question_route_activity'), bad)
        with closing(sqlite3.connect(self.desktop / 'state.sqlite3')) as db:
            indexes = {row[0] for row in db.execute(
                "SELECT name FROM sqlite_schema WHERE type='index' AND tbl_name='desktop_question_routes'")}
            routes = db.execute('SELECT count(*) FROM desktop_question_routes').fetchone()[0]
        self.assertLessEqual({'desktop_question_routes_child_submitted', 'desktop_question_routes_parent_submitted'}, indexes)
        self.assertEqual(routes, 0)
        self.assertFalse((self.root / 'unexpected-tool').exists())


if __name__ == '__main__':
    unittest.main()
