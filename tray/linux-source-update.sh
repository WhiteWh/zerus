#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 4 ]; then
    echo "Usage: linux-source-update.sh prepare|install STAGE COMMIT BUILD" >&2
    exit 2
fi
mode="$1"
stage="$2"
commit="$3"
build="$4"
case "$mode" in prepare|install) ;; *) echo "Unknown mode: $mode" >&2; exit 2 ;; esac
case "$stage" in /*) ;; *) echo "The update stage must be an absolute path" >&2; exit 2 ;; esac
if [ "$stage" = "/" ] || [[ ! "$commit" =~ ^[0-9a-f]{40}$ ]] || [[ ! "$build" =~ ^[1-9][0-9]*$ ]]; then
    echo "Invalid source update identity" >&2
    exit 2
fi
source="$stage/source"
mkdir -p "$stage"
exec >>"$stage/$mode.log" 2>&1
echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) $mode $commit ($build)"
if [ "$mode" = "install" ]; then
    rm -f "$stage/install.exit"
    trap 'code=$?; printf "%s\n" "$code" > "$stage/install.exit.new"; mv -f "$stage/install.exit.new" "$stage/install.exit"' EXIT
fi

export CARGO_TARGET_DIR="$source/target"
export ZERUS_DEV_BUILD_NUMBER="$build"
export ZERUS_FORK_BUILD="$build"
export ZERUS_FORK_FEED_URL="https://github.com/WhiteWh/zerus/releases/latest/download/zerus-fork-feed.json"

verify_source() {
    test "$(git -C "$source" rev-parse HEAD)" = "$commit"
    test -z "$(git -C "$source" status --porcelain --untracked-files=no)"
}

if [ "$mode" = "prepare" ]; then
    if [ -e "$source" ]; then
        echo "Refusing to replace an existing source tree: $source" >&2
        exit 1
    fi
    git clone --filter=blob:none --no-checkout --branch whitewh/main \
        https://github.com/WhiteWh/zerus.git "$source"
    git -C "$source" checkout --detach "$commit"
    verify_source
    cargo build --manifest-path "$source/Cargo.toml" --locked --release
    prefix_path="${CMAKE_PREFIX_PATH:-}"
    for prefix in "$HOME"/.local/share/zerus/kf6-*; do
        [ -d "$prefix" ] || continue
        prefix_path="${prefix_path:+$prefix_path:}$prefix"
    done
    cmake_args=(-B "$source/tray/build" -S "$source/tray"
        -DCMAKE_BUILD_TYPE=Release -DBUILD_TESTING=OFF
        -DZERUS_DEV_BUILD_NUMBER="$build" -DZERUS_FORK_BUILD="$build")
    if [ -n "$prefix_path" ]; then
        cmake_args+=("-DCMAKE_PREFIX_PATH=$prefix_path")
    fi
    cmake "${cmake_args[@]}"
    jobs="$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)"
    cmake --build "$source/tray/build" --target hgs-tray -j"$jobs"
else
    test -x "$source/install.sh"
    test -x "$source/tray/install.sh"
    verify_source
    export HGS_INSTALL_PREFIX="$HOME/.local"
    unset HGS_STATE_DIR HGS_CONFIG_DIR
    "$source/install.sh"
    # WhiteWh fork: the fork's features live in the Qt GUI until they move to
    # the Tauri client, so the fork's own update keeps Qt unless the
    # installation explicitly chose otherwise.
    HGS_LINUX_GUI="${HGS_LINUX_GUI:-qt}" HGS_TRAY_RESTART=1 "$source/tray/install.sh"
fi
