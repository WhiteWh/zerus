"""Installer candidate pairing, sealing and retry contracts with synthetic bytes."""
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
import conpty_pair
import nightly_common
import release_common
import windows_installer_release as installer
from windows_portable_fixture import pinned_pair, portable_zip
from windows_installer_fixture import installer_files
import test_release_publication as stable_fixture
import test_nightly_publication as nightly_fixture

COMMIT = 'a' * 40
PRODUCT = '0.37.0'


class InstallerPublication(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='zerus-installer-publication-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        pin = patch.object(conpty_pair, 'PIN', pinned_pair())
        pin.start()
        self.addCleanup(pin.stop)
        self.zip = portable_zip(self.root / 'portable.zip')
        self.exe, self.receipt = installer_files(self.root, self.zip, PRODUCT, PRODUCT, COMMIT)

    def check(self):
        return installer.require_build(self.exe, self.zip, self.receipt, PRODUCT, PRODUCT, COMMIT)

    def test_ci_binding_catches_changed_executable_and_zip_with_unchanged_source_manifest(self):
        original = self.check()
        self.exe.write_bytes(self.exe.read_bytes() + b'changed')
        with self.assertRaisesRegex(ValueError, 'differs'):
            self.check()
        self.exe, self.receipt = installer_files(self.root, self.zip, PRODUCT, PRODUCT, COMMIT)
        self.zip.write_bytes(self.zip.read_bytes() + b'changed')
        with self.assertRaisesRegex(ValueError, 'differs'):
            self.check()
        self.assertEqual(original['source_commit'], COMMIT)

    def test_build_metadata_is_closed_bounded_and_binds_version_and_source(self):
        original = self.receipt.read_text()
        for key, value in [('version', PRODUCT + '.r60.gaaaaaaa.n7'), ('source_commit', 'b' * 40),
                           ('product_version', '0.38.0'), ('installer_size', True),
                           ('installer_sha256', 'b' * 64), ('extra', 'unrecognized')]:
            with self.subTest(key=key):
                info = json.loads(original)
                info[key] = value
                self.receipt.write_text(json.dumps(info))
                with self.assertRaises(ValueError):
                    self.check()
        self.receipt.write_text(original[:-1] + ', "schema": 1}')
        with self.assertRaisesRegex(ValueError, 'Duplicate'):
            self.check()
        self.receipt.write_bytes(b' ' * (installer.INFO_LIMIT + 1))
        with self.assertRaises(ValueError):
            self.check()

    def test_symlinked_publication_inputs_are_refused(self):
        for attribute in ('exe', 'receipt', 'zip'):
            with self.subTest(input=attribute):
                original = getattr(self, attribute)
                link = self.root / ('link-' + original.name)
                link.symlink_to(original)
                setattr(self, attribute, link)
                with self.assertRaises(ValueError):
                    self.check()
                setattr(self, attribute, original)

    def test_stable_and_nightly_candidates_seal_the_same_tested_bytes_and_retry_unchanged(self):
        for nightly, module in [(False, stable_fixture), (True, nightly_fixture)]:
            with self.subTest(nightly=nightly):
                case = module.NightlyTests() if nightly else module.PublicationTests()
                case.setUp()
                self.addCleanup(case.doCleanups)
                directory = case.candidate(windows=True)
                info = json.loads((directory / 'release-info.json').read_text())
                version = info['version']
                product = info.get('product_version', version)
                portable = directory / info['windows_portable']
                executable, receipt = installer_files(self.root, portable, version, product, COMMIT)
                build = installer.require_build(executable, portable, receipt, version, product, COMMIT)
                installer.add(directory, executable, build, version, info)
                (directory / 'release-info.json').write_text(json.dumps(info))
                packages = nightly_common.PACKAGES if nightly else release_common.PACKAGES
                release_common.finish_candidate(directory, packages)
                validate = nightly_common.validate_nightly if nightly else lambda d: release_common.validate_candidate(d, version)
                _, sums = validate(directory)
                setup = directory / info['windows_installer']['asset']
                self.assertEqual(setup.read_bytes(), executable.read_bytes())
                self.assertEqual(sums[setup.name], build['installer_sha256'])
                self.assertEqual(info['windows_installer']['portable_sha256'], sums[portable.name])
                before = {str(p.relative_to(directory)): p.read_bytes() for p in directory.rglob('*') if p.is_file()}
                validate(directory)
                self.assertEqual(before, {str(p.relative_to(directory)): p.read_bytes() for p in directory.rglob('*') if p.is_file()})
                setup.write_bytes(b'MZchanged after sealing')
                with self.assertRaisesRegex(ValueError, '[Cc]hecksum'):
                    validate(directory)
                setup.write_bytes(executable.read_bytes())
                info['windows_installer']['portable_sha256'] = 'b' * 64
                (directory / 'release-info.json').write_text(json.dumps(info))
                release_common.finish_candidate(directory, packages)
                with self.assertRaisesRegex(ValueError, 'paired ZIP'):
                    validate(directory)

    def test_changed_installer_after_validation_never_enters_candidate_metadata(self):
        build = self.check()
        output = self.root / 'candidate'
        output.mkdir()
        import shutil
        shutil.copyfile(self.zip, output / 'zerus-0.37.0-windows-x86_64.zip')
        info = dict(windows_portable='zerus-0.37.0-windows-x86_64.zip')
        self.exe.write_bytes(self.exe.read_bytes() + b'changed after initial validation')
        with self.assertRaisesRegex(ValueError, 'changed during'):
            installer.add(output, self.exe, build, PRODUCT, info)
        self.assertNotIn('windows_installer', info)

    def test_asset_metadata_requires_its_zip_and_exact_setup_name(self):
        sums = {'zerus-0.37.0-windows-x86_64.zip': 'a' * 64}
        value = dict(asset=installer.installer_name(PRODUCT), portable_sha256='a' * 64)
        self.assertEqual(installer.validate_metadata(value, PRODUCT, next(iter(sums)), sums), value['asset'])
        for change in [dict(asset='../setup.exe'), dict(portable_sha256='b' * 64), dict(extra=True)]:
            with self.subTest(change=change), self.assertRaises(ValueError):
                installer.validate_metadata({**value, **change}, PRODUCT, next(iter(sums)), sums)
        with self.assertRaises(ValueError):
            installer.validate_metadata(value, PRODUCT, None, sums)
