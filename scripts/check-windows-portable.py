#!/usr/bin/env python3
"""Check a Windows portable zip: layout, notices, size limits and provenance.

Every platform reads only the zip directory, build-info.json and the ConPTY
pair, whose SHA-256 must be the pinned one, as the release publishers do. On
Windows, --run also extracts the bundle into a disposable folder and starts its
executables with a minimal environment: no Qt, build, user or HGS_* paths, and
PATH without the bundle. New packages use Tauri plus the separately installed
WebView2 Evergreen runtime; legacy archives keep their old Qt checks. --selftest then also enables local sessions in that folder's
temporary profile with `hgs.exe session-host install`, which must stage the
pinned pair into the host image; no session host starts.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib
import zipfile

import conpty_pair
from release_common import require, sha256
from windows_portable import TOP, UNPACKED_LIMIT, ZIP_LIMIT, inspect

ROOT = Path(__file__).resolve().parents[1]
# Variables Windows programs need to start; everything else is dropped.
SYSTEM_VARIABLES = {"SYSTEMROOT", "SYSTEMDRIVE", "WINDIR", "COMSPEC", "PATHEXT", "COMPUTERNAME", "OS",
                    "NUMBER_OF_PROCESSORS", "PROCESSOR_ARCHITECTURE", "USERNAME", "USERDOMAIN"}


def isolated_environment(base):
    environment = {key: value for key, value in os.environ.items() if key.upper() in SYSTEM_VARIABLES}
    system = environment.get("SYSTEMROOT") or environment.get("WINDIR") or r"C:\Windows"
    home = base / "home"
    folders = {"HOME": home, "USERPROFILE": home, "APPDATA": home / "AppData" / "Roaming",
               "LOCALAPPDATA": home / "AppData" / "Local", "TEMP": base / "temp", "TMP": base / "temp",
               "HGS_CONFIG_DIR": base / "config", "HGS_STATE_DIR": base / "state"}
    for path in folders.values():
        path.mkdir(parents=True, exist_ok=True)
    environment.update({key: str(path) for key, path in folders.items()})
    environment["PATH"] = os.pathsep.join([str(Path(system) / "System32"), system])
    return environment


def without_error_dialogs():
    """Children inherit the error mode: a missing DLL then fails at once instead of
    waiting for someone to close a loader dialog on an unattended machine."""
    import ctypes
    ctypes.windll.kernel32.SetErrorMode(0x0001 | 0x0002)  # SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX


def run(program, arguments, environment, cwd):
    result = subprocess.run([str(program), *arguments], env=environment, cwd=cwd, capture_output=True,
                            stdin=subprocess.DEVNULL, timeout=120)
    output = result.stdout.decode("utf-8", "replace").strip()
    require(result.returncode == 0, f"{program.name} {' '.join(arguments)} exited {result.returncode}: "
            f"{output} {result.stderr.decode('utf-8', 'replace').strip()}")
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("zip", type=Path)
    parser.add_argument("--commit", help="expected source commit [default: this checkout's HEAD]")
    parser.add_argument("--run", action="store_true", help="Windows only: extract and run --version checks")
    parser.add_argument("--selftest", action="store_true",
                        help="with --run: also check the control-only local box and hgs-tray --selftest")
    args = parser.parse_args()
    manifest = inspect(args.zip)
    commit = args.commit or subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    version = (ROOT / "VERSION").read_text(encoding="utf-8").strip()
    cli_version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
    require(manifest["source_commit"] == commit, f"Bundle commit {manifest['source_commit']} is not {commit}.")
    require(manifest["version"] == version and manifest["cli_version"] == cli_version,
            "Bundle versions differ from VERSION/Cargo.toml.")
    with zipfile.ZipFile(args.zip) as archive:
        entries = archive.infolist()
    unpacked = sum(entry.file_size for entry in entries)
    print(f"{args.zip.name}: {args.zip.stat().st_size} bytes (limit {ZIP_LIMIT}), {len(entries)} files, "
          f"{unpacked} bytes unpacked (limit {UNPACKED_LIMIT}).")
    for entry in sorted(entries, key=lambda item: item.file_size, reverse=True)[:5]:
        print(f"  {entry.file_size:>11}  {entry.filename}")
    if args.run:
        require(os.name == "nt", "--run starts Windows executables; use it on Windows only.")
        without_error_dialogs()
        with tempfile.TemporaryDirectory(prefix="zerus-portable-", ignore_cleanup_errors=True) as temporary:
            base = Path(temporary)
            with zipfile.ZipFile(args.zip) as archive:
                # inspect() rejected absolute, parent, device and duplicate names.
                archive.extractall(base / "bundle")
            bundle = base / "bundle" / TOP
            environment = isolated_environment(base)
            hgs, tray = bundle / "hgs.exe", bundle / "hgs-tray.exe"
            for program, expected in ((hgs, f"hgs {cli_version}"), (tray, f"Zerus {version}")):
                reported = run(program, ["--version"], environment, base)
                require(reported == expected, f"{program.name} --version: {reported!r}, expected {expected!r}.")
            if manifest.get("mobile_connector_bundled"):
                connector_version = run(bundle / "zerus-mobile-connector.exe", ["--version"], environment, base)
                require(connector_version.startswith("zerus-mobile-connector "), "Bundled connector does not start.")
                status = json.loads(run(bundle / "zerus-mobile-connector.exe", ["--manage", "status", "--config",
                                       str(base / "config/mobile/connector.json")], environment, base))
                require(status.get("configured") is False and status.get("running") is False,
                        "Fresh portable connector status is not isolated/unconfigured.")
            print(f"hgs.exe {cli_version} and hgs-tray.exe {version} start from the extracted bundle.")
            if args.selftest:
                box = json.loads(run(hgs, ["ls", "--json", "--local"], environment, base))
                require(isinstance(box, dict) and box.get("ok") is True and box.get("local_sessions") is False and
                        box.get("sessions") == [], "hgs.exe does not report a control-only local computer.")
                # hgs-tray must find the hgs.exe next to it, not one from PATH.
                if manifest.get("desktop") == "tauri":
                    report = json.loads(run(tray, ["--build-info"], environment, base))
                    require(report.get("version") == version and report.get("sourceCommit") == commit
                            and report.get("sourceDirty") is False and report.get("ZerusGUI") == "tauri"
                            and report.get("CFBundleIdentifier") == "com.hgdev.hgs-tray",
                            "Portable desktop is not the exact clean Tauri switch build.")
                    print("Tauri desktop build/source/switch identity matches the portable package.")
                else:
                    report = run(tray, ["--selftest"], environment, base)
                    require(report.startswith("hgs ok: "), f"Unexpected hgs-tray --selftest output: {report}")
                    print(f"Control-only local computer {box.get('host')!r}; hgs-tray --selftest: {report}")
                # Enabling local sessions from the extracted folder stages the
                # ConPTY pair next to hgs.exe into the host image (spec D2, D6).
                # Only the temporary profile changes, and nothing starts a host.
                installed = json.loads(run(hgs, ["session-host", "install", "--json"], environment, base))
                image = installed.get("image") if isinstance(installed, dict) else None
                require(isinstance(image, dict) and image.get("pair") == "bundled" and image.get("conpty") == "bundled",
                        f"hgs.exe session-host install did not stage the ConPTY pair: {installed}")
                staged = Path(image["path"])
                for name, pinned in conpty_pair.PIN["files"].items():
                    require((staged / name).is_file() and sha256(staged / name) == pinned["sha256"],
                            f"The host image lacks the pinned {name}: {staged}")
                print(f"session-host install staged hgs.exe with ConPTY {conpty_pair.PIN['version']} "
                      f"into host image {image.get('name')}.")
    print(f"Windows portable bundle verified for commit {commit}.")


if __name__ == "__main__":
    try:
        main()
    except ValueError as error:
        raise SystemExit(f"check-windows-portable: {error}") from None
