"""Display-only indexed reads through real private desktop request/feed proxies."""
from contextlib import closing
import json
import sqlite3
import subprocess
import unittest
import uuid
import test_desktop as kernel
import test_desktop_feed as source


class Catalog(unittest.TestCase):
    setUp = source.Feed.setUp
    cli = source.Feed.cli
    serve = source.Feed.serve
    stop = source.Feed.stop
    status = source.Feed.status
    until = source.Feed.until
    feed = source.Feed.feed
    send = source.Feed.send
    item = source.Feed.item
    begin = source.Feed.begin

    def page(self, cursor=None, computer='peer', **extra):
        request = dict(op='catalog_page', request_id=str(uuid.uuid4()), computer=computer, cursor=cursor, **extra)
        return self.cli('desktop', 'request', '--json', data=request)

    def seed(self, process, frames, rows=1000, generation=1):
        pages = max(1, (rows + 127) // 128)
        self.send(process, frames, self.begin(generation=generation, rows=rows, pages=pages))
        for index in range(pages):
            items = [self.item(n) for n in range(index * 128, min((index + 1) * 128, rows))]
            self.send(process, frames, dict(t='legacy_page', computer='peer', generation=generation, index=index, items=items))
        self.send(process, frames, dict(t='legacy_complete', computer='peer', generation=generation))

    def test_capability_missing_service_and_strict_read_requests(self):
        help_result = subprocess.run([str(kernel.HGS), '--help'], env=self.env, capture_output=True, text=True, timeout=5)
        self.assertEqual(help_result.returncode, 0, help_result.stderr)
        self.assertIn('Desktop catalog ABI: desktop-catalog-v1', [line.strip() for line in help_result.stdout.splitlines()])
        self.assertIn('Desktop catalog changes ABI: desktop-catalog-changes-v1', [line.strip() for line in help_result.stdout.splitlines()])
        self.cli('desktop', 'request', '--json', data=dict(op='catalog_page', request_id=str(uuid.uuid4()), computer='peer'), ok=False)
        self.assertFalse(self.state.exists())
        self.serve()
        for request in (dict(op='catalog_page', request_id='bad', computer='peer'),
                        dict(op='catalog_page', request_id=str(uuid.uuid4()), computer='bad alias'),
                        dict(op='catalog_page', request_id=str(uuid.uuid4()), computer='peer', after=0),
                        dict(op='catalog_close', request_id=str(uuid.uuid4()), computer='peer')):
            result = self.cli('desktop', 'request', '--json', data=request)
            self.assertFalse(result['accepted'])
        self.assertFalse(self.page()['accepted'])  # absent source, no fabricated empty coverage
        self.assertFalse((self.root / 'unexpected-tool').exists())

    def test_typed_replacement_walk_and_one_use_close(self):
        self.serve()
        process, _, frames = self.feed()
        self.send(process, frames, self.begin(rows=1000, pages=8))
        partial = self.page()
        self.assertEqual(partial['items'], [])
        self.assertFalse(partial['complete'])
        for index in range(8):
            self.send(process, frames, dict(t='legacy_page', computer='peer', generation=1, index=index,
                                           items=[self.item(n) for n in range(index * 128, min((index + 1) * 128, 1000))]))
        self.send(process, frames, dict(t='legacy_complete', computer='peer', generation=1))
        cursor = None
        count = 0
        for index in range(8):
            value = self.page(cursor)
            self.assertTrue(value['accepted'])
            self.assertTrue(value['complete'])
            self.assertTrue(value['display_only'])
            self.assertFalse(value['authority'])
            self.assertFalse(value['durable'])
            self.assertLessEqual(len(value['items']), 128)
            for row in value['items']:
                self.assertEqual(row, self.item(count))
                count += 1
            old, cursor = cursor, value['next_cursor']
            if old:
                self.assertEqual(self.page(old)['code'], 'catalog_cursor_expired')
        self.assertEqual(count, 1000)
        self.assertIsNone(cursor)
        self.assertEqual(self.cli('desktop', 'status', '--json')['catalog_reads']['retained_walks'], 0)
        cursor = self.page()['next_cursor']
        self.assertEqual(self.page(cursor, computer='other')['code'], 'catalog_cursor_scope')
        result = self.cli('desktop', 'request', '--json', data=dict(op='catalog_close', request_id=str(uuid.uuid4()), computer='peer', cursor=cursor))
        self.assertTrue(result['closed'])
        self.assertEqual(self.page(cursor)['code'], 'catalog_cursor_expired')
        self.assertFalse(self.cli('desktop', 'status', '--json')['writers_enabled'])

    def test_watch_append_watermark_and_replacement(self):
        self.serve()
        process, _, frames = self.feed()
        hello = dict(v=1, t='hello', seq=1, machine_id='example-machine', host='example', hgs_version='1.56.0',
                     stream='example-stream', heartbeat_ms=10000, max_frame_bytes=1 << 20, topics=['catalog'],
                     limits=dict(activity_subscriptions=16, page_bytes=512 << 10, queue_bytes=4 << 20,
                                 activity_tail=100, activity_read=200, control_line_bytes=64 << 10))

        def watch(frame, generation=1):
            self.send(process, frames, dict(t='watch', computer='peer', generation=generation, frame=frame))

        watch(hello)
        for index in range(2):
            watch(dict(v=1, t='snapshot', seq=index + 2, topic='catalog', items=[self.item(n) for n in range(index * 128, (index + 1) * 128)],
                       page=index, last=index == 1, cursor=1))
        first = self.page()
        watch(dict(v=1, t='delta', seq=4, topic='catalog', upsert=[self.item(256)], remove=[], cursor=2))
        final = self.page(first['next_cursor'])
        self.assertEqual([r['key'] for r in final['items']], [self.item(n)['key'] for n in range(128, 256)])
        self.assertIsNone(final['next_cursor'], 'new inserts cannot extend the existing walk')
        self.assertEqual(first['changes_abi'], 'desktop-catalog-changes-v1')
        self.assertEqual(first['start_revision'], '0')
        self.assertEqual(final['start_revision'], '0')
        self.assertEqual(final['revision'], '1')
        self.assertTrue(final['stale'])
        self.assertFalse(final['complete'])
        request = dict(op='catalog_changes', request_id=str(uuid.uuid4()), computer='peer',
                       epoch=first['epoch'], incarnation=first['incarnation'], generation=first['generation'],
                       projection=first['projection'], since_revision=first['start_revision'])
        changes = self.cli('desktop', 'request', '--json', data=request)
        self.assertTrue(changes['accepted'])
        self.assertEqual(changes['through_revision'], '1')
        self.assertIsNone(changes['next_cursor'])
        self.assertEqual(changes['items'], [dict(op='upsert', revision='1', **self.item(256))])
        request['request_id'] = str(uuid.uuid4())
        request['since_revision'] = '1'
        self.assertEqual(self.cli('desktop', 'request', '--json', data=request)['items'], [])
        cursor = self.page()['next_cursor']
        watch(hello, generation=2)
        watch(dict(v=1, t='snapshot', seq=2, topic='catalog', items=[self.item(999)], page=0, last=True, cursor=1), generation=2)
        self.assertEqual(self.page(cursor)['code'], 'catalog_read_refused')
        request['request_id'] = str(uuid.uuid4())
        reset = self.cli('desktop', 'request', '--json', data=request)
        self.assertEqual(reset['code'], 'catalog_changes_reset')
        self.assertTrue(reset['reset_required'])
        self.assertEqual(self.page()['items'], [self.item(999)])
        process.stdin.close()
        process.wait(timeout=5)
        self.until(lambda: not self.status()['producer_connected'])
        retained = self.page()
        self.assertEqual(retained['items'], [self.item(999)])
        self.assertFalse(retained['complete'])

    def test_service_restart_expires_cursors_and_preserves_last_known_rows(self):
        service, _ = self.serve()
        process, _, frames = self.feed()
        self.seed(process, frames, rows=300)
        cursor = self.page()['next_cursor']
        self.stop(service)
        process.stdin.close()
        process.wait(timeout=5)
        self.serve()
        self.assertEqual(self.page(cursor)['code'], 'catalog_cursor_expired')
        value = self.page()
        self.assertEqual(len(value['items']), 128)
        self.assertFalse(value['complete'])
        self.assertFalse(value['authority'])

    def test_schema_twelve_upgrade_preserves_last_known_rows_without_a_watermark(self):
        service, _ = self.serve()
        process, _, frames = self.feed()
        self.seed(process, frames, rows=300)
        self.stop(service)
        process.stdin.close()
        process.wait(timeout=5)
        with closing(sqlite3.connect(self.desktop / 'state.sqlite3')) as db, db:
            kernel.Desktop.remove_changes_schema(db)
            db.execute('PRAGMA user_version=12')
        _, status = self.serve()
        self.assertEqual(status['schema'], kernel.SCHEMA)
        value = self.page()
        self.assertEqual(len(value['items']), 128)
        self.assertFalse(value['complete'])
        self.assertFalse(value['online'])
        self.assertTrue(value['last_known'])
        for field in ('projection', 'start_revision', 'revision'):
            self.assertIsNone(value[field])
        with closing(sqlite3.connect(self.desktop / 'state.sqlite3')) as db:
            self.assertEqual(db.execute('SELECT count(*) FROM desktop_catalog_rows').fetchone()[0], 300)
            self.assertEqual(db.execute('SELECT count(*) FROM desktop_maintenance').fetchone()[0], 1)

    def test_large_valid_singleton_and_corrupt_encoded_key_keep_service_alive(self):
        service, status = self.serve()
        process, _, frames = self.feed()
        self.send(process, frames, self.begin(rows=1))
        row = self.item(0)
        row['row']['private'] = 'x' * 1_000_000
        self.send(process, frames, dict(t='legacy_page', computer='peer', generation=1, index=0, items=[row]))
        self.send(process, frames, dict(t='legacy_complete', computer='peer', generation=1))
        value = self.page()
        self.assertEqual(value['items'], [row])
        self.assertIsNone(value['next_cursor'])
        with closing(sqlite3.connect(self.desktop / 'state.sqlite3')) as db, db:
            db.create_function("desktop_shared_flags", 1, lambda raw: 0)
            db.execute("UPDATE desktop_catalog_rows SET key=?,json='{}',bytes=200066", ('\x01' * 200_000,))
        refused = self.page()
        self.assertEqual(refused['code'], 'catalog_read_refused')
        self.assertNotIn('private', json.dumps(refused))
        self.assertIsNone(service.poll())
        self.assertEqual(self.cli('desktop', 'status', '--json')['epoch'], status['epoch'])
        self.seed(process, frames, rows=1, generation=2)
        self.assertEqual(self.page()['items'], [self.item(0)])
        self.assertFalse((self.root / 'unexpected-tool').exists())
