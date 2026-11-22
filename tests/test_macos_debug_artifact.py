"""Pure debug-handoff controls; no compiler, uploaded binary or native agent runs."""
import copy
import datetime
import hashlib
import importlib.util
import json
import os
import re
import shutil
from pathlib import Path
import struct
import subprocess
import tempfile
import itertools
from types import SimpleNamespace
import test_ci_security as security
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('mac_debug', ROOT / 'scripts/ci/macos-debug-artifact.py')
artifact = importlib.util.module_from_spec(spec)
spec.loader.exec_module(artifact)


class Fixture:
    def __init__(self, root):
        self.root = root
        self.directory = root / 'handoff'
        self.bound = dict(source='a' * 40, repository='example/zerus', run_id=101, attempt=1,
                          platform=dict(system='Darwin', machine='arm64'))
        self.compiler = dict(release='1.99.1', verbose='rustc 1.99.1\nhost: aarch64-apple-darwin\nrelease: 1.99.1\n',
                             compiler=dict(bytes=16, sha256='b' * 64))
        for relative in artifact.HELPERS:
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('Synthetic source helper ' + relative)
        for relative in artifact.FILES.values():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b'\xcf\xfa\xed\xfe' + struct.pack('<I', 0x0100000C) + b'synthetic-not-executable')
        with patch.object(artifact, 'prerequisites', return_value=dict(python='fixture', node='v24.0.0', tmux='tmux 3.8')):
            self.sha = artifact.seal(root, self.directory, self.bound, self.compiler)

    def verify(self, **overrides):
        return artifact.verify(self.root, self.directory, overrides.get('bound', self.bound),
            overrides.get('sha', self.sha), overrides.get('attempt', '1'), overrides.get('release', '1.99.1'))

    def change(self, edit):
        path = self.directory / 'manifest.json'
        data = json.loads(path.read_text())
        edit(data)
        path.write_text(json.dumps(data))
        self.sha = hashlib.sha256(path.read_bytes()).hexdigest()


class ArtifactControls(unittest.TestCase):
    def test_seed_provenance_is_admitted_before_any_debug_handoff_seal(self):
        with tempfile.TemporaryDirectory() as folder:
            f=Fixture(Path(folder));destination=f.root/'new-handoff'
            helper=SimpleNamespace(verify=unittest.mock.Mock(side_effect=ValueError('seed source mismatch')))
            args=['macos-debug-artifact.py','seal','--directory',str(destination)]
            with patch.object(artifact,'ROOT',f.root),patch.object(artifact,'context',return_value=f.bound), \
                 patch.object(artifact,'toolchain',return_value=f.compiler),patch.object(artifact,'seed_helper',return_value=helper), \
                 patch.object(artifact,'seal') as seal,patch.object(artifact.sys,'argv',args):
                with self.assertRaisesRegex(ValueError,'seed source mismatch'):artifact.main()
                seal.assert_not_called()
            helper.verify.assert_called_once_with(f.root,f.root/'target/debug/hgs-effort-seed.json',f.root/artifact.FILES['hgs'])
            self.assertFalse(destination.exists())

    def test_read_only_cargo_hardlinks_become_exclusive_handoff_copies(self):
        with tempfile.TemporaryDirectory() as folder:
            f = Fixture(Path(folder))
            for name, relative in artifact.FILES.items():
                os.link(f.root / relative, f.root / (name + '-cargo-uplift'))
            second = f.root / 'second-handoff'
            with patch.object(artifact, 'prerequisites', return_value=dict(python='fixture', node='v24', tmux='3.8')):
                sha = artifact.seal(f.root, second, f.bound, f.compiler)
            data = artifact.verify(f.root, second, f.bound, sha, '1', '1.99.1')
            for name, relative in artifact.FILES.items():
                self.assertEqual((f.root / relative).stat().st_nlink, 2)
                self.assertEqual((second / name).stat().st_nlink, 1)
                self.assertEqual(data['files'][name], artifact.identity(f.root / relative, allow_linked=True))

    def test_exact_minimal_receipt_and_partial_rerun_keep_the_original_producer(self):
        with tempfile.TemporaryDirectory() as folder:
            f = Fixture(Path(folder))
            data = f.verify(bound={**f.bound, 'attempt': 3})
            self.assertEqual(data['producer_attempt'], 1)
            self.assertEqual(set(p.name for p in f.directory.iterdir()), {'manifest.json', 'hgs', 'fake_agent','effort_seed'})
            for name in ('hgs', 'fake_agent','effort_seed'):
                self.assertEqual(data['files'][name]['sha256'], hashlib.sha256((f.directory / name).read_bytes()).hexdigest())
            self.assertEqual(data['toolchain'], f.compiler)

    def test_prelookup_admission_rejects_empty_multiple_unknown_or_future_ids(self):
        valid = ['123', 'a' * 64, '1', '1.99.1', '2']
        artifact.admit(*valid)
        for index, values in [(0, ['', '0', '1,2', 'latest', str(2**53)]),
                              (1, ['', 'f' * 63, 'A' * 64]), (2, ['', '0', '3']),
                              (3, ['stable', 'nightly', '1.99.1;exit']), (4, ['', '0'])]:
            for value in values:
                args = valid.copy(); args[index] = value
                with self.subTest(index=index, value=value), self.assertRaises(ValueError):
                    artifact.admit(*args)

    def test_source_run_platform_attempt_toolchain_and_unknown_fields_refuse(self):
        mutations = [lambda d: d.update(source='b' * 40), lambda d: d.update(run_id=102),
            lambda d: d.update(run_id=True), lambda d: d.update(repository='other/zerus'),
            lambda d: d.update(producer_attempt=2), lambda d: d.update(schema=True),
            lambda d: d.update(unknown=True), lambda d: d['platform'].update(machine='x86_64'),
            lambda d: d['toolchain'].update(release='1.99.2'),
            lambda d: d['toolchain'].update(verbose='host: x86_64-apple-darwin\nrelease: 1.99.1\n'),
            lambda d: d['toolchain']['compiler'].update(sha256='unknown'),
            lambda d: d['helpers'].update({'unknown': 'a' * 64}),
            lambda d: d['prerequisites'].update(secret='no')]
        for edit in mutations:
            with self.subTest(edit=edit), tempfile.TemporaryDirectory() as folder:
                f = Fixture(Path(folder)); f.change(edit)
                with self.assertRaises(ValueError): f.verify()

    def test_missing_extra_tampered_linked_or_wrong_machine_bytes_refuse(self):
        def missing(f): (f.directory / 'fake_agent').unlink()
        def extra(f): (f.directory / 'private.json').write_text('must not be uploaded')
        def tampered(f): (f.directory / 'hgs').write_bytes(b'changed')
        def symlink(f):
            (f.directory / 'hgs').unlink(); (f.directory / 'hgs').symlink_to(f.root / 'target/debug/hgs')
        def hardlink(f): os.link(f.directory / 'hgs', f.root / 'alias')
        def wrong_macho(f):
            path=f.directory / 'hgs'; path.write_bytes(b'\xcf\xfa\xed\xfe' + struct.pack('<I', 0x01000007))
            f.change(lambda d: d['files'].update(hgs=artifact.identity(path)))
        for edit in (missing, extra, tampered, symlink, hardlink, wrong_macho):
            with self.subTest(edit=edit.__name__), tempfile.TemporaryDirectory() as folder:
                f = Fixture(Path(folder)); edit(f)
                with self.assertRaises((ValueError, OSError)): f.verify()

    def test_digest_helper_change_and_rebound_attempt_are_not_accepted(self):
        with tempfile.TemporaryDirectory() as folder:
            f = Fixture(Path(folder))
            with self.assertRaises(ValueError): f.verify(sha='c' * 64)
            with self.assertRaises(ValueError): f.verify(attempt='2')
            (f.root / artifact.HELPERS[0]).write_text('changed helper')
            with self.assertRaises(ValueError): f.verify()

    def test_actual_runtime_platform_disagrees_with_runner_label(self):
        env=dict(RUNNER_OS='macOS', RUNNER_ARCH='ARM64', SOURCE_COMMIT='a' * 40,
                 GITHUB_REPOSITORY='example/zerus', GITHUB_RUN_ID='101', GITHUB_RUN_ATTEMPT='1')
        with patch.dict(os.environ, env), patch.object(artifact.platform, 'system', return_value='Darwin'), \
             patch.object(artifact.platform, 'machine', return_value='arm64'), \
             patch.object(artifact.subprocess, 'check_output', side_effect=lambda argv, **kw:
                          'a' * 40 + '\n' if argv[1] == 'rev-parse' else ''):
            self.assertEqual(artifact.context()['platform'], {'system':'Darwin', 'machine':'arm64'})
            with patch.object(artifact.platform, 'machine', return_value='x86_64'), self.assertRaises(ValueError): artifact.context()
            with patch.object(artifact.platform, 'system', return_value='Linux'), self.assertRaises(ValueError): artifact.context()
            with patch.object(artifact.subprocess, 'check_output', side_effect=['a' * 40 + '\n', 'src/cli.rs\n']), self.assertRaises(ValueError): artifact.context()

    def test_install_cli_retains_public_input_receipt_without_running_a_subject(self):
        with tempfile.TemporaryDirectory() as folder:
            f=Fixture(Path(folder)); current={**f.bound,'attempt':2}
            args=['macos-debug-artifact.py','install','--directory',str(f.directory),'--artifact-id','123',
                  '--manifest-sha256',f.sha,'--producer-attempt','1','--rust-release','1.99.1']
            with patch.object(artifact,'ROOT',f.root),patch.object(artifact,'context',return_value=current), \
                 patch.object(artifact,'prerequisites',return_value={'python':'fixture','node':'v24','tmux':'3.8'}), \
                 patch.object(artifact,'seed_receipt') as seed_receipt, \
                 patch.dict(os.environ,GITHUB_RUN_ATTEMPT='2'),patch.object(artifact.sys,'argv',args):
                artifact.main()
            seed_receipt.assert_called_once()
            self.assertEqual(seed_receipt.call_args.args[1]['files'],f.verify()['files'])
            record=json.loads((f.root/'artifacts/test-results/debug-input.json').read_text())
            self.assertEqual((record['producer_attempt'],record['consumer_attempt']),(1,2))
            self.assertEqual(record['artifact_id'],'123')
            self.assertEqual(record['files'],f.verify()['files'])
            self.assertNotIn('environment',record)

    def test_verified_install_checks_exact_compiler_and_refuses_cached_links(self):
        with tempfile.TemporaryDirectory() as folder:
            f=Fixture(Path(folder)); data=f.verify()
            with patch.object(artifact, 'toolchain', return_value={**f.compiler, 'release':'1.99.2'}), self.assertRaises(ValueError):
                artifact.install(f.root, f.directory, data, check_rust=True)
            fake=f.root / artifact.FILES['fake_agent']; fake.unlink(); fake.symlink_to(f.root / 'elsewhere')
            hgs=f.root / artifact.FILES['hgs']; before=hgs.read_bytes()
            with patch.object(artifact, 'toolchain', return_value=f.compiler), self.assertRaises(ValueError):
                artifact.install(f.root, f.directory, data, check_rust=True)
            self.assertEqual(hgs.read_bytes(), before)
            fake.unlink()
            with patch.object(artifact, 'toolchain', return_value=f.compiler):
                artifact.install(f.root, f.directory, data, check_rust=True)
            for name,relative in artifact.FILES.items():
                self.assertEqual((f.root / relative).stat().st_mode & 0o777,0o500 if name=='effort_seed' else 0o755)


class CliPhases(unittest.TestCase):
    def test_default_preserves_every_command_and_split_excludes_only_the_other_phase(self):
        selected_bash=shutil.which('bash')
        self.assertIsNotNone(selected_bash, 'CLI phase controls require an installed Bash')
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder); script=root/'scripts/ci/cli.sh';script.parent.mkdir(parents=True)
            script.write_bytes((ROOT/'scripts/ci/cli.sh').read_bytes())
            binaries=root/'bin';binaries.mkdir()
            for name in ('cargo','python3','bash'):
                path=binaries/name
                path.write_text('#!/bin/sh\n[ -z "${HGS_SHOULD_NOT_EXIST:-}" ] || exit 88\nprintf "%s:%s\\n" '+name+' "$*" >> "$CALL_LOG"\n')
                path.chmod(0o755)
            # Exercise both native branches without depending on the host.
            uname=binaries/'uname'
            uname.write_text('#!/bin/sh\nprintf "%s\\n" "$CLI_PHASE_TEST_SYSTEM"\n')
            uname.chmod(0o755)
            for system in ('Linux','Darwin'):
                results={}
                for phase in ('all','debug','remaining','unknown'):
                    log=root/(system+'-'+phase+'.log')
                    env=dict(os.environ, PATH=str(binaries)+':/usr/bin:/bin', CALL_LOG=str(log),
                             CLI_PHASE_TEST_SYSTEM=system, HGS_SHOULD_NOT_EXIST='contaminated')
                    result=subprocess.run([selected_bash,str(script),*([phase] if phase!='all' else [])],
                                          env=env,capture_output=True,text=True)
                    with self.subTest(system=system,phase=phase):
                        self.assertEqual(result.returncode, 2 if phase=='unknown' else 0,result.stderr)
                    results[phase]=log.read_text().splitlines() if log.exists() else []
                debug=['python3:-']
                if system=='Linux':
                    # zerus-5sl7r: the pinned-image real-child resume test runs
                    # before the full suite, so its failures never hide it.
                    debug+=['cargo:build --locked','cargo:build --locked --example fake_agent',
                            'cargo:test --locked --bin hgs desktop::session_resume::tests::actor::real_cli::'
                            'real_cli_resume_from_the_pinned_image_starts_its_supervisor_and_sends'
                            ' -- --ignored --exact --test-threads=1']
                debug+=['cargo:test --locked','cargo:build --locked']
                native_cases=[
                    'real_cli_recover_only_reconciles_the_original_receipt_and_never_falls_back_to_terminal_input',
                    'real_cli_original_child_kill_and_revoke_never_release_input_or_replay_after_reopen',
                ]
                if system=='Linux':
                    native_cases.append('real_cli_actor_sigkill_keeps_original_custody_and_restart_never_reconstructs_consent')
                debug += ['cargo:test --locked --bin hgs desktop::task_start_reconcile::tests::'+case+
                          ' -- --ignored --exact --test-threads=1' for case in native_cases]
                debug.append('cargo:build --locked --example fake_agent')
                remaining=['python3:-','bash:tests/test_hgs.sh',
                           'cargo:+1.99.1 test --locked --test voice_recovery_fixture --no-run --target-dir target/speech-recovery --message-format=json',
                           'python3:scripts/ci/speech-recovery-executable.py artifacts/test-results/speech-recovery-build.jsonl',
                           'python3:scripts/ci/python-tests.py','cargo:build --release --locked']
                with self.subTest(system=system):
                    self.assertEqual(results['debug'],debug)
                    self.assertEqual(results['remaining'],remaining)
                    self.assertEqual(results['all'],debug+remaining[1:])
                    self.assertEqual(results['unknown'],[])


class WorkflowContracts(unittest.TestCase):
    def setUp(self):
        self.checks=(ROOT/'.github/workflows/checks.yml').read_text()

    def test_completed_producer_forks_two_source_bound_siblings_without_polling(self):
        job,field=security.job,security.field
        producer=job(self.checks,'cli-macos-debug')
        self.assertEqual(field(producer,'needs'),'source')
        self.assertNotRegex(producer, r'(?m)^    cache-mode:')
        self.assertLess(producer.index('id: debug-upload'),producer.index('name: Save Rust debug cache'))
        self.assertLess(producer.index('scripts/ci/cli.sh debug'),producer.index('id: seal'))
        self.assertLess(producer.index('id: seal'),producer.index('id: debug-upload'))
        self.assertNotIn('cli.sh remaining',producer)
        for name in ('cli-macos-core','cli-macos-native'):
            body=job(self.checks,name)
            self.assertEqual(field(body,'needs'),'[source, cli-macos-debug]')
            self.assertIn("needs.cli-macos-debug.result == 'success'",field(body,'if'))
            self.assertIn('artifact-ids: ${{ needs.cli-macos-debug.outputs.artifact_id }}',body)
            self.assertNotIn('pattern:',body)
            self.assertNotIn('github-token:',body)
            self.assertLess(body.index('macos-debug-artifact.py admit'),body.index('actions/download-artifact@'))
            self.assertLess(body.index('macos-debug-artifact.py install'),body.index('cli.sh remaining' if name.endswith('core') else 'agent-compatibility.py --hgs'))
            self.assertNotIn('sleep',body)
            self.assertNotIn('while ',body)
        core=job(self.checks,'cli-macos-core');native=job(self.checks,'cli-macos-native')
        self.assertIn('rustup toolchain install "$MAC_DEBUG_RUST" --profile minimal',core)
        self.assertNotIn('rustup toolchain install stable',core)
        self.assertIn('--check-rust',core)
        self.assertLess(core.index('cli.sh remaining'),core.index('name: Save Rust cache'))
        self.assertEqual(field(native,'cache-mode'),'none')
        self.assertNotIn('actions/cache',native)
        self.assertNotIn('rustup',native)
        self.assertEqual(field(job(self.checks,'cli-macos'),'name'),'CLI (macos-15)')
        self.assertIn('run: bash scripts/ci/cli.sh\n',job(self.checks,'cli'))

    def cache_keys(self, name):
        body=security.job(self.checks,name)
        restore=next(step for step in security.steps(body) if 'uses: actions/cache/restore@' in step)
        key=re.search(r'(?m)^          key: (.+)$',restore)[1]
        fallback_field=re.search(r'(?m)^          restore-keys: (.+)$',restore)[1]
        fallbacks=(re.search(r'(?m)^          restore-keys: \|\n((?:            .+\n)+)',restore)[1]
                   if fallback_field=='|' else fallback_field)
        paths=re.search(r'(?m)^          path: \|\n((?:            .+\n)+)',restore)[1]
        substitutions={"${{ runner.os }}":'macOS',"${{ runner.arch }}":'ARM64',
                       "${{ steps.rust.outputs.rustc }}":'a1b2c3',"${{ steps.rust.outputs.epoch }}":'2026-10',
                       "${{ hashFiles('Cargo.lock') }}":'d'*64}
        def render(value):
            for old,new in substitutions.items():value=value.replace(old,new)
            self.assertNotIn('${{',value)
            return value
        return render(key),[render(value.strip()) for value in fallbacks.splitlines()],paths

    def test_profile_keys_share_legacy_version_paths_and_one_trim_family(self):
        debug,debug_fallback,debug_paths=self.cache_keys('cli-macos-debug')
        complete,core_fallback,core_paths=self.cache_keys('cli-macos-core')
        legacy=debug.removesuffix('debug');family=legacy[:-64]
        self.assertNotEqual(debug,complete)
        self.assertEqual(complete,legacy+'complete')
        self.assertEqual(debug_fallback,[complete,legacy,family])
        self.assertEqual(core_fallback,[debug,legacy,family])
        self.assertEqual(debug.rsplit('-',1)[0],complete.rsplit('-',1)[0])
        self.assertEqual(debug.rsplit('-',1)[0],legacy.rsplit('-',1)[0])
        self.assertEqual(debug_paths,core_paths)
        self.assertEqual([value.strip() for value in debug_paths.splitlines()],
                         ['~/.cargo/registry','~/.cargo/git','target'])

    def test_cold_producer_saves_unit_fingerprints_for_core_then_each_warm_exact_key_wins(self):
        debug,debug_fallback,_=self.cache_keys('cli-macos-debug')
        complete,core_fallback,_=self.cache_keys('cli-macos-core')
        # Model the action's exact match then newest prefix match for each
        # ordered restore key. Values represent the cached build fingerprints.
        caches={}
        def restore(key,fallback):
            for candidate in [key,*fallback]:
                if candidate in caches:return candidate,caches[candidate]
                found=[value for value in caches if value.startswith(candidate)]
                if found:
                    newest=found[-1];return newest,caches[newest]
            return None
        self.assertIsNone(restore(debug,debug_fallback))
        caches[debug]={'unit-dependencies','debug-dependencies'}
        self.assertEqual(restore(complete,core_fallback),(debug,caches[debug]))
        caches[complete]=caches[debug]|{'smoke','python','release-dependencies'}
        self.assertEqual(restore(debug,debug_fallback),(debug,caches[debug]))
        self.assertEqual(restore(complete,core_fallback),(complete,caches[complete]))
        # A complete cache remains the producer's first fallback after a
        # missing/evicted debug entry, without a shared primary-key writer.
        del caches[debug]
        self.assertEqual(restore(debug,debug_fallback),(complete,caches[complete]))

    def test_legacy_lock_and_previous_lock_family_remain_ordered_migration_fallbacks(self):
        for name in ('cli-macos-debug','cli-macos-core'):
            primary,fallback,_=self.cache_keys(name)
            other,legacy,family=fallback
            with self.subTest(job=name):
                self.assertEqual([key for key in fallback if legacy==key],[legacy])
                self.assertTrue(primary.startswith(legacy) and other.startswith(legacy))
                previous=family+'c'*64
                self.assertFalse(previous.startswith(legacy))
                self.assertTrue(previous.startswith(fallback[-1]))
                self.assertEqual(fallback.index(legacy),1)
                self.assertEqual(fallback.index(family),2)

    def test_existing_trim_keeps_only_two_total_debug_complete_and_legacy_entries(self):
        ci=(ROOT/'.github/workflows/ci.yml').read_text();namespace={}
        exec(compile(security.inline_python(security.job(ci,'cache-trim')),'existing-trim-policy','exec'),namespace)
        debug,_,_=self.cache_keys('cli-macos-debug');complete,_,_=self.cache_keys('cli-macos-core')
        legacy=debug.removesuffix('debug')
        entries=[dict(id=index,key=key,ref='refs/heads/main',created_at=f'2026-10-23T10:00:0{index}Z')
                 for index,key in enumerate((legacy,debug,complete),1)]
        now=datetime.datetime(2026,10,23,12,tzinfo=datetime.timezone.utc)
        self.assertEqual([entry['key'] for entry in namespace['stale'](entries,now)],[legacy])
        old_lock=legacy[:-64]+'c'*64
        entries.append(dict(id=4,key=old_lock,ref='refs/heads/main',created_at='2026-10-23T09:00:00Z'))
        self.assertEqual(set(entry['key'] for entry in namespace['stale'](entries,now)),{legacy,old_lock})

    def test_both_saves_inherit_caller_authority_and_need_successful_work_plus_exact_key_miss(self):
        for name,required in (('cli-macos-debug',('cli.sh debug','id: seal','id: debug-upload')),
                              ('cli-macos-core',('macos-debug-artifact.py install','cli.sh remaining'))):
            body=security.job(self.checks,name)
            self.assertNotRegex(body,r'(?m)^    cache-mode:')
            save=next(step for step in security.steps(body) if 'uses: actions/cache/save@' in step)
            condition=re.search(r'(?m)^        if: (.+)$',save)[1]
            self.assertEqual(condition,"inputs.save_caches && steps.rust-cache.outputs.cache-hit != 'true'")
            self.assertNotIn('always()',condition)
            for command in required:self.assertLess(body.index(command),body.index(save))
            for requested,hit,previous_success in itertools.product((False,True),('true','false',''),(False,True)):
                expression=condition.replace('inputs.save_caches',repr(requested)).replace(
                    'steps.rust-cache.outputs.cache-hit',repr(hit)).replace('&&',' and ')
                # No status override: Actions requires prior success as well.
                selected=previous_success and eval(expression,{'__builtins__':{}})
                self.assertEqual(selected,requested and hit!='true' and previous_success)
        for name in ('cli-macos-native','cli-macos'):
            self.assertEqual(security.field(security.job(self.checks,name),'cache-mode'),'none')
        # The called workflow keeps false as its public default. Current
        # caller trust tests separately exercise main versus PR/manual tokens.
        self.assertRegex(self.checks,r'(?s)      save_caches:.*?        default: false')

    def test_public_macos_aggregate_rejects_every_incomplete_dependency(self):
        body=security.job(self.checks,'cli-macos')
        code=compile(security.inline_python(body),'mac-cli-aggregate','exec')
        names=('source','cli-macos-debug','cli-macos-core','cli-macos-native')
        for values in itertools.product(('success','failure','cancelled','skipped'),repeat=4):
            data={name:{'result':result} for name,result in zip(names,values)}
            with self.subTest(values=values),patch.dict(os.environ,RESULTS=json.dumps(data)):
                if all(v=='success' for v in values): exec(code,{})
                else:
                    with self.assertRaises(SystemExit): exec(code,{})
        for missing in names:
            with patch.dict(os.environ,RESULTS=json.dumps({n:{'result':'success'} for n in names if n!=missing})),self.assertRaises(SystemExit): exec(code,{})

    def test_selected_and_full_suite_gate_requires_new_native_even_if_aggregate_is_faked_success(self):
        body=security.job(self.checks,'gate');code=compile(security.inline_python(body),'suite-results','exec')
        mac=('cli-macos-debug','cli-macos-core','cli-macos-native','cli-macos','connector-macos','clippy-macos')
        windows=('windows-tests','windows-desktop','windows-host','client-native-windows')
        always=('source','quick','client-frontend','relay','connector','android')
        for linux,macos,win in itertools.product((False,True),repeat=3):
            results={name:{'result':'success'} for name in always}
            results.update({name:{'result':'success' if macos else 'skipped'} for name in mac})
            results.update({name:{'result':'skipped'} for name in ('arch-package','ios')})
            results.update({name:{'result':'success' if win else 'skipped'} for name in windows})
            results['cli']={'result':'success' if linux else 'skipped'}
            results['client-native-linux']={'result':'success' if linux else 'skipped'}
            results['client-native-macos-core']={'result':'success' if macos else 'skipped'}
            results['client-native-macos-tauri']={'result':'success' if macos else 'skipped'}
            env=dict(RESULTS=json.dumps(results),RUN_LINUX=json.dumps(linux),RUN_MACOS=json.dumps(macos),RUN_ARCH='false',RUN_WINDOWS=json.dumps(win),RUN_IOS='false')
            with patch.dict(os.environ,env):exec(code,{})
            for name in mac[:4]:
                for result in ('failure','cancelled','skipped' if macos else 'success'):
                    changed=copy.deepcopy(results);changed[name]={'result':result}
                    with self.subTest(linux=linux,macos=macos,name=name,result=result),patch.dict(os.environ,{**env,'RESULTS':json.dumps(changed)}),self.assertRaises(SystemExit):exec(code,{})
                changed=copy.deepcopy(results);del changed[name]
                with patch.dict(os.environ,{**env,'RESULTS':json.dumps(changed)}),self.assertRaises(KeyError):exec(code,{})
            for name in windows:
                for result in ('failure','cancelled','skipped' if win else 'success'):
                    changed=copy.deepcopy(results);changed[name]={'result':result}
                    with self.subTest(linux=linux,macos=macos,windows=win,name=name,result=result),patch.dict(os.environ,{**env,'RESULTS':json.dumps(changed)}),self.assertRaises(SystemExit):exec(code,{})
                changed=copy.deepcopy(results);del changed[name]
                with patch.dict(os.environ,{**env,'RESULTS':json.dumps(changed)}),self.assertRaises(KeyError):exec(code,{})


if __name__ == '__main__': unittest.main()
