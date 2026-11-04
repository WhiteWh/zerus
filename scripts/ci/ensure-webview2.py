#!/usr/bin/env python3
"""CI-only WebView2 Evergreen prerequisite for the disposable Windows runner.

Uses Microsoft's documented bootstrapper, verifies its Authenticode publisher
before execution and checks the documented runtime registration afterwards.
Product installers only check the prerequisite; they never call this helper.
https://learn.microsoft.com/microsoft-edge/webview2/concepts/distribution
"""
import base64
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
from urllib.parse import urlparse
from urllib.request import urlopen

URL = "https://go.microsoft.com/fwlink/p/?LinkId=2124703"
LIMIT = 4 * 1024 * 1024
KEY = r"Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"


def ci_only(platform, environment):
    if platform != "nt" or environment.get("GITHUB_ACTIONS") != "true" or environment.get("RUNNER_OS") != "Windows":
        raise ValueError("WebView2 installation is restricted to the Windows GitHub Actions runner")


def valid_version(value):
    return (isinstance(value, str) and len(value) <= 64
            and re.fullmatch(r"[0-9]+(?:\.[0-9]+){3}", value) is not None
            and any(int(part) for part in value.split(".")))


def runtime_version():
    import winreg
    for hive, view in ((winreg.HKEY_CURRENT_USER, 0), (winreg.HKEY_LOCAL_MACHINE, winreg.KEY_WOW64_32KEY)):
        try:
            with winreg.OpenKey(hive, KEY, 0, winreg.KEY_READ | view) as key:
                value, kind = winreg.QueryValueEx(key, "pv")
                if kind == winreg.REG_SZ and valid_version(value):
                    return value
        except FileNotFoundError:
            pass
    return None


def download(destination):
    until = time.monotonic() + 120
    # TLS validation stays enabled; the mutable Evergreen bootstrapper is
    # authenticated by Windows before it can execute, rather than a stale hash.
    with urlopen(URL, timeout=20) as source:
        final = urlparse(source.geturl())
        if final.scheme != "https" or not (final.hostname or "").endswith(".microsoft.com"):
            raise ValueError("WebView2 bootstrapper left the Microsoft HTTPS download service")
        declared = source.headers.get("Content-Length")
        if declared is not None and not 0 < int(declared) <= LIMIT:
            raise ValueError("WebView2 bootstrapper exceeds its download limit")
        total = 0
        with destination.open("xb") as output:
            while True:
                if time.monotonic() >= until:
                    raise ValueError("WebView2 bootstrapper download timed out")
                block = source.read1(min(64 * 1024, LIMIT + 1 - total))
                if not block:
                    break
                total += len(block)
                if total > LIMIT:
                    raise ValueError("WebView2 bootstrapper exceeds its download limit")
                output.write(block)
        if not total or (declared is not None and total != int(declared)):
            raise ValueError("WebView2 bootstrapper download is empty or incomplete")


def verify_signature(path):
    script = """$ErrorActionPreference='Stop'
$s=Get-AuthenticodeSignature -LiteralPath $env:ZERUS_WEBVIEW2_BOOTSTRAPPER
@{status=$s.Status.ToString();subject=$s.SignerCertificate.Subject}|ConvertTo-Json -Compress
"""
    encoded = base64.b64encode(script.encode("utf-16le")).decode("ascii")
    environment = dict(os.environ, ZERUS_WEBVIEW2_BOOTSTRAPPER=str(path))
    powershell = Path(os.environ["SystemRoot"]) / "System32/WindowsPowerShell/v1.0/powershell.exe"
    result = subprocess.run([str(powershell), "-NoProfile", "-NonInteractive", "-EncodedCommand", encoded],
                            env=environment, stdin=subprocess.DEVNULL, capture_output=True, timeout=60, check=True)
    if len(result.stdout) > 8192:
        raise ValueError("Unexpected WebView2 signature response")
    signature = json.loads(result.stdout.decode("utf-8-sig"))
    if (signature.get("status") != "Valid" or not isinstance(signature.get("subject"), str)
            or re.search(r"(?:^|,\s*)O=Microsoft Corporation(?:,|$)", signature["subject"]) is None):
        raise ValueError("WebView2 bootstrapper lacks a valid Microsoft Authenticode signature")


def ensure_runtime():
    ci_only(os.name, os.environ)
    version = runtime_version()
    if version:
        print(f"WebView2 Evergreen already installed: {version}")
        return
    with tempfile.TemporaryDirectory(prefix="zerus-webview2-", dir=os.environ["RUNNER_TEMP"]) as temporary:
        bootstrapper = Path(temporary) / "MicrosoftEdgeWebview2Setup.exe"
        download(bootstrapper)
        verify_signature(bootstrapper)
        result = subprocess.run([str(bootstrapper), "/silent", "/install"], stdin=subprocess.DEVNULL,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=120)
        if result.returncode not in (0, 3010):
            raise ValueError(f"WebView2 bootstrapper exited {result.returncode}")
        version = runtime_version()
        if not version:
            raise ValueError("WebView2 setup did not establish a registered Evergreen runtime")
        print(f"WebView2 Evergreen installed for CI: {version}")


if __name__ == "__main__":
    ensure_runtime()
