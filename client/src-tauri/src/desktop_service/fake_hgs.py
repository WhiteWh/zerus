#!/usr/bin/env python3
"""Isolated desktop-v1 fixture; never launches a real CLI, service or agent."""
import json
import contextlib
import io
import hashlib
import base64
import socket
import struct
import os
import select
from pathlib import Path
import sys
import time
import uuid

root = Path(os.path.abspath(__file__)).parent
config = json.loads((root / 'fixture.json').read_text())
epoch = (root / 'runtime-epoch').read_text() if (root / 'runtime-epoch').exists() else '00000000-0000-4000-8000-000000000001'
cursor = '00000000-0000-4000-8000-000000000002'
projection = 'a' * 32
abi = 'desktop-catalog-changes-v1'
args = sys.argv[1:]
watch_seq, watch_status = 0, None
value = json.loads(sys.stdin.readline()) if args[:2] in [['desktop', 'request'], ['desktop', 'frontend-request']] else {}


def wait(path):
    until = time.monotonic() + 20
    while not path.exists():
        if time.monotonic() > until:
            raise SystemExit('fixture deadline')
        time.sleep(.005)


# Delay only this fixture's first watch before it records startup. This models
# a process that has spawned but has not been scheduled through its greeting.
if args == ['desktop', 'watch', '--json'] and config.get('audit_first_watch_hold'):
    held = root / 'audit-first-watch-held'
    if not held.exists():
        held.touch()
        wait(root / 'audit-first-watch-release')

with (root / 'calls').open('a') as log:
    log.write(json.dumps({'args': args, 'value': value, 'pid': os.getpid()}) + '\n')


def counter(name):
    path = root / name
    number = int(path.read_text()) + 1 if path.exists() else 1
    path.write_text(str(number))
    return number


def status():
    if config.get('produced_status') is not None:
        return config['produced_status']
    online = not config.get('offline', False)
    catalog_online = online and not config.get('coverage_offline', False)
    counts = {'epoch': epoch, 'scope': 'shared_host_state', 'display_only': True, 'authority': False,
              'local_marks_included': False, 'computers': [{'computer': '', 'incarnation': 1, 'generation': 1,
              'online': catalog_online, 'complete': catalog_online, 'total': 2, 'attention': None}],
              'aggregate': {'complete': False, 'total': 2, 'attention': None, 'known_total': 2, 'known_attention': 0}}
    if config.get('counts_degraded'):
        counts = {'epoch': epoch, 'scope': 'shared_host_state', 'display_only': True, 'authority': False,
                  'local_marks_included': False, 'aggregate': {'complete': False, 'attention': None}, 'error': 'counts_unavailable'}
    elif config.get('counts_null'):
        counts = None
    elif config.get('counts_computer'):
        counts['computers'][0]['computer'] = config['counts_computer']
    background = {name: {'ready': config.get('ready', False), 'code': 'fixture'}
                  for name in ['fleet', 'presence', 'reads', 'attention', 'approvals']}
    if config.get('cache'):
        background['cache_maintenance'] = {'ready': True, 'code': 'running'}
    sources = [{'computer': '', 'incarnation': 1, 'generation': 1,
                'status': 'live' if online else 'reconnecting', 'accepting': online}]
    if config.get('peer_failure'):
        sources.append(dict(sources[0], computer='broken', incarnation=2))
    return {'abi': 'desktop-v1', 'configuration': 'a' * 64, 'epoch': epoch, 'generation': 1, 'host': 'caption-only',
            'lifecycle': {'abi': 'desktop-lifecycle-v1', 'phase': 'serving', 'ipc_ready': True,
                          'configuration': 'a' * 64, 'epoch': epoch, 'generation': 1, 'background': background},
            'attention_counts': counts, 'automatic_approvals': False, 'catalog_reads': {'changes_abi': abi},
            'background_runtime': {'epoch': epoch, 'display_only': True, 'authority': False, 'state': 'running',
                                   'computers': sources} if not config.get('peer_choices') else None,
            'detail_subscriptions': {'abi': 'desktop-details-v1', 'stopping': False} if config.get('details') else None}


def native_hello():
    return {'machine_id': '00000000-0000-4000-8000-000000000009', 'host': 'untrusted-caption',
            'hgs_version': 'fixture', 'stream': epoch, 'heartbeat_ms': 10000, 'max_frame_bytes': 1048576,
            'topics': ['activity', 'processes', 'tasks', 'organization'],
            'limits': {'activity_subscriptions': 16, 'page_bytes': 1048576, 'queue_bytes': 1048576,
                       'activity_tail': 100, 'activity_read': 100, 'control_line_bytes': 8192}}


def detail_follow():
    route_id = value['id']
    spec = value['subscription']
    key = ('run:' + spec['session']['run_id']) if 'session' in spec else spec.get('project', '')
    path = root / ('route-' + route_id)
    previous = json.loads(path.read_text()) if path.exists() else None
    revision_path = root / 'detail-revision'
    revision = previous['revision'] if previous else (int(revision_path.read_text()) + 1 if revision_path.exists() else 1)
    revision_path.write_text(str(revision))
    route = {'lease': '00000000-0000-4000-8000-000000000003', 'id': route_id,
             'computer': value['computer'], 'generation': 1, 'revision': revision,
             'detail_seq': previous['detail_seq'] if previous else 0}
    events = []
    if not previous:
        for state in ['starting', 'ready']:
            events.append(['detail_status', dict(route, state=state, code=state, complete=False)])
    topic = spec['topic']
    bodies = []
    if topic == 'activity':
        if spec.get('after') is not None:
            bodies.append({'t': 'events', 'topic': topic, 'key': key, 'cursor': 101,
                           'events': [{'seq': 1, 'type': 'UserPrompt', 'detail': 'older'}]})
        else:
            bodies.extend([{'t': 'snapshot', 'topic': topic, 'key': key, 'page': 0, 'last': False,
                            'cursor': 101, 'items': [{'seq': 100, 'type': 'UserPrompt', 'detail': 'first'}]},
                           {'t': 'snapshot', 'topic': topic, 'key': key, 'page': 1, 'last': True,
                            'cursor': 101, 'items': [{'seq': 101, 'type': 'Stop', 'detail': 'reply'}]}])
        bodies.append({'t': 'state', 'topic': topic, 'key': key, 'rev': 1,
                       'value': {'name': spec['session']['name'], 'run_id': spec['session']['run_id'],
                                 'conversation_id': spec['session'].get('conversation_id'),
                                 'message_events': [{'seq': 3, 'detail': 'all-day message', 'type': 'UserPrompt'}]}})
    elif topic == 'processes':
        bodies.append({'t': 'state', 'topic': topic, 'key': key, 'rev': route['detail_seq'] + 1,
                       'value': {'processes': {'items': [], 'active_count': 0}, 'process_work': None}})
    elif topic == 'tasks':
        bodies.append({'t': 'snapshot', 'topic': topic, 'key': key, 'page': 0, 'last': True,
                       'cursor': 0, 'items': [{'id': 'task'}],
                       'value': {'project': {'id': key}, 'damaged': [], 'digest': 'task-digest'}})
    elif topic == 'organization':
        full = {'node_id': native_hello()['machine_id'], 'swarm_id': 'swarm', 'digest': 'org-digest',
                'organization': {'projects': []}}
        bodies.append({'t': 'snapshot', 'topic': topic, 'page': 0, 'last': True, 'cursor': 0,
                       'items': [{'key': 'organization', 'row': full}]})
    if config.get('native_gap') and topic == 'activity':
        bodies.extend([{'t': 'gap', 'topic': topic, 'key': key, 'reason': 'overflow'},
                       {'t': 'snapshot', 'topic': topic, 'key': key, 'page': 0, 'last': True,
                        'cursor': 200, 'items': [{'seq': 200, 'type': 'Stop', 'detail': 'new tail'}]}])
    for i, body in enumerate(bodies):
        route['detail_seq'] += 1
        events.append(['detail', dict(route, frame=dict(body, v=1, seq=10 + i * 71))])
    if config.get('detail_gap') and topic == 'activity' and not (root / 'sequence-gap-sent').exists():
        (root / 'sequence-gap-sent').touch()
        broken = dict(route, detail_seq=route['detail_seq'] + 2, frame=dict(bodies[-1], v=1, seq=999))
        events.append(['detail', broken])
    path.write_text(json.dumps(route))
    pending = root / ('pending-' + str(time.monotonic_ns()) + '-' + value['request_id'])
    temporary = pending.with_suffix('.tmp')
    temporary.write_text(json.dumps(events))
    temporary.rename(pending)
    return {'request_id': value['request_id'], 'accepted': True, 'durable': False,
            'lease': route['lease'], 'id': route_id, 'generation': 1, 'revision': revision,
            'state': 'starting', 'code': 'awaiting_fence'}


def frame(seq, kind, body):
    global watch_seq, watch_status
    watch_seq = seq
    if kind in ('hello', 'status', 'heartbeat'):
        watch_status = body
    print(json.dumps({'abi': 'desktop-v1', 'epoch': epoch, 'seq': seq, 't': kind, 'value': body}), flush=True)


def hint(seq, revision, selected=projection, computer=''):
    frame(seq, 'catalog_changed', {'computer': computer, 'incarnation': 2 if computer else 1, 'generation': 1,
          'projection': selected, 'revision': revision, 'online': True, 'last_known': False,
          'complete': True, 'reset_required': False, 'changes_abi': abi, 'attention_counts': status()['attention_counts']})
    if config.get('replace_projection') and selected == 'b' * 32 and (config.get('projection_publication_gate') or os.environ.get('ZERUS_TEST_SERVICE_PROJECTION_PUBLICATION_GATE') == '1'):
        # Hold this exact publication boundary until the next page selects state.
        wait(root / 'replacement-page-selected')


def row(key):
    return {'key': key, 'row': {'name': 'codex/example/' + key, 'agent': 'codex', 'state': 'running', 'status': 'ready'}}


def change(key, revision, remove=False):
    return {'op': 'remove' if remove else 'upsert', 'key': key, 'revision': revision,
            'row': None if remove else row(key)['row']}


def desktop_request():
    op, request_id = value['op'], value['request_id']
    if op == 'cache_maintenance_policy':
        path = root / 'cache-policy'
        revision, limit = json.loads(path.read_text()) if path.exists() else (0, 0)
        print(json.dumps(dict(request_id=request_id, abi='desktop-cache-maintenance-v1', configuration='a' * 64,
                              root=value['root'], revision=revision, limit=limit, configured=revision > 0)))
        return
    if op in ['approval_audit_list', 'approval_audit_detail']:
        journal = '00000000-0000-4000-8000-00000000000' + ('4' if (root / 'audit-replaced').exists() else '3')
        if config.get('audit_hold'):
            (root / 'audit-held').touch()
            wait(root / 'audit-release')
        reply = dict(abi='desktop-approval-audit-v1', request_id=request_id,
                     configuration='a' * 64, service_epoch=epoch, read_only=True,
                     accepted=True, journal=journal)
        if value.get('journal') and value['journal'] != journal:
            reply.update(accepted=False, code='approval_audit_scope_changed')
        elif op == 'approval_audit_detail':
            reply['result'] = dict(id=value['id'], status='approved', detail='Service audit outcome',
                                   question={'questions': [{'question': 'Service-owned question', 'body': 'Fixture body'}]}, answers=[])
        else:
            high = value.get('through') or 53
            ids = list(range(1, high + 1))
            if value.get('before'):
                ids = [i for i in ids if i < value['before']]
            if value.get('after'):
                ids = [i for i in ids if i > value['after']][:50]
            else:
                ids = ids[-50:]
            ids.reverse()
            reply['result'] = dict(through=high, next=ids[-1] if ids and ids[-1] > 1 else None,
                                   previous=ids[0] if ids and ids[0] < high else None,
                                   entries=[dict(id=i, at=1800000000000, host='a.example', session='agent/project/fixture',
                                                 provider='claude', prompt='Service-owned question', status='approved') for i in ids])
        fault = config.get('audit_fault')
        if fault == 'oversize':
            reply['overflow'] = 'x' * (256 * 1024)
        elif fault in ['request_id', 'configuration', 'service_epoch', 'journal']:
            reply[fault] = 'wrong'
        elif fault == 'refused':
            reply.update(accepted=False, code='approval_audit_unavailable')
        elif fault == 'rows':
            reply['result']['entries'] *= 2
        print(json.dumps(reply))
        return
    if op in ['delivery_result', 'maintenance_result'] and config.get('lookup_unavailable') and not (root / 'lookup-back').exists():
        raise SystemExit('fixture lookup transport unavailable')
    common = {'request_id': request_id, 'accepted': True, 'durable': False, 'abi': 'desktop-catalog-v1',
              'changes_abi': abi, 'epoch': epoch, 'computer': value.get('computer', ''), 'incarnation': 1,
              'generation': 1, 'projection': projection, 'display_only': True, 'authority': False,
              'complete': True, 'online': True, 'last_known': False, 'stale': False, 'next_cursor': None}
    if config.get('peer_failure') and value.get('computer') == 'broken':
        common['incarnation'] = 2
        if (root / 'peer-scope-recovered').exists():
            common['projection'] = 'b' * 32
    if config.get('peer_choices') and value.get('computer') == 'offline':
        common.update(incarnation=2, online=False, last_known=True, complete=False)
    if op == 'detail_follow':
        if config.get('follow_timeout') and value['subscription']['topic'] == 'activity':
            (root / 'follow-held').touch()
            time.sleep(15)
        if config.get('detail_refused'):
            print(json.dumps({'request_id': request_id, 'accepted': False, 'code': 'unsupported_topic'}))
        else:
            print(json.dumps(detail_follow()))
    elif op == 'detail_unfollow':
        print(json.dumps({'request_id': request_id, 'accepted': True, 'durable': False,
                          'lease': value['lease'], 'id': value['id'], 'state': 'unfollowed'}))
    elif op == 'detail_source':
        metadata = native_hello() if value['part'] == 'hello' else {'host': 'metadata-caption', 'ok': True,
                    'machine_id': '00000000-0000-4000-8000-000000000099', 'peers': [],
                    'metrics': {'platform': 'linux'}}
        print(json.dumps({'request_id': request_id, 'accepted': True, 'durable': False,
                          'lease': value['lease'], 'computer': value['computer'],
                          'generation': value['generation'], 'part': value['part'],
                          'native': True, 'online': True, 'complete': True, 'value': metadata}))
    elif op == 'catalog_page':
        number = counter('page-number')
        page = dict(common, start_revision='0', revision='0', items=[row('row')])
        if config.get('peer_choices') and value['computer'] == 'offline':
            page['start_revision'] = None
        if config.get('dirty_walk'):
            page.update(start_revision='0', revision='2', stale=True, complete=False)
            if number == 1:
                (root / 'page-started').touch()
                wait(root / 'hinted')
                page.update(next_cursor=cursor, items=[row('first')])
        if config.get('replace_projection') == 'early' and number == 1:
            (root / 'page-started').touch()
            wait(root / 'release-old-page')
            page.update(next_cursor=cursor)
        elif config.get('replace_projection') and (root / 'projection-replaced').exists():
            page.update(projection='b' * 32, items=[row('replacement')])
        if config.get('failure') and number > 1:
            page.update(start_revision='5', revision='5', items=[row('recovered')])
        if config.get('offline') or config.get('coverage_offline'):
            page.update(online=False, last_known=True, complete=False, start_revision=None)
        if config.get('large'):
            offset = (number - 1) * 128
            count = config['large']
            page.update(items=[row('row-' + str(i)) for i in range(offset, min(offset + 128, count))],
                        next_cursor=str(uuid.UUID(int=number + 100)) if offset + 128 < count else None)
        if config.get('replace_projection') and number == 2 and (config.get('projection_publication_gate') or os.environ.get('ZERUS_TEST_SERVICE_PROJECTION_PUBLICATION_GATE') == '1'):
            (root / 'replacement-page-selected').touch()
        print(json.dumps(page))
    elif op == 'catalog_changes':
        number = counter('delta-number')
        page = dict(common, projection=value['projection'], since_revision=value['since_revision'],
                    through_revision=value['since_revision'], revision=value['since_revision'], items=[])
        if config.get('peer_failure'):
            if value['computer'] == 'broken':
                if not any((root / path).exists() for path in ['peer-recovered', 'peer-scope-recovered']):
                    page = {'request_id': request_id, 'accepted': False, 'durable': False,
                            'code': 'catalog_changes_gap', 'reset_required': True}
            elif (root / 'peer-hints').exists():
                page.update(through_revision='1', revision='1', items=[change('after-peer', '1')])
        if config.get('counts_transition') and (root / 'drop-counts').exists():
            page.update(through_revision='1', revision='1', items=[change('after-counts', '1')])
        if config.get('dirty_walk'):
            if number == 1:
                (root / 'delta-started').touch()
                wait(root / 'delta-hinted')
                page.update(through_revision='2', revision='3', stale=True, items=[change('inserted', '1'), change('first', '2', True)], next_cursor=cursor)
            elif number == 2:
                (root / 'delta-page-two').touch()
                wait(root / 'finish-delta')
                page.update(through_revision='2', revision='3', stale=True, items=[change('first', '2', True)])
            else:
                (root / 'refresh-started').touch()
                wait(root / 'finish-refresh')
                page.update(through_revision='3', revision='3', items=[change('fresh', '3')])
        if config.get('delta_live') and number == 2:
            (root / 'live-delta-started').touch()
            wait(root / 'live-delta-hinted')
            wait(root / 'finish-live-delta')
            page.update(through_revision='1', revision='2', stale=True, items=[change('first-delta', '1')])
        elif config.get('delta_live') and number == 3:
            (root / 'next-live-delta-started').touch()
            wait(root / 'finish-next-live-delta')
            page.update(through_revision='2', revision='2', items=[change('second-delta', '2')])
        if config.get('failure') == 'changed_through' and number <= 2:
            page.update(through_revision=str(number), revision=str(number), next_cursor=cursor if number == 1 else None)
        elif config.get('failure') and number == 1:
            if config['failure'] == 'unknown':
                raise SystemExit('uncertain fixture exchange')
            page = {'request_id': request_id, 'accepted': False, 'durable': False,
                    'code': 'catalog_changes_gap', 'reset_required': True}
        if config.get('replace_projection') and config['replace_projection'] != 'early' and number == 1:
            page.update(through_revision='1', revision='1', items=[change('obsolete', '1')], next_cursor=cursor)
            (root / 'initial-complete').touch()
            if config['replace_projection'] != 'exhausted':
                wait(root / 'release-old-page')
        elif config.get('replace_projection') == 'exhausted' and number == 2:
            page.update(through_revision='1', revision='1')
        if config.get('large'):
            if number == 1:
                (root / 'initial-complete').touch()
            else:
                page.update(through_revision='3', revision='3', items=[change('row-0', '1'), change('row-1', '2', True), change('new', '3')])
        print(json.dumps(page))
    elif op == 'read_intent':
        path = root / 'read-seen'
        confirmed = path.exists()
        path.touch()
        print(json.dumps({'request_id': request_id, 'accepted': True, 'durable': confirmed, 'op': op}))
    elif op in ['delivery_result', 'pending_reads_import_result', 'cache_maintenance_result'] and (root / ('receipt-' + value['original_request_id'])).exists():
        receipt = json.loads((root / ('receipt-' + value['original_request_id'])).read_text())
        print(json.dumps({'request_id': request_id, 'op': op, 'original_request_id': value['original_request_id'],
                          'accepted': True, 'durable': False, 'found': True, 'result': receipt}))
    elif op == 'maintenance_result':
        receipt_path = root / ('receipt-' + value['original_request_id'])
        if receipt_path.exists():
            receipt = json.loads(receipt_path.read_text())
            receipt['request_id'] = request_id
            print(json.dumps(receipt))
        else:
            print(json.dumps({'request_id': request_id, 'original_request_id': value['original_request_id'], 'accepted': False, 'code': 'unknown_request'}))
    elif op in ['delivery_result', 'pending_reads_import_result', 'cache_maintenance_result']:
        print(json.dumps({'request_id': request_id, 'accepted': True, 'durable': False, 'op': op,
                          'original_request_id': value['original_request_id'], 'found': False, 'result': None}))
    elif op == 'catalog_close':
        print(json.dumps({'request_id': request_id, 'accepted': True, 'durable': False}))
    else:
        raise SystemExit('Unexpected fixture operation')


if args == ['--help']:
    print('Desktop cache maintenance ABI: desktop-cache-maintenance-v1')
    if config.get('audit_help_hold') and counter('audit-help-number') > 1:
        (root / 'audit-help-held').touch()
        wait(root / 'audit-help-release')
    print('Desktop service ABI: desktop-v1\nDesktop catalog changes ABI: ' + abi if config.get('supported', True) else 'Older fixture CLI')
    if config.get('catalog_relay_abi', True):
        print('Desktop catalog relay ABI: desktop-catalog-relay-v1')
    if config.get('frontend_abi', True):
        print('Desktop frontend request ABI: desktop-ui-request-v1')
    if config.get('activation_abi', False):
        print('Desktop activation ABI: desktop-activation-v1')
    if config.get('pending_reads_abi', True):
        print('Desktop pending read import ABI: desktop-pending-reads-v1')
    if config.get('maintenance_abi', True):
        print('Desktop maintenance ABI: desktop-maintenance-v1')
    if config.get('audit_abi', True):
        print('Desktop approval audit ABI: desktop-approval-audit-v1')
elif args == ['desktop', 'frontend-request', '--json']:
    request = value['request']
    assert value['abi'] == 'desktop-ui-request-v1'
    if config.get('frontend_unreachable') and not (root / 'service-back').exists():
        raise SystemExit('fixture service unavailable before mutation')
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(2)
        stream.connect(str(root / 'native-cache.sock'))
        if sys.platform == 'darwin':
            import ctypes
            library = ctypes.CDLL('/usr/lib/libSystem.B.dylib', use_errno=True)
            peer = library.getpeereid
            peer.argtypes = [ctypes.c_int, ctypes.POINTER(ctypes.c_uint), ctypes.POINTER(ctypes.c_uint)]
            peer.restype = ctypes.c_int
            uid_value, gid_value = ctypes.c_uint(), ctypes.c_uint()
            assert peer(stream.fileno(), ctypes.byref(uid_value), ctypes.byref(gid_value)) == 0
            # Darwin SDK sys/un.h: SOL_LOCAL=0, LOCAL_PEERPID=2.
            pid = struct.unpack('i', stream.getsockopt(0, 2, 4))[0]
            uid = uid_value.value
        else:
            pid, uid, _ = struct.unpack('3i', stream.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
        assert pid == os.getppid() and uid == os.getuid()
        stream.sendall(('desktop-ui-request-v1 ' + value['attempt'] + '\n').encode())
        reply = json.loads(stream.makefile('rb').readline(4097))
    evidence = reply['request']
    assert reply['hello']['nonce'] == value['attempt']
    assert evidence['child_pid'] == os.getpid() and evidence['attempt'] == value['attempt']
    assert evidence['request_id'] == request['request_id']
    fingerprint = hashlib.sha256(json.dumps(request, separators=(',', ':'), ensure_ascii=False).encode()).hexdigest()
    assert evidence['fingerprint'] == fingerprint
    (root / 'original-child-bound').touch()
    if config.get('frontend_hold'):
        wait(root / 'release-frontend')
    if request['op'] == 'activation_wait':
        n = counter('activation-waits')
        first_failure = config.get('activation_fail_first')
        if n > (2 if config.get('activation_idle_first') or first_failure else 1):
            wait(root / 'release-activation')
        if first_failure and n == 1:
            wait(root / 'release-first-activation')
            if first_failure == 'pipe' and os.fork() == 0:
                # This private descendant holds only inherited stdout/stderr.
                # The original registered CLI exits; release without PID signals.
                (root / 'activation-pipe-held').touch()
                until = time.monotonic() + 15
                while not (root / 'release-activation-pipe').exists() and time.monotonic() < until:
                    time.sleep(.005)
                os._exit(0)
            raise SystemExit('fixture lost its first activation reply')
        target = {'computer': '', 'run': 'exact-run', 'conversation': 'exact-conversation'}
        token = base64.urlsafe_b64encode(json.dumps(dict(host='', name='fixture', notice='reply',
            run=target['run'], conversation=target['conversation'])).encode()).rstrip(b'=').decode()
        activation = {'activation_id': '11111111-1111-4111-8111-111111111111',
                      'post_request_id': '22222222-2222-4222-8222-222222222222', 'target': target,
                      'token': token, 'platform_context': config.get('activation_platform_context',
                          None if sys.platform == 'darwin' else {'platform': 'wayland', 'activation_token': 'ephemeral-fixture'})}
        answer = {'abi': 'desktop-activation-v1', 'request_id': request['request_id'],
                  'configuration': 'a' * 64, 'service_epoch': epoch,
                  'activation': None if config.get('activation_idle_first') and n == 1 else activation}
        bad = config.get('activation_invalid')
        if bad == 'transport': raise SystemExit('fixture lost activation reply')
        if bad == 'request': answer['request_id'] = str(uuid.uuid4())
        if bad == 'epoch': answer['service_epoch'] = str(uuid.uuid4())
        if bad == 'configuration': answer['configuration'] = 'b' * 64
        if bad == 'target': answer['activation']['target']['conversation'] = 'other'
        print(json.dumps(answer), flush=True)
        raise SystemExit(0)
    if config.get('frontend_refuse'):
        print(json.dumps({'request_id': request['request_id'], 'accepted': False, 'durable': False,
                          'code': 'frontend_authentication_refused'}))
    else:
        if request['op'] == 'import_pending_reads':
            page = request['page']
            receipt = {'request_id': request['request_id'], 'op': request['op'],
                       'accepted': True, 'durable': True, 'source': page['source'],
                       'after': page['after'], 'through': page['after'] + len(page['marks']),
                       'superseded': len(page['marks']) if config.get('pending_reads_superseded') else 0,
                       'delivered': False}
        elif request['op'] == 'cache_maintenance':
            assert evidence['ownership_mode'] == 'service'
            command = request['command']
            if command['phase'] == 'prepare':
                receipt = dict(abi='desktop-cache-maintenance-v1', request_id=request['request_id'], op=request['op'],
                    accepted=False, durable=True, pending=True, configuration=command['configuration'],
                    preparation_id=request['request_id'], operation_id=command['operation_id'],
                    revision=command['revision'] + int(command['policy_change']), limit=command['limit'], policy_saved=command['policy_change'],
                    phase='prepared', binding_sha256='b' * 64, removed=0, kept=0, unknown=0, drained=False)
                (root / 'cache-policy').write_text(json.dumps([receipt['revision'], command['limit']]))
            else:
                receipt = json.loads((root / ('receipt-' + command['preparation_id'])).read_text())
                assert receipt['binding_sha256'] == command['binding_sha256']
                assert receipt['operation_id'] == request['request_id']
                receipt.update(request_id=request['request_id'], accepted=True, phase='settled', pending=False,
                               removed=1, drained=True)
            for original in [receipt['preparation_id'], receipt['operation_id']]:
                (root / ('receipt-' + original)).write_text(json.dumps(receipt))
        elif request['op'] == 'automatic_approvals':
            receipt = {'request_id': request['request_id'], 'op': request['op'], 'enabled': request['enabled'],
                       'accepted': True, 'durable': True, 'revision': 1}
        else:
            gui = request.get('update', {}).get('gui') if request.get('update') else None
            if gui:
                assert gui['app_version'] == reply['hello']['build']['version']
                assert gui['source_commit'] == reply['hello']['build']['source_commit']
                assert Path(gui['executable']).resolve() == Path(os.readlink('/proc/' + str(os.getppid()) + '/exe')).resolve()
            receipt = {'request_id': request['request_id'], 'original_request_id': request['request_id'],
                       'accepted': True, 'durable': True, 'configured': True, 'revision': 1, 'recovery_save': 'not_requested'}
        (root / ('receipt-' + request['request_id'])).write_text(json.dumps(receipt))
        if config.get('frontend_uncertain'):
            raise SystemExit('fixture lost its reply after actor commit')
        if config.get('frontend_retain') and os.fork() == 0:
            # The original owned CLI exits; only this private fixture holds
            # its inherited output pipes. Release by file, never by a PID kill.
            until = time.monotonic() + 10
            while not (root / 'release-retainer').exists() and time.monotonic() < until:
                time.sleep(.005)
            os._exit(0)
        print(json.dumps(receipt))
elif args == ['desktop', 'status', '--json']:
    print(json.dumps(status()))
elif args == ['desktop', 'watch', '--json']:
    initial = status()
    if config.get('details'):
        initial['details'] = {'abi': 'desktop-details-v1', 'epoch': epoch,
                              'lease': '00000000-0000-4000-8000-000000000003',
                              'limits': {'demands_per_subscriber': 64}}
    frame(0, 'hello', initial)
    if config.get('silent_watch'):
        # Keep the original watch alive, but send no further frame at all.
        # EOF still belongs to the subscriber that owns this stdin pipe.
        sys.stdin.read()
        raise SystemExit(0)
    if config.get('peer_choices'):
        for seq, stage in enumerate(['known', 'invalid', 'warming', 'stopping', 'removed', 'empty'], 1):
            wait(root / ('roster-' + stage))
            updated = status()
            updated['generation'] = updated['lifecycle']['generation'] = seq + 1
            computers = [{'computer': '', 'incarnation': 1, 'generation': 1, 'status': 'live', 'accepting': True}]
            if stage in ['known', 'invalid']:
                computers.append({'computer': 'offline', 'incarnation': 2, 'generation': 1,
                                  'status': 'reconnecting', 'accepting': False})
            if stage != 'empty':
                computers.append({'computer': 'waiting', 'incarnation': 3, 'generation': 0,
                                  'status': 'probing', 'accepting': False})
            if stage == 'warming':
                computers = [dict(computers[0], generation=0, status='probing', accepting=False)]
            updated['background_runtime'] = {'epoch': epoch, 'display_only': True, 'authority': stage == 'invalid',
                'state': 'stopping' if stage == 'stopping' else 'running', 'computers': [] if stage == 'stopping' else computers}
            frame(seq, 'status', updated)
            if stage == 'known' and config.get('roster_disconnect_early'):
                raise SystemExit(0)
        wait(root / 'roster-end')
        raise SystemExit(0)
    if config.get('coverage_offline'):
        wait(root / 'repeat-status')
        for seq in [1, 2]:
            repeated = status()
            repeated['generation'] = seq + 1
            repeated['lifecycle']['generation'] = seq + 1
            frame(seq, 'status', repeated)
    if config.get('counts_transition'):
        wait(root / 'drop-counts')
        degraded = status()
        degraded['attention_counts'] = {'aggregate': {'complete': False}, 'error': 'counts_unavailable'}
        frame(1, 'status', degraded)
        frame(2, 'catalog_changed', {'computer': '', 'incarnation': 1, 'generation': 1,
                                    'projection': projection, 'revision': '1', 'online': True,
                                    'attention_counts': degraded['attention_counts']})
        wait(root / 'restore-counts')
        frame(3, 'heartbeat', status())
    if config.get('details'):
        frame(7, 'status', status())  # No replacement lease on ordinary status.
    if config.get('dirty_walk'):
        wait(root / 'page-started')
        hint(1, '2')
        hint(2, '2')
        (root / 'hinted').touch()
        wait(root / 'delta-started')
        hint(3, '3')
        hint(4, '3')
        (root / 'delta-hinted').touch()
    if config.get('replace_projection'):
        wait(root / ('page-started' if config['replace_projection'] == 'early' else 'initial-complete'))
        if config['replace_projection'] == 'exhausted':
            wait(root / 'release-replacement-hint')
        # The next request must see the state announced by this hint.
        (root / 'projection-replaced').touch()
        hint(1, '0', 'b' * 32)
    if config.get('delta_live'):
        wait(root / 'send-delta')
        hint(1, '1')
        wait(root / 'live-delta-started')
        hint(2, '2')
        (root / 'live-delta-hinted').touch()
    if config.get('large'):
        wait(root / 'initial-complete')
        hint(1, '3')
    if config.get('peer_failure'):
        wait(root / 'peer-hints')
        hint(1, '1')
        for seq in range(2, 6):
            hint(seq, str(seq), computer='broken')
        repeated = status()
        repeated['generation'] = repeated['lifecycle']['generation'] = 2
        frame(6, 'status', repeated)
        wait(root / 'peer-scope-recovered')
        hint(7, '0', selected='b' * 32, computer='broken')
    # A live service keeps sending heartbeats while its subscriber is idle.
    # Keep the latest scripted status (including intentional lease omissions),
    # not a fresh default that could undo a generation/counts change.
    seq, seen = max(100, watch_seq), set()
    heartbeat = time.monotonic() + 1
    while not select.select([sys.stdin], [], [], .005 if config.get('details') else .1)[0]:
        if config.get('details'):
            for path in sorted(root.glob('pending-*')):
                if path.suffix == '.tmp' or path.name in seen:
                    continue
                seen.add(path.name)
                for kind, body in json.loads(path.read_text()):
                    seq += 7  # Outer targeted sequences are monotone, not contiguous.
                    frame(seq, kind, body)
        if time.monotonic() >= heartbeat:
            seq += 1
            frame(seq, 'heartbeat', watch_status)
            heartbeat = time.monotonic() + 1
    sys.stdin.read()  # Detaches only when the owning subscriber closes stdin.
elif args == ['desktop', 'request', '--json']:
    desktop_request()
elif args == ['desktop', 'catalog-relay', '--json']:
    # Optional fault injection and lifecycle evidence for this private relay.
    delay = int(os.environ.get('ZERUS_TEST_SERVICE_RELAY_FINALIZE_DELAY_MS', config.get('relay_finalize_delay_ms', 0)))
    assert 0 <= delay <= 2000
    exit_number = counter('relay-number') if delay or os.environ.get('ZERUS_TEST_SERVICE_RELAY_EXIT_TRACE') == '1' else 0
    target = int(os.environ.get('ZERUS_TEST_SERVICE_RELAY_FINALIZE_NUMBER', config.get('relay_finalize_number', 1)))
    trace_exit = os.environ.get('ZERUS_TEST_SERVICE_RELAY_EXIT_TRACE') == '1'

    def relay_event(event):
        if trace_exit:
            with (root / 'relay-lifecycle').open('a') as log:
                log.write(json.dumps(dict(event=event, pid=os.getpid(), number=exit_number,
                                          at=time.monotonic_ns())) + '\n')

    def finalize_relay():
        relay_event('finalize-start')
        if exit_number == target:
            time.sleep(delay / 1000)
        relay_event('finalize-done')

    if trace_exit or delay:
        import atexit
        atexit.register(finalize_relay)
    print(json.dumps({'abi': 'desktop-catalog-relay-v1', 'configuration': ('b' if config.get('relay_fault') == 'configuration' else 'a') * 64}), flush=True)
    for line in sys.stdin:
        value = json.loads(line)
        assert value['op'] in ['catalog_page', 'catalog_changes', 'catalog_close']
        with (root / 'calls').open('a') as log:
            log.write(json.dumps({'args': args, 'value': value, 'pid': os.getpid()}) + '\n')
        fault = config.get('relay_fault')
        if fault == 'hang':
            (root / 'relay-held').touch()
            wait(root / 'release-relay')
        if fault == 'oversize':
            print('x' * (1048576 + 4096 + 1), flush=True)
            continue
        if fault == 'incomplete':
            print('{', end='', flush=True)
            raise SystemExit(0)
        if fault == 'stderr':
            sys.stderr.write('x' * (64 * 1024 + 1))
            sys.stderr.flush()
            wait(root / 'release-relay')
        captured = io.StringIO()
        with contextlib.redirect_stdout(captured):
            desktop_request()
        response = json.loads(captured.getvalue())
        print(json.dumps({'abi': 'desktop-catalog-relay-v1', 'request_id': str(uuid.uuid4()) if fault == 'identity' else value['request_id'], 'value': response}), flush=True)
    relay_event('stdin-eof')
    # Every reply was flushed above. This private proxy has no shutdown work;
    # don't put interpreter finalizers inside the real child's bounded join.
    os._exit(0)
else:
    raise SystemExit('Forbidden fallback CLI call')
