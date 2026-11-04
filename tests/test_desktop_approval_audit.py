"""Selected-service audit reads through the real CLI and private passive actor."""
import json
import os
import sqlite3
import unittest
import uuid

import test_desktop as kernel


class ApprovalAudit(unittest.TestCase):
    setUp = kernel.Desktop.setUp
    cli = kernel.Desktop.cli
    serve = kernel.Desktop.serve
    stop = kernel.Desktop.stop

    def request(self, status, **fields):
        value = dict(op='approval_audit_list', request_id=str(uuid.uuid4()),
                     configuration=status['configuration'], epoch=status['epoch'])
        value.update(fields)
        reply = self.cli('desktop', 'request', '--json', data=value)
        self.assertEqual(reply['request_id'], value['request_id'])
        self.assertEqual(reply['configuration'], status['configuration'])
        self.assertEqual(reply['service_epoch'], status['epoch'])
        self.assertTrue(reply['read_only'])
        return reply

    def rows(self):
        root = self.state / 'approval-audit'
        root.mkdir(mode=0o700)
        path = root / 'journal.sqlite3'
        with sqlite3.connect(path) as db:
            db.executescript('''PRAGMA user_version=1;
                CREATE TABLE attempts(id INTEGER PRIMARY KEY, identity TEXT NOT NULL UNIQUE,
                    request_id TEXT NOT NULL UNIQUE, at INTEGER NOT NULL, host TEXT NOT NULL,
                    session TEXT NOT NULL, provider TEXT NOT NULL, prompt TEXT NOT NULL,
                    question TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'prepared',
                    detail TEXT NOT NULL DEFAULT '');
                CREATE INDEX attempts_session ON attempts(session,id DESC);
                CREATE INDEX attempts_host ON attempts(host,id DESC);
                CREATE INDEX attempts_host_session ON attempts(host,session,id DESC);''')
            question = json.dumps(dict(question=dict(question_id='original'), answers=[]))
            db.executemany('INSERT INTO attempts VALUES(?,?,?,?,?,?,?,?,?,?,?)',
                           [(n, f'fixture-{n}', str(uuid.uuid4()), n, 'a.example',
                             'agent/project/fixture', 'claude', 'Fixture question',
                             question, 'approved', 'Fixture outcome') for n in range(1, 54)])
        db.close()
        path.chmod(0o600)
        return path

    def test_passive_reader_keeps_page_identity_and_database_bytes(self):
        self.state.mkdir(mode=0o700)
        path = self.rows()
        original = path.read_bytes()
        _, status = self.serve()
        first = self.request(status)
        self.assertTrue(first['accepted'])
        self.assertEqual(len(first['result']['entries']), 50)
        self.assertEqual(first['result']['through'], 53)
        page = dict(journal=first['journal'], through=first['result']['through'])
        second = self.request(status, before=first['result']['next'], **page)
        self.assertTrue(second['accepted'])
        self.assertEqual(second['journal'], first['journal'])
        self.assertEqual([v['id'] for v in second['result']['entries']], [3, 2, 1])
        detail = self.request(status, op='approval_audit_detail', journal=first['journal'], id=53)
        self.assertTrue(detail['accepted'])
        self.assertEqual(detail['result']['question']['question_id'], 'original')
        stale = self.request(status, epoch=str(uuid.uuid4()))
        self.assertFalse(stale['accepted'])
        self.assertEqual(stale['code'], 'approval_audit_scope_changed')
        current = self.cli('desktop', 'status', '--json')
        self.assertEqual(current['epoch'], status['epoch'])
        self.assertFalse(current['automatic_approvals'])
        self.assertEqual(current['writers_enabled'], status['writers_enabled'])
        self.assertEqual(path.read_bytes(), original)
        self.assertFalse((self.root / 'unexpected-tool').exists())

    def test_absent_read_creates_no_audit_state(self):
        _, status = self.serve()
        absent = self.request(status)
        self.assertTrue(absent['accepted'])
        self.assertEqual(absent['result']['entries'], [])
        self.assertIsNone(absent['journal'])
        self.assertFalse((self.state / 'approval-audit').exists())

    @unittest.skipUnless(os.name == 'posix', 'Unix open-journal replacement')
    def test_replacement_refuses_old_page(self):
        _, status = self.serve()
        path = self.rows()
        first = self.request(status)
        self.assertTrue(first['accepted'])
        os.replace(path, path.with_suffix('.original'))
        # Even an absent replacement cannot make old cursors adopt a new DB.
        stale = self.request(status, journal=first['journal'], through=53)
        self.assertFalse(stale['accepted'])
        self.assertEqual(stale['code'], 'approval_audit_unavailable')
        refreshed = self.request(status)
        self.assertTrue(refreshed['accepted'])
        self.assertIsNone(refreshed['journal'])


if __name__ == '__main__':
    unittest.main()
