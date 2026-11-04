#!/usr/bin/env bash
# Build and validate this exact checkout; full recipe tests run by default.
set -euo pipefail
build_only=false
if [ "$#" -gt 1 ]; then echo 'Usage: build-arch-package.sh [--build-only]' >&2; exit 2; fi
case "${1:-}" in
    '') ;;
    --build-only) build_only=true ;;
    *) echo 'Usage: build-arch-package.sh [--build-only]' >&2; exit 2 ;;
esac
if [ "$build_only" = true ]; then
    if [[ "${ZERUS_RELEASE_CHANNEL:-}" != dev || ! "${ZERUS_DEV_BUILD_NUMBER:-}" =~ ^[1-9][0-9]*$ ]]; then
        echo '--build-only requires the Dev channel and a positive Dev build number.' >&2
        exit 2
    fi
fi
cd "$(dirname "$0")/.."
if [ "$(id -u)" = 0 ]; then echo 'makepkg must run as an unprivileged user.' >&2; exit 1; fi
repo_root="$PWD"
git rev-parse --verify HEAD > /dev/null
package_build="$repo_root/.ci-build/arch-package"
mkdir -p "$package_build" artifacts/test-results
(
    cd packaging/aur/zerus-git
    makepkg --printsrcinfo | diff - .SRCINFO
)
cp packaging/aur/zerus-git/PKGBUILD "$package_build/PKGBUILD"
rm -f "$package_build"/*.pkg.tar.zst artifacts/zerus-git-*.pkg.tar.zst
# Only this disposable copy uses a local source; the distributable recipe keeps
# its anonymous upstream URL. No token or personal checkout path enters AUR.
python3 - "$package_build/PKGBUILD" "$repo_root" <<'PY'
from pathlib import Path
import sys
path=Path(sys.argv[1])
upstream="source=('zerus::git+https://github.com/ufna/zerus.git#branch=main')"
text=path.read_text()
assert text.count(upstream)==1
uri=Path(sys.argv[2]).as_uri()
assert "'" not in uri
text=text.replace(upstream,"source=('zerus::git+"+uri+"')")
path.write_text(text)
PY
makepkg_args=(--cleanbuild --force --noconfirm)
if [ "$build_only" = true ]; then makepkg_args+=(--nocheck); fi
(
    cd "$package_build"
    makepkg "${makepkg_args[@]}" 2>&1 | tee "$repo_root/artifacts/test-results/makepkg.log"
    makepkg --printsrcinfo > "$repo_root/artifacts/test-results/built-package.SRCINFO"
    namcap PKGBUILD ./*.pkg.tar.zst 2>&1 | tee "$repo_root/artifacts/test-results/namcap.log"
    if grep -q ' E: ' "$repo_root/artifacts/test-results/namcap.log"; then
        echo 'Package lint errors must be fixed before publication.' >&2
        exit 1
    fi
    cp ./*.pkg.tar.zst "$repo_root/artifacts/"
)
python3 scripts/check-package.py --root "$package_build/pkg/zerus-git"
cp "$package_build/pkg/zerus-git/usr/share/doc/zerus/build-info.json" artifacts/desktop-build-info.json
