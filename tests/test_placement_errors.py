"""Optional placement failures never stop product launch or catalog service.

The real launch/provider and fake-SSH catalog fixtures are reused. Only the
queue-path replacement is injected here; children/catalog rows use writers.
The tracked-parent metadata and scripted native-provider seams are the same
ones explicitly documented by test_child_project.
"""
import copy
import subprocess
import time
import unittest

import test_child_project as child
import test_swarm


class BlockedQueue:
    def check_placement(self):
        parents = self.catalog()
        queue = self.state / 'placement-intents'
        queue.write_text('fixture: keep this unrelated blocking file')
        if hasattr(self, 'agent_env'):
            name = self.named('blocked-queue')
            result = subprocess.run([str(child.HGS), self.agent, str(self.folder),
                                     '-n', 'blocked-queue', '-d', '--new'],
                                    env=dict(self.as_child_of(parents['work']), **self.agent_env),
                                    capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(result.stderr.count('hgs: initial placement unavailable; launch continues:'), 1)
        else:
            name = self.launch('blocked-queue', parents['work'])
        record = self.binding(name)
        self.assertNotIn('initial_placement', record)
        self.assertEqual(self.project_of(name), 'work', 'ordinary first placement was lost')
        self.assertEqual(queue.read_text(), 'fixture: keep this unrelated blocking file')
        self.assertEqual(sorted(self.state.glob('placement-intents*')), [queue])


class BlockedQueueLifecycle(BlockedQueue, child.LifecycleChild):
    pass


class BlockedQueueNative(BlockedQueue, child.NativeChild):
    pass


SwarmFixture = type('SwarmFixture', (unittest.TestCase,), {
    name: value for name, value in test_swarm.Swarm.__dict__.items()
    if name not in ('__dict__', '__weakref__') and not name.startswith('test_')})


class BlockedQueueWorker(SwarmFixture):
    def test_a_broken_placement_queue_keeps_sync_and_local_refresh_running(self):
        self.join('a', 'b')
        self.join('b', 'a')
        before = self.call('b', 'get')
        desired = copy.deepcopy(before['organization'])
        desired['projects'][0]['name'] = 'Changed before the worker round'
        project = desired['projects'][0]['id']
        self.patch('b', before, desired)
        state = self.root / 'a/state'
        state.mkdir(exist_ok=True)
        queue = state / 'placement-intents'
        queue.write_text('fixture: broken queue')
        local = self.root / 'a/swarm/session-labels-cache/attempt-000.json'
        # Explicit cold-cache fault seam: no handcrafted label-cache row.
        # Only the product worker may publish the next local attempt.
        local.unlink(missing_ok=True)
        env = dict(self.env, HGS_CONFIG_DIR=str(self.root / 'a'), HGS_STATE_DIR=str(state))
        log = self.root / 'worker.log'
        with log.open('w') as output:
            worker = subprocess.Popen([str(test_swarm.HGS), 'swarm', 'worker'], env=env,
                                      stdout=output, stderr=subprocess.STDOUT)
            try:
                deadline = time.monotonic() + 8
                synced = False
                while time.monotonic() < deadline:
                    self.assertIsNone(worker.poll(), log.read_text())
                    rows = self.call('a', 'get')['organization']['projects']
                    synced = any(row['id'] == project and row['name'] == desired['projects'][0]['name']
                                 for row in rows)
                    if (synced and local.exists() and
                            'hgs swarm: initial placement remains pending:' in log.read_text()):
                        break
                    time.sleep(.05)
                self.assertTrue(synced, 'broken placement queue stopped actual cross-node catalog sync: ' + log.read_text())
                self.assertTrue(local.exists(), 'placement error skipped the round local-label refresh: ' + log.read_text())
                self.assertEqual(queue.read_text(), 'fixture: broken queue')
                self.assertIn('hgs swarm: initial placement remains pending:', log.read_text())
            finally:
                if worker.poll() is None:
                    worker.terminate()  # Only this exact worker created above.
                worker.wait(timeout=10)

    def test_a_broken_queue_does_not_skip_local_work_when_no_peer_is_due(self):
        # No join: the product store has no peer to sync. This reaches the
        # local refresh after the optional pass, independently of sync_peer's
        # own local refresh when it does contact a peer.
        self.assertFalse(self.call('a', 'get')['peers'])
        state = self.root / 'a/state'
        state.mkdir(exist_ok=True)
        queue = state / 'placement-intents'
        queue.write_text('fixture: broken queue')
        local = self.root / 'a/swarm/session-labels-cache/attempt-000.json'
        local.unlink(missing_ok=True)  # Same explicit private cold-cache seam.
        env = dict(self.env, HGS_CONFIG_DIR=str(self.root / 'a'), HGS_STATE_DIR=str(state))
        log = self.root / 'worker.log'
        with log.open('w') as output:
            worker = subprocess.Popen([str(test_swarm.HGS), 'swarm', 'worker'], env=env,
                                      stdout=output, stderr=subprocess.STDOUT)
            try:
                deadline = time.monotonic() + 8
                while not local.exists() and time.monotonic() < deadline:
                    self.assertIsNone(worker.poll(), log.read_text())
                    time.sleep(.05)
                self.assertTrue(local.exists(), 'placement error skipped local work: ' + log.read_text())
                self.assertIn('hgs swarm: initial placement remains pending:', log.read_text())
            finally:
                if worker.poll() is None:
                    worker.terminate()
                worker.wait(timeout=10)
