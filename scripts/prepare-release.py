#!/usr/bin/env python3
"""Assemble reviewed source/binary candidates and AUR recipes; never publish."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile

from macos_release import add_macos
from release_common import binary_dependencies
import windows_portable
import windows_installer_release

ROOT = Path(__file__).resolve().parents[1]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", type=Path, required=True, help="verified Arch CI package")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--windows-zip", type=Path,
                        help="tested Windows portable zip from the same CI run (copied unchanged)")
    parser.add_argument("--windows-installer", type=Path, help="tested installer from the same CI run")
    parser.add_argument("--windows-installer-info", type=Path, help="the installer CI build metadata")
    parser.add_argument("--macos", type=Path, help="signed and notarized macOS artifact from the macos-signing job")
    args = parser.parse_args()
    version = (ROOT / "VERSION").read_text().strip()
    if not re.fullmatch(r"\d+\.\d+\.\d+", args.version) or args.version != version:
        parser.error("candidate version must match VERSION exactly")
    if subprocess.check_output(["git", "status", "--porcelain", "--untracked-files=normal"], cwd=ROOT, text=True).strip():
        parser.error("commit the reviewed checkout before preparing an exact-source candidate")
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    epoch = int(subprocess.check_output(["git", "show", "-s", "--format=%ct", "HEAD"], cwd=ROOT, text=True))
    installer_build = None
    if args.windows_zip:
        if not args.windows_installer or not args.windows_installer_info:
            parser.error("the Windows ZIP requires its tested installer and build metadata")
        try:
            installer_build = windows_installer_release.require_build(
                args.windows_installer, args.windows_zip, args.windows_installer_info, version, version, revision)
        except ValueError as error:
            parser.error(str(error))
    elif args.windows_installer or args.windows_installer_info:
        parser.error("a Windows installer requires its paired portable ZIP")
    args.output.mkdir(parents=True, exist_ok=True)
    if any(args.output.iterdir()):
        parser.error("use an empty candidate output directory; never overwrite an existing candidate")
    source = args.output / f"zerus-{version}-source.tar.gz"
    archive = subprocess.check_output(["git", "archive", "--format=tar", f"--prefix=zerus-{version}/", "HEAD"], cwd=ROOT)
    source.write_bytes(gzip.compress(archive, mtime=epoch))
    binary = args.output / f"zerus-{version}-arch-x86_64.tar.gz"
    with tempfile.TemporaryDirectory(prefix="zerus-release-") as temporary:
        root = Path(temporary)
        entries = subprocess.check_output(["tar", "--zstd", "-tf", str(args.package.resolve())], text=True).splitlines()
        for name in entries:
            path = Path(name.removeprefix("./"))
            if path.is_absolute() or ".." in path.parts or (path.parts and path.parts[0] not in {"usr", ".PKGINFO", ".MTREE", ".BUILDINFO"}):
                parser.error(f"unexpected package entry: {name}")
        subprocess.run(["tar", "--zstd", "-xf", str(args.package.resolve()), "-C", str(root)], check=True)
        info = json.loads((root / "usr/share/doc/zerus/build-info.json").read_text())
        if info.get("source_commit") != revision or info.get("version") != version:
            parser.error("the verified package must come from this exact commit and version")
        libraries = binary_dependencies((root / ".BUILDINFO").read_text())
        with binary.open("wb") as output, gzip.GzipFile(filename="", mode="wb", fileobj=output, mtime=epoch) as zipped:
            with tarfile.open(fileobj=zipped, mode="w") as bundled:
                prefix = f"zerus-{version}-arch-x86_64"
                for path in sorted((root / "usr").rglob("*")):
                    relative = path.relative_to(root)
                    member = bundled.gettarinfo(str(path), arcname=f"{prefix}/{relative}")
                    member.uid = member.gid = 0
                    member.uname = member.gname = "root"
                    member.mtime = epoch
                    if member.isfile():
                        with path.open("rb") as content:
                            bundled.addfile(member, content)
                    else:
                        bundled.addfile(member)
    values = {"@VERSION@": version, "@SOURCE_SHA256@": digest(source), "@BINARY_SHA256@": digest(binary),
              "@BINARY_LIBRARY_DEPENDS@": " ".join(f"'{name}>={value}'" for name, value in libraries.items())}
    for flavor in ("zerus", "zerus-ade-bin"):
        directory = args.output / "aur" / flavor
        directory.mkdir(parents=True, exist_ok=True)
        text = (ROOT / "packaging/aur" / flavor / "PKGBUILD.in").read_text()
        for key, value in values.items():
            text = text.replace(key, value)
        (directory / "PKGBUILD").write_text(text)
    (args.output / "SHA256SUMS").write_text("".join(f"{digest(path)}  {path.name}\n" for path in (source, binary)))
    info = {"version": version, "source_commit": revision, "architecture": "x86_64", "distribution": "Arch Linux",
            "publication": "candidate", "binary_dependencies": libraries, "native_agents_bundled": False}
    if args.windows_zip:
        # The tested CI bytes under their stable name: renamed, never rebuilt.
        info["windows_portable"] = windows_portable.asset_name(version)
        shutil.copyfile(args.windows_zip, args.output / info["windows_portable"])
    if installer_build:
        windows_installer_release.add(args.output, args.windows_installer, installer_build, version, info)
    if args.macos:
        add_macos(args.output, args.macos, version, info)
    (args.output / "release-info.json").write_text(json.dumps(info, indent=2) + "\n")
    print("Created source and Arch binary archives, SHA256SUMS and release AUR recipes"
          + (" plus the tested Windows zip" if args.windows_zip else "")
          + (" and the notarized macOS bundle with its cask" if args.macos else "") + ". Nothing was published.")


if __name__ == "__main__":
    main()
