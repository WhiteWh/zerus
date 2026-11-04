"""One passive producer through real private CLI/IPC; no upstream or native calls."""
from contextlib import closing
import json
import os
import queue
import socket
import sqlite3
import subprocess
import threading
import time
import unittest
import uuid
import test_desktop as kernel


class Feed(unittest.TestCase):
    setUp = kernel.Desktop.setUp
    cli = kernel.Desktop.cli
    serve = kernel.Desktop.serve
    stop = kernel.Desktop.stop
    watch = kernel.Desktop.watch

    def status(self):
        return self.cli('desktop', 'status', '--json')['passive_feed']

    def until(self, check):
        deadline = time.monotonic() + 6
        while time.monotonic() < deadline:
            value = check()
            if value:
                return value
            time.sleep(0.02)
        self.fail('bounded feed condition did not settle')

    def feed(self):
        process = subprocess.Popen([str(kernel.HGS), 'desktop', 'feed', '--json'], env=self.env,
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        frames = queue.Queue(maxsize=32)

        def read():
            for line in process.stdout:
                frames.put(json.loads(line), timeout=5)

        reader = threading.Thread(target=read, daemon=True)
        reader.start()

        def close():
            if not process.stdin.closed:
                process.stdin.close()
            process.wait(timeout=5)
            reader.join(timeout=5)
            self.assertFalse(reader.is_alive(), 'feed output reader did not finish')
            process.stdout.close()
            process.stderr.close()

        self.addCleanup(close)
        return process, frames.get(timeout=5), frames

    def send(self, process, frames, value, accepted=True):
        process.stdin.write(json.dumps(value) + '\n')
        process.stdin.flush()
        receipt = frames.get(timeout=5)
        self.assertEqual(receipt['t'], 'feed_receipt')
        self.assertEqual(receipt['value']['accepted'], accepted)
        self.assertFalse(receipt['value']['durable'])
        self.assertTrue(receipt['value']['display_only'])
        return receipt

    def item(self, n):
        return dict(key=f'run:run-{n}', row=dict(name=f'codex/example/session-{n}', run_id=f'run-{n}',
                    conversation_id=f'conversation-{n}', process_work=dict(count=3, future=True), unknown=dict(kept='exact')))

    def begin(self, generation=1, rows=0, pages=1):
        return dict(t='legacy_begin', computer='peer', generation=generation,
                    metadata=dict(machine_id='example-machine', extra='preserved'), rows=rows, pages=pages)

    def complete_empty(self, process, frames, generation=1):
        self.send(process, frames, self.begin(generation))
        self.send(process, frames, dict(t='legacy_page', computer='peer', generation=generation, index=0, items=[]))
        return self.send(process, frames, dict(t='legacy_complete', computer='peer', generation=generation))

    def test_exact_capability_and_missing_service_write_nothing(self):
        result = subprocess.run([str(kernel.HGS), '--help'], env=self.env, capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('Desktop source feed ABI: desktop-source-feed-v1', [line.strip() for line in result.stdout.splitlines()])
        self.cli('desktop', 'feed', '--json', ok=False)
        self.assertFalse(self.state.exists())
        self.assertFalse((self.root / 'unexpected-tool').exists())

    def test_typed_pages_publish_only_on_complete_and_keep_writers_passive(self):
        self.serve()
        _, _, notifications = self.watch()
        process, hello, frames = self.feed()
        self.assertEqual(hello['t'], 'feed')
        self.assertTrue(hello['value']['accepted'])
        self.send(process, frames, self.begin(rows=1000, pages=8))
        with closing(sqlite3.connect(self.desktop / 'state.sqlite3')) as db:
            for index in range(8):
                items = [self.item(n) for n in range(index * 128, min((index + 1) * 128, 1000))]
                self.send(process, frames, dict(t='legacy_page', computer='peer', generation=1, index=index, items=items))
                self.assertIsNone(db.execute('SELECT visible FROM desktop_catalog_sources').fetchone()[0])
            self.send(process, frames, dict(t='legacy_complete', computer='peer', generation=1))
            stored = db.execute('SELECT count(*),min(json) FROM desktop_catalog_rows WHERE generation=(SELECT visible FROM desktop_catalog_sources)').fetchone()
            self.assertEqual(stored[0], 1000)
            self.assertEqual(json.loads(stored[1])['process_work'], dict(count=3, future=True))
            self.assertEqual(json.loads(stored[1])['unknown'], dict(kept='exact'))
            self.assertEqual(json.loads(db.execute('SELECT meta FROM desktop_catalog_sources').fetchone()[0])['extra'], 'preserved')
        changes = [notifications.get(timeout=5) for _ in range(10)]
        self.assertTrue(all(frame['t'] == 'catalog_changed' for frame in changes))
        self.assertTrue(changes[-1]['value']['complete'])
        status = self.cli('desktop', 'status', '--json')
        self.assertFalse(status['writers_enabled'])
        self.assertFalse(status['complete'])
        self.assertEqual(status['upstreams'], [])
        self.assertTrue(status['passive_feed']['sources'][0]['complete'])
        self.assertFalse(status['passive_feed']['sources'][0]['authority'])
        for op in ('presence', 'read', 'post', 'approve'):
            result = self.cli('desktop', 'request', '--json', data=dict(op=op))
            self.assertFalse(result['accepted'])
        self.assertFalse((self.root / 'unexpected-tool').exists())

    def test_shared_counts_follow_publication_offline_and_service_restart(self):
        service, status = self.serve()
        _, hello, notifications = self.watch()
        self.assertIsNone(hello['value']['attention_counts']['aggregate']['attention'])
        process, _, frames = self.feed()
        self.send(process, frames, self.begin(rows=1))
        row = self.item(0)
        row['row'].update(tracked=True, activity='busy', phase='input', attention_signature='sig')
        self.send(process, frames, dict(t='legacy_page', computer='peer', generation=1, index=0, items=[row]))
        self.send(process, frames, dict(t='legacy_complete', computer='peer', generation=1))
        change = [notifications.get(timeout=5) for _ in range(3)][-1]
        counts = change['value']['attention_counts']
        self.assertEqual(counts['epoch'], status['epoch'])
        self.assertEqual(counts['revision'], change['seq'])
        self.assertEqual(counts['projection_generation'], status['generation'])
        self.assertEqual(counts['scope'], 'shared_host_state')
        self.assertFalse(counts['local_marks_included'])
        self.assertFalse(counts['authority'])
        self.assertEqual(counts['aggregate']['attention'], 1)
        process.stdin.close()
        process.wait(timeout=5)
        self.until(lambda: not self.status()['producer_connected'] and not self.status()['retiring'])
        counts = self.cli('desktop', 'status', '--json')['attention_counts']
        self.assertIsNone(counts['aggregate']['attention'])
        self.assertFalse(counts['aggregate']['complete'])
        self.assertEqual(counts['computers'][0]['total'], 1)
        self.assertFalse(counts['computers'][0]['online'])
        self.stop(service)
        _, restarted = self.serve()
        self.assertNotEqual(restarted['epoch'], status['epoch'])
        self.assertIsNone(restarted['attention_counts']['aggregate']['attention'])
        self.assertFalse(restarted['attention_counts']['aggregate']['complete'])

    def test_exclusion_eof_retirement_and_new_incarnation(self):
        self.serve()
        first, hello, frames = self.feed()
        self.complete_empty(first, frames)
        duplicate, refused, _ = self.feed()
        self.assertFalse(refused['value']['accepted'])
        duplicate.wait(timeout=5)
        first.stdin.close()
        first.wait(timeout=5)
        self.until(lambda: not self.status()['producer_connected'] and not self.status()['retiring'])
        with closing(sqlite3.connect(self.desktop / 'state.sqlite3')) as db:
            self.assertEqual(db.execute('SELECT active FROM desktop_catalog_sources').fetchone()[0], 0)
            self.assertIsNotNone(db.execute('SELECT visible FROM desktop_catalog_sources').fetchone()[0])
        second, next_hello, next_frames = self.feed()
        self.assertGreater(next_hello['value']['incarnation'], hello['value']['incarnation'])
        self.complete_empty(second, next_frames)
        self.send(second, next_frames, self.begin(generation=1), accepted=False)
        second.wait(timeout=5)
        self.until(lambda: not self.status()['producer_connected'])

    def test_watch_and_legacy_switch_requires_new_generation(self):
        self.serve()
        process, _, frames = self.feed()
        hello = dict(v=1, t='hello', seq=1, machine_id='example-machine', host='example', hgs_version='1.56.0',
                     stream='example-stream', heartbeat_ms=10000, max_frame_bytes=1 << 20, topics=['catalog'], limits=dict(activity_subscriptions=16,
                     page_bytes=512 << 10, queue_bytes=4 << 20, activity_tail=100, activity_read=200, control_line_bytes=64 << 10))
        self.send(process, frames, dict(t='watch', computer='peer', generation=1, frame=hello))
        snapshot = dict(v=1, t='snapshot', seq=2, topic='catalog', items=[self.item(1)], page=0, last=True, cursor=1)
        self.send(process, frames, dict(t='watch', computer='peer', generation=1, frame=snapshot))
        self.assertTrue(self.status()['sources'][0]['complete'])
        self.complete_empty(process, frames, generation=2)
        self.assertEqual(self.status()['sources'][0]['generation'], 2)
        self.send(process, frames, dict(t='watch', computer='peer', generation=1, frame=snapshot), accepted=False)
        process.wait(timeout=5)

    def test_malformed_and_oversized_input_close_only_the_producer(self):
        service, _ = self.serve()
        for body in ('{"t":"keepalive","extra":"private-fixture"}\n', '{bad private-fixture}\n',
                     json.dumps(dict(t='legacy_page', computer='peer', generation=1, index=0,
                                     items=[self.item(n) for n in range(129)])) + '\n', 'x' * ((1 << 20) + 2049) + '\n'):
            result = subprocess.run([str(kernel.HGS), 'desktop', 'feed', '--json'], env=self.env,
                                    input=body, capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue(json.loads(result.stdout.splitlines()[0])['value']['accepted'])
            self.assertNotIn('private-fixture', result.stderr)
            self.until(lambda: not self.status()['producer_connected'] and not self.status()['retiring'])
            self.assertIsNone(service.poll())
        process, _, frames = self.feed()
        self.send(process, frames, dict(t='keepalive'))
        self.assertFalse((self.root / 'unexpected-tool').exists())

    @unittest.skipIf(os.name == 'nt', 'Unix raw socket authentication/configuration fixture')
    def test_pipelined_hello_and_strict_role_configuration(self):
        _, status = self.serve()
        role = dict(abi='desktop-source-feed-v1', producer=str(uuid.uuid4()), mode='display_only')
        for configuration, request, accepted in ((status['configuration'], role, True), ('wrong', role, False),
                                                 (status['configuration'], dict(role, mode='writer'), False),
                                                 (status['configuration'], dict(role, abi='newer'), False)):
            hello = dict(abi='desktop-v1', configuration=configuration, op='feed', request=request)
            with socket.socket(socket.AF_UNIX) as client:
                client.settimeout(5)
                client.connect(str(self.desktop / 'service.sock'))
                client.sendall((json.dumps(hello) + '\n{"t":"keepalive"}\n').encode())
                with client.makefile('rb') as output:
                    receipt = json.loads(output.readline())
                    self.assertEqual(receipt['value']['accepted'], accepted)
                    if accepted:
                        self.assertTrue(json.loads(output.readline())['value']['accepted'])
            self.until(lambda: not self.status()['producer_connected'] and not self.status()['retiring'])

    def test_quiet_producer_does_not_delay_service_shutdown(self):
        service, _ = self.serve()
        process, _, _ = self.feed()
        start = time.monotonic()
        self.stop(service)
        self.assertLess(time.monotonic() - start, 5)
        process.stdin.close()
        process.wait(timeout=5)
        with closing(sqlite3.connect(self.desktop / 'state.sqlite3')) as db:
            self.assertEqual(db.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone(), (0, 0, 0))
        self.assertFalse((self.root / 'unexpected-tool').exists())
