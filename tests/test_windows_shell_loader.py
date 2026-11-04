"""Offline parser and refusal controls for the disposable loader diagnostic."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('loader_diagnostic', ROOT / 'scripts/ci/windows-shell-loader.py')
loader = importlib.util.module_from_spec(spec)
spec.loader.exec_module(loader)


class LoaderDiagnosticTests(unittest.TestCase):
    def test_imports_keep_dll_and_symbol_spelling_without_addresses(self):
        text = '''    KERNEL32.dll
            00000001 Import Address Table
            123 GetLastError
            ABC WaitForSingleObject
            Ordinal 3
    msvcp140.dll
            4 __std_find_trivial_1
'''
        self.assertEqual(loader.imports(text), {
            'kernel32.dll': ['GetLastError', 'WaitForSingleObject'],
            'msvcp140.dll': ['__std_find_trivial_1'],
        })

    def test_forwarded_exports_are_still_named_exports(self):
        self.assertEqual(loader.exports('''    1 0 00002000 GetLastError = NTDLL.RtlGetLastWin32Error
    2 A 00002010 WaitForSingleObject
    3 B          GetCurrentProcess (forwarded to KERNELBASE.GetCurrentProcess)
'''), {'GetLastError', 'WaitForSingleObject', 'GetCurrentProcess'})

    def test_dll_case_aliases_do_not_create_colliding_json_keys(self):
        self.assertEqual(loader.imports('    kernel32.dll\n      1 GetLastError\n    KERNEL32.dll\n      2 GetCurrentProcess\n'),
                         {'kernel32.dll': ['GetLastError', 'GetCurrentProcess']})

    def test_unrecognized_output_supplies_no_missing_symbol_conclusion(self):
        self.assertEqual(loader.imports('tool unavailable\n'), {})
        self.assertEqual(loader.exports('tool unavailable\n'), set())

    def test_import_inventory_refuses_growth(self):
        with self.assertRaises(ValueError):
            loader.imports(''.join(f'    example-{i}.dll\n' for i in range(65)))

    def test_refuses_outside_disposable_ci_before_process_or_output(self):
        with tempfile.TemporaryDirectory() as scratch:
            output = Path(scratch) / 'not-created'
            with patch.dict(loader.os.environ, {'GITHUB_ACTIONS': ''}), patch.object(loader.subprocess, 'Popen') as spawn:
                with self.assertRaisesRegex(ValueError, 'disposable'):
                    loader.diagnose(Path(scratch), output)
                spawn.assert_not_called()
                self.assertFalse(output.exists())


if __name__ == '__main__':
    unittest.main()
