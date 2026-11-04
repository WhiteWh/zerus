#!/usr/bin/env python3
"""Sign, notarize and staple Zerus.app with a Developer ID identity.

Runs only trusted repository code on an already checked bundle and compiles
nothing. Every nested code object is signed inside-out with the hardened
runtime. Without notarization it can sign for local verification, but it never
creates a release archive.
"""
import argparse
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
import time

from macos_stapler import run_stapler
from macos_release import LEGACY_QT_VERSION

ROOT = Path(__file__).resolve().parents[1]
APP_ENTITLEMENTS = ROOT / "packaging/macos/Zerus.entitlements"
MINIMUM_SYSTEM = "14.0"
MACH_O = {b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xce\xfa\xed\xfe", b"\xfe\xed\xfa\xce",
          b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca"}
UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
# Stable, nightly or a numbered Dev release; main() also checks the source base.
RELEASE = re.compile(r"\d+\.\d+\.\d+(?:\.r\d+\.g[0-9a-f]{7}\.n\d+|-dev\.[1-9]\d*)?")


def is_mach_o(path):
    with path.open("rb") as stream:
        return stream.read(4) in MACH_O


def bundle_executable(bundle):
    return bundle / "Contents/MacOS" / plistlib.loads((bundle / "Contents/Info.plist").read_bytes())["CFBundleExecutable"]


def signing_order(app):
    """Standalone code first, then nested bundles deepest first, then the application."""
    standalone, bundles, main_binaries = [], [], {bundle_executable(app)}
    for directory, names, files in os.walk(app):
        for name in names:
            path = Path(directory) / name
            if path.is_symlink():
                continue
            if name.endswith(".app"):
                bundles.append(path)
                main_binaries.add(bundle_executable(path))
            elif name.endswith(".framework"):
                stem = name[:-len(".framework")]
                versions = [version for version in sorted((path / "Versions").iterdir())
                            if version.is_dir() and not version.is_symlink()] if (path / "Versions").is_dir() else [path]
                bundles += versions
                main_binaries.update(version / stem for version in versions)
        standalone += [Path(directory) / name for name in files
                       if not (Path(directory) / name).is_symlink() and is_mach_o(Path(directory) / name)]
    depth = lambda path: len(path.parts)
    standalone = sorted((path for path in standalone if path not in main_binaries), key=depth, reverse=True)
    return standalone + sorted(bundles, key=depth, reverse=True) + [app]


def entitlements_for(path, app):
    if path == app:
        return APP_ENTITLEMENTS
    return None


def codesign_command(path, identity, entitlements, keychain=None):
    command = ["codesign", "--force", "--sign", identity, "--options", "runtime"]
    command.append("--timestamp=none" if identity == "-" else "--timestamp")
    if keychain:
        command += ["--keychain", str(keychain)]
    if entitlements:
        command += ["--entitlements", str(entitlements)]
    return command + [str(path)]


def release_archive(app, output, version, notarization_id, qt_version, team_id,
                    dmg=None, dmg_notarization_id=None):
    if not (isinstance(notarization_id, str) and UUID.fullmatch(notarization_id)):
        raise ValueError("Refusing to create a release archive without an accepted notarization.")
    if dmg is not None or dmg_notarization_id is not None:
        if not (isinstance(dmg_notarization_id, str) and UUID.fullmatch(dmg_notarization_id) and
                dmg is not None and dmg.is_file() and not dmg.is_symlink()):
            raise ValueError("Refusing to release a disk image without an accepted notarization.")
    output.mkdir(parents=True)
    asset = output / f"zerus-{version}-macos-arm64.zip"
    subprocess.run(["ditto", "-c", "-k", "--keepParent", str(app), str(asset)], check=True)
    metadata = {
        "asset": asset.name, "qt_version": qt_version, "team_id": team_id,
        "notarization_id": notarization_id, "minimum_system_version": MINIMUM_SYSTEM}
    if dmg is not None:
        name = f"zerus-{version}-macos-arm64.dmg"
        shutil.copyfile(dmg, output / name)
        metadata["dmg"] = {"asset": name, "notarization_id": dmg_notarization_id}
    (output / "macos-info.json").write_text(json.dumps(metadata, indent=2) + "\n")
    return asset


def create_disk_image(app, image):
    """A read-only drag-to-Applications image, preserving the stapled app bytes."""
    if image.exists() or image.is_symlink():
        raise ValueError("Refusing to overwrite a disk image.")
    with tempfile.TemporaryDirectory(prefix="zerus-dmg-") as temporary:
        staging = Path(temporary) / "volume"
        staging.mkdir()
        subprocess.run(["ditto", str(app), str(staging / "Zerus.app")], check=True)
        (staging / "Applications").symlink_to("/Applications")
        command = ["hdiutil", "create", "-volname", "Zerus", "-fs", "HFS+", "-format", "UDZO",
                   "-srcfolder", str(staging), str(image)]
        for delay in (2, 5, None):
            result = subprocess.run(command, capture_output=True, text=True)
            print(result.stdout, end="")
            print(result.stderr, end="", file=sys.stderr)
            if result.returncode == 0:
                break
            if delay is None or "Resource busy" not in result.stdout + result.stderr:
                result.check_returncode()
            image.unlink(missing_ok=True)
            time.sleep(delay)


def notary_credentials(key_file, key_id, issuer):
    # Team API keys name their issuer; individual API keys must not.
    return ["--key", str(key_file), "--key-id", key_id, *(["--issuer", issuer] if issuer else [])]


def notarize_upload(upload, key_file, key_id, issuer):
    credentials = notary_credentials(key_file, key_id, issuer)
    result = subprocess.run(["xcrun", "notarytool", "submit", str(upload), *credentials, "--wait",
                             "--timeout", "2h", "--output-format", "json"], capture_output=True, text=True)
    try:
        submission = json.loads(result.stdout)
    except ValueError:
        submission = {}
    if not isinstance(submission, dict):
        submission = {}
    identifier, status = submission.get("id", ""), submission.get("status")
    valid_id = isinstance(identifier, str) and UUID.fullmatch(identifier)
    if result.returncode or status != "Accepted" or not valid_id:
        if valid_id:
            log = subprocess.run(["xcrun", "notarytool", "log", identifier, *credentials], capture_output=True, text=True)
            print(log.stdout, file=sys.stderr)
        raise SystemExit(f"Notarization was not accepted ({status}): {result.stderr.strip()}")
    return identifier


def staple(path):
    # Apple can accept a submission while its CloudKit ticket endpoint drops
    # the connection. Both ticket operations use the same bounded policy;
    # permanent errors and exhausted retries remain fatal.
    for action in ("staple", "validate"):
        result = run_stapler(action, path)
        print(result.stdout, end="")
        print(result.stderr, end="", file=sys.stderr)
        result.check_returncode()


def notarize(app, key_file, key_id, issuer):
    with tempfile.TemporaryDirectory(prefix="zerus-notarize-") as temporary:
        upload = Path(temporary) / "Zerus.zip"
        subprocess.run(["ditto", "-c", "-k", "--keepParent", str(app), str(upload)], check=True)
        identifier = notarize_upload(upload, key_file, key_id, issuer)
    staple(app)
    return identifier


def sign_disk_image(app, image, identity, keychain, key_file, key_id, issuer):
    create_disk_image(app, image)
    command = ["codesign", "--sign", identity, "--timestamp"]
    if keychain:
        command += ["--keychain", str(keychain)]
    subprocess.run([*command, str(image)], check=True)
    subprocess.run(["codesign", "--verify", "--strict", str(image)], check=True)
    identifier = notarize_upload(image, key_file, key_id, issuer)
    staple(image)
    subprocess.run(["spctl", "--assess", "--type", "open", "--context", "context:primary-signature",
                    "--verbose=2", str(image)], check=True)
    return identifier


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--app", type=Path, required=True, help="checked unsigned Zerus.app")
    parser.add_argument("--identity", required=True, help="Developer ID Application identity, or - for ad-hoc")
    parser.add_argument("--team-id", required=True)
    parser.add_argument("--keychain", type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--release", help="stable, nightly or numbered Dev release (default: --version)")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--notary-key-file", type=Path)
    parser.add_argument("--notary-key-id")
    parser.add_argument("--notary-issuer", help="issuer ID of a team API key; omit for an individual key")
    args = parser.parse_args()
    notary = [args.notary_key_file, args.notary_key_id]
    if (any(notary) or args.notary_issuer) and not all(notary):
        parser.error("notarization needs the key file and key ID together")
    release = args.release or args.version
    source_dev = (ROOT / "DEV_VERSION").read_text().strip()
    numbered_dev = (release.startswith(source_dev + "-dev.")
                    and args.version == (ROOT / "VERSION").read_text().strip())
    if not RELEASE.fullmatch(release) or not (release.split(".r", 1)[0] == args.version or numbered_dev):
        parser.error("--release must match the stable version or the numbered nightly/Dev source")
    if not re.fullmatch(r"[A-Z0-9]{10}", args.team_id):
        parser.error("invalid Team ID")
    if args.output.exists():
        parser.error("use a fresh output directory")
    info = plistlib.loads((args.app / "Contents/Info.plist").read_bytes())
    if info.get("CFBundleShortVersionString") != args.version or info.get("ZerusReleaseVersion") != release:
        parser.error("the bundle version does not match --version and --release")
    qt_version = (LEGACY_QT_VERSION if info.get("ZerusGUI") == "tauri" else
                  json.loads((ROOT / "packaging/macos/qt.json").read_text())["qt_version"])
    work = args.output.with_name(args.output.name + ".signing")
    app = work / "Zerus.app"
    subprocess.run(["ditto", str(args.app), str(app)], check=True)
    for path in signing_order(app):
        subprocess.run(codesign_command(path, args.identity, entitlements_for(path, app), args.keychain), check=True)
    subprocess.run(["codesign", "--verify", "--deep", "--strict", "--verbose=2", str(app)], check=True)
    if args.identity != "-":
        details = subprocess.run(["codesign", "-dv", str(app)], capture_output=True, text=True).stderr
        if f"TeamIdentifier={args.team_id}" not in details.splitlines():
            raise SystemExit("The signature does not belong to the expected Team ID.")
    if not all(notary):
        print(f"Signed {app} for local verification. No release archive was created.")
        return
    identifier = notarize(app, args.notary_key_file, args.notary_key_id, args.notary_issuer)
    dmg = work / "Zerus.dmg"
    dmg_identifier = sign_disk_image(app, dmg, args.identity, args.keychain, args.notary_key_file,
                                    args.notary_key_id, args.notary_issuer)
    asset = release_archive(app, args.output, release, identifier, qt_version, args.team_id, dmg, dmg_identifier)
    print(f"Notarized app {identifier} and disk image {dmg_identifier}; created {asset} and its DMG.")


if __name__ == "__main__":
    main()
