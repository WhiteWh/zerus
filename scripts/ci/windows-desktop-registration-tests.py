#!/usr/bin/env python3
"""Run the fixed policy controls without changing Windows execution policy."""
import base64
from pathlib import Path
import subprocess


root = Path(__file__).resolve().parents[2]
policy = root / "scripts/ci/fixtures/windows-desktop-registration-legacy.ps1"
argument = str(policy).replace("'", "''")
for name in ("windows-desktop-registration-tests.ps1", "windows-registration-json-tests.ps1"):
    source = (root / "scripts/ci" / name).read_text(encoding="utf-8")
    # Every corpus line is lstripped to fit CreateProcessW. Do not add an
    # indentation-sensitive here-string (the existing one is C# source only).
    source = "\n".join(line.lstrip() for line in source.splitlines())
    assert len(source.encode("utf-8")) <= 16 * 1024
    command = "& {\n" + source + "\n} -RegistrationSource '" + argument + "'"
    encoded = base64.b64encode(command.encode("utf-16le")).decode("ascii")
    assert len(encoded) < 30_000
    subprocess.run(
        ["powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", encoded],
        check=True,
        timeout=60,
    )
