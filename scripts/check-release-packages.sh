#!/usr/bin/env bash
# Validate Zerus Next source/binary recipes using local archives, without publication.
set -euo pipefail
cd "$(dirname "$0")/.."
if [ "$(id -u)" = 0 ]; then echo 'makepkg must run as an unprivileged user.' >&2; exit 1; fi
release_dir=$(realpath "${1:-dist}")
repo_root="$PWD"
mkdir -p .ci-build/release-packages artifacts/test-results
for flavor in zerus zerus-ade-bin; do
    recipe_dir="$release_dir/aur/$flavor"
    (cd "$recipe_dir"; makepkg --printsrcinfo > .SRCINFO)
    work="$repo_root/.ci-build/release-packages/$flavor"
    mkdir -p "$work"
    rm -f "$work"/*.pkg.tar.zst
    cp "$recipe_dir/PKGBUILD" "$work/PKGBUILD"
    python3 - "$work/PKGBUILD" "$release_dir" "$flavor" <<'PY'
from pathlib import Path
import json, sys
recipe, directory, flavor = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3]
version = json.loads((directory / "release-info.json").read_text())["version"]
suffix = "source" if flavor == "zerus" else "arch-x86_64"
archive = directory / f"zerus-{version}-{suffix}.tar.gz"
uri = archive.as_uri()
assert "'" not in uri
text = recipe.read_text()
start = text.index("source=(")
end = text.index("\n", start)
recipe.write_text(text[:start] + "source=('" + uri + "')" + text[end:])
PY
    (
        cd "$work"
        makepkg --cleanbuild --force --noconfirm 2>&1 | tee "$repo_root/artifacts/test-results/$flavor-makepkg.log"
        namcap "$recipe_dir/PKGBUILD" ./*.pkg.tar.zst 2>&1 | tee "$repo_root/artifacts/test-results/$flavor-namcap.log"
        if grep -q ' E: ' "$repo_root/artifacts/test-results/$flavor-namcap.log"; then exit 1; fi
        cp ./*.pkg.tar.zst "$release_dir/"
    )
    python3 scripts/check-package.py --root "$work/pkg/$flavor" --package-name "$flavor"
done
mkdir -p "$release_dir/aur/zerus-git"
cp packaging/aur/zerus-git/PKGBUILD "$release_dir/aur/zerus-git/PKGBUILD"
python3 - "$release_dir/aur/zerus-git/PKGBUILD" <<'PY'
from pathlib import Path
import re, subprocess, sys
path = Path(sys.argv[1])
version = Path("VERSION").read_text().strip()
count = subprocess.check_output(["git", "rev-list", "--count", "HEAD"], text=True).strip()
revision = subprocess.check_output(["git", "rev-parse", "--short", "HEAD"], text=True).strip()
path.write_text(re.sub(r"^pkgver=.*$", f"pkgver={version}.r{count}.g{revision}", path.read_text(), flags=re.M))
PY
(cd "$release_dir/aur/zerus-git"; makepkg --printsrcinfo > .SRCINFO)
python3 - "$release_dir" <<'PY'
from pathlib import Path
import sys
sys.path.insert(0, "scripts")
from release_common import finish_candidate, validate_candidate
directory = Path(sys.argv[1])
finish_candidate(directory)
validate_candidate(directory, Path("VERSION").read_text().strip())
PY
printf 'Zerus Next source and binary packages validated; all three stable AUR metadata pairs prepared. Nothing was published.\n'
