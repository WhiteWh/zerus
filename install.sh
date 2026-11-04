#!/usr/bin/env bash
# Build and atomically install the Rust CLI. No agent or tmux process is restarted.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
prefix="${HGS_INSTALL_PREFIX:-$HOME/.local}"
if [ "${1:-}" = "--connector-only" ]; then
    if [ "$#" -ne 3 ]; then
        echo "Usage: install.sh --connector-only BINARY SHA256" >&2
        exit 2
    fi
    exec python3 "$here/scripts/install-mobile-connector-binary.py" "$2" "$3" "$prefix"
fi
if command -v cargo >/dev/null 2>&1; then
    cargo_bin="$(command -v cargo)"
elif [ -x "$HOME/.cargo/bin/cargo" ]; then
    cargo_bin="$HOME/.cargo/bin/cargo"
elif [ -x /opt/homebrew/bin/cargo ]; then
    cargo_bin=/opt/homebrew/bin/cargo
else
    echo "hgs: cargo not found; install the newest stable Rust to build the CLI" >&2
    exit 1
fi

PATH="$(dirname "$cargo_bin"):$PATH"
export PATH
"$cargo_bin" build --manifest-path "$here/Cargo.toml" --locked --release
build_dir="${CARGO_TARGET_DIR:-$here/target}"
mkdir -p "$prefix/bin"
binary_stage="$(mktemp "$prefix/bin/.hgs.XXXXXXXX")"
shim_stage="$(mktemp "$prefix/bin/.hgs-state.XXXXXXXX")"
connector_stage=""
trap 'rm -f "$binary_stage" "$shim_stage" ${connector_stage:+"$connector_stage"}' EXIT
install -m 0755 "$build_dir/release/hgs" "$binary_stage"
install -m 0644 "$here/hgs_state.py" "$shim_stage"
changed=0
desktop_changed=0
if [ -L "$prefix/bin/hgs" ] || ! cmp -s "$binary_stage" "$prefix/bin/hgs"; then desktop_changed=1; fi
replace_if_changed() {
    if [ ! -L "$2" ] && cmp -s "$1" "$2"; then
        rm -f "$1"
    else
        mv -f "$1" "$2"
        changed=1
    fi
}
# Existing Python hooks work until the binary arrives. For an old symlink-based
# install, stage the previously absent sibling helper before replacing the link:
# running Bash invocations may resolve their helper again after that replacement.
if [ -e "$prefix/bin/hgs_state.py" ]; then
    replace_if_changed "$binary_stage" "$prefix/bin/hgs"
    replace_if_changed "$shim_stage" "$prefix/bin/hgs_state.py"
else
    replace_if_changed "$shim_stage" "$prefix/bin/hgs_state.py"
    replace_if_changed "$binary_stage" "$prefix/bin/hgs"
fi
if [ "$changed" -eq 1 ]; then echo "Installed hgs Rust CLI."; fi
"$prefix/bin/hgs" --version
# The Rust mobile connector builds hyper, rustls, ring and SQLite, so until
# gateways run it a source install builds it only when asked
# (HGS_MOBILE_CONNECTOR=1) or to update an installed copy; 0 always skips. Its
# service is never enabled, started or restarted here.
connector="$prefix/bin/zerus-mobile-connector"
case "${HGS_MOBILE_CONNECTOR:-}" in
    1) build_connector=1 ;;
    0) build_connector=0 ;;
    "") if [ -e "$connector" ]; then build_connector=1; else build_connector=0; fi ;;
    *) echo "hgs: HGS_MOBILE_CONNECTOR must be 0 or 1" >&2; exit 2 ;;
esac
if [ "$build_connector" -eq 1 ]; then
    "$cargo_bin" build --manifest-path "$here/services/mobile/connector/Cargo.toml" --locked --release \
        --target-dir "$build_dir"
    connector_stage="$(mktemp "$prefix/bin/.zerus-mobile-connector.XXXXXXXX")"
    install -m 0755 "$build_dir/release/zerus-mobile-connector" "$connector_stage"
    if [ ! -L "$connector" ] && cmp -s "$connector_stage" "$connector"; then
        rm -f "$connector_stage"
    else
        mv -f "$connector_stage" "$connector"
        echo "Installed zerus-mobile-connector."
        # A running connector keeps the old binary until it restarts: only a
        # person who has drained phone actions should restart it.
        running=""
        if [ "$(uname -s)" = Darwin ]; then
            agent="$(launchctl print "gui/$(id -u)/com.hgdev.zerus-mobile-connector" 2>/dev/null || true)"
            case "$agent" in
                *"$connector"*) running="launchctl kickstart -k gui/$(id -u)/com.hgdev.zerus-mobile-connector" ;;
            esac
        elif systemctl --user is-active --quiet zerus-mobile-connector.service 2>/dev/null; then
            unit="$(systemctl --user show zerus-mobile-connector.service -p ExecStart 2>/dev/null || true)"
            case "$unit" in
                *"path=$connector "*) running="systemctl --user restart zerus-mobile-connector.service" ;;
            esac
        fi
        if [ -n "$running" ]; then
            echo "The running mobile connector still uses its previous binary. Pause phone actions until" \
                "'zerus-mobile-connector --check --config <file>' reports no running request, then restart it: $running"
        fi
    fi
    "$connector" --version
fi
# macOS /bin/bash 3.2 treats an empty "${array[@]}" as unbound under set -u and,
# with the EXIT trap above, stops here with status 0: guard every optional list.
if [ -z "${HGS_INSTALL_PREFIX:-}" ] && [ -z "${HGS_STATE_DIR:-}" ] && [ "$(uname -s)" = "Darwin" ]; then
    session_args=()
    if [ "$changed" -eq 1 ]; then session_args+=(--restart); fi
    python3 "$here/scripts/install-macos-session-service.py" "$prefix/bin/hgs" ${session_args[@]+"${session_args[@]}"}
fi
if [ -z "${HGS_INSTALL_PREFIX:-}" ] && [ -z "${HGS_STATE_DIR:-}" ] && [ "${HGS_RECOVERY_SERVICE:-1}" != "0" ]; then
    worker_args=()
    if [ "$changed" -eq 1 ]; then worker_args+=(--restart); fi
    python3 "$here/scripts/install-recovery-service.py" "$prefix/bin/hgs" ${worker_args[@]+"${worker_args[@]}"}
fi
if [ -z "${HGS_INSTALL_PREFIX:-}" ] && [ -z "${HGS_STATE_DIR:-}" ] && [ -z "${HGS_CONFIG_DIR:-}" ] && [ "${HGS_SWARM_SERVICE:-1}" != "0" ]; then
    swarm_args=()
    if [ "$changed" -eq 1 ]; then swarm_args+=(--restart); fi
    python3 "$here/scripts/install-swarm-service.py" "$prefix/bin/hgs" ${swarm_args[@]+"${swarm_args[@]}"}
fi
if [ -z "${HGS_INSTALL_PREFIX:-}" ] && [ -z "${HGS_STATE_DIR:-}" ] && [ -z "${HGS_CONFIG_DIR:-}" ] && [ "${HGS_VOICE_SERVICE:-1}" != "0" ]; then
    voice_args=()
    if [ "$changed" -eq 1 ]; then voice_args+=(--restart); fi
    python3 "$here/scripts/install-voice-service.py" "$prefix/bin/hgs" ${voice_args[@]+"${voice_args[@]}"}
fi
# Only deliberate per-user source upgrades refresh an existing desktop service.
# Package/staging/root/foreign configuration installs never touch user services.
# Run after the existing connector/worker steps: a headless pending desktop
# refresh must not prevent those independent updates from completing.
if [ "$(uname -s)" = "Linux" ] && [ "$(id -u)" -ne 0 ] && \
   [ -z "${HGS_INSTALL_PREFIX:-}" ] && [ -z "${DESTDIR:-}" ] && \
   [ -z "${HGS_CONFIG_DIR:-}" ] && [ -z "${HGS_STATE_DIR:-}" ]; then
    desktop_status=0
    python3 "$here/scripts/install-linux-desktop-service.py" refresh --hgs "$prefix/bin/hgs" || desktop_status=$?
    case "$desktop_status" in
        0) ;;
        3) echo "Desktop backend refresh pending. CLI, connector and worker installation completed; refresh from the current graphical session." ;;
        *) exit "$desktop_status" ;;
    esac
fi

# Desktop refusals must not interrupt the existing workers' update path.
if [ -z "${HGS_INSTALL_PREFIX:-}" ] && [ -z "${HGS_STATE_DIR:-}" ] && [ -z "${HGS_CONFIG_DIR:-}" ] && [ "$(uname -s)" = Darwin ]; then
    desktop_args=(--refresh-runner)
    if [ "$desktop_changed" -eq 1 ]; then desktop_args+=(--restart); fi
    python3 "$here/scripts/install-macos-desktop-service.py" "$prefix/bin/hgs" ${desktop_args[@]+"${desktop_args[@]}"}
fi
