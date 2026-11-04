#!/usr/bin/env python3
"""Assemble the unsigned Apple Silicon Zerus.app from verified build outputs.

The result adds the hgs CLI, connector, per-user setup command and every notice
to the selected GUI, thins all code to arm64 and applies
an ad-hoc signature so the bundle can be checked locally. The macos-signing job
replaces that signature with the Developer ID one. Nothing is published here.
"""
import argparse
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess

from macos_notices import QT_NOTICES, copy_notices, ffmpeg_findings, ffmpeg_provenance, ffmpeg_source_note

ROOT = Path(__file__).resolve().parents[1]
MINIMUM_SYSTEM = "14.0"
# Installers zerus-setup runs from Contents/Resources/setup, with the local modules they import.
SETUP_SCRIPTS = ("install-macos-session-service.py", "install-recovery-service.py",
                 "install-swarm-service.py", "launchd_agent.py", "install-mobile-connector-service.py",
                 "install-macos-desktop-service.py", "desktop-revoke-hold.py")
SOURCE_NOTE = """# Qt in Zerus.app

Zerus.app contains unmodified Qt {version} frameworks and plugins from the official
Qt online repository, used under the GNU Lesser General Public License v3
(LGPL-3.0-only; its text and the GPL-3.0 text it incorporates are in this folder).
The frameworks are dynamically linked from `Contents/Frameworks` and can be
replaced with compatible builds.

Exact corresponding Qt sources:

- https://download.qt.io/archive/qt/{series}/{version}/single/qt-everywhere-src-{version}.tar.xz
- Git tags `v{version}` in the Qt Project repositories at https://code.qt.io

Per-module licenses, including their third-party components, are listed in the
SPDX documents next to this file. Zerus' own sources are at
https://github.com/ufna/zerus.

Standard license and exception texts named by those documents are also included
as `<SPDX-ID>.txt`; custom LicenseRef texts remain in the SPDX documents. This
conservative set includes license alternatives and optional components described
by Qt's module metadata, without asserting that each component is compiled.
"""


def run(*command):
    subprocess.run([str(part) for part in command], check=True)


def thin_to_arm64(bundle):
    """Replace every universal Mach-O file with its arm64 slice."""
    for directory, _, files in os.walk(bundle):
        for name in files:
            path = Path(directory) / name
            if path.is_symlink():
                continue
            with path.open("rb") as stream:
                if stream.read(4) not in (b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca"):
                    continue
            architectures = subprocess.check_output(["lipo", "-archs", str(path)], text=True).split()
            if architectures != ["arm64"]:
                mode = path.stat().st_mode
                run("lipo", path, "-thin", "arm64", "-output", path.with_name(name + ".arm64"))
                path.with_name(name + ".arm64").replace(path)
                path.chmod(mode)


def install(source, destination, mode=0o644):
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    destination.chmod(mode)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--gui", choices=("qt", "tauri"), default="qt")
    parser.add_argument("--qt", type=Path, help="legacy Qt prefix created by install-qt-macos.py")
    parser.add_argument("--app", type=Path, required=True, help="tested release GUI app")
    parser.add_argument("--cli", type=Path, required=True, help="release hgs executable")
    parser.add_argument("--connector", type=Path, required=True,
                        help="release zerus-mobile-connector executable (services/mobile/connector)")
    parser.add_argument("--licenses", type=Path, required=True, help="output of collect-licenses.py")
    parser.add_argument("--client-licenses", type=Path, help="Cargo notices for the Tauri video build")
    parser.add_argument("--frontend-licenses", type=Path, help="locked frontend dependency notices")
    parser.add_argument("--version", required=True)
    parser.add_argument("--release", help="stable, nightly or numbered Dev release (default: --version)")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.version != (ROOT / "VERSION").read_text().strip():
        parser.error("bundle version must match VERSION")
    if args.release:
        nightly = re.fullmatch(re.escape(args.version) + r"(?:\.r\d+\.g[0-9a-f]{7}\.n\d+)?", args.release)
        dev = re.fullmatch(re.escape((ROOT / "DEV_VERSION").read_text().strip()) + r"-dev\.[1-9]\d*", args.release)
        if not nightly and not dev:
            parser.error("--release must match the stable version or the numbered nightly/Dev source")
    qt, provenance = None, None
    if args.gui == "qt":
        if args.qt is None:
            parser.error("the legacy Qt bundle requires --qt")
        qt = json.loads((args.qt / "zerus-qt.json").read_text())["qt_version"]
        if qt != json.loads((ROOT / "packaging/macos/qt.json").read_text())["qt_version"]:
            parser.error("the Qt prefix does not match packaging/macos/qt.json")
        provenance = ffmpeg_provenance(ROOT)
    else:
        for directory in (args.client_licenses, args.frontend_licenses):
            if directory is None or not (directory / "inventory.json").is_file():
                parser.error("collect the Tauri Cargo and frontend notices first")
    if not (args.licenses / "inventory.json").is_file():
        parser.error("collect Cargo dependency notices first")
    if args.output.exists() and any(args.output.iterdir()):
        parser.error("use an empty output directory")
    args.output.mkdir(parents=True, exist_ok=True)
    work = args.output / "work"
    # macdeployqt derives the executable name from the bundle name, so deploy Qt
    # into hgs-tray.app and rename the finished bundle afterwards.
    staged = work / "hgs-tray.app"
    run("ditto", args.app, staged)

    info_path = staged / "Contents/Info.plist"
    info = plistlib.loads(info_path.read_bytes())
    if (info.get("CFBundleShortVersionString") != args.version
            or info.get("CFBundleIdentifier") not in ("com.hgdev.hgs-tray.local", "com.hgdev.hgs-tray")):
        parser.error("the GUI build does not match this version and bundle identifier")
    if args.gui == "tauri" and (info.get("ZerusGUI") != "tauri"
                                or info.get("ZerusReleaseVersion") != (args.release or args.version)):
        parser.error("the source must be the Tauri switch bundle for this release")
    # LaunchServices needs CFBundleExecutable once the bundle is no longer named hgs-tray.app.
    # Terminal integration runs osascript; under the hardened runtime TCC attributes its
    # Apple Events to Zerus, which therefore declares their purpose.
    info.update(CFBundleIdentifier="com.hgdev.hgs-tray", CFBundleName="Zerus", CFBundleDisplayName="Zerus", CFBundleExecutable="hgs-tray",
                CFBundleVersion=args.version, LSMinimumSystemVersion=MINIMUM_SYSTEM,
                ZerusReleaseVersion=args.release or args.version,
                NSAppleEventsUsageDescription="Zerus opens your agent sessions in Terminal.")
    info_path.write_bytes(plistlib.dumps(info))

    if args.gui == "qt":
        run(args.qt / "bin/macdeployqt", staged, "-verbose=1", "-no-codesign")
        # The NMEA position source needs QtSerialPort, which Zerus neither uses nor ships.
        (staged / "Contents/PlugIns/position/libqtposition_nmea.dylib").unlink(missing_ok=True)

    contents = staged / "Contents"
    install(args.cli, contents / "MacOS/hgs", 0o755)
    # Opt-in through zerus-setup --mobile-connector install; signed with the other code.
    install(args.connector, contents / "MacOS/zerus-mobile-connector", 0o755)
    resources = contents / "Resources"
    install(ROOT / "packaging/macos/zerus-setup", resources / "bin/zerus-setup", 0o755)
    for name in SETUP_SCRIPTS:
        install(ROOT / "scripts" / name, resources / "setup" / name)
    install(ROOT / "hgs_state.py", resources / "setup/hgs_state.py")
    install(ROOT / "tray/packaging/com.hgdev.hgs-tray.plist", resources / "setup/com.hgdev.hgs-tray.plist")
    install(ROOT / "README.md", resources / "doc/README.md")
    install(ROOT / "tmux.conf", resources / "doc/tmux.conf")
    licenses = resources / "licenses"
    install(ROOT / "LICENSE", licenses / "LICENSE")
    if args.gui == "qt":
        install(ROOT / "THIRD_PARTY_NOTICES.md", licenses / "THIRD_PARTY_NOTICES.md")
        install(ROOT / "tray/vendor/libvterm/LICENSE", licenses / "libvterm-LICENSE")
        install(ROOT / "tray/vendor/md4c/LICENSE.md", licenses / "md4c-LICENSE")
        install(ROOT / "tray/vendor/qrcodegen/LICENSE", licenses / "qrcodegen-LICENSE")
    else:
        install(ROOT / "packaging/macos/TAURI-THIRD-PARTY-NOTICES.md", licenses / "THIRD_PARTY_NOTICES.md")
    install(ROOT / "docs/licenses/codex-LICENSE.txt", licenses / "codex-LICENSE.txt")
    shutil.copytree(args.licenses, licenses / "rust")
    if args.gui == "qt":
        copy_notices(ROOT, args.qt, qt, licenses / "qt")
        series = ".".join(qt.split(".")[:2])
        (licenses / "qt/SOURCE.md").write_text(SOURCE_NOTE.format(version=qt, series=series)
                                              + ffmpeg_source_note(provenance))
        install(ROOT / "packaging/macos/ffmpeg.json", licenses / "qt/FFMPEG-SOURCE.json")
    else:
        shutil.copytree(args.client_licenses, licenses / "client-rust")
        shutil.copytree(args.frontend_licenses, licenses / "frontend")

    # The official Qt frameworks are universal; ship Apple Silicon code only.
    thin_to_arm64(staged)
    if args.gui == "qt" and (findings := ffmpeg_findings(staged, provenance)):
        raise ValueError("\n".join(findings))
    bundle = args.output / "Zerus.app"
    run("ditto", staged, bundle)
    shutil.rmtree(work)
    # Thinning invalidates signatures. An ad-hoc signature keeps the unsigned
    # candidate runnable for checks; release signing replaces it.
    run("codesign", "--force", "--deep", "--sign", "-", bundle)
    print(f"Assembled {bundle} for Zerus {args.version} ({args.gui}). Nothing was signed for release.")


if __name__ == "__main__":
    main()
