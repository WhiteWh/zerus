"""Retained read-only relay through private passive CLI/IPC, never owner state."""
import json
import queue
import subprocess
import threading
import unittest
import uuid
import test_desktop as kernel
import test_desktop_catalog as catalog


class CatalogRelay(unittest.TestCase):
    setUp = catalog.Catalog.setUp
    cli = catalog.Catalog.cli
    serve = catalog.Catalog.serve
    stop = catalog.Catalog.stop
    feed = catalog.Catalog.feed
    send = catalog.Catalog.send
    begin = catalog.Catalog.begin
    item = catalog.Catalog.item
    seed = catalog.Catalog.seed

    def relay(self):
        child = subprocess.Popen([str(kernel.HGS), 'desktop', 'catalog-relay', '--json'],
                                 env=self.env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, text=True)
        output = queue.Queue(maxsize=4)

        def read():
            for line in child.stdout:
                output.put(json.loads(line), timeout=5)

        reader = threading.Thread(target=read, daemon=True)
        reader.start()

        def close():
            if not child.stdin.closed:
                child.stdin.close()
            child.wait(timeout=5)
            reader.join(timeout=5)
            self.assertFalse(reader.is_alive())
            child.stdout.close()
            child.stderr.close()

        self.addCleanup(close)
        return child, output.get(timeout=5), output

    def read(self, child, output, op='catalog_page', **values):
        request = dict(op=op, request_id=str(uuid.uuid4()), computer='peer', **values)
        child.stdin.write(json.dumps(request) + '\n')
        child.stdin.flush()
        response = output.get(timeout=5)
        self.assertEqual(response['abi'], 'desktop-catalog-relay-v1')
        self.assertEqual(response['request_id'], request['request_id'])
        self.assertEqual(response['value']['request_id'], request['request_id'])
        self.assertIsNone(child.poll(), 'one original child serves the entire walk')
        return response['value']

    def test_one_child_preserves_pages_one_use_cursors_and_passive_policy(self):
        _, status = self.serve()
        source, _, frames = self.feed()
        self.seed(source, frames, rows=300)
        child, hello, output = self.relay()
        self.assertEqual(hello, dict(abi='desktop-catalog-relay-v1', configuration=status['configuration']))
        first = self.read(child, output)
        second = self.read(child, output, cursor=first['next_cursor'])
        self.assertEqual(self.read(child, output, cursor=first['next_cursor'])['code'], 'catalog_cursor_expired')
        final = self.read(child, output, cursor=second['next_cursor'])
        self.assertEqual(len(first['items']) + len(second['items']) + len(final['items']), 300)
        self.assertIsNone(final['next_cursor'])
        another = self.read(child, output)
        self.assertTrue(self.read(child, output, op='catalog_close', cursor=another['next_cursor'])['closed'])
        current = self.cli('desktop', 'status', '--json')
        self.assertFalse(current['writers_enabled'])
        self.assertEqual(current['catalog_reads']['retained_walks'], 0)
        self.assertFalse((self.root / 'unexpected-tool').exists())

    def test_missing_service_is_not_started_and_mutations_never_reach_it(self):
        help_result = subprocess.run([str(kernel.HGS), '--help'], env=self.env, capture_output=True, text=True, timeout=5)
        self.assertIn('Desktop catalog relay ABI: desktop-catalog-relay-v1', [line.strip() for line in help_result.stdout.splitlines()])
        request = dict(op='catalog_page', request_id=str(uuid.uuid4()), computer='peer')
        result = subprocess.run([str(kernel.HGS), 'desktop', 'catalog-relay', '--json'],
                                env=self.env, input=json.dumps(request) + '\n', capture_output=True, text=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.state.exists())
        self.serve()
        before = self.cli('desktop', 'status', '--json')
        for op in ['revoke', 'automatic_approvals', 'maintenance_configure', 'read_intent']:
            request['op'] = op
            result = subprocess.run([str(kernel.HGS), 'desktop', 'catalog-relay', '--json'],
                                    env=self.env, input=json.dumps(request) + '\n', capture_output=True, text=True, timeout=5)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('only catalog reads', result.stderr)
        after = self.cli('desktop', 'status', '--json')
        self.assertEqual(after['lifecycle']['phase'], before['lifecycle']['phase'])
        self.assertFalse(after['writers_enabled'])
        self.assertFalse(after['automatic_approvals'])
        self.assertFalse((self.root / 'unexpected-tool').exists())


if __name__ == '__main__':
    unittest.main()
