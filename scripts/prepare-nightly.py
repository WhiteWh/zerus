#!/usr/bin/env python3
"""Repackage the exact verified Arch binary and Windows zip into an immutable nightly candidate."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from macos_release import add_macos
from nightly_common import PACKAGE, ROOT, nightly_version, render_recipe, windows_asset
from release_common import require, sha256
import windows_portable
import windows_installer_release


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--windows-zip", type=Path, required=True,
                        help="portable zip from the same CI run (copied unchanged)")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--run-id", type=int, required=True)
    parser.add_argument("--run-number", type=int, required=True)
    parser.add_argument("--windows-installer", type=Path, required=True, help="tested installer from the same CI run")
    parser.add_argument("--windows-installer-info", type=Path, required=True, help="the installer CI build metadata")
    parser.add_argument("--macos", type=Path, help="signed and notarized macOS artifact from the macos-signing job")
    args = parser.parse_args()
    require(args.run_id > 0, "Invalid nightly run ID.")
    product = (ROOT / "VERSION").read_text().strip()
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    count = int(subprocess.check_output(["git", "rev-list", "--count", "HEAD"], cwd=ROOT, text=True))
    version = nightly_version(product, count, commit, args.run_number)
    installer_build = windows_installer_release.require_build(
        args.windows_installer, args.windows_zip, args.windows_installer_info, version, product, commit)
    args.output.mkdir(parents=True, exist_ok=True)
    require(not any(args.output.iterdir()), "Use an empty nightly output directory.")
    with tempfile.TemporaryDirectory(prefix="zerus-nightly-") as temporary:
        verified = Path(temporary) / "verified"
        # Existing assembly validates package paths, embedded source/version and
        # .BUILDINFO library floors. It never compiles or publishes anything.
        subprocess.run([sys.executable, str(ROOT / "scripts/prepare-release.py"),
                        "--version", product, "--package", str(args.package.resolve()),
                        "--output", str(verified)], cwd=ROOT, check=True)
        info = json.loads((verified / "release-info.json").read_text())
        info.update(version=version, product_version=product, channel="nightly",
                    revision_count=count, workflow_run_id=args.run_id,
                    workflow_run_number=args.run_number, release_tag=f"nightly-{args.run_id}")
        binary = args.output / f"zerus-{version}-arch-x86_64.tar.gz"
        shutil.copyfile(verified / f"zerus-{product}-arch-x86_64.tar.gz", binary)
    # The tested CI bytes under their nightly name: renamed, never rebuilt.
    info["windows_portable"] = windows_asset(version)
    shutil.copyfile(args.windows_zip, args.output / info["windows_portable"])
    windows_installer_release.add(args.output, args.windows_installer, installer_build, version, info)
    recipe = args.output / "aur" / PACKAGE
    recipe.mkdir(parents=True)
    (recipe / "PKGBUILD").write_text(render_recipe(info, sha256(binary)))
    if args.macos:
        add_macos(args.output, args.macos, version, info, "nightly")
    (args.output / "release-info.json").write_text(json.dumps(info, indent=2) + "\n")
    print(f"Prepared exact nightly {version}{' with its notarized macOS bundle' if args.macos else ''}; "
          "validate and seal before publication.")


if __name__ == "__main__":
    main()
