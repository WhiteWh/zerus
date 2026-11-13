# Synthetic CLI prefix. Real native cache children; fake actor/Terminate replies.
import sys, os, json
# The private fixture source embeds its own data root; execution may be sealed.
__file__ = FIXTURE_SOURCE
from pathlib import Path
root = Path(__file__).absolute().parent
def retain_output():
    import time
    parent = os.getpid()
    if os.fork() == 0:
        (root / 'termination-pipe-holder').write_text(json.dumps(dict(
            parent=parent, pid=os.getpid(), stdout=os.readlink('/proc/self/fd/1'),
            stderr=os.readlink('/proc/self/fd/2'))))
        until = time.monotonic() + 20
        while not (root / 'termination-release').exists() and time.monotonic() < until:
            time.sleep(.005)
        (root / 'termination-holder-exited').touch()
        os._exit(0)
# Only transport-enabled component controls use this prefix. Actual CLI
# dispatch/prefix/timeout behavior is tested through the real hgs binary.
if sys.argv[1:2] == ['--desktop-helper-rendezvous-v1']:
    release = b'HGS-ORIGINAL-CHILD-V1\n'
    received = b''
    while len(received) < len(release):
        part = os.read(0, len(release) - len(received))
        if not part:
            (root / 'rendezvous-refused-input').write_bytes(received)
            sys.exit(2)
        received += part
    assert received == release
    del sys.argv[1]
    with (root / 'rendezvous-calls').open('a') as log:
        log.write(json.dumps(sys.argv[1:]) + '\n')
original_args = sys.argv[1:]
args = list(original_args)
if args and args[0].startswith('@'):
    args.pop(0)
if args[:1] == ['--expect-machine']:
    machine = args[1]
    args = args[2:]
else:
    machine = None
offer_case = json.loads((root / 'fixture.json').read_text()).get('termination_offer_case')
if args[:2] == ['desktop', 'status'] and offer_case == 'status':
    print(json.dumps(dict(abi='desktop-v1', running=True, configuration='b' * 64, epoch='00000000-0000-4000-8000-000000000001')))
    sys.exit(0)
if args == ['--help']:
    print('guarded-create-v1')
    if offer_case != 'lifecycle':
        print('scoped native lifecycle action')
    if offer_case != 'abi':
        print('Desktop termination outcome ABI: desktop-termination-outcome-v1')
    print('Desktop frontend request ABI: desktop-ui-request-v1')
elif args[:1] in [['inspect'], ['session-action']] or args[:2] in [['desktop', 'frontend-request']]:
    import hashlib, json, os, socket, time
    config = json.loads((root / 'fixture.json').read_text())
    capture = json.loads((root / 'termination-capture').read_text())
    epoch = (root / 'runtime-epoch').read_text() if (root / 'runtime-epoch').exists() else '00000000-0000-4000-8000-000000000001'
    if args[:2] == ['desktop', 'frontend-request'] and config.get('termination_refused_registration'):
        # Actual EOF must be observed before the parent's guarded spawn returns.
        raw = sys.stdin.buffer.readline(65537)
        (root / 'termination-refused-input').write_bytes(raw)
        if raw:
            sys.exit(1)
        until = time.monotonic() + 10
        while time.monotonic() < until:
            time.sleep(.005)
        sys.exit(1)
    value = json.loads(sys.stdin.readline()) if args[:1] == ['session-action'] or args[:2] == ['desktop', 'frontend-request'] else None
    with (root / 'termination-calls').open('a') as log:
        log.write(json.dumps(dict(args=original_args, value=value, pid=os.getpid())) + '\n')
    def digest(value):
        return hashlib.sha256(json.dumps(value, separators=(',', ':'), ensure_ascii=False).encode()).hexdigest()
    if args[:1] == ['inspect']:
        if config.get('termination_inspect_pipe'):
            retain_output()
        if offer_case == 'inspect_exit':
            print('private fixture stderr must not reach a diagnostic', file=sys.stderr)
            sys.exit(7)
        if offer_case == 'inspect_json':
            print('private fixture body must not reach a diagnostic')
            sys.exit(0)
        target = capture['target']
        changed = config.get('termination_changed_capture') and (root / 'termination-retained').exists()
        print(json.dumps(dict(name=target['name'], run_id=target['run_id'], conversation_id=target['conversation_id'],
            runtime_state='live', tracked=True, agent='codex', git_worktree=True,
            git_root='relative' if offer_case == 'capture' else capture['root'], git_common_dir=capture['common_dir'],
            worktree_owner_id='replacement-owner' if changed else capture['owner_id'])), flush=True)
    elif args[:1] == ['session-action']:
        assert machine == capture['machine']
        assert value['expected_run_id'] == capture['target']['run_id']
        assert value['expected_conversation_id'] == capture['target']['conversation_id']
        assert value['action'] == 'terminate'
        retained = json.loads((root / 'termination-retained').read_text())
        assert value['request_id'] == retained['original_request_id']
        assert config.get('termination_allow', True)
        if config.get('termination_effect_lost'):
            sys.exit(1)
        if config.get('termination_effect_wait'):
            time.sleep(31)
        if config.get('termination_effect_oversize'):
            if config.get('termination_effect_oversize_pipe'):
                retain_output()
            sys.stdout.write('x' * 65537)
            sys.stdout.flush()
            sys.exit(0)
        if config.get('termination_effect_completed_pipe') and os.fork() == 0:
            until = time.monotonic() + 20
            while not (root / 'termination-release').exists() and time.monotonic() < until:
                time.sleep(.005)
            os._exit(0)
        if config.get('termination_effect_cleanup'):
            status = config['termination_effect_cleanup']
            print('ZERUS_WORKTREE_CLEANUP ' + json.dumps(dict(status=status,
                path=capture['root'], common_dir=capture['common_dir'])), file=sys.stderr, flush=True)
        print(json.dumps(dict(request_id=value['request_id'], name=capture['target']['name'],
            run_id=capture['target']['run_id'], conversation_id=capture['target']['conversation_id'], status='completed',
            result_target=dict(name=capture['target']['name'], run_id=capture['target']['run_id'],
                conversation_id=capture['target']['conversation_id'], archive_id='11111111-2222-4333-8444-555555555555'))), flush=True)
    else:
        request = value['request']
        assert request['op'] == 'termination_outcome'
        intent = request['intent']
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(2)
            stream.connect(str(root / 'native-cache.sock'))
            stream.sendall(('desktop-ui-request-v1 ' + value['attempt'] + '\n').encode())
            evidence = json.loads(stream.makefile('rb').readline(8193))['request']
        assert evidence['child_pid'] == os.getpid()
        assert evidence['attempt'] == value['attempt']
        assert evidence['request_id'] == intent['request_id']
        assert evidence['ownership_mode'] == 'service'
        assert evidence['fingerprint'] == digest(request)
        retained = dict(abi='desktop-termination-outcome-v1', configuration=intent['scope']['configuration'],
            confirmation_id=intent['confirmation_id'], choice=intent['choice'], original_request_id=intent['request_id'],
            original_service_epoch=intent['scope']['service_epoch'],
            fingerprint=digest([intent['scope']['configuration'], intent['confirmation_id'], intent['choice'],
                intent['request_id'], intent['capture'], intent['original']]),
            capture_sha256=digest(intent['capture']), request_sha256=hashlib.sha256(intent['original'].encode()).hexdigest())
        if config.get('termination_bad_first_epoch'):
            retained['original_service_epoch'] = 'ffffffff-ffff-4fff-8fff-ffffffffffff'
        (root / 'termination-retained').write_text(json.dumps(retained))
        if config.get('termination_rpc_lost'):
            sys.exit(1)
        if config.get('termination_pipe'):
            retain_output()
        print(json.dumps(dict(op='termination_outcome', request_id=intent['request_id'], accepted=True, durable=True,
            phase='retained', status=retained, cleanup_authorized=False)), flush=True)
    sys.exit(0)
elif args[:2] == ['desktop', 'request']:
    import io, json
    line = sys.stdin.readline()
    value = json.loads(line)
    sys.stdin = io.StringIO(line)
    if value.get('op') == 'termination_outcome_status':
        with (root / 'termination-calls').open('a') as log:
            log.write(json.dumps(dict(args=original_args, value=value)) + '\n')
        retained = json.loads((root / 'termination-retained').read_text())
        config = json.loads((root / 'fixture.json').read_text())
        reply = dict(op='termination_outcome_status', request_id=value['request_id'],
            original_request_id=value['original_request_id'], accepted=True, durable=False,
            found=True, status=retained, cleanup_authorized=False)
        case = config.get('termination_lookup_case')
        if case == 'missing':
            reply.update(found=False, status=None)
        elif case == 'unknown':
            reply = dict(op='termination_outcome_status', request_id=value['request_id'], accepted=False,
                durable=False, code='termination_outcome_unavailable')
        elif case == 'corrupt':
            reply['status']['request_sha256'] = '0' * 64
        elif case == 'wrong-original':
            reply['original_request_id'] = 'ffffffff-ffff-4fff-8fff-ffffffffffff'
        elif case == 'cleanup-authority':
            reply['cleanup_authorized'] = True
        print(json.dumps(reply), flush=True)
        sys.exit(0)
