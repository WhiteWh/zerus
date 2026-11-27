#!/usr/bin/env python3
"""Bundle tested hgs, lore, the Tauri desktop and connector into the Windows zip.

The app-local MSVC runtime and pinned ConPTY pair accompany the unchanged
binaries. WebView2 uses the installed Evergreen runtime; Qt is never deployed.
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import tomllib
import zipfile

import conpty_pair
from lore_dependency import lore_version
from release_common import require
from windows_portable import TOP, inspect

# WhiteWh fork, task #65: the fork's releases and the feed its desktop reads.
FORK_REPOSITORY = "WhiteWh/zerus"
FORK_RELEASES = "https://github.com/WhiteWh/zerus/releases/"

ROOT = Path(__file__).resolve().parents[1]
CRT_REQUIRED = ("vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll")


def visual_studio():
    """The newest Visual Studio with the x64 C++ tools, as vswhere reports it."""
    program_files = os.environ.get("ProgramFiles(x86)") or os.environ.get("ProgramFiles") or r"C:\Program Files (x86)"
    vswhere = Path(program_files) / "Microsoft Visual Studio" / "Installer" / "vswhere.exe"
    require(vswhere.is_file(), f"vswhere is missing: {vswhere}")
    output = subprocess.check_output([str(vswhere), "-latest", "-products", "*", "-requires",
                                      "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
                                      "-property", "installationPath", "-utf8"])
    install = output.decode("utf-8").strip()
    require(install and Path(install).is_dir(), "vswhere found no Visual Studio with the x64 C++ tools.")
    return Path(install)


def crt_directory(install):
    """App-local x64 CRT: the default redistributable version, never a hard-coded one."""
    redist = install / "VC" / "Redist" / "MSVC"
    default = install / "VC" / "Auxiliary" / "Build" / "Microsoft.VCRedistVersion.default.txt"
    if default.is_file():
        versions = [default.read_text(encoding="utf-8").strip()]
    else:
        versions = sorted((path.name for path in redist.iterdir() if re.fullmatch(r"\d+(?:\.\d+)+", path.name)),
                          key=lambda value: tuple(map(int, value.split("."))), reverse=True)[:1]
    require(versions, f"No MSVC redistributable under {redist}.")
    # Microsoft.VC143.CRT for Visual Studio 2022, Microsoft.VC145.CRT for 2026.
    folders = sorted((redist / versions[0] / "x64").glob("Microsoft.VC14*.CRT"))
    require(len(folders) == 1, f"Expected one x64 CRT folder under {redist / versions[0]}.")
    return folders[0]


def copy(source, destination):
    require(source.is_file() and not source.is_symlink(), f"Missing bundle input: {source}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)


# WhiteWh fork (zerus-y0cj): the standalone executor package that
# scripts/build-artifact-executor.py writes, unpacked beside hgs.exe.
EXECUTOR_TOP = "hgs-executor-windows-x86_64/"
EXECUTOR_LIMIT = 256 * 1024 * 1024


def add_executor(package, stage):
    """hgs-executor.exe next to hgs.exe, its linked notices under licenses/executor
    and its README and examples under executor/; returns its build identity."""
    require(package.is_file() and not package.is_symlink(), f"Missing executor package: {package}")
    placed = {}
    with zipfile.ZipFile(package) as archive:
        entries = archive.infolist()
        require(0 < len(entries) <= 1000 and sum(e.file_size for e in entries) <= EXECUTOR_LIMIT,
                "Unexpected executor package size")
        for entry in entries:
            name = entry.filename
            require(name.startswith(EXECUTOR_TOP) and not entry.is_dir() and ".." not in name.split("/")
                    and "\\" not in name, f"Unsafe executor package entry: {name!r}")
            relative = name[len(EXECUTOR_TOP):]
            if relative == "hgs-executor.exe":
                target = stage / "hgs-executor.exe"
            elif relative.startswith("licenses/"):
                target = stage / "licenses/executor" / relative[len("licenses/"):]
            elif relative in ("README.md", "install-shared-executor.ps1") or relative.startswith("examples/"):
                target = stage / "executor" / relative
            else:
                raise ValueError(f"Unexpected executor package entry: {name!r}")
            target.parent.mkdir(parents=True, exist_ok=True)
            data = archive.read(entry)
            target.write_bytes(data)
            placed[relative] = hashlib.sha256(data).hexdigest()
    attestation = json.loads((stage / "licenses/executor/build.json").read_text(encoding="utf-8"))
    require(placed.get("hgs-executor.exe") == attestation.get("binary_sha256"),
            "hgs-executor.exe differs from its build attestation")
    require(placed.get("licenses/inventory.json") == attestation.get("inventory_sha256"),
            "The executor's license inventory differs from its build attestation")
    return {"binary": "hgs-executor.exe", "binary_sha256": attestation["binary_sha256"],
            "source_sha256": attestation["source_sha256"], "desync_revision": attestation["desync_revision"]}


def source_commit():
    return subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()


def source_time():
    """Entry time: SOURCE_DATE_EPOCH or the commit time, so rebuilt zips stay comparable."""
    value = os.environ.get("SOURCE_DATE_EPOCH") or subprocess.check_output(
        ["git", "log", "-1", "--format=%ct", "HEAD"], cwd=ROOT, text=True).strip()
    moment = datetime.datetime.fromtimestamp(int(value), datetime.timezone.utc)
    return max(moment.timetuple()[:6], (1980, 1, 1, 0, 0, 0))


def write_zip(stage, output, date_time):
    """Sorted, '/'-separated regular files under one top-level folder."""
    files = []
    for path in sorted(stage.rglob("*")):
        require(not path.is_symlink(), f"Symlinks cannot be bundled: {path}")
        if path.is_file():
            files.append(path)
    with zipfile.ZipFile(output, "w", zipfile.ZIP_DEFLATED) as archive:
        for path in files:
            entry = zipfile.ZipInfo(f"{TOP}/{path.relative_to(stage).as_posix()}", date_time=date_time)
            entry.compress_type = zipfile.ZIP_DEFLATED
            entry.create_system = 3
            entry.external_attr = (stat.S_IFREG | 0o644) << 16
            entry.file_size = path.stat().st_size
            with path.open("rb") as source, archive.open(entry, "w") as target:
                shutil.copyfileobj(source, target, 1024 * 1024)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--cli", type=Path, required=True, help="tested hgs.exe")
    parser.add_argument("--lore", type=Path, required=True, help="tested lore.exe (task 128 subprocess)")
    parser.add_argument("--connector", type=Path, required=True, help="tested zerus-mobile-connector.exe")
    parser.add_argument("--gui", type=Path, required=True, help="tested hgs-tray.exe")
    # WhiteWh fork: a relay for this computer alone (docs/fork-updates.md).
    parser.add_argument("--mobile-relay", type=Path, help="tested zerus-relay.exe")
    parser.add_argument("--output", type=Path, required=True, help="zip to write")
    parser.add_argument("--crt-dir", type=Path, help="Microsoft.VC14*.CRT folder [default: found with vswhere]")
    parser.add_argument("--rust-licenses", type=Path,
                        help="notices from scripts/collect-licenses.py [default: collected for this host target]")
    parser.add_argument("--frontend-licenses", type=Path, help="collected production frontend notices")
    parser.add_argument("--conpty", type=Path,
                        help="folder with the pinned conpty.dll and OpenConsole.exe from scripts/fetch-conpty.py "
                             "[default: fetched from nuget.org]")
    # WhiteWh fork, task #65 (owner, 2026-11-06): the fork's zip names its fork
    # build and feed (the values its hgs-tray.exe embeds). Task #388 (owner,
    # 2026-11-08, "forget Qt"): it ships only the Tauri desktop; the one-release
    # Qt fallback (qt/zerus-qt.exe) is gone.
    parser.add_argument("--fork", action="store_true",
                        help="a WhiteWh fork zip: fork identity in build-info.json and the fork README section")
    parser.add_argument("--fork-build", default=os.environ.get("ZERUS_FORK_BUILD", ""),
                        help="the fork build number [default: $ZERUS_FORK_BUILD; empty for an experimental build]")
    parser.add_argument("--fork-feed", default=os.environ.get("ZERUS_FORK_FEED_URL", ""),
                        help="the fork feed [default: $ZERUS_FORK_FEED_URL]")
    parser.add_argument("--executor", type=Path,
                        help="fork only: the standalone hgs-executor-windows-x86_64.zip to ship beside hgs.exe")
    args = parser.parse_args()
    require(args.output.suffix == ".zip", "The output must be a .zip file.")
    crt = args.crt_dir or crt_directory(visual_studio())
    version = (ROOT / "VERSION").read_text(encoding="utf-8").strip()
    cli_version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
    manifest = {"product": "Zerus", "version": version, "cli_version": cli_version, "platform": "Windows",
                "architecture": "x86_64", "desktop": "tauri", "webview2_runtime": "evergreen", "native_agents_bundled": False, "mobile_connector_bundled": True,
                "lore_version": lore_version(ROOT),
                "source_commit": source_commit()}
    if args.fork:
        build = args.fork_build.strip()
        feed = args.fork_feed.strip()
        require(build == "" or build.isdigit() and int(build) > 0, "The fork build is a positive number or empty.")
        require(feed == "" or feed.startswith(FORK_RELEASES), f"The fork feed is a release asset of {FORK_REPOSITORY}.")
        require(bool(build) == bool(feed), "A fork build has both a build number and a feed, or neither (experimental).")
        manifest["fork"] = {"repository": FORK_REPOSITORY, "build": int(build) if build else None, "feed": feed}
    require(not args.executor or args.fork, "Only a fork zip ships the artifact executor.")
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="zerus-windows-") as temporary:
        stage = Path(temporary) / TOP
        # Microsoft's ConPTY next to hgs.exe (spec D2, D6): `hgs session-host
        # install` stages both files from there into the host image, whose
        # session host loads conpty.dll by absolute path; conpty.dll runs the
        # OpenConsole.exe next to it. Only the two pinned files are taken.
        if args.conpty:
            pair = conpty_pair.check_folder(args.conpty)
        else:
            pair = conpty_pair.fetch(Path(temporary) / "conpty")
        copy(args.cli, stage / "hgs.exe")
        copy(args.lore, stage / "lore.exe")
        copy(args.gui, stage / "hgs-tray.exe")
        copy(args.connector, stage / "zerus-mobile-connector.exe")
        if args.mobile_relay:
            copy(args.mobile_relay, stage / "zerus-relay.exe")
        if args.executor:
            manifest["artifact_executor"] = add_executor(args.executor.resolve(), stage)
        for dll in sorted(crt.glob("*.dll")):
            copy(dll, stage / dll.name)
        missing = [name for name in CRT_REQUIRED if not (stage / name).is_file()]
        require(not missing, f"The MSVC runtime under {crt} lacks: {', '.join(missing)}")
        for name, path in pair.items():
            copy(path, stage / name)
        for name in ("LICENSE", "THIRD_PARTY_NOTICES.md"):
            copy(ROOT / name, stage / name)
        licenses = stage / "licenses"
        copy(ROOT / "docs/licenses/codex-LICENSE.txt", licenses / "codex-LICENSE.txt")
        copy(ROOT / "docs/licenses/conpty-LICENSE.txt", licenses / "conpty-LICENSE.txt")
        copy(ROOT / "docs/licenses/conpty-NOTICE.md", licenses / "conpty-NOTICE.md")
        if args.rust_licenses:
            require((args.rust_licenses / "inventory.json").is_file(), "Collect dependency license texts first.")
            shutil.copytree(args.rust_licenses, licenses / "rust")
        else:
            relay = ["--manifest-path", str(ROOT / "services/mobile/relay/Cargo.toml")] if args.mobile_relay else []
            subprocess.run([sys.executable, str(ROOT / "scripts/collect-licenses.py"), str(licenses / "rust"),
                            "--manifest-path", str(ROOT / "Cargo.toml"),
                            "--manifest-path", str(ROOT / "services/mobile/connector/Cargo.toml"),
                            "--manifest-path", str(ROOT / "crates/lore-bin/Cargo.toml"), *relay],
                           cwd=ROOT, check=True)
            subprocess.run([sys.executable, str(ROOT / "scripts/collect-licenses.py"), str(licenses / "rust/desktop"),
                            "--manifest-path", str(ROOT / "client/src-tauri/Cargo.toml"),
                            "--manifest-features", f"{ROOT / 'client/src-tauri/Cargo.toml'}=video"],
                           cwd=ROOT, check=True)
        require((licenses / "rust/desktop/inventory.json").is_file(), "Collect Tauri dependency license texts first.")
        if args.frontend_licenses:
            require((args.frontend_licenses / "inventory.json").is_file(), "Collect frontend dependency license texts first.")
            shutil.copytree(args.frontend_licenses, licenses / "frontend")
        else:
            subprocess.run([sys.executable, str(ROOT / "client/scripts/collect-notices.py"),
                            str(licenses / "frontend")], cwd=ROOT, check=True)
        readme = (ROOT / "packaging/windows/README.txt").read_text(encoding="utf-8")
        for key in ("version", "cli_version", "source_commit"):
            readme = readme.replace(f"@{key.upper()}@", manifest[key])
        readme = readme.replace("@CONPTY_VERSION@", conpty_pair.PIN["version"])
        if args.fork:
            fork = (ROOT / "packaging/windows/README-fork.txt").read_text(encoding="utf-8")
            build = manifest["fork"]["build"]
            fork = fork.replace("@FORK_BUILD@", str(build) if build else "none (experimental build)")
            fork = fork.replace("@FORK_FEED@", manifest["fork"]["feed"] or "none (experimental build)")
            readme = readme.rstrip("\n") + "\n\n" + fork
        require(not re.search(r"@[A-Z_]+@", readme), "Unreplaced placeholder in the Windows README.")
        # Notepad and other Windows tools expect CRLF in a plain-text README.
        (stage / "README.txt").write_bytes(readme.replace("\n", "\r\n").encode("utf-8"))
        (stage / "build-info.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
        partial = output.with_name(output.name + ".partial")
        partial.unlink(missing_ok=True)
        try:
            write_zip(stage, partial, source_time())
            # The same layout and manifest checks the release publishers repeat.
            inspect(partial)
        except BaseException:
            partial.unlink(missing_ok=True)
            raise
    os.replace(partial, output)
    fork = f", fork build {manifest['fork']['build']}" if args.fork else ""
    print(f"Wrote {output} ({output.stat().st_size} bytes) for Zerus {version}, hgs {cli_version}, "
          f"Tauri/WebView2 Evergreen{fork}, commit {manifest['source_commit']}.")


if __name__ == "__main__":
    try:
        main()
    except ValueError as error:
        raise SystemExit(f"package-windows: {error}") from None

