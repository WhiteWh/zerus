#!/usr/bin/env bash
# Local gate: every Rust crate the desktop client ships has a license notice, for
# this host and for the macOS and Windows targets (scripts/collect-licenses.py
# stops at the first crate without one). Notices of crates that publish none live
# in packaging/rust-licenses. Shipped desktop builds enable the video feature, so
# the gate resolves it too. Needs the crates in Cargo's cache:
#   cargo +1.99.1 fetch --locked --manifest-path client/src-tauri/Cargo.toml
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
manifest="$root/client/src-tauri/Cargo.toml"
features=(--manifest-features "$manifest=video")
python3 "$root/scripts/collect-licenses.py" "$out/host" --manifest-path "$manifest" "${features[@]}"
for target in aarch64-apple-darwin x86_64-pc-windows-msvc x86_64-unknown-linux-gnu; do
    python3 "$root/scripts/collect-licenses.py" "$out/$target" --manifest-path "$manifest" "${features[@]}" --target "$target"
done
