#!/usr/bin/env python3
"""Build the per-user Windows installer from the tested portable zip.

The zip is checked as release assembly checks it, its files are extracted
unchanged next to installation.json and the payload list, and Inno Setup 6
compiles packaging/windows/zerus.iss over them. Nothing is rebuilt or re-signed:
every payload file keeps the zip's bytes. --stage-only writes the payload and
prints the compiler command without running it, on any platform.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from release_common import require
import windows_installer
import windows_installer_release

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "packaging" / "windows" / "zerus.iss"
ICON = ROOT / "client" / "src-tauri" / "icons" / "icon.ico"


def compiler(explicit):
    """ISCC.exe: --iscc, else Inno Setup 6 where its installer puts it."""
    if explicit:
        return Path(explicit)
    for variable in ("ProgramFiles(x86)", "ProgramFiles"):
        base = os.environ.get(variable)
        if base and (Path(base) / "Inno Setup 6" / "ISCC.exe").is_file():
            return Path(base) / "Inno Setup 6" / "ISCC.exe"
    found = shutil.which("ISCC.exe") or shutil.which("iscc")
    require(found, "Inno Setup 6 is not installed; pass --iscc.")
    return Path(found)


def command(iscc, payload, output, version, tauri=False):
    return [str(iscc), "/Q",
            f"/DAppVersion={version}",
            f"/DNumericVersion={windows_installer.numeric_version(version)}",
            f"/DPayloadDir={payload}",
            f"/DOutputDir={output.parent}",
            f"/DOutputName={output.stem}",
            f"/DIconFile={ICON}",
            f"/DTauriDesktop={int(tauri)}",
            str(SCRIPT)]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("zip", type=Path, help="tested portable zip (scripts/package-windows.py)")
    parser.add_argument("--version", help="release version the installer carries: the zip's product version "
                        "(the default) or its nightly version")
    parser.add_argument("--output", type=Path, required=True, help="installer to write (.exe)")
    parser.add_argument("--info", type=Path, help="write bounded CI build metadata for candidate assembly")
    parser.add_argument("--iscc", help="ISCC.exe [default: Inno Setup 6 under Program Files]")
    parser.add_argument("--stage-only", type=Path, metavar="FOLDER",
                        help="write the payload into this empty folder and print the compiler command")
    args = parser.parse_args()
    require(args.output.suffix == ".exe", "The installer must be an .exe file.")
    output = args.output.resolve()
    if args.info:
        require(not args.stage_only and not args.info.exists(), "Build metadata requires a fresh compiled installer.")
    if args.stage_only:
        args.stage_only.mkdir(parents=True, exist_ok=True)
        record, expected = windows_installer.stage(args.zip, args.stage_only, args.version)
        print(" ".join(command(Path("ISCC.exe"), args.stage_only.resolve(), output, record["version"], record.get("desktop") == "tauri")))
        print(f"Staged {len(expected)} files of Zerus {record['version']} in {args.stage_only}.")
        return
    iscc = compiler(args.iscc)
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="zerus-installer-") as temporary:
        payload = Path(temporary) / "payload"
        payload.mkdir()
        # The version is checked against the zip before anything is staged or compiled,
        # and the installer carries the version its installation.json records.
        record, expected = windows_installer.stage(args.zip, payload, args.version)
        subprocess.run(command(iscc, payload, output, record["version"], record.get("desktop") == "tauri"), check=True, stdin=subprocess.DEVNULL)
    require(output.is_file(), f"Inno Setup wrote no {output}.")
    if args.info:
        args.info.parent.mkdir(parents=True, exist_ok=True)
        with args.info.open("x", encoding="utf-8") as metadata:
            json.dump(windows_installer_release.build_info(output, args.zip, record), metadata, indent=2)
            metadata.write("\n")
    print(f"{output.name}: {output.stat().st_size} bytes, {len(expected)} files of Zerus {record['version']} "
          f"({record['source_commit']}).")


if __name__ == "__main__":
    try:
        main()
    except ValueError as error:
        sys.exit(f"package-windows-installer: {error}")
