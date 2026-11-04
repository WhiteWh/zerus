# Private transport prefix; the remainder is the existing service fixture.
# Reads the real native cache, never manufactures original-child evidence.
import sys
if sys.argv[1:] == ['--help']:
    import json
    from pathlib import Path
    c = json.loads((Path(__file__).parent / 'fixture.json').read_text())
    if c.get('clone_help_fail'):
        sys.exit(1)
    if not c.get('clone_no_abi'):
        print('Desktop creation clone ABI: desktop-creation-clone-v1', flush=True)
elif sys.argv[1:3] in [['desktop', 'frontend-request'], ['desktop', 'request']]:
    import io
    line = sys.stdin.readline()
    sys.stdin = io.StringIO(line)
    import json
    value = json.loads(line)
    request = value.get('request', value)
    if request.get('op') in ['creation_clone', 'creation_clone_result']:
        import hashlib
        import os
        import socket
        import time
        from pathlib import Path
        root = Path(__file__).parent
        config = json.loads((root / 'fixture.json').read_text())
        with (root / 'clone-calls').open('a') as log:
            log.write(json.dumps({'args': sys.argv[1:], 'value': value, 'pid': os.getpid()}) + '\n')
        abi = 'desktop-creation-clone-v1'
        def digest(obj):
            return hashlib.sha256(json.dumps(obj, separators=(',', ':'), ensure_ascii=False).encode()).hexdigest()
        if request['op'] == 'creation_clone':
            intent = request['intent']
            with socket.socket(socket.AF_UNIX) as stream:
                stream.settimeout(2)
                stream.connect(str(root / 'native-cache.sock'))
                stream.sendall(('desktop-ui-request-v1 ' + value['attempt'] + '\n').encode())
                reply = json.loads(stream.makefile('rb').readline(8193))
            evidence = reply['request']
            assert evidence['child_pid'] == os.getpid() and evidence['attempt'] == value['attempt']
            assert evidence['request_id'] == intent['clone_request_id']
            assert evidence['ownership_mode'] == 'service'
            assert evidence['fingerprint'] == digest(request)
            (root / 'clone-bound').touch()
            if config.get('clone_hold'):
                (root / 'clone-held').touch()
                if os.fork() == 0:
                    until = time.monotonic() + 12
                    while not (root / 'clone-release').exists() and time.monotonic() < until:
                        time.sleep(.005)
                    os._exit(0)
                sys.exit(1)
            original = json.loads(intent['original'])
            plan = dict(clone_request_id=intent['clone_request_id'], expected_machine=original['expected_machine'],
                        url=original['url'], parent=original['parent'], name=original['repository_name'])
            binding = dict(kind='creation_clone', schema=1, configuration=intent['scope']['configuration'],
                           creation_id=intent['creation_id'], clone_request_id=intent['clone_request_id'], revision=intent['revision'],
                           source_sha256=hashlib.sha256(intent['original'].encode()).hexdigest(), plan_sha256=digest(plan),
                           consent_sha256=hashlib.sha256(b'desktop-creation-clone-consent-v1\0' + intent['original'].encode()).hexdigest())
            repository = dict(status='cloned', request_id=plan['clone_request_id'], fingerprint=digest([plan['url'], plan['parent'], plan['name']]),
                              name=plan['name'], parent=plan['parent'], path=plan['parent'].rstrip('/') + '/' + plan['name'])
            receipt = dict(binding=binding, admission_epoch=intent['scope']['service_epoch'], revision=9,
                           phase='cloned', drained=True, code='', repository=repository)
            (root / 'clone-receipt').write_text(json.dumps(receipt))
            if config.get('clone_lost'):
                sys.exit(1)
            if config.get('clone_wrong_identity'):
                receipt['binding']['clone_request_id'] = '00000000-0000-4000-8000-000000000099'
            print(json.dumps(dict(abi=abi, request_id=intent['clone_request_id'], scope=intent['scope'],
                                 accepted=True, durable=True, code='', receipt=receipt)))
        else:
            query = request['query']
            receipt = json.loads((root / 'clone-receipt').read_text()) if (root / 'clone-receipt').exists() else None
            print(json.dumps(dict(abi=abi, request_id=query['request_id'], scope=query['scope'], read_only=True,
                                 found=receipt is not None, receipt=receipt)))
        sys.exit(0)
