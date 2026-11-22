"""Long legitimate native child replies must not poison the whole listing.

Only saved metadata is synthetic. Full canonical/indexed rows, inspection,
resident worker and read-only publication use the actual product paths.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time
import unittest

import test_listing_index as listing


Fixture = type('Fixture', (unittest.TestCase,), {
    name: value for name, value in listing.ListingIndex.__dict__.items()
    if name not in ('__dict__', '__weakref__') and not name.startswith('test_')})


class ChildDetail(Fixture):
    def parent(self, paused=True, details=None):
        record, old = self.saved(1)
        record.update(name='claude/example/parent', agent='claude', base=['claude'],
                      paused=paused, reply_id='original-parent-reply',
                      subagents={key: dict(id=key, name='Worker ' + key,
                          announced=True, state='finished',
                          reply_id='original-' + key, started=1, ended=2, detail=value)
                          for key, value in (details or {
                              'first': '界' * 8000, 'second': '🦀' * 6000}).items()})
        path = self.state / (hashlib.sha256(record['name'].encode()).hexdigest() + '.json')
        old.unlink()
        path.write_text(json.dumps(record, ensure_ascii=False))
        return record, path

    def worker_pid(self):
        st = (self.state / 'listing-metadata/worker.lock').stat()
        identity = (os.major(st.st_dev), os.minor(st.st_dev), st.st_ino)
        with open('/proc/locks') as handle:
            for line in handle:
                parts = line.split()
                if len(parts) < 6 or parts[1] != 'FLOCK':
                    continue
                major, minor, inode = parts[5].split(':')
                if (int(major, 16), int(minor, 16), int(inode)) == identity:
                    return int(parts[4])
        self.fail('the private resident worker does not hold its lease')

    def proof(self, record, path):
        canonical = path.read_bytes()
        expected = self.canonical()
        self.assert_rows(expected, self.complete()['sessions'])
        result = self.call('inspect', record['name'])
        self.assertEqual(result.returncode, 0, result.stderr)
        selected = json.loads(result.stdout)
        for key, child in record['subagents'].items():
            # The existing inspector/list roster is a 240-character preview;
            # the canonical child keeps the complete native reply unchanged.
            text = child['detail']
            preview = text[:240] + ('…' if len(text) > 240 else '')
            self.assertEqual(selected['subagents'][key]['detail'], preview)
            self.assertEqual(selected['subagents'][key]['reply_id'], child['reply_id'])
        self.assertEqual(path.read_bytes(), canonical, 'display rewrote canonical child data')

    def test_unicode_child_details_keep_complete_rows_and_selected_text(self):
        record, path = self.parent()
        before = path.read_bytes()
        self.ready()
        self.proof(record, path)
        self.assertEqual(path.read_bytes(), before)
        import sqlite3
        with sqlite3.connect(self.state / 'listing-metadata/index.sqlite3') as db:
            self.assertEqual(db.execute('SELECT count(*) FROM sources WHERE issue IS NOT NULL').fetchone()[0], 0)
            seed = json.loads(db.execute('SELECT seed FROM sources WHERE source=?', (path.name,)).fetchone()[0])
            self.assertTrue(seed['record']['_listing']['requires_canonical'])
            self.assertFalse(seed['record']['_listing']['live_public'])
            self.assertNotIn('detail', seed['record']['subagents']['first'])

    def test_resident_worker_preserves_long_details_across_later_cuts(self):
        record, path = self.parent(details={'first': 'Short native reply'})
        self.ready()
        pid = self.worker_pid()
        birth = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[19]
        generation = json.loads(self.call('listing-index', 'status', '--json').stdout)['generation']
        for number, state in enumerate(('finished', 'ended', 'unknown')):
            record['subagents']['first'].update(detail='界🦀' * (4000 + number), state=state)
            staged = path.with_suffix('.tmp')
            staged.write_text(json.dumps(record, ensure_ascii=False))
            staged.replace(path)
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                status = json.loads(self.call('listing-index', 'status', '--json').stdout)
                if status.get('state') == 'ready' and status.get('generation', 0) > generation:
                    generation = status['generation']
                    break
                time.sleep(.02)
            else:
                self.fail('native replacement with a long detail never became ready')
            self.assertEqual(self.worker_pid(), pid)
            self.assertEqual(Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[19], birth)
            self.proof(record, path)


if __name__ == '__main__':
    unittest.main()
