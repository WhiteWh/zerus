#!/usr/bin/env bash
# Install/update Zerus on a Mac using an existing SSH connection.
#   ./mac-install.sh <ssh-alias> [--no-tray]
# --no-tray installs the CLI only. Otherwise the GUI is built and installed too.
# Optional reverse peer: HGS_MAC_PEER (existing SSH alias), or also set
# HGS_MAC_PEER_HOST to add a new SSH entry. HGS_MAC_PEER_USER, HGS_MAC_PEER_KEY
# and HGS_MAC_PEER_COLOR are optional. No machine-specific defaults are supplied.
# Fresh installs and upgrades use Zerus Next; HGS_MAC_GUI=qt selects the Qt fallback.
set -euo pipefail
host=""; with_tray=1
for a in "$@"; do
  case "$a" in
    --no-tray)  with_tray=0 ;;
    # Шапка файла и есть справка; печатаем её целиком до `set -`, а не «строки 2-8»:
    # фиксированный диапазон разъезжается с текстом при первой же правке шапки.
    -h|--help)  sed -n '2,/^set -/{/^#/p;}' "$0"; exit 0 ;;
    -*)         echo "mac-install: unknown option $a" >&2; exit 1 ;;
    *)          host="$a" ;;
  esac
done
[ -n "$host" ] || { echo "Usage: ./mac-install.sh <ssh-alias> [--no-tray]" >&2; exit 2; }
here="$(cd "$(dirname "$0")" && pwd)"

# Reverse connectivity is opt-in; SSH credentials remain local configuration.
peer="${HGS_MAC_PEER:-}"
peer_host="${HGS_MAC_PEER_HOST:-}"
peer_user="${HGS_MAC_PEER_USER:-}"
peer_key="${HGS_MAC_PEER_KEY:-}"
peer_color="${HGS_MAC_PEER_COLOR:-#7aa2f7}"
if [ -z "$peer" ] && [ -n "$peer_host$peer_user$peer_key" ]; then
  echo "mac-install: set HGS_MAC_PEER when supplying peer connection settings" >&2
  exit 2
fi
if [ -z "$peer_host" ] && [ -n "$peer_user$peer_key" ]; then
  echo "mac-install: set HGS_MAC_PEER_HOST when supplying a peer user or key" >&2
  exit 2
fi
# These values are written as single SSH/config tokens. Reject injected lines.
for value in "$host" "$peer" "$peer_host" "$peer_user" "$peer_key" "$peer_color"; do
  case "$value" in
    *[[:space:]]*|*\"*|*\'*|*\`*|*\$*) echo "mac-install: connection settings must be single literal tokens" >&2; exit 2 ;;
  esac
done

# Значения едут на мак позиционными аргументами удалённого bash, поэтому каждое
# закавычиваем: в '#7aa2f7' решётка иначе начала бы комментарий в удалённом шелле.
shq() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"; }

ssh -o BatchMode=yes "$host" 'mkdir -p ~/.local/bin ~/.local/src/hgs/.rust-build ~/.config/tmux ~/.config/hgs'
# Build natively on the Mac. Never copy a Linux target/ or CMake cache across hosts.
# Replace these entries rather than unpacking over them: a file deleted or moved
# here (src/platform.rs became src/platform/mod.rs) would otherwise stay on the Mac
# and break its build. target/ is kept for incremental builds.
# The mobile connector, the relay and crypto libraries it links, the replica crate and
# the desktop crates hgs takes by path are named file by file, so no Linux target/ of
# theirs travels either.
sources=(Cargo.toml Cargo.lock build.rs .cargo src scripts install.sh hgs hgs_state.py tests README.md LICENSE THIRD_PARTY_NOTICES.md docs mac-install.sh tmux.conf .gitignore
  services/mobile/connector/Cargo.toml services/mobile/connector/Cargo.lock services/mobile/connector/.cargo
  services/mobile/connector/src services/mobile/relay/Cargo.toml services/mobile/relay/src
  services/mobile/crypto/Cargo.toml services/mobile/crypto/src
  crates/replica/Cargo.toml crates/replica/Cargo.lock crates/replica/.cargo crates/replica/src
  crates/watch-protocol/Cargo.toml crates/watch-protocol/Cargo.lock crates/watch-protocol/.cargo crates/watch-protocol/src
  client/core/Cargo.toml client/core/src
  crates/content-search/Cargo.toml crates/content-search/src
  crates/desktop-catalog/Cargo.toml crates/desktop-catalog/src
  crates/desktop-ledger/Cargo.toml crates/desktop-ledger/src
  crates/desktop-migration/Cargo.toml crates/desktop-migration/src
  crates/desktop-read-policy/Cargo.toml crates/desktop-read-policy/src
  crates/freedesktop-notify/Cargo.toml crates/freedesktop-notify/src
  crates/presence-sampler/Cargo.toml crates/presence-sampler/src)
tar czf - -C "$here" "${sources[@]}" \
  | ssh -o BatchMode=yes "$host" "cd ~/.local/src/hgs/.rust-build && rm -rf ${sources[*]} && tar xzf -"
# install.sh builds the mobile connector only on request (HGS_MOBILE_CONNECTOR=1)
# or to update a copy already installed on the Mac.
ssh -o BatchMode=yes "$host" "HGS_MOBILE_CONNECTOR=$(shq "${HGS_MOBILE_CONNECTOR:-}") bash -s" <<'BUILD'
set -eu
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
if ! command -v cargo >/dev/null 2>&1; then
  HOMEBREW_NO_AUTO_UPDATE=1 /opt/homebrew/bin/brew install rust
fi
bash "$HOME/.local/src/hgs/.rust-build/install.sh"
# Old callbacks and saved launch paths still refer to this checkout. Only publish
# their entry points after a working native binary exists, using atomic renames.
cd "$HOME/.local/src/hgs/.rust-build"
copy="Cargo.toml Cargo.lock build.rs .cargo src crates client/core/Cargo.toml client/core/src scripts install.sh tests README.md LICENSE THIRD_PARTY_NOTICES.md docs mac-install.sh tmux.conf .gitignore"
(cd .. && rm -rf $copy)
tar cf - $copy | tar xf - -C ..
for entry in hgs hgs_state.py; do
  stage="$(mktemp "../.$entry.XXXXXXXX")"
  if [ "$entry" = hgs ]; then mode=0755; else mode=0644; fi
  install -m "$mode" "$entry" "$stage"
  mv -f "$stage" "../$entry"
done
BUILD
scp -o BatchMode=yes -q "$here/tmux.conf" "$host:~/.config/tmux/tmux.conf"
# A running tmux server keeps its old key table until told otherwise (the C-v binding lives here).
ssh -o BatchMode=yes "$host" '/opt/homebrew/bin/tmux source-file ~/.config/tmux/tmux.conf 2>/dev/null || true'
ssh -o BatchMode=yes "$host" "bash -s -- $(shq "$host") $(shq "$peer") $(shq "$peer_host") \
$(shq "$peer_user") $(shq "$peer_key") $(shq "$peer_color")" <<'REMOTE'
set -eu
self="$1"; peer="$2"; peer_host="$3"; peer_user="$4"; peer_key="$5"; peer_color="$6"
if ! command -v tmux >/dev/null 2>&1 && [ ! -x /opt/homebrew/bin/tmux ]; then
  HOMEBREW_NO_AUTO_UPDATE=1 /opt/homebrew/bin/brew install tmux
fi
if [ ! -f ~/.config/hgs/config ]; then
  # HGS_TAB_COLORS is Konsole-only (Tabby has no D-Bus); the tab *title* works everywhere.
  printf 'HGS_SELF="%s %s"\nHGS_PEERS="%s"\nHGS_TAB=1\nHGS_TAB_COLORS="%s"\n' \
    "$self" "$(hostname -s)" "$peer" "${peer:+$peer=$peer_color}" > ~/.config/hgs/config
fi
# A config from before hgs 1.2 has no tab settings: add the missing keys, touch nothing else.
grep -q '^HGS_TAB=' ~/.config/hgs/config || printf 'HGS_TAB=1\n' >> ~/.config/hgs/config
grep -q '^HGS_TAB_COLORS=' ~/.config/hgs/config \
  || printf 'HGS_TAB_COLORS="%s"\n' "${peer:+$peer=$peer_color}" >> ~/.config/hgs/config
if [ -n "$peer_host" ] && ! grep -q '^# BEGIN hgs' ~/.ssh/config 2>/dev/null; then
  mkdir -p ~/.ssh
  chmod 700 ~/.ssh
  printf '# BEGIN hgs\nHost %s\n  HostName %s\n' "$peer" "$peer_host" >> ~/.ssh/config
  if [ -n "$peer_user" ]; then printf '  User %s\n' "$peer_user" >> ~/.ssh/config; fi
  if [ -n "$peer_key" ]; then printf '  IdentityFile %s\n' "$peer_key" >> ~/.ssh/config; fi
  printf '  ConnectTimeout 5\n  ServerAliveInterval 15\n  ServerAliveCountMax 3\n# END hgs\n' \
    >> ~/.ssh/config
  chmod 600 ~/.ssh/config
fi
echo "hgs on $(hostname -s): $(~/.local/bin/hgs --version), tmux $(/opt/homebrew/bin/tmux -V 2>/dev/null || tmux -V)"
cat ~/.config/hgs/config
REMOTE

[ "$with_tray" -eq 1 ] || exit 0

# --- трей ---------------------------------------------------------------------------------
# Чекаута репо на маке нет, поэтому исходники трея едут туда тарболом. build/ и .cache/
# исключены НАМЕРЕННО: линуксовый CMakeCache.txt в тарболе отравляет маковскую сборку (в
# кэше зашиты пути и компилятор ЭТОЙ машины), .cache — мусор clangd.
# Каталог назначения постоянный (~/.local/src/hgs, как в README для линукса), а не /tmp:
# там переживает build/, поэтому повторный прогон собирается инкрементально, а install.sh
# по cdhash бандла видит, что ничего не изменилось, и не дёргает живой LaunchAgent.
# DEV_VERSION sits beside tray/: its CMake reads the Dev base version from there.
# The Tauri client's sources travel too, so tray/install.sh builds Next by default, with
# VERSION, crates/watch-protocol and the desktop read-policy and migration crates,
# which client/core and client/src-tauri take by path. Source folders are replaced;
# their build caches stay. A numbered Dev build needs a clean Git checkout on the
# Mac instead (docs/macos-distribution.md).
# Cargo resolves target-specific dependency manifests on the Mac too, including
# the Linux notification adapter.
tar czf - -C "$here" --exclude='tray/build' --exclude='tray/.cache' \
  --exclude='client/node_modules' --exclude='client/dist' --exclude='client/*/target' \
  --exclude='client/src-tauri/gen' \
  tray client crates/watch-protocol/Cargo.toml crates/watch-protocol/src \
  crates/desktop-read-policy/Cargo.toml crates/desktop-read-policy/src \
  crates/desktop-migration/Cargo.toml crates/desktop-migration/src \
  crates/freedesktop-notify/Cargo.toml crates/freedesktop-notify/src DEV_VERSION VERSION \
  | ssh -o BatchMode=yes "$host" 'mkdir -p ~/.local/src/hgs && cd ~/.local/src/hgs && rm -rf client/src client/core/src client/media/src client/src-tauri/src client/src-tauri/capabilities crates/watch-protocol/src crates/desktop-read-policy/src crates/desktop-migration/src crates/freedesktop-notify/src && tar xzf -'
# PATH неинтерактивного ssh на маке — без Homebrew; cmake/brew tray/install.sh ищет сам
# (find_tool в его маковской ветке). Signing uses the Mac's existing keychain;
# an optional exact identity selector travels as one literal environment value.
ssh -o BatchMode=yes "$host" "HGS_MAC_CLI_BUILD=~/.local/src/hgs/.rust-build/target/release/hgs HGS_MACOS_SIGN_IDENTITY=$(shq "${HGS_MACOS_SIGN_IDENTITY:-}") HGS_MAC_GUI=$(shq "${HGS_MAC_GUI:-client}") bash ~/.local/src/hgs/tray/install.sh"
