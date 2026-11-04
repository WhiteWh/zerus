#!/usr/bin/env python3
"""Retain a failed Windows test image and bounded import/export diagnostics.

This runs only on disposable CI, after a failure. Candidate DLL paths are
observations, not a trace of the Windows loader's actual resolution.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
STREAM_LIMIT = 2 * 1024 * 1024
TOTAL_LIMIT = 16 * 1024 * 1024
IMAGE_LIMIT = 256 * 1024 * 1024


def imports(text):
    result, current = {}, None
    for line in text.splitlines():
        match = re.fullmatch(r"\s+([\w.-]+\.dll)\s*", line, re.IGNORECASE)
        if match:
            # DLL lookup is case-insensitive; keep JSON usable by PowerShell 5.
            current = match[1].lower()
            result.setdefault(current, [])
        elif current and (match := re.fullmatch(r"\s+[0-9A-Fa-f]+\s+([A-Za-z_?][^\s]*)\s*", line)):
            result[current].append(match[1])
    if len(result) > 64 or any(len(names) > 8192 for names in result.values()):
        raise ValueError("Import inventory exceeds diagnostic bounds")
    return result


def exports(text):
    return set(re.findall(r"(?m)^\s+\d+\s+[0-9A-Fa-f]+\s+(?:[0-9A-Fa-f]{8,16}\s+)?([A-Za-z_?]\S*)", text))


class Capture:
    def __init__(self, root):
        self.root, self.end, self.total = root, time.monotonic() + 120, 0

    def run(self, args, label):
        out, err = self.root / (label + '.stdout'), self.root / (label + '.stderr')
        with out.open('wb') as stdout, err.open('wb') as stderr:
            with subprocess.Popen(list(map(str, args)), stdout=stdout, stderr=stderr) as process:
                try:
                    while process.poll() is None:
                        if time.monotonic() >= self.end or out.stat().st_size + err.stat().st_size > STREAM_LIMIT:
                            raise TimeoutError("Diagnostic time/output limit")
                        time.sleep(0.05)
                except BaseException:
                    process.kill()
                    process.wait(timeout=5)
                    raise
                code = process.returncode
        size = out.stat().st_size + err.stat().st_size
        self.total += size
        if size > STREAM_LIMIT or self.total > TOTAL_LIMIT:
            raise ValueError("Diagnostic output exceeds its bound")
        return code, out.read_text(encoding='utf-8', errors='replace')


def regular_copy(source, destination):
    if source.is_symlink() or not source.is_file() or source.stat().st_size > IMAGE_LIMIT:
        raise ValueError("Unexpected diagnostic image")
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    with destination.open('rb') as stream:
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    return {'file': destination.name, 'bytes': destination.stat().st_size, 'sha256': digest}


def diagnose(target, output):
    if os.name != 'nt' or os.environ.get('GITHUB_ACTIONS') != 'true':
        raise ValueError("Windows disposable GitHub CI only")
    output.mkdir(parents=True, exist_ok=True)
    capture = Capture(output)
    report = {'workflow_source': os.environ.get('GITHUB_SHA'), 'path': os.environ.get('PATH'),
              'interpretation': 'candidate export inspection, not an observed loader trace'}
    try:
        code, source = capture.run(['git', 'rev-parse', 'HEAD'], 'checked-out-source')
        if code or not re.fullmatch('[0-9a-f]{40}', source.strip()):
            raise ValueError('Cannot bind the checked-out source')
        report['source'] = source.strip()
        # The registration fixture runs the CLI after the shell tests. Retain
        # that exact image too when a later native startup/status check fails.
        cli = target / 'debug/hgs.exe'
        report['cli_image'] = regular_copy(cli, output / 'cli/hgs.exe') if cli.exists() else None
        images = list((target / 'debug/deps').glob('zerus_client_lib-*.exe'))
        if len(images) != 1:
            raise ValueError(f"Expected one test image; found {len(images)}")
        image = images[0]
        report['image'] = regular_copy(image, output / 'baseline' / image.name)
        vswhere = Path(os.environ['ProgramFiles(x86)']) / 'Microsoft Visual Studio/Installer/vswhere.exe'
        code, text = capture.run([vswhere, '-latest', '-products', '*', '-requires',
                                 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64',
                                 '-property', 'installationPath', '-utf8'], 'vswhere')
        if code or not text.strip():
            raise ValueError("No selected Visual Studio")
        install = Path(text.strip())
        version = (install / 'VC/Auxiliary/Build/Microsoft.VCToolsVersion.default.txt').read_text().strip()
        if not re.fullmatch(r'\d+(?:\.\d+)+', version):
            raise ValueError("Invalid Visual C++ version")
        dumpbin = install / 'VC/Tools/MSVC' / version / 'bin/Hostx64/x64/dumpbin.exe'
        code, text = capture.run([dumpbin, '/imports', image], 'test-imports')
        if code:
            raise ValueError("Cannot read test imports")
        required = imports(text)
        if not required:
            raise ValueError("No parsed import table; loader dependencies remain unknown")
        report['imports'] = required
        # Reuse the release packager's exact app-local CRT selection.
        sys.path.insert(0, str(ROOT / 'scripts'))
        spec = importlib.util.spec_from_file_location('package_windows', ROOT / 'scripts/package-windows.py')
        package = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(package)
        crt = package.crt_directory(install)
        report['redist'] = str(crt)
        folders = [image.parent, Path(os.environ['SystemRoot']) / 'System32', ROOT,
                   *map(Path, os.environ.get('PATH', '').split(os.pathsep)), crt]
        folders = list(dict.fromkeys(folders))
        if len(folders) > 96:
            raise ValueError("Unexpected DLL path count")
        report['candidates'] = []
        for name, symbols in required.items():
            matches = [folder / name for folder in folders if (folder / name).is_file()]
            if len(matches) > 8:
                raise ValueError("Unexpected duplicate DLL count")
            for path in dict.fromkeys(matches):
                code, text = capture.run([dumpbin, '/exports', path], f'exports-{len(report["candidates"])}')
                found = (exports(text) or None) if code == 0 else None
                report['candidates'].append({'dll': name, 'path': str(path), 'exports_read': found is not None,
                                             'missing_named_imports': sorted(set(symbols) - found) if found is not None else None})
        control = output / 'app-local-crt'
        dlls = list(crt.glob('*.dll'))
        if not 1 <= len(dlls) <= 32:
            raise ValueError("Unexpected CRT file count")
        cli_bytes = report['cli_image']['bytes'] if report['cli_image'] else 0
        if 2 * image.stat().st_size + cli_bytes + sum(path.stat().st_size for path in dlls) > IMAGE_LIMIT:
            raise ValueError('Diagnostic images exceed their aggregate bound')
        report['control_image'] = regular_copy(image, control / image.name)
        report['control_crt'] = [regular_copy(path, control / path.name) for path in dlls]
        # This only lists methods. It never converts a failed test into a pass.
        code, _ = capture.run([control / image.name, 'windows_shell::tests', '--list'], 'crt-control-list')
        report['control_list_exit'] = code
        event = "$ErrorActionPreference='Stop'; @('Application','System') | ForEach-Object { Get-WinEvent -FilterHashtable @{LogName=$_;Id=26,1000,1001;StartTime=(Get-Date).AddMinutes(-10)} -MaxEvents 24 -ErrorAction SilentlyContinue } | Where-Object {$_.Message -like '*zerus_client_lib*'} | Select-Object TimeCreated,Id,ProviderName,@{N='Message';E={$_.Message.Substring(0,[Math]::Min(4096,$_.Message.Length))}} | ConvertTo-Json -Depth 4"
        capture.run(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', event], 'loader-events')
    except Exception as error:
        report['diagnostic_error'] = str(error)
    finally:
        (output / 'loader-report.json').write_text(json.dumps(report, indent=2), encoding='utf-8')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    diagnose(args.target, args.output)
