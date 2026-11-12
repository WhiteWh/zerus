"""`hgs terminate|archive SESSION --with-children` (zerus-sa9qn.17), in private state without real agents.

Only the explicit flag ends a parent's live child sessions, after the parent and
through each child's own rules; a child's refusal is reported and never retried.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

HGS = Path(os.environ.get('HGS_TEST_BIN', Path(__file__).resolve().parents[1] / 'target/debug/hgs')).resolve()
MACHINE = '11111111-1111-4111-8111-111111111111'


def sid(n):
    return f'00000000-0000-4000-8000-{n:012d}'


@unittest.skipIf(os.name == 'nt', 'POSIX fixture records')
class EndChildren(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='zec-', dir='/tmp')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.state = self.root / 'state'; self.state.mkdir()
        self.home = self.root / 'home'; self.home.mkdir()
        sockets = self.root / 'sockets'; sockets.mkdir()
        self.env = {k: v for k, v in os.environ.items() if not k.startswith('HGS_') and k != 'TMUX'}
        self.env.update(HOME=str(self.home), USERPROFILE=str(self.home), TMUX_TMPDIR=str(sockets),
                        HGS_CONFIG_DIR=str(self.root / 'config'), HGS_STATE_DIR=str(self.state),
                        HGS_SELF='fixture', HGS_PEERS='')
        (self.state / 'local-sessions').touch()
        self.start = subprocess.check_output(['ps', '-p', str(os.getpid()), '-o', 'lstart='],
                                             env=dict(self.env, LC_ALL='C', TZ='UTC'), text=True).strip()
        # The parent, two children, a grandchild, a child whose agent still runs
        # without its terminal (every action refuses it), a link another computer
        # forwarded, an already archived child's sibling chain and a stranger.
        self.parent = self.record(1, 'claude', 'lead')
        self.first = self.record(2, 'codex', 'worker-a', parent=1)
        self.second = self.record(3, 'kimi', 'worker-b', parent=1, source='adopt')
        self.grandchild = self.record(4, 'codex', 'grand', parent=2)
        self.busy = self.record(5, 'claude', 'busy', parent=1, alive=True)
        self.forwarded = self.record(6, 'codex', 'forwarded', parent=1, source='forwarded')
        self.stranger = self.record(7, 'claude', 'stranger')
        self.children = [self.first, self.second, self.busy, self.grandchild]

    def record(self, n, agent, label, parent=None, source='launch', alive=False):
        record = dict(version=1, name=f'{agent}/example/{label}', agent=agent, run_id=f'run-{n}',
                      conversation_id=f'conversation-{n}', session_id=sid(n), account_id='fixture',
                      subagents={}, created=1700000000 + n, cwd=str(self.root),
                      pid=os.getpid() if alive else 1, process_start=self.start if alive else 'not this process')
        if parent:
            record['parent'] = dict(schema=1, machine=MACHINE, host='fixture', session_id=sid(parent),
                                    name=f'parent-{parent}', conversation_id=f'conversation-{parent}',
                                    run_id=f'run-{parent}', source=source, at=1700000000)
        self.path(record).write_text(json.dumps(record))
        return record

    def path(self, record):
        return self.state / (hashlib.sha256(record['name'].encode()).hexdigest() + '.json')

    def hgs(self, *args):
        return subprocess.run([str(HGS), *args], env=self.env, capture_output=True, text=True, timeout=60)

    def archived(self):
        directory = self.state / 'archives'
        if not directory.is_dir():
            return []
        return sorted(json.loads(p.read_text())['name'] for p in directory.glob('*.json'))

    def listing(self):
        result = self.hgs('ls', '--local', '--json')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return {row['name']: row['state'] for row in json.loads(result.stdout)['sessions']}

    def assert_untouched(self, *records):
        for record in records:
            self.assertTrue(self.path(record).exists(), record['name'])
            self.assertNotIn(record['name'], self.archived())

    def test_fixture_lists_every_session_saved_with_its_link(self):
        states = self.listing()
        for record in [self.parent, *self.children, self.forwarded, self.stranger]:
            self.assertEqual(states.get(record['name']), 'stopped', states)

    def test_without_the_flag_no_child_is_touched(self):
        for command in ('archive', 'terminate'):
            with self.subTest(command=command):
                result = self.hgs(command, self.parent['name'])
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(self.archived(), [self.parent['name']])
                self.assert_untouched(*self.children, self.forwarded, self.stranger)
                self.assertNotIn('child session', result.stdout + result.stderr)
                self.setUp()

    def check_propagated(self, command, verb):
        result = self.hgs(command, self.parent['name'], '--with-children')
        out = result.stdout + result.stderr
        # One child refused: the command reports it and fails, nothing retried.
        self.assertEqual(result.returncode, 1, out)
        ended = [self.parent, self.first, self.second, self.grandchild]
        self.assertEqual(self.archived(), sorted(r['name'] for r in ended), out)
        for record in ended:
            self.assertFalse(self.path(record).exists(), record['name'])
        self.assert_untouched(self.busy, self.forwarded, self.stranger)
        # The parent first, then each child breadth first, every child before
        # the grandchild, each reported by its own command as it would be alone.
        done = 'terminated and archived' if command == 'terminate' else 'archived'
        lines = [line.split(' (')[0] for line in result.stdout.splitlines()]
        self.assertEqual(lines, [f'hgs: {done} {r["name"]}' for r in (self.parent, self.first, self.second, self.grandchild)])
        self.assertIn(f'hgs: child session {self.busy["name"]} was not {verb}', result.stderr)
        self.assertIn('not retried', result.stderr)
        self.assertIn(f'1 of 4 child sessions were not {verb}; nothing was retried', result.stderr)
        self.assertEqual(len([line for line in out.splitlines() if self.busy['name'] in line]), 1, out)
        # The refused child is still listed, as before; a repeat names no ended parent.
        self.assertEqual(self.listing().get(self.busy['name']), 'stopped')
        again = self.hgs(command, self.parent['name'], '--with-children')
        self.assertNotEqual(again.returncode, 0)
        self.assertIn('is not one listed live session here', again.stderr)
        self.assertNotIn(self.busy['name'], again.stdout + again.stderr)

    def test_archive_with_children_ends_each_live_descendant_after_the_parent(self):
        self.check_propagated('archive', 'archived')

    def test_terminate_with_children_ends_each_live_descendant_after_the_parent(self):
        self.check_propagated('terminate', 'terminated')

    def test_a_parent_that_is_refused_ends_no_child(self):
        self.record(1, 'claude', 'lead', alive=True)
        for command in ('archive', 'terminate'):
            with self.subTest(command=command):
                result = self.hgs(command, self.parent['name'], '--with-children')
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertEqual(self.archived(), [])
                self.assert_untouched(self.parent, *self.children, self.forwarded, self.stranger)
                self.assertNotIn('child session', result.stdout + result.stderr)

    def test_a_dry_run_lists_the_children_and_changes_nothing(self):
        result = self.hgs('terminate', '--dry-run', self.parent['name'], '--with-children')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        lines = result.stdout.splitlines()
        would = lambda r, level: f'hgs: would also terminate child session {r["name"]} (level {level})'
        self.assertEqual(lines[0], f'hgs: would terminate and archive {self.parent["name"]}')
        # Every child, in listing order, before the grandchild.
        self.assertEqual(sorted(lines[1:4]), sorted(would(r, 1) for r in (self.first, self.second, self.busy)))
        self.assertEqual(lines[4:], [would(self.grandchild, 2)])
        self.assertEqual(self.archived(), [])
        self.assert_untouched(self.parent, *self.children)

    def test_the_expected_run_and_session_id_select_the_parent(self):
        changed = self.hgs('terminate', self.parent['name'], '--expected-run-id', 'run-other', '--with-children')
        self.assertNotEqual(changed.returncode, 0)
        self.assertEqual(self.archived(), [])
        self.assert_untouched(self.parent, *self.children)
        by_id = self.hgs('archive', '--session-id', sid(2), '--with-children')
        self.assertEqual(by_id.returncode, 0, by_id.stdout + by_id.stderr)
        self.assertEqual(self.archived(), sorted([self.first['name'], self.grandchild['name']]))
        self.assert_untouched(self.parent, self.second, self.busy)

    def move_to_id_layout(self, record):
        """Moves `record` into the ID layout, as test_pause.py does: its label
        index, sessions/<id>.json and ids/ entry, then the label path."""
        digest = hashlib.sha256(record['name'].encode()).hexdigest()
        for folder in ('labels', 'sessions', 'ids'):
            (self.state / folder).mkdir(exist_ok=True)
        (self.state / 'labels' / f'{digest}.json').write_text(
            json.dumps(dict(version=1, label=record['name'], ids=[record['session_id']])) + '\n')
        moved = self.state / 'sessions' / f'{record["session_id"]}.json'
        moved.write_text(json.dumps(dict(record, layout='id')) + '\n')
        (self.state / 'ids' / f'{record["session_id"]}.json').write_text(
            json.dumps(dict(version=1, name=record['name'], kind='id')) + '\n')
        self.path(record).unlink()
        return moved

    def test_each_child_is_bound_to_its_own_session_on_a_shared_label(self):
        # The child moves to the ID layout; an unrelated session then holds its
        # label. Only the listed child is archived, never the label's holder.
        moved = self.move_to_id_layout(self.first)
        twin = dict(self.first, session_id=sid(9), run_id='run-twin', conversation_id='conversation-twin')
        twin.pop('parent')
        self.path(twin).write_text(json.dumps(twin))
        twin_bytes = self.path(twin).read_bytes()
        result = self.hgs('archive', self.parent['name'], '--with-children')
        out = result.stdout + result.stderr
        self.assertFalse(moved.exists(), out)
        self.assertEqual(self.path(twin).read_bytes(), twin_bytes, out)
        archived = [json.loads(p.read_text()) for p in (self.state / 'archives').glob('*.json')]
        firsts = [a['session_id'] for a in archived if a['name'] == self.first['name']]
        self.assertEqual(firsts, [self.first['session_id']], out)


if __name__ == '__main__':
    unittest.main()
