#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."

platform="${1:?usage: scripts/ci/client-native.sh linux|macos|windows [core|media|tauri-test|tauri-clippy]}"
stage="${2:-all}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target/client-native}"

core_tests() {
    presence_tests
    cargo +1.99.1 test --locked --manifest-path client/core/Cargo.toml --no-fail-fast
    if [ "$platform" = macos ]; then
        cargo +1.99.1 clippy --locked --manifest-path client/core/Cargo.toml \
            --features macos-native-peer --all-targets -- -D warnings
        # Negative selection/owned-child tests only. The separate signed-peer
        # fixtures stay ignored and receive no signing identity or key access.
        cargo +1.99.1 test --locked --manifest-path client/core/Cargo.toml \
            --features macos-native-peer macos_notify::peer::installation
    fi
}

presence_tests() {
    rustup component add --toolchain 1.99.1 rustfmt
    cargo +1.99.1 fmt --check --manifest-path crates/presence-sampler/Cargo.toml
    cargo +1.99.1 clippy --locked --manifest-path crates/presence-sampler/Cargo.toml --all-targets -- -D warnings
    cargo +1.99.1 test --locked --manifest-path crates/presence-sampler/Cargo.toml
}

media_check() {
    cargo +1.99.1 check --locked --manifest-path client/media/Cargo.toml --all-targets --features video
}

tauri_tests() {
    cargo +1.99.1 test --locked --manifest-path client/src-tauri/Cargo.toml
}

tauri_clippy() {
    cargo +1.99.1 clippy --locked --manifest-path client/src-tauri/Cargo.toml --all-targets -- -D warnings
}

case "$platform:$stage" in
    linux:all)
        presence_tests
        cargo +1.99.1 test --locked --manifest-path client/media/Cargo.toml --features video
        env -u DISPLAY WAYLAND_DISPLAY=zerus-none GDK_BACKEND=x11 WEBKIT_DISABLE_DMABUF_RENDERER=1 ZERUS_CLIENT_HEADLESS=1 \
            xvfb-run -a -s "-screen 0 1440x900x24" \
            cargo +1.99.1 test --locked --manifest-path client/src-tauri/Cargo.toml
        cargo +1.99.1 clippy --locked --manifest-path client/src-tauri/Cargo.toml --all-targets --features video -- -D warnings
        # The default-off Linux notification transport compiles nowhere else.
        cargo +1.99.1 clippy --locked --manifest-path client/core/Cargo.toml --all-targets --features linux-native-notify -- -D warnings
        dbus-run-session -- cargo +1.99.1 test --locked --manifest-path client/core/Cargo.toml --features linux-native-notify linux_notify
        ;;
    macos:all)
        core_tests
        tauri_tests
        media_check
        tauri_clippy
        ;;
    windows:all)
        core_tests
        media_check
        tauri_clippy
        ;;
    windows:core|macos:core)
        core_tests
        ;;
    windows:media|macos:media)
        media_check
        ;;
    macos:tauri-test)
        tauri_tests
        ;;
    windows:tauri-clippy|macos:tauri-clippy)
        tauri_clippy
        ;;
    *)
        echo "usage: $0 linux|macos|windows [core|media|tauri-test|tauri-clippy]" >&2
        exit 2
        ;;
esac
