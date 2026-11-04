"""Detail leases over real desktop-v1 CLI IPC, using private local state only."""
import time
import unittest
import uuid

import test_desktop


def new_id():
    return str(uuid.uuid4())


class Details(unittest.TestCase):
    def setUp(self):
        # Reuse the maintained private-state fixture without inheriting its
        # test methods or hiding its independent cleanup failures.
        self.d = test_desktop.Desktop()
        self.d.addCleanup = self.addCleanup
        self.d.setUp()
        self.d.env['HGS_PEERS'] = ''
        self.d.env['DBUS_SESSION_BUS_ADDRESS'] = 'unix:path=' + str(self.d.root / 'missing-bus')
        self.d.env['XDG_RUNTIME_DIR'] = str(self.d.root / 'tmp')
        self.d.env.pop('DISPLAY', None)
        self.d.env.pop('WAYLAND_DISPLAY', None)

    def command(self, lease, epoch, **values):
        return self.d.cli('desktop', 'request', '--json', data={
            'request_id': new_id(), 'epoch': epoch, 'lease': lease, **values})

    def ready(self):
        self.d.serve(background=True)
        until = time.monotonic() + 10
        while time.monotonic() < until:
            current = self.d.cli('desktop', 'status', '--json')
            if current['background_runtime']['transport_ready']:
                return current
            time.sleep(0.02)
        self.fail('private local source did not become ready')

    def test_passive_service_does_not_advertise_or_admit_detail_demand(self):
        self.d.serve()
        _, hello, _ = self.d.watch()
        self.assertNotIn('details', hello['value'])
        result = self.command(new_id(), hello['epoch'], op='detail_follow',
                              id=new_id(), computer='', subscription={'topic': 'organization'})
        self.assertFalse(result['accepted'])
        self.assertEqual(result['code'], 'details_unavailable')

    def test_real_watch_lease_bootstrap_and_disconnect_cleanup_share_local_source(self):
        status = self.ready()
        watcher, hello, _ = self.d.watch()
        details = hello['value']['details']
        self.assertEqual(details['abi'], 'desktop-details-v1')
        source = next(c for c in status['background_runtime']['computers'] if c['computer'] == '')
        result = self.command(details['lease'], hello['epoch'], op='detail_source',
                              computer='', generation=source['generation'], part='hello')
        self.assertTrue(result['accepted'])
        self.assertTrue(result['native'])
        self.assertTrue(result['online'])
        self.assertTrue(result['complete'])
        self.assertIn('activity', result['value']['topics'])
        # An unconfigured alias records bounded demand only. It must not
        # start a remote source or use any SSH/owner connection settings.
        result = self.command(details['lease'], hello['epoch'], op='detail_follow',
                              id=new_id(), computer='unconfigured.example',
                              subscription={'topic': 'organization'})
        self.assertTrue(result['accepted'])
        self.assertEqual(result['code'], 'source_offline')
        current = self.d.cli('desktop', 'status', '--json')
        self.assertEqual(current['detail_subscriptions']['demands'], 1)
        self.assertEqual(current['background_runtime']['workers'], 1)
        watcher.stdin.close()
        watcher.wait(timeout=5)
        until = time.monotonic() + 5
        while time.monotonic() < until:
            current = self.d.cli('desktop', 'status', '--json')
            if current['detail_subscriptions']['demands'] == 0:
                break
            time.sleep(0.02)
        self.assertEqual(current['detail_subscriptions']['demands'], 0)
        self.assertEqual(current['detail_subscriptions']['groups'], 0)
        self.assertEqual(current['background_runtime']['workers'], 1)
        stale = self.command(details['lease'], hello['epoch'], op='detail_source',
                             computer='', generation=source['generation'], part='hello')
        self.assertFalse(stale['accepted'])
        self.assertEqual(stale['code'], 'stale_detail_lease')

    def test_revoke_closes_detail_leases_before_original_source_drain(self):
        self.ready()
        watcher, hello, _ = self.d.watch()
        lease = hello['value']['details']['lease']
        result = self.command(lease, hello['epoch'], op='detail_follow', id=new_id(),
                              computer='unconfigured.example', subscription={'topic': 'organization'})
        self.assertTrue(result['accepted'])
        revoked = self.d.cli('desktop', 'revoke', '--json', '--request-id', new_id())
        self.assertTrue(revoked['revoked'])
        watcher.wait(timeout=5)
        status = self.d.cli('desktop', 'status', '--json')
        self.assertTrue(status['background_runtime']['drained'])
        self.assertEqual(status['detail_subscriptions']['subscribers'], 0)
        self.assertEqual(status['detail_subscriptions']['groups'], 0)
        refused = self.command(lease, hello['epoch'], op='detail_follow', id=new_id(),
                               computer='', subscription={'topic': 'organization'})
        self.assertFalse(refused['accepted'])
        self.assertEqual(refused['code'], 'details_stopping')
