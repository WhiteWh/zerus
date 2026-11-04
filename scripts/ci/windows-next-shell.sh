#!/usr/bin/env bash
# Disposable Windows runner only. The tests use private pipes and registry keys.
set -euo pipefail
cd "$(dirname "$0")/../.."
export RUSTUP_TOOLCHAIN=1.99.1
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target/client-native}"
on_exit() {
    status=$?
    if [ "$status" -ne 0 ]; then
        python scripts/ci/windows-shell-loader.py --target "$CARGO_TARGET_DIR" \
            --output artifacts/windows-shell-loader || true
    fi
    exit "$status"
}
trap on_exit EXIT
native_status=0
python scripts/ci/windows-desktop-registration-tests.py || native_status=1
cargo test --locked --manifest-path crates/presence-sampler/Cargo.toml || native_status=1
# The service/relay binary and native tests come from this exact checkout.
cargo build --locked --bin hgs
cargo test --locked desktop::ui_authority || native_status=1
for selected in actions::read_barrier:: desktop_service::cache::tests desktop_service::frontend::tests::cache_ windows_shell::tests instance::tests platform::windows::startup::tests platform::windows::drawing::tests cli::tests lifecycle::tests window_layer::tests; do
    cargo test --locked --manifest-path client/src-tauri/Cargo.toml --lib "$selected" || native_status=1
done
cargo test --locked --manifest-path client/src-tauri/Cargo.toml --lib --no-run --message-format=json > "$CARGO_TARGET_DIR/windows-native-tests.jsonl"
export HGS_TEST_BIN
HGS_TEST_BIN=$(cygpath -w "$CARGO_TARGET_DIR/debug/hgs.exe")
export HGS_WINDOWS_FRONTEND_FIXTURE
HGS_WINDOWS_FRONTEND_FIXTURE=$(python - "$CARGO_TARGET_DIR/windows-native-tests.jsonl" <<'PYTHON'
import json, sys
from pathlib import Path
executables = []
for line in Path(sys.argv[1]).read_text(encoding='utf-8').splitlines():
    value = json.loads(line)
    if value.get('reason') == 'compiler-artifact' and value['target']['name'] == 'zerus_client_lib' and value['profile']['test'] and value.get('executable'):
        executables.append(value['executable'])
if len(executables) != 1:
    raise SystemExit('one exact native frontend test executable required')
print(executables[0])
PYTHON
)
npm --prefix client ci --ignore-scripts --no-audit --no-fund
(cd client && npm run tauri -- build --debug --no-bundle)
cargo build --locked --bin hgs
mkdir -p artifacts/zerus-next-windows-shell
cp "$CARGO_TARGET_DIR/debug/hgs-tray.exe" "$CARGO_TARGET_DIR/debug/hgs.exe" artifacts/zerus-next-windows-shell/
# Keep exact native fixture bytes/drivers for the same private real-PC checks.
cp "$HGS_WINDOWS_FRONTEND_FIXTURE" artifacts/zerus-next-windows-shell/windows-frontend-tests.exe
python - "$CARGO_TARGET_DIR" <<'PYTHON'
from pathlib import Path
import shutil, sys
images = list((Path(sys.argv[1]) / 'debug/deps').glob('zerus_client_core-*.exe'))
if len(images) != 1 or images[0].stat().st_size > 256 << 20:
    raise SystemExit('one bounded exact core test executable required')
shutil.copyfile(images[0], 'artifacts/zerus-next-windows-shell/windows-core-tests.exe')
PYTHON
mkdir -p artifacts/zerus-next-windows-shell/native-tests
cp tests/windows_frontend_auth.py tests/test_desktop.py artifacts/zerus-next-windows-shell/native-tests/
# Run independent runtime suites after preserving exact compiled bytes.
# Any failure remains the shell step's exit status and the job stays failed.
HGS_DESKTOP_REGISTRATION_TRACE=1 python tests/test_windows_desktop_registration.py --hgs "$CARGO_TARGET_DIR/debug/hgs.exe" || native_status=1
python tests/test_windows_notification_registration.py --hgs "$CARGO_TARGET_DIR/debug/hgs.exe" || native_status=1
python -m unittest discover -s tests -p windows_frontend_auth.py -v || native_status=1
exit "$native_status"
