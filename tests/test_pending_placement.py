"""Launch placement survives real catalog contention, using product writers.

The tracked parent record is the existing fixture's explicit metadata seam:
its actual PID is this controller, so hgs proves the launch ancestry itself.
All child bindings, parent links and catalog membership rows use hgs writers.
The native agents are scripted fixtures, not released providers/models.
"""
from concurrent.futures import ThreadPoolExecutor
import fcntl
import json
import subprocess
import time

import test_child_project as child


class PendingPlacement(child.Placement, child.Base):
    def setUp(self):
        child.Base.setUp(self)
        self.leads = []
        self.worker_stops = []
        self.state = self.root / 'state'
        self.state.mkdir(exist_ok=True)
        (self.state / 'local-sessions').touch()
        self.config = self.root / 'cfg'
        self.folder = self.project.resolve()

    def tearDown(self):
        for stop in reversed(self.worker_stops):
            stop()
        directory = self.state / 'listing-metadata'
        if directory.is_dir():
            result = subprocess.run([str(child.HGS), 'listing-index', 'disable', '--json'],
                                    env=self.env, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            deadline = time.monotonic() + 5
            while (directory / 'worker.lock').exists():
                with (directory / 'worker.lock').open('rb') as held:
                    try:
                        fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
                        break
                    except BlockingIOError:
                        self.assertLess(time.monotonic(), deadline, 'private display worker did not settle')
                time.sleep(.02)
        for path in self.leads:
            path.unlink(missing_ok=True)
        child.Base.tearDown(self)

    named = child.LifecycleChild.named
    binding = child.LifecycleChild.binding
    held_locks = child.LifecycleChild.held_locks
    launch = child.LifecycleChild.launch

    def catalog(self):
        parents = child.Placement.catalog(self)
        names = [row['name'] for row in parents.values()] + [self.named('explicit')]
        self.unrelated = {key: value for key, value in self.swarm('get')['versions'].items()
                          if any(name in key for name in names)}
        return parents

    def check_unrelated(self):
        after = self.swarm('get')['versions']
        self.assertEqual({key: after.get(key) for key in self.unrelated}, self.unrelated)

    def worker(self):
        log = (self.root / 'catalog-worker.log').open('w')
        process = subprocess.Popen([str(child.HGS), 'swarm', 'worker'],
                                   env=self.env, stdout=log, stderr=subprocess.STDOUT)
        def stop():
            if process.poll() is None:
                process.terminate()  # Only this exact child we created.
                process.wait(timeout=10)
            log.close()
        self.worker_stops.append(stop)
        self.addCleanup(stop)
        return process

    def contended(self, tags, parent):
        lock = self.config / 'swarm/catalog.lock'
        with lock.open('r+') as held:
            fcntl.flock(held, fcntl.LOCK_EX)
            start = time.monotonic()
            with ThreadPoolExecutor(max_workers=2) as pool:
                launches = [pool.submit(self.launch, tag, parent) for tag in tags]
                names = [future.result(timeout=30) for future in launches]
            # Both supervisors have passed their three-second placement cap.
            # Keep the actual catalog lock continuously held for longer still.
            while time.monotonic() - start < 4:
                time.sleep(.02)
            self.assertTrue(self.free(self.state / '.lock'))
            elapsed = time.monotonic() - start
            fcntl.flock(held, fcntl.LOCK_UN)
        self.assertLess(elapsed, 15, 'catalog contention blocked native launch')
        facts = [dict(name=name, session_id=self.binding(name)['session_id'],
                      run_id=self.binding(name)['run_id'],
                      parent=self.binding(name).get('parent'),
                      project=self.project_of(name), explicit=self.explicit(name)) for name in names]
        print('CONTENTION', json.dumps(dict(held_seconds=elapsed, children=facts)), flush=True)
        self.assertTrue(all(not row['explicit'] for row in facts))
        self.assertEqual(len({row['session_id'] for row in facts}), len(names))
        return names

    def eventual(self, names, project):
        process = self.worker()
        def placed():
            self.assertIsNone(process.poll(), (self.root/'catalog-worker.log').read_text())
            return all(self.project_of(name) == project for name in names)
        self.until(placed, 'new launch placement was lost after the catalog became available', within=12)
        self.check_unrelated()

    def move(self, name, project):
        before = self.swarm('get')
        desired = json.loads(json.dumps(before['organization']))
        token = self.node + '\n' + name
        session_id = self.binding(name)['session_id']
        id_token = self.node + '\n' + session_id
        local = {self.node, self.env['HGS_SELF']}
        def matches(value, identifier):
            machine, _, selected = value.partition('\n')
            return machine in local and selected == identifier
        for row in desired['projects']:
            row['sessions'] = [value for value in row['sessions'] if not matches(value, name)]
            row['session_ids'] = [value for value in row.get('session_ids', []) if not matches(value, session_id)]
            if row['id'] == project:
                row['sessions'].append(token)
                row['session_ids'].append(id_token)
        self.swarm('apply', request=dict(base=before['organization'], desired=desired,
                                        versions=before['versions']))
        self.assertEqual(self.project_of(name), project or 'ungrouped')

    def pending(self, name):
        row = self.binding(name)
        return self.state / 'placement-intents' / (row['session_id'] + '.' + row['run_id'] + '.json')

    def drain(self, name):
        pending = self.pending(name)
        self.assertTrue(pending.exists(), 'the creation intent was not durably retained')
        self.worker()
        self.until(lambda: not pending.exists(), 'the completed intent was not drained', within=12)
        self.check_unrelated()

    def end(self, name):
        self.t('send-keys', '-t', '=' + name + ':', '-l', 'exit')
        self.t('send-keys', '-t', '=' + name + ':', 'Enter')
        self.until(lambda: not self.binding(name) or self.binding(name).get('completion_source') == 'supervisor',
                   'the fixture supervisor did not confirm exit')

    def test_a_person_tombstone_while_pending_wins_over_folder_inference(self):
        parents = self.catalog()
        name, = self.contended(['person-cleared'], parents['work'])
        self.move(name, 'other')
        self.move(name, None)  # Actual removal writer produces the tombstone.
        self.drain(name)
        self.assertTrue(self.explicit(name))
        self.assertEqual(self.project_of(name), 'ungrouped')

    def test_native_end_in_a_still_running_run_keeps_creation_placement(self):
        parents = self.catalog()
        name, = self.contended(['native-context-end'], parents['work'])
        before = self.binding(name)
        # This scripted native command emits the real SessionEnd hook but
        # keeps the same supervised process alive, like a context transition.
        self.t('send-keys', '-t', '=' + name + ':', '-l', 'end')
        self.t('send-keys', '-t', '=' + name + ':', 'Enter')
        self.until(lambda: self.binding(name).get('session_end'), 'the native hook was not written')
        self.assertEqual(self.binding(name)['run_id'], before['run_id'])
        self.eventual([name], 'work')
        self.t('send-keys', '-t', '=' + name + ':', '-l', 'switch')
        self.t('send-keys', '-t', '=' + name + ':', 'Enter')
        self.until(lambda: self.binding(name)['conversation_id'] != before['conversation_id'],
                   'the fixture did not open its next native context')
        self.assertEqual(self.binding(name)['run_id'], before['run_id'])
        self.assertEqual(self.project_of(name), 'work')

    def test_an_ended_session_does_not_receive_late_placement(self):
        parents = self.catalog()
        name, = self.contended(['already-ended'], parents['work'])
        pending = self.pending(name)
        self.end(name)
        self.worker()
        self.until(lambda: not pending.exists(), 'the ended intent was not retired', within=12)
        self.check_unrelated()
        self.assertFalse(self.explicit(name))

    def test_a_replacement_does_not_inherit_the_old_creation_intent(self):
        parents = self.catalog()
        name, = self.contended(['replaced'], parents['work'])
        pending = self.pending(name)
        old = self.binding(name)
        self.end(name)
        self.until(lambda: not self.binding(name), 'the old supervisor did not retire')
        # Record retirement precedes the shell/tmux teardown. A new-only
        # launch checks BOTH, so wait for this exact private terminal too.
        def terminal_gone():
            result = subprocess.run([str(self.bin / 'tmux'), 'has-session', '-t', '=' + name],
                                    env=self.env, capture_output=True, text=True)
            self.assertIn(result.returncode, (0, 1), result.stderr)
            return result.returncode == 1
        self.until(terminal_gone, 'the old private terminal did not retire')
        replacement = self.launch('replaced', None)
        current = self.binding(replacement)
        self.assertNotEqual(current['session_id'], old['session_id'])
        self.assertNotEqual(current['run_id'], old['run_id'])
        self.assertEqual(self.project_of(replacement), 'tree')
        self.worker()
        self.until(lambda: not pending.exists(), 'the replaced intent was not retired', within=12)
        self.assertEqual(self.project_of(replacement), 'tree')

    def test_a_deleted_parent_project_resolves_to_explicit_inbox(self):
        parents = self.catalog()
        name, = self.contended(['parent-project-gone'], parents['gone'])
        self.drain(name)
        self.assertTrue(self.explicit(name))
        self.assertEqual(self.project_of(name), 'ungrouped')

    def test_adopting_a_pending_child_cancels_its_launch_placement(self):
        parents = self.catalog()
        name, = self.contended(['adopted-after-launch'], parents['work'])
        row = self.binding(name)
        result = subprocess.run([str(child.HGS), 'parent', name, '--json'],
            input=json.dumps(dict(action='adopt', replace=True,
                expected_run_id=row['run_id'], expected_conversation_id=row['conversation_id'])),
            env=self.as_child_of(parents['inbox']), capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.drain(name)
        self.assertFalse(self.explicit(name))
        self.assertEqual(self.project_of(name), 'ungrouped')

    def test_adoption_does_not_infer_for_a_session_created_before_catalog(self):
        name = self.launch('before-catalog', None)
        self.assertFalse(self.pending(name).exists())
        parents = self.catalog()
        row = self.binding(name)
        request = dict(action='adopt', expected_run_id=row['run_id'],
                       expected_conversation_id=row['conversation_id'])
        result = subprocess.run([str(child.HGS), 'parent', name, '--json'],
            input=json.dumps(request), env=self.as_child_of(parents['work']),
            capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        process = self.worker()
        self.until(lambda: (self.config/'swarm/worker.lock').exists(), 'the catalog owner did not start')
        time.sleep(3.2)
        self.assertIsNone(process.poll())
        self.check_unrelated()
        self.assertFalse(self.pending(name).exists())
        self.assertFalse(self.explicit(name))
        self.assertEqual(self.project_of(name), 'ungrouped')

    def test_listing_does_not_infer_for_a_session_created_before_catalog(self):
        name = self.launch('listed-before-catalog', None)
        self.assertFalse(self.pending(name).exists())
        # No ancestry-only handwritten parent records: the complete display
        # fixture here consists exclusively of the product's own launch rows.
        self.node = self.swarm('hello')['node_id']
        self.swarm('initialize', request={'projects': [dict(id='tree', name='Tree',
            color='#234567', sessions=[], folders=[dict(id='anchor', machine_id=self.node,
            path=str(self.folder), name='')])]})
        self.hgs('listing-index', 'enable', '--json')
        def displayed():
            result = subprocess.run([str(child.HGS), 'ls', '--json', '--local'],
                                    env=self.env, capture_output=True, text=True, timeout=10)
            if result.returncode == 0:
                self.assertIn(name, result.stdout)
                return True
            self.assertEqual(result.stdout, '')
            return False
        self.until(displayed, 'no complete private listing body became available', within=12)
        process = self.worker()
        self.until(lambda: (self.config/'swarm/worker.lock').exists(), 'the catalog owner did not start')
        time.sleep(3.2)
        self.assertIsNone(process.poll())
        self.assertFalse(self.pending(name).exists())
        self.assertFalse(self.explicit(name))
        self.assertEqual(self.project_of(name), 'ungrouped')

    def test_concurrent_siblings_are_eventually_placed_after_catalog_contention(self):
        parents = self.catalog()
        names = self.contended(['sibling-one', 'sibling-two'], parents['work'])
        self.eventual(names, 'work')
        self.assertEqual(self.project_of(names[0]), 'work')
        self.assertEqual(self.project_of(names[1]), 'work')

    def test_a_person_move_while_placement_is_pending_wins(self):
        parents = self.catalog()
        moved, sibling = self.contended(['person-moved', 'pending-sibling'], parents['work'])
        self.move(moved, 'other')  # Actual project mutation writer, not a forged row.
        self.eventual([sibling], 'work')
        self.assertEqual(self.project_of(moved), 'other')

    def test_a_new_sessions_folder_inference_survives_catalog_contention(self):
        self.catalog()
        names = self.contended(['folder-inferred'], None)
        self.eventual(names, 'tree')
