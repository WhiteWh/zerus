"""The per-user Windows installer: its payload is the portable zip's files, unchanged.

The installer (packaging/windows/zerus.iss) carries every file of the tested
portable zip byte for byte, so third-party DLLs and EXEs keep their bytes and
signatures. It adds only `installation.json`, which tells the desktop app that an
installer owns the folder, and `uninstall/`, which holds Inno Setup's uninstaller
and the payload list the installer uses to remove files an older version left.
"""
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import zipfile

from release_common import require
from windows_portable import TOP, inspect

# Added beside the zip's files; the desktop app reads it (UpdateController).
INSTALLATION = "installation.json"
# Inno Setup's uninstaller (UninstallFilesDir) and the payload list.
UNINSTALL = "uninstall"
PAYLOAD_LIST = f"{UNINSTALL}/payload.txt"
# nightly_common.nightly_version: <product>.r<revisions>.g<commit[:7]>.n<run number>.
NIGHTLY = re.compile(r"(?P<product>\d+\.\d+\.\d+)\.r[1-9]\d*\.g(?P<commit>[0-9a-f]{7})\.n(?P<run>[1-9]\d*)")
STABLE = re.compile(r"\d+\.\d+\.\d+")


def installer_name(version):
    """The release asset of the installer built from the zip of `version`."""
    return f"zerus-{version}-windows-x86_64-setup.exe"


def numeric_version(version):
    """The four-part file version Windows shows for the installer: x.y.z.0 for a
    stable release, x.y.z.N for nightly run N (0 beyond the 16-bit limit)."""
    if STABLE.fullmatch(version):
        return f"{version}.0"
    match = NIGHTLY.fullmatch(version)
    require(match is not None, f"Unsupported installer version: {version!r}")
    run = int(match["run"])
    return f"{match['product']}.{run if run <= 65535 else 0}"


def release_version(manifest, version=None):
    """The release version an installer of the zip that build-info.json `manifest`
    describes may carry: the zip's product version for a stable release (the
    default), or the nightly version of that product version and source commit.
    Dev builds have no Windows build."""
    if version is None:
        return manifest["version"]
    if STABLE.fullmatch(version):
        require(version == manifest["version"],
                f"Version {version} is not the zip's product version {manifest['version']}.")
        return version
    match = NIGHTLY.fullmatch(version)
    require(match is not None, f"Unsupported installer version {version!r}: a stable x.y.z or a nightly "
            "x.y.z.rN.gCOMMIT.nN version; Dev builds have no Windows build.")
    require(match["product"] == manifest["version"] and match["commit"] == manifest["source_commit"][:7],
            f"Nightly {version} is not a build of the zip's product version {manifest['version']} "
            f"at commit {manifest['source_commit'][:7]}.")
    return version


def installation_record(manifest, version):
    """installation.json for the build that build-info.json describes, released as
    `version` (release_version): the same version the installer itself carries."""
    record = {"schema": 1, "kind": "installer", "product": "Zerus", "version": version,
              "product_version": manifest["version"], "source_commit": manifest["source_commit"]}
    if manifest.get("desktop") == "tauri":
        record["desktop"] = "tauri"
    return record


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as file:
        for block in iter(lambda: file.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def stage(zip_path, destination, version=None):
    """Extract the checked zip's files into the empty folder `destination` and add
    installation.json and the payload list. `version` must suit the zip
    (release_version) and is checked before anything is written. Returns
    (installation.json, expected), where expected maps every relative path the
    installer installs, except the uninstaller, to its SHA-256."""
    manifest = inspect(zip_path)
    record = installation_record(manifest, release_version(manifest, version))
    destination = Path(destination)
    require(destination.is_dir() and not any(destination.iterdir()), f"{destination} must be an empty folder.")
    expected = {}
    with zipfile.ZipFile(zip_path) as archive:
        # inspect() accepted only safe, unique, regular files below TOP.
        entries = [(entry, PurePosixPath(entry.filename).relative_to(TOP)) for entry in archive.infolist()]
        # Every name is checked before anything is written.
        for _, relative in entries:
            # The installer reads the payload list without knowing its encoding.
            require(relative.as_posix().isascii(), f"Installer payload names must be ASCII: {relative}")
            require(relative.parts[0].casefold() != UNINSTALL and relative.as_posix().casefold() != INSTALLATION,
                    f"The portable zip must not contain {relative}: the installer adds it.")
        for entry, relative in entries:
            target = destination.joinpath(*relative.parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            digest = hashlib.sha256()
            with archive.open(entry) as source, open(target, "xb") as output:
                for block in iter(lambda: source.read(1 << 20), b""):
                    digest.update(block)
                    output.write(block)
            expected[relative.as_posix()] = digest.hexdigest()
    written = destination / INSTALLATION
    written.write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8", newline="\n")
    expected[INSTALLATION] = sha256(written)
    listing = destination.joinpath(*PAYLOAD_LIST.split("/"))
    listing.parent.mkdir()
    # The installer compares these case-insensitively with the files it finds.
    listing.write_text("".join(path.replace("/", "\\") + "\r\n" for path in sorted(expected)), encoding="utf-8")
    expected[PAYLOAD_LIST] = sha256(listing)
    return record, expected


def problems(folder, expected):
    """How the installed `folder` differs from `expected` (stage()): missing,
    changed or extra files. Inno Setup's own unins*.exe and unins*.dat may sit in
    uninstall/ besides the payload list."""
    folder = Path(folder)
    found = {}
    for path in sorted(folder.rglob("*")):
        if path.is_file():
            found[path.relative_to(folder).as_posix()] = path
    issues = []
    folded = {name.casefold(): name for name in found}
    for name, digest in sorted(expected.items()):
        actual = folded.get(name.casefold())
        if actual is None:
            issues.append(f"missing {name}")
        elif sha256(found[actual]) != digest:
            issues.append(f"changed {name}")
    wanted = {name.casefold() for name in expected}
    uninstaller = re.compile(rf"(?i){UNINSTALL}/unins\d{{3}}\.(?:exe|dat)")
    for name in sorted(found):
        if name.casefold() not in wanted and not uninstaller.fullmatch(name):
            issues.append(f"extra {name}")
    return issues
