#!/usr/bin/env bash
# Install the signed candidate through a local tap exactly as users install the
# channel's cask (stable, or nightly with its release tag), then check Gatekeeper,
# setup, the bundled CLI and uninstallation.
# Runs only on ephemeral hosted runners; it installs into /Applications.
set -euo pipefail
cd "$(dirname "$0")/../.."
signed="$(cd "${1:?signed artifact directory}" && pwd)"
channel="${2:-stable}"
tag="${3:-}"
version="$(cat VERSION)"
# Bash 3.2 can brace-expand a quoted heredoc inside process substitution.
# A command substitution also propagates metadata-validation failure to set -e.
metadata="$(python3 - "$signed/macos-info.json" "$channel" <<'PY'
import json, re, sys
sys.path.insert(0, "scripts")
from macos_release import cask_for, validate_macos_info
info = json.load(open(sys.argv[1]))
release = re.fullmatch(r"zerus-(.+)-macos-arm64\.zip", info["asset"]).group(1)
validate_macos_info({"macos": info, "homebrew_casks": [cask_for(sys.argv[2])]}, release, sys.argv[2])
if "dmg" not in info:
    raise SystemExit("The signed candidate must include its notarized disk image.")
print(cask_for(sys.argv[2]), release, info["team_id"])
PY
)"
read -r cask release team <<< "$metadata"
asset="$signed/zerus-$release-macos-arm64.zip"
image="$signed/zerus-$release-macos-arm64.dmg"
codesign --verify --strict "$image"
python3 scripts/macos_stapler.py validate "$image"
spctl --assess --type open --context context:primary-signature --verbose=2 "$image"
dmg_mount="$(mktemp -d "${RUNNER_TEMP:?}/zerus-dmg-mount.XXXXXX")"
trap 'hdiutil detach "$dmg_mount" >/dev/null 2>&1 || true; rmdir "$dmg_mount" 2>/dev/null || true' EXIT
hdiutil attach -readonly -nobrowse -mountpoint "$dmg_mount" "$image"
test -L "$dmg_mount/Applications"
test "$(readlink "$dmg_mount/Applications")" = /Applications
test -d "$dmg_mount/Zerus.app"
test ! -L "$dmg_mount/Zerus.app"
python3 scripts/check-macos-bundle.py "$dmg_mount/Zerus.app" --gui tauri --version "$version" --release "$release" --team-id "$team" --selftest --source-commit "$(git rev-parse HEAD)"
hdiutil detach "$dmg_mount"
rmdir "$dmg_mount"
trap - EXIT
digest="$(shasum -a 256 "$asset" | cut -d ' ' -f 1)"
mkdir -p artifacts/homebrew
python3 - "$release" "$digest" "$channel" "$tag" "artifacts/homebrew/$cask.rb" <<'PY'
import sys
sys.path.insert(0, "scripts")
from macos_release import render_cask
release, digest, channel, tag, target = sys.argv[1:]
open(target, "w").write(render_cask(release, digest, channel, tag or None))
PY
brew tap-new --no-git zerus-ci/candidate > /dev/null
tap="$(brew --repository zerus-ci/candidate)"
mkdir -p "$tap/Casks"
# Lint the exact published cask from a tap, where Homebrew applies its cask rules.
cp "artifacts/homebrew/$cask.rb" "$tap/Casks/$cask.rb"
brew style --cask "zerus-ci/candidate/$cask"
# Then install the same cask with the not yet published URL pointing at the candidate bytes.
sed -e "s|^  url \".*\"$|  url \"file://$asset\"|" "artifacts/homebrew/$cask.rb" > "$tap/Casks/$cask.rb"
grep -q "^  url \"file://$asset\"$" "$tap/Casks/$cask.rb"
brew install --cask "zerus-ci/candidate/$cask"
app=/Applications/Zerus.app
# Assess the installed, quarantined app before any execution or attribute change.
python3 scripts/check-macos-bundle.py "$app" --gui tauri --version "$version" --release "$release" --team-id "$team"
# Hosted tests cannot answer the normal first-open confirmation. Clear only this
# verified disposable installation's download attribute for unattended execution.
# Public casks and release archives retain their normal quarantine behavior.
xattr -dr com.apple.quarantine "$app"
python3 scripts/check-macos-bundle.py "$app" --gui tauri --version "$version" --release "$release" --team-id "$team" --selftest --source-commit "$(git rev-parse HEAD)"
fixture_home="${RUNNER_TEMP:?}/zerus-home"
mkdir -p "$fixture_home"
HOME="$fixture_home" "$(brew --prefix)/bin/zerus-setup"
test "$(readlink "$fixture_home/.local/bin/hgs")" = "$app/Contents/MacOS/hgs"
HOME="$fixture_home" "$fixture_home/.local/bin/hgs" --version
HOME="$fixture_home" "$fixture_home/.local/bin/hgs" ls --json --local > /dev/null
HOME="$fixture_home" "$(brew --prefix)/bin/zerus-setup" --remove
test ! -e "$fixture_home/.local/bin/hgs"
brew uninstall --cask "$cask"
test ! -e "$app"
echo "$cask $release: cask installation, Gatekeeper, setup and removal checks passed."
