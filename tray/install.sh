#!/usr/bin/env bash
set -euo pipefail

# Ставит hgs-tray в домашний каталог, без root
# (модель для этого скрипта), которому нужен /usr/local/bin и системный polkit-файл.
# Веток две, по `uname -s`: на Linux это бинарь в ~/.local/bin + systemd --user-юнит, на
# macOS -- .app-бандл в ~/Applications + LaunchAgent в ~/Library/LaunchAgents. Обе
# идемпотентны: каждый шаг сравнивает то, что уже стоит, с тем, что должно быть, и
# печатает "Installed" в конце, только если реально что-то поменял -- это единственная
# строка, которую парсит changed_when роли hgs_sessions
# (roles/hgs_sessions/tasks/main.yml), так что менять её текст здесь и там нужно синхронно.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHANGED=0
# A changed build replaces the installed GUI through update-installed.py: it quits a
# running GUI like its Quit action (drafts are saved), keeps one rollback copy, starts
# it again and checks drafts and native agents. HGS_TRAY_RESTART=0 only stages the
# build on disk; the running GUI keeps its drafts and the new UI opens on next launch.
RESTART_RUNNING="${HGS_TRAY_RESTART:-1}"
UPDATE=0
case "$RESTART_RUNNING" in 0|1) ;; *) echo "HGS_TRAY_RESTART must be 0 or 1" >&2; exit 1 ;; esac
JOBS="$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)"

echo "=== hgs-tray installer ==="

if [ "$(uname -s)" = "Darwin" ]; then

# =============================== macOS =======================================
LABEL="com.hgdev.hgs-tray"
APP_DEST="${HOME}/Applications/hgs-tray.app"
BUILD_APP="${SCRIPT_DIR}/build/hgs-tray.app"
PLIST_SRC="${SCRIPT_DIR}/packaging/${LABEL}.plist"
PLIST_DEST="${HOME}/Library/LaunchAgents/${LABEL}.plist"
RELOAD=0

# The desktop backend is independent of GUI lifetime. Preflight its exact owned
# registration before replacing the app; GUI-only updates retain a live backend.
if [ -x "${HOME}/.local/bin/hgs" ]; then
    HGS_BIN="${HOME}/.local/bin/hgs"
elif command -v hgs >/dev/null 2>&1; then
    HGS_BIN="$(command -v hgs)"
else
    echo 'hgs-tray: install the hgs CLI with ./install.sh or mac-install.sh first.' >&2
    exit 1
fi
python3 "${SCRIPT_DIR}/../scripts/install-macos-desktop-service.py" "${HGS_BIN}" --check-only

# Неинтерактивный ssh на мак приходит с голым PATH (/usr/bin:/bin:/usr/sbin:/sbin):
# /opt/homebrew/bin туда добавляет только `brew shellenv` из профиля интерактивной
# оболочки. Скрипт должен работать и по ssh, и из Терминала, поэтому инструмент ищем
# сначала в PATH, а потом по штатным путям Homebrew (arm64, затем Intel).
find_tool() {
    if command -v "$1" >/dev/null 2>&1; then command -v "$1"
    elif [ -x "/opt/homebrew/bin/$1" ]; then echo "/opt/homebrew/bin/$1"
    elif [ -x "/usr/local/bin/$1" ]; then echo "/usr/local/bin/$1"
    else return 1
    fi
}

# Сравнить два бандла побайтово нечем: cmp на исполняемом файле упёрся бы в подпись, а
# `diff -r` -- ещё и в _CodeSignature/. cdhash -- это хэш CodeDirectory, то есть кода,
# Info.plist и запечатанных ресурсов разом: одинаковый бандл -> одинаковый cdhash.
# Нет бандла (или он без подписи) -- пустая строка, она не совпадёт ни с чем.
bundle_cdhash() {
    codesign -d --verbose=4 "$1" 2>&1 | sed -n 's/^CDHash=//p'
}

# --- 0. which GUI -------------------------------------------------------------
# Fresh installs and upgrades build Zerus Next. HGS_MAC_GUI=qt remains an explicit
# fallback until Mac acceptance; update-installed.py keeps Qt as the rollback copy
# when switching to the client (docs/macos-distribution.md#tauri-client-switch).
GUI="${HGS_MAC_GUI:-client}"
case "${GUI}" in qt|client) ;; *) echo "HGS_MAC_GUI must be qt or client" >&2; exit 1 ;; esac
DESKTOP_ARGS=()
if [ "${GUI}" = "client" ]; then
    # Capture the root build before the GUI changes CARGO_TARGET_DIR. The SSH
    # wrapper supplies its retained native build explicitly; never sign that file.
    LOCAL_CLI_BUILD="${HGS_MAC_CLI_BUILD:-${CARGO_TARGET_DIR:-${SCRIPT_DIR}/../target}/release/hgs}"
    python3 "${SCRIPT_DIR}/prepare-local-macos.py" --check-cli \
        --hgs "${LOCAL_CLI_BUILD}" --runner-hgs "${HGS_BIN}"
fi

# --- 1. build -----------------------------------------------------------------
echo "[1/4] Building..."
if [ "${GUI}" = "client" ]; then
    CLIENT_DIR="$(cd "${SCRIPT_DIR}/../client" && pwd)"
    NPM="$(find_tool npm)" || { echo "hgs-tray: npm not found (brew install node)" >&2; exit 1; }
    export PATH="${HOME}/.cargo/bin:$(dirname "${NPM}"):${PATH}"
    export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-1.99.1}"
    # The bundle is taken from client/src-tauri/target below, whatever the caller's
    # CARGO_TARGET_DIR; one JOBS bounds this build and the Qt one alike.
    export CARGO_TARGET_DIR="${CLIENT_DIR}/src-tauri/target"
    export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-${JOBS}}"
    (cd "${CLIENT_DIR}" && "${NPM}" ci --no-audit --no-fund)
    SWITCH_CONFIG="${CLIENT_DIR}/src-tauri/target/switch.conf.json"
    python3 "${CLIENT_DIR}/scripts/switch-config.py" "${SWITCH_CONFIG}"
    # Voice and round video ship together when the client has the video feature.
    FEATURES=()
    if grep -q '^video = ' "${CLIENT_DIR}/src-tauri/Cargo.toml" 2>/dev/null; then
        FEATURES=(--features video)
    elif grep -q '^media = ' "${CLIENT_DIR}/src-tauri/Cargo.toml" 2>/dev/null; then
        FEATURES=(--features media)
    fi
    # ZERUS_DEV_BUILD_NUMBER reaches the client's build.rs through the environment.
    (cd "${CLIENT_DIR}" && "${NPM}" exec -- tauri build --config "${SWITCH_CONFIG}" --bundles app ${FEATURES[@]+"${FEATURES[@]}"})
    BUILD_APP="${CLIENT_DIR}/src-tauri/target/release/bundle/macos/Zerus.app"
else
    CMAKE="$(find_tool cmake)" || { echo "hgs-tray: cmake not found (brew install cmake)" >&2; exit 1; }
    BREW="$(find_tool brew)"   || { echo "hgs-tray: brew not found -- Qt6 comes from it" >&2; exit 1; }
    # An Xcode or Command Line Tools update removes the SDK a cached configuration
    # names, and Qt's imported targets then fail to configure. Start from a clean
    # cache when any cached SDK is gone; sources and build outputs stay.
    stale_sdk() {
        local sdk
        [ -f "$1" ] || return 1
        while IFS= read -r sdk; do
            [ -d "${sdk}" ] || return 0
        done < <(grep -o '/[^][;:"=]*\.sdk' "$1" | sort -u)
        return 1
    }
    if stale_sdk "${SCRIPT_DIR}/build/CMakeCache.txt"; then
        echo "      Cached macOS SDK is gone (Xcode or Command Line Tools updated); reconfiguring."
        rm -rf "${SCRIPT_DIR}/build/CMakeCache.txt" "${SCRIPT_DIR}/build/CMakeFiles"
    fi
    "${CMAKE}" -B "${SCRIPT_DIR}/build" -S "${SCRIPT_DIR}" \
        -DCMAKE_PREFIX_PATH="$("${BREW}" --prefix qt)" -DCMAKE_BUILD_TYPE=Release
    "${CMAKE}" --build "${SCRIPT_DIR}/build" -j"${JOBS}"
fi

# --- 2. .app bundle -------------------------------------------------------------
echo "[2/4] Installing the .app bundle to ~/Applications..."
# Keep the uninstalled build's separate privacy identity. Only this disposable
# copy receives the public identity and compatible certificate before replacement.
LOCAL_STAGE="$(mktemp -d "${TMPDIR:-/tmp}/zerus-source-install.XXXXXX")"
trap 'rm -rf "${LOCAL_STAGE}"' EXIT
if [ "${GUI}" = "client" ]; then
    python3 "${SCRIPT_DIR}/prepare-local-macos.py" --app "${BUILD_APP}" \
        --hgs "${LOCAL_CLI_BUILD}" --runner-hgs "${HGS_BIN}" \
        --output "${LOCAL_STAGE}/hgs-tray.app" --installed "${APP_DEST}"
    # The parent remains private; the installed bundle has its normal app mode.
    chmod 755 "${LOCAL_STAGE}/hgs-tray.app"
    python3 "${SCRIPT_DIR}/prepare-local-macos.py" --check-cli \
        --hgs "${LOCAL_CLI_BUILD}" --runner-hgs "${HGS_BIN}"
    if [ "$(bundle_cdhash "${LOCAL_STAGE}/hgs-tray.app/Contents/MacOS/hgs")" != \
         "$(bundle_cdhash "${APP_DEST}/Contents/MacOS/hgs")" ]; then
        DESKTOP_ARGS+=(--restart)
    fi
    DESKTOP_ARGS+=(--runner-hgs "${HGS_BIN}")
    DESKTOP_HGS="${APP_DEST}/Contents/MacOS/hgs"
else
    ditto "${BUILD_APP}" "${LOCAL_STAGE}/hgs-tray.app"
    python3 "${SCRIPT_DIR}/sign-local-macos.py" "${LOCAL_STAGE}/hgs-tray.app" "${APP_DEST}"
    DESKTOP_HGS="${HGS_BIN}"
fi
BUILD_APP="${LOCAL_STAGE}/hgs-tray.app"
# The numbered Dev guard before anything is replaced (docs/reference.md); without
# the live update, also no switch between Qt and the client.
if [ "${RESTART_RUNNING}" = "1" ]; then
    python3 "${SCRIPT_DIR}/update-installed.py" --check "${BUILD_APP}" "${APP_DEST}" --live
else
    python3 "${SCRIPT_DIR}/update-installed.py" --check "${BUILD_APP}" "${APP_DEST}"
fi
mkdir -p "${HOME}/Applications"
if [ "$(bundle_cdhash "${BUILD_APP}")" != "$(bundle_cdhash "${APP_DEST}")" ]; then
    if [ "${RESTART_RUNNING}" = "1" ]; then
        # Replaced in step 4, once the LaunchAgent below is current.
        UPDATE=1
    else
        # rm перед cp: cp -R поверх существующего бандла смешал бы старые файлы с новыми.
        rm -rf "${APP_DEST}"
        cp -R "${BUILD_APP}" "${APP_DEST}"
        # Refresh this bundle's registration after replacing its icon/resources.
        # Do not reset LaunchServices globally or restart the user's Dock.
        LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
        if [ -x "${LSREGISTER}" ]; then "${LSREGISTER}" -f "${APP_DEST}"; fi
        RELOAD=1
    fi
    CHANGED=1
fi

# --- 3. LaunchAgent -------------------------------------------------------------
echo "[3/4] Installing the LaunchAgent to ~/Library/LaunchAgents..."
# launchd не разворачивает в plist ни ~, ни переменные окружения, поэтому путь к бандлу и
# путь к hgs подставляются здесь -- тем же приёмом, каким linux-ветка ниже правит Exec= в
# .desktop. ~/.local/bin в PATH у launchd нет вовсе, так что --hgs обязателен: без него
# трей не находит hgs и молча показывает пустой флот.
# stderr агента (StandardErrorPath в plist): туда же, куда macOS складывает логи
# пользовательских программ. Без файла launchd молча выбрасывает stderr, и зависание
# главного потока (см. MainThreadWatchdog.h) не оставляет ни строчки.
LOG_FILE="${HOME}/Library/Logs/hgs-tray.log"
mkdir -p "${HOME}/Library/LaunchAgents" "${HOME}/Library/Logs"
NEW_PLIST="$(sed -e "s|__APP__|${APP_DEST}|g" -e "s|__HGS__|${HGS_BIN}|g" \
                -e "s|__LOG__|${LOG_FILE}|g" "${PLIST_SRC}")"
if [ "$(cat "${PLIST_DEST}" 2>/dev/null || true)" != "${NEW_PLIST}" ]; then
    printf '%s\n' "${NEW_PLIST}" > "${PLIST_DEST}"
    RELOAD=1
    CHANGED=1
fi

# --- 4. load --------------------------------------------------------------------
echo "[4/4] Loading ${LABEL}..."
if [ "${UPDATE}" = "1" ]; then
    # Quits a running GUI, swaps the bundle and bootstraps the current LaunchAgent.
    python3 "${SCRIPT_DIR}/update-installed.py" "${BUILD_APP}" "${APP_DEST}"
    RELOAD=0
fi
# The selected signed service image is in the INSTALLED pair, never the staging
# copy. Only a real backend change reloads this owned service, not native agents.
python3 "${SCRIPT_DIR}/../scripts/install-macos-desktop-service.py" "${DESKTOP_HGS}" ${DESKTOP_ARGS[@]+"${DESKTOP_ARGS[@]}"}
# launchctl list печатает "PID<TAB>Status<TAB>Label"; PID "-" значит, что агент
# зарегистрирован, но не работает (Status -- код его последнего выхода).
AGENT_PID="$(launchctl list 2>/dev/null | awk -v l="${LABEL}" '$3 == l { print $1 }')"
if { [ "${RELOAD}" = "1" ] && [ "${RESTART_RUNNING}" = "1" ]; } || [ -z "${AGENT_PID}" ] || [ "${AGENT_PID}" = "-" ]; then
    # У launchctl нет "перезагрузить", а load поверх уже загруженного лейбла -- ошибка,
    # отсюда пара unload+load. На первом прогоне выгружать нечего, поэтому || true.
    launchctl unload "${PLIST_DEST}" 2>/dev/null || true
    launchctl load "${PLIST_DEST}"
    CHANGED=1
fi
# Живой агент, у которого ничего не поменялось, не трогается вовсе -- ровно как
# `systemctl --user enable --now` на активном юните в linux-ветке.

if [ "${GUI}" = "client" ]; then
    HINT_SELFTEST="Check without the menu bar:  ${APP_DEST}/Contents/MacOS/hgs-tray --build-info"
else
    HINT_SELFTEST="Check without the menu bar:  ${APP_DEST}/Contents/MacOS/hgs-tray --selftest --hgs ${HGS_BIN}"
fi
HINT_STATUS="Agent status:                launchctl list | grep ${LABEL}"
HINT_LOG="Agent stderr:                ${LOG_FILE}"

else

# =============================== Linux =======================================
PREFIX="${HGS_INSTALL_PREFIX:-${HOME}/.local}"
BIN_DEST="${PREFIX}/bin/hgs-tray"
BUILD_BIN="${SCRIPT_DIR}/build/hgs-tray"
SCRATCH=0
if [ -n "${HGS_INSTALL_PREFIX:-}" ]; then SCRATCH=1; fi
LIVE="${RESTART_RUNNING}"
if [ "${SCRATCH}" = "1" ]; then LIVE=0; fi
# Fresh installs and upgrades use Zerus Next. Qt remains an explicit fallback
# until Linux acceptance: HGS_LINUX_GUI=qt ./tray/install.sh.
GUI="${HGS_LINUX_GUI:-client}"
case "${GUI}" in qt|client) ;; *) echo "HGS_LINUX_GUI must be qt or client" >&2; exit 1 ;; esac
if [ "${GUI}" = "client" ] && [ "${LIVE}" = "1" ]; then
    if [ -x "${PREFIX}/bin/hgs" ]; then
        HGS_BIN="${PREFIX}/bin/hgs"
    elif command -v hgs >/dev/null 2>&1; then
        HGS_BIN="$(command -v hgs)"
    else
        echo "hgs-tray: install the hgs CLI first (./install.sh)." >&2
        exit 1
    fi
    # Refuse an unsupported CLI or custom service before replacing the GUI.
    python3 "${SCRIPT_DIR}/../scripts/install-linux-desktop-service.py" --hgs "${HGS_BIN}" --check
fi

# --- 1. build -----------------------------------------------------------------
echo "[1/4] Building..."
if [ "${GUI}" = "client" ]; then
    CLIENT_DIR="$(cd "${SCRIPT_DIR}/../client" && pwd)"
    NPM="$(command -v npm)" || { echo "hgs-tray: npm not found" >&2; exit 1; }
    export PATH="${HOME}/.cargo/bin:$(dirname "${NPM}"):${PATH}"
    export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-1.99.1}"
    export CARGO_TARGET_DIR="${CLIENT_DIR}/src-tauri/target"
    export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-${JOBS}}"
    (cd "${CLIENT_DIR}" && "${NPM}" ci --no-audit --no-fund)
    FEATURES=()
    if grep -q '^video = ' "${CLIENT_DIR}/src-tauri/Cargo.toml" 2>/dev/null; then
        if command -v pkg-config >/dev/null 2>&1 && pkg-config --exists gstreamer-1.0 gstreamer-app-1.0; then
            FEATURES=(--features video)
        else
            echo "hgs-tray: GStreamer development files not found; building with media only, without round video."
            FEATURES=(--features media)
        fi
    elif grep -q '^media = ' "${CLIENT_DIR}/src-tauri/Cargo.toml" 2>/dev/null; then
        FEATURES=(--features media)
    fi
    # ZERUS_DEV_BUILD_NUMBER reaches the client's build.rs through the environment.
    (cd "${CLIENT_DIR}" && "${NPM}" exec -- tauri build --config "${CLIENT_DIR}/src-tauri/tauri.switch.linux.conf.json" --no-bundle ${FEATURES[@]+"${FEATURES[@]}"})
    BUILD_BIN="${CARGO_TARGET_DIR}/release/hgs-tray"
else
    cmake -B "${SCRIPT_DIR}/build" -S "${SCRIPT_DIR}" -DCMAKE_BUILD_TYPE=Release
    cmake --build "${SCRIPT_DIR}/build" -j"${JOBS}"
fi

# --- 2. binary ------------------------------------------------------------------
echo "[2/4] Installing binary to ${PREFIX}/bin..."
CHECK=(--check "${BUILD_BIN}" "${BIN_DEST}")
if [ "${LIVE}" = "1" ]; then CHECK+=(--live); fi
# The numbered Dev guard before anything is replaced (docs/reference.md).
python3 "${SCRIPT_DIR}/update-installed.py" "${CHECK[@]}"
# An explicit local selection persists independently of the binary, so later
# previews and the updater's service restarts keep it. Packages use the stock unit.
LAUNCHER_CHANGE="$(python3 "${SCRIPT_DIR}/linux-launcher.py" --install "${PREFIX}")"
if [ -n "${LAUNCHER_CHANGE}" ]; then CHANGED=1; fi
LAUNCH_BIN="${BIN_DEST}"
if [ -f "${PREFIX}/share/hgs/local-webkit.json" ]; then
    LAUNCH_BIN="${PREFIX}/libexec/hgs-tray-launch"
fi
RELOAD=0
mkdir -p "${PREFIX}/bin"
if ! cmp -s "${BUILD_BIN}" "${BIN_DEST}" 2>/dev/null; then
    if [ "${LIVE}" = "1" ]; then
        # Replaced in step 4, once the unit below is current.
        UPDATE=1
    else
        install -Dm755 "${BUILD_BIN}" "${BIN_DEST}.new"
        mv -f "${BIN_DEST}.new" "${BIN_DEST}"
        RELOAD=1
    fi
    CHANGED=1
fi

# The optional embedded DeepSeek native UI belongs to Qt. Client installs keep
# it for the one-command Qt rollback; Qt installs maintain it as before.
PLUGIN_BUILD="${SCRIPT_DIR}/build/plugins/libhgs-tray-native-ui.so"
PLUGIN_DEST="${PREFIX}/lib/hgs-tray/plugins/libhgs-tray-native-ui.so"
if [ "${GUI}" = "qt" ]; then
    if [ -f "${PLUGIN_BUILD}" ]; then
        if ! cmp -s "${PLUGIN_BUILD}" "${PLUGIN_DEST}" 2>/dev/null; then
            install -Dm755 "${PLUGIN_BUILD}" "${PLUGIN_DEST}.new"
            mv -f "${PLUGIN_DEST}.new" "${PLUGIN_DEST}"
            CHANGED=1
        fi
    elif [ -e "${PLUGIN_DEST}" ]; then
        rm -f "${PLUGIN_DEST}"
        CHANGED=1
    fi
fi

HELPER_SOURCE="${SCRIPT_DIR}/linux-source-update.sh"
HELPER_DEST="${HOME}/.local/lib/hgs-tray/linux-source-update.sh"
if ! cmp -s "${HELPER_SOURCE}" "${HELPER_DEST}" 2>/dev/null; then
    install -Dm755 "${HELPER_SOURCE}" "${HELPER_DEST}.new"
    mv -f "${HELPER_DEST}.new" "${HELPER_DEST}"
    CHANGED=1
fi

# --- 3. systemd --user unit + autostart .desktop -------------------------------
echo "[3/4] Installing the systemd --user unit and the .desktop launcher..."
ICON_ROOT="${PREFIX}/share/icons/hicolor"
ICON_CHANGED=0
DESKTOP_CHANGED=0
for size in 16 24 32 48 64 128 256 512 1024; do
    ICON_SOURCE="${SCRIPT_DIR}/resources/icons/hgs-zerus-${size}.png"
    ICON_DEST="${ICON_ROOT}/${size}x${size}/apps/hgs-zerus.png"
    if ! cmp -s "${ICON_SOURCE}" "${ICON_DEST}" 2>/dev/null; then
        install -Dm644 "${ICON_SOURCE}" "${ICON_DEST}"
        ICON_CHANGED=1
        CHANGED=1
    fi
done
SYMBOLIC_SOURCE="${SCRIPT_DIR}/resources/icons/hgs-zerus-symbolic.svg"
# A distinct name for the selected mark also invalidates Plasma's cached task
# icon; changing SVG bytes at the previous Z path does not refresh every panel.
SYMBOLIC_DEST="${ICON_ROOT}/scalable/apps/hgs-zerus-swarm-symbolic.svg"
if ! cmp -s "${SYMBOLIC_SOURCE}" "${SYMBOLIC_DEST}" 2>/dev/null; then
    install -Dm644 "${SYMBOLIC_SOURCE}" "${SYMBOLIC_DEST}"
    ICON_CHANGED=1
    CHANGED=1
fi
if [ "${ICON_CHANGED}" = "1" ] && command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "${ICON_ROOT}" >/dev/null 2>&1 || true
fi
if [ "${SCRATCH}" = "1" ]; then
    UNIT_DEST="${PREFIX}/lib/systemd/user/hgs-tray.service"
else
    UNIT_DEST="${HOME}/.config/systemd/user/hgs-tray.service"
fi
mkdir -p "$(dirname "${UNIT_DEST}")"
# The default unit stays byte-identical to the packaged %h form. Scratch and
# opted-in local launches use a literal path, quoted and escaped for systemd.
if [ "${SCRATCH}" = "1" ] || [ "${LAUNCH_BIN}" != "${BIN_DEST}" ]; then
    SYSTEMD_BIN_DEST="${LAUNCH_BIN//\\/\\\\}"
    SYSTEMD_BIN_DEST="${SYSTEMD_BIN_DEST//\"/\\\"}"
    SYSTEMD_BIN_DEST="${SYSTEMD_BIN_DEST//%/%%}"
    NEW_UNIT=""
    while IFS= read -r line || [ -n "${line}" ]; do
        case "${line}" in
            ExecStart=*) line="ExecStart=\"${SYSTEMD_BIN_DEST}\"" ;;
        esac
        NEW_UNIT+="${line}"$'\n'
    done < "${SCRIPT_DIR}/packaging/hgs-tray.service"
    NEW_UNIT="${NEW_UNIT%$'\n'}"
else
    NEW_UNIT="$(cat "${SCRIPT_DIR}/packaging/hgs-tray.service")"
fi
if [ "$(cat "${UNIT_DEST}" 2>/dev/null || true)" != "${NEW_UNIT}" ]; then
    printf '%s\n' "${NEW_UNIT}" > "${UNIT_DEST}"
    CHANGED=1
fi

DESKTOP_DEST="${PREFIX}/share/applications/hgs-tray.desktop"
mkdir -p "${DESKTOP_DEST%/*}"
# The launcher opens Sessions through the running instance; login start is only
# through the service above, never another copy of this desktop file.
NEW_DESKTOP=""
DESKTOP_EXEC="${BIN_DEST} --sessions"
if [ "${LAUNCH_BIN}" != "${BIN_DEST}" ]; then
    # Desktop Entry values unescape once before Exec's quoted-argument parser.
    DESKTOP_EXEC="$(python3 - "${LAUNCH_BIN}" <<'PY'
import sys
value = sys.argv[1].replace('\\', r'\\\\').replace('"', r'\\"').replace('`', r'\\`').replace('$', r'\\$').replace('%', '%%')
print('"' + value + '" --sessions')
PY
)"
fi
while IFS= read -r line || [ -n "${line}" ]; do
    case "${line}" in
        Exec=*) line="Exec=${DESKTOP_EXEC}" ;;
        Icon=*) line="Icon=${SYMBOLIC_DEST}" ;;
    esac
    NEW_DESKTOP+="${line}"$'\n'
done < "${SCRIPT_DIR}/packaging/hgs-tray.desktop"
NEW_DESKTOP="${NEW_DESKTOP%$'\n'}"
if [ "$(cat "${DESKTOP_DEST}" 2>/dev/null || true)" != "${NEW_DESKTOP}" ]; then
    printf '%s\n' "${NEW_DESKTOP}" > "${DESKTOP_DEST}"
    DESKTOP_CHANGED=1
    CHANGED=1
fi

# Plasma keeps its own application cache in addition to the GTK icon cache.
# Refresh it after changing either the launcher or its icon assets.
if { [ "${ICON_CHANGED}" = "1" ] || [ "${DESKTOP_CHANGED}" = "1" ]; } && command -v kbuildsycoca6 >/dev/null 2>&1; then
    kbuildsycoca6 --noincremental >/dev/null 2>&1 || true
fi

# --- 4. enable + start ----------------------------------------------------------
if [ "${SCRATCH}" = "1" ]; then
    echo "[4/4] Scratch prefix ${PREFIX}: no service-manager action."
else
    if [ "${GUI}" = "client" ] && [ "${LIVE}" = "1" ]; then
        SERVICE_HELPER="${PREFIX}/lib/zerus/install-linux-desktop-service.py"
        for SERVICE_SOURCE in desktop-revoke-hold.py install-linux-desktop-service.py; do
            SERVICE_DEST="${PREFIX}/lib/zerus/${SERVICE_SOURCE}"
            if ! cmp -s "${SCRIPT_DIR}/../scripts/${SERVICE_SOURCE}" "${SERVICE_DEST}"; then
                install -Dm755 "${SCRIPT_DIR}/../scripts/${SERVICE_SOURCE}" "${SERVICE_DEST}.new"
                mv -f "${SERVICE_DEST}.new" "${SERVICE_DEST}"
                CHANGED=1
            fi
        done
        # The helper imports this login's environment before service creation.
        # Unchanged backend code stays alive across a UI-only update.
        DESKTOP_STATUS=0
        python3 "${SERVICE_HELPER}" --hgs "${HGS_BIN}" || DESKTOP_STATUS=$?
        case "${DESKTOP_STATUS}" in
            0) ;;
            3) echo "Desktop backend refresh pending. Continuing the GUI update; refresh the backend from the current graphical session." ;;
            *) exit "${DESKTOP_STATUS}" ;;
        esac
    fi
    echo "[4/4] Reloading and enabling hgs-tray.service..."
    systemctl --user daemon-reload
    if [ "$(systemctl --user is-enabled hgs-tray.service 2>/dev/null || true)" != "enabled" ]; then
        CHANGED=1
    fi
    if [ "$(systemctl --user is-active hgs-tray.service 2>/dev/null || true)" != "active" ]; then
        CHANGED=1
    fi
    if [ "${UPDATE}" = "1" ]; then
        # Quits a running GUI, swaps the binary and starts the unit again.
        python3 "${SCRIPT_DIR}/update-installed.py" "${BUILD_BIN}" "${BIN_DEST}"
    fi
    # enable --now: start -- не restart, поэтому уже активный трей второй раз не дёргает.
    systemctl --user enable --now hgs-tray.service
fi

if [ "${GUI}" = "client" ]; then
    HINT_SELFTEST="Check without a display:  ${BIN_DEST} --build-info"
else
    HINT_SELFTEST="Check without a display:  ${BIN_DEST} --selftest"
fi
if [ "${SCRATCH}" = "1" ]; then
    HINT_STATUS="Unit file (not loaded):     ${UNIT_DEST}"
else
    HINT_STATUS="Unit status:              systemctl --user status hgs-tray.service"
fi

fi

if [ "${RELOAD}" = "1" ] && [ "${RESTART_RUNNING}" = "0" ]; then
    echo "Updated on disk. The running Zerus keeps its drafts; the new UI opens on next launch."
fi

echo
if [ "${CHANGED}" = "1" ]; then
    echo "Installed hgs-tray."
else
    echo "hgs-tray is already installed and unchanged."
fi
echo
echo "${HINT_SELFTEST}"
echo "${HINT_STATUS}"
# Только у маковской ветки: на Linux stderr юнита и так в журнале.
if [ -n "${HINT_LOG:-}" ]; then echo "${HINT_LOG}"; fi
