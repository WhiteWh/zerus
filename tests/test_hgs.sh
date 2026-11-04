#!/usr/bin/env bash
# Smoke tests for ../hgs.
# Run: HGS_TEST_BIN=/absolute/path/to/hgs bash tests/test_hgs.sh
# The test driver also supports Bash 3.2 on the Mac; hgs itself is native Rust.
#
# tmux isolation here is load-bearing AND fragile, so read this before adding a tmux call.
# hgs shells out to plain `tmux`, so the suite steers it with TMUX_TMPDIR rather than -L.
# The trap: a TMUX_TMPDIR pointing at a MISSING directory does not make tmux fail — it
# silently falls back to the real socket. A `kill-server` then takes out the user's own
# live sessions. That is not hypothetical; it happened. So every tmux call in this file
# goes through t(), which refuses to run unless the isolated socket dir is really there.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
HGS="${HGS_TEST_BIN:-$here/../hgs}"
HGS="$(cd "$(dirname "$HGS")" && pwd)/$(basename "$HGS")"
[ -x "$HGS" ] || { echo "FATAL: executable not found: $HGS" >&2; exit 99; }
TM="$(command -v tmux || echo /opt/homebrew/bin/tmux)"
tmp="$(mktemp -d)"
cleanup() {
  trap - EXIT
  # Kill our server BEFORE the directory goes away: afterwards the same command would
  # resolve to the real socket.
  [ -d "$tmp/tmux" ] && TMUX_TMPDIR="$tmp/tmux" "$TM" kill-server 2>/dev/null
  [ -d "$tmp/tmux2" ] && TMUX_TMPDIR="$tmp/tmux2" "$TM" kill-server 2>/dev/null
  rm -rf "$tmp"
}
trap cleanup EXIT
# Every tmux call in this file. Never call "$TM" or tmux directly.
t() {
  if [ -z "${TMUX_TMPDIR:-}" ] || [ ! -d "$TMUX_TMPDIR" ]; then
    echo "FATAL: TMUX_TMPDIR='${TMUX_TMPDIR:-}' missing — refusing to run tmux, it would hit the REAL socket" >&2
    exit 99
  fi
  "$TM" "$@"
}
# Stop the suite's isolated servers with stop_server, not a bare kill-server. tmux answers
# kill-server once the server has told its clients to leave, not once it is gone: until its
# process ends, the old server still listens on the socket and drops each new client ("server
# exited unexpectedly"). A new-session sent right after the kill then starts nothing -- under
# load often enough to leave a test waiting for a pane that never ran. So wait for the process.
stop_server() {   # $1 = t or t2, the guarded runner of the server to stop
  local pid state _n=0
  pid="$("$1" display-message -p '#{pid}' 2>/dev/null)"
  "$1" kill-server 2>/dev/null
  [ -n "$pid" ] || return 0
  while [ "$_n" -lt 100 ]; do
    state="$(ps -o stat= -p "$pid" 2>/dev/null | tr -d ' ')"
    case "$state" in ''|Z*) return 0 ;; esac   # gone, or a zombie that holds no socket
    sleep 0.1; _n=$((_n+1))
  done
  echo "note: tmux server $pid still running 10s after kill-server" >&2
}
mkdir -p "$tmp/cfg" "$tmp/proj/sample-project" "$tmp/proj/example.com" "$tmp/proj/withlauncher" "$tmp/tmux"
printf '# comment\nsample-project=%s/proj/sample-project\nexample.com=%s/proj/example.com\n' "$tmp" "$tmp" > "$tmp/cfg/projects"
echo "wl=$tmp/proj/withlauncher" >> "$tmp/cfg/projects"
cp -f "$tmp/cfg/projects" "$tmp/cfg/projects.local"
echo "ansibleonly=$tmp/proj/example.com" >> "$tmp/cfg/projects"
printf '#!/bin/sh\nexec claude "$@"\n' > "$tmp/proj/withlauncher/run_claude.sh"; chmod +x "$tmp/proj/withlauncher/run_claude.sh"
printf 'HGS_SELF="testhost arch"\nHGS_PEERS=""\n' > "$tmp/cfg/config"
export HGS_CONFIG_DIR="$tmp/cfg" TMUX_TMPDIR="$tmp/tmux" HGS_CLAUDE_PROJECTS="$tmp/claude-projects"
export HGS_STATE_DIR="$tmp/state" HGS_TRACKING=0
unset HGS_SESSION HGS_RUN_ID HGS_EXPECTED_ID
# fake Claude history for sample-project only (dir path encoded: non-alnum -> "-")
mkdir -p "$tmp/claude-projects/$(printf "%s" "$tmp/proj/sample-project" | sed "s/[^A-Za-z0-9]/-/g")"
touch "$tmp/claude-projects/$(printf "%s" "$tmp/proj/sample-project" | sed "s/[^A-Za-z0-9]/-/g")/s1.jsonl"
mkdir -p "$tmp/claude-projects/$(printf "%s" "$tmp/proj/withlauncher" | sed "s/[^A-Za-z0-9]/-/g")"
touch "$tmp/claude-projects/$(printf "%s" "$tmp/proj/withlauncher" | sed "s/[^A-Za-z0-9]/-/g")/s1.jsonl"
unset TMUX
# hgs hands a new session the box's DISPLAY/WAYLAND_DISPLAY/XAUTHORITY only when they are
# MISSING here (a session created over ssh has none). Every other test in this file expects a
# bare new-session line, so give the suite a desktop-like env; the injection has its own tests.
export WAYLAND_DISPLAY="${WAYLAND_DISPLAY:-wayland-test}" DISPLAY="${DISPLAY:-:9}" \
       XAUTHORITY="${XAUTHORITY:-$tmp/xauth-test}"
# The exact transport pair is part of every Claude/Codex launch assertion.
export HGS_AGENT_HINT=1 HGS_AGENT_HINT_OFF=
hint_env="-e HGS_AGENT_HINT=1 -e HGS_AGENT_HINT_OFF="
pass=0; fail=0

# check <name> <want-rc> <grep -E pattern> [args...]  — runs hgs with args
check() {
  local name="$1" want_rc="$2" pat="$3"; shift 3
  local out rc
  out="$("$HGS" "$@" 2>&1)"; rc=$?
  if [ "$rc" = "$want_rc" ] && printf '%s\n' "$out" | grep -qE -- "$pat"; then
    pass=$((pass+1)); echo "ok   $name"
  else
    fail=$((fail+1)); echo "FAIL $name (rc=$rc want $want_rc; pattern: $pat)"; printf '%s\n' "$out" | sed 's/^/     /'
  fi
}
# check_not <name> <grep -E pattern> [args...] — pattern must be ABSENT (rc must be 0)
check_not() {
  local name="$1" pat="$2"; shift 2
  local out rc
  out="$("$HGS" "$@" 2>&1)"; rc=$?
  if [ "$rc" = 0 ] && ! printf '%s\n' "$out" | grep -qE -- "$pat"; then
    pass=$((pass+1)); echo "ok   $name"
  else
    fail=$((fail+1)); echo "FAIL $name (rc=$rc; unwanted pattern: $pat)"; printf '%s\n' "$out" | sed 's/^/     /'
  fi
}

P="$tmp/proj/sample-project"
# --- naming / project resolution ---
check "map name -> new-session in project dir" 0 "tmux -u new-session -e HGS_CLIENT=local $hint_env -s claude/sample-project -c $P " --dry-run claude sample-project
check "path project -> name from basename"     0 "new-session -e HGS_CLIENT=local $hint_env -s claude/sample-project -c $P " --dry-run claude "$P"
check "dot in project name -> underscore"       0 "new-session -e HGS_CLIENT=local $hint_env -s claude/example_com "       --dry-run claude example.com
check "tag -> /tag suffix"                      0 "new-session -e HGS_CLIENT=local $hint_env -s claude/sample-project/review " --dry-run claude sample-project -n review
check "unknown project -> rc 1 + known list"    1 "unknown project 'nope'.*sample-project" --dry-run claude nope
check "detached flag"                           0 "new-session -d -e HGS_CLIENT=local $hint_env -s claude/sample-project " --dry-run claude sample-project -d
# A project alias created as a symlink remains the user's chosen project name.
ln -s "$P" "$tmp/proj/site-alias"
check "symlink project keeps alias session name" 0 "new-session -e HGS_CLIENT=local $hint_env -s claude/site-alias " --dry-run claude "$tmp/proj/site-alias"
check "symlink project keeps logical launch directory" 0 " -c $tmp/proj/site-alias " --dry-run claude "$tmp/proj/site-alias"
# --- HGS_CLIENT: who is looking at the session (hgs paste reads it; README "Картинка по Ctrl+V") ---
check "client: local create -> -e HGS_CLIENT=local" 0 "^tmux -u new-session -e HGS_CLIENT=local $hint_env -s claude/sample-project " --dry-run claude sample-project
check "client: --client mac create -> -e HGS_CLIENT=mac" 0 "^tmux -u new-session -e HGS_CLIENT=mac $hint_env -s claude/sample-project " --client mac --dry-run claude sample-project
check "client: detached keeps -e"                   0 "^tmux -u new-session -d -e HGS_CLIENT=local $hint_env -s claude/sample-project " --dry-run claude sample-project -d
check "client: --client without name -> rc 1"       1 "needs a peer name" --client
check "client: a -> set-environment then attach"    0 "^tmux set-environment -t =claude/x HGS_CLIENT local\$" --dry-run a claude/x
check "client: --client mac a -> mac"               0 "^tmux set-environment -t =claude/x HGS_CLIENT mac\$" --client mac --dry-run a claude/x
check "client: a still prints attach"               0 "^tmux -u attach-session -t =claude/x\$" --dry-run a claude/x
out="$(cd "$P" && "$HGS" --dry-run claude 2>&1)"; rc=$?
if [ "$rc" = 0 ] && printf '%s\n' "$out" | grep -qE -- "new-session -e HGS_CLIENT=local $hint_env -s claude/sample-project -c $P "; then pass=$((pass+1)); echo "ok   cwd project"; else fail=$((fail+1)); echo "FAIL cwd project"; printf '%s\n' "$out"; fi

# --- launch command: $SHELL -lic 'exec env HGS_AGENT_HINT=1 HGS_AGENT_HINT_OFF= "$0" --run "$@"' <hgs> <n> <resume...> <base...> ---
# n=0 -> no resume variant: a new session starts a NEW conversation unless -c is given.
check "default -> new conversation"     0 " -lic 'exec env HGS_AGENT_HINT=1 HGS_AGENT_HINT_OFF= \"\\\$0\" --run \"\\\$@\"' [^ ]+/hgs 0 claude\$" --dry-run claude sample-project
check "claude -c -> resume variant"     0 "/hgs 2 claude -c claude\$"                --dry-run claude sample-project -c
check "--resume is an alias of -c"      0 "/hgs 2 claude -c claude\$"                --dry-run claude sample-project --resume
check "--fresh beats -c"                0 "/hgs 0 claude\$"                          --dry-run claude sample-project -c --fresh
check "-c, no claude history -> fresh"  0 "/hgs 0 claude\$"                          --dry-run claude example.com -c
check "tag -> new conversation"         0 "/hgs 0 claude\$"                          --dry-run claude sample-project -n review
check "tag + -c -> resume in tag"       0 "new-session -e HGS_CLIENT=local $hint_env -s claude/sample-project/review .*/hgs 2 claude -c claude\$" --dry-run claude sample-project -n review -c
check "launcher: run_claude.sh used"    0 "new-session -e HGS_CLIENT=local $hint_env -s claude/wl .*/hgs 0 [^ ]+/run_claude.sh\$" --dry-run claude wl
check "launcher: -c both variants"      0 "new-session -e HGS_CLIENT=local $hint_env -s claude/wl .*/hgs 2 [^ ]+/run_claude.sh -c [^ ]+/run_claude.sh\$" --dry-run claude wl -c
check "launcher: --bare -> plain agent" 0 "/hgs 0 claude\$"                          --dry-run claude wl --bare
check "launcher: only matching cmd"     0 "new-session -e HGS_CLIENT=local $hint_env -s codex/wl .*/hgs 0 codex\$" --dry-run codex wl
# Codex uses a subcommand for resume; both runner variants must use the project launcher.
printf '#!/bin/sh\nexec codex "$@"\n' > "$tmp/proj/withlauncher/run_codex.sh"
check "codex launcher: non-executable ignored" 0 "/hgs 0 codex\$" --dry-run codex wl
chmod +x "$tmp/proj/withlauncher/run_codex.sh"
check "codex launcher: fresh"          0 "new-session -e HGS_CLIENT=local $hint_env -s codex/wl .*/hgs 0 [^ ]+/run_codex.sh\$" --dry-run codex wl
check "codex launcher: resume both variants" 0 "/hgs 3 [^ ]+/run_codex.sh resume --last [^ ]+/run_codex.sh\$" --dry-run codex wl -c
check "codex launcher: bare resume"    0 "/hgs 3 codex resume --last codex\$" --dry-run codex wl -c --bare
check "codex launcher: fresh beats resume" 0 "/hgs 0 [^ ]+/run_codex.sh\$" --dry-run codex wl -c --fresh
check "codex launcher: explicit resume" 0 "/hgs 0 [^ ]+/run_codex.sh resume abc\$" --dry-run codex wl -c -- resume abc
check "codex launcher: config argument stays quoted" 0 "/hgs 0 [^ ]+/run_codex.sh -c .model=\"test model\".\$" --dry-run codex wl -- -c 'model="test model"'
check "claude explicit --resume <id>"   0 "/hgs 0 claude --resume abc\$"             --dry-run claude sample-project -- --resume abc
check "-c + own -r <id> -> no double"   0 "/hgs 0 claude -r abc\$"                   --dry-run claude sample-project -c -- -r abc
check "extra args without -c"           0 "/hgs 0 claude --model opus\$"             --dry-run claude sample-project -- --model opus
check "-c + extra args both variants"   0 "/hgs 4 claude -c --model opus claude --model opus\$" --dry-run claude sample-project -c -- --model opus
check "claude --rc"                     0 "/hgs 0 claude --remote-control claude/sample-project\$" --dry-run claude sample-project --rc
check "claude --rc -c"                  0 "/hgs 4 claude -c --remote-control claude/sample-project claude --remote-control claude/sample-project\$" --dry-run claude sample-project --rc -c
check "kimi default -> fresh"           0 "/hgs 0 kimi\$"                            --dry-run kimi sample-project
check "kimi -c"                         0 "/hgs 2 kimi -c kimi\$"                    --dry-run kimi sample-project -c
check "codex default -> fresh"          0 "/hgs 0 codex\$"                           --dry-run codex sample-project
check "codex -c"                        0 "/hgs 3 codex resume --last codex\$"       --dry-run codex sample-project -c
check "codex subcommand -> as is"       0 "/hgs 0 codex resume abc\$"                --dry-run codex sample-project -- resume abc
check "-c on a cmd without resume -> warn" 0 "no resume mode known for sh"           --dry-run sh sample-project -c
check "sh -> login shell, no runner"  0 "new-session -e HGS_CLIENT=local -s sh/sample-project -c $P -- [^ ]+ -l\$" --dry-run sh sample-project
check "arbitrary command"             0 "new-session -e HGS_CLIENT=local -s htop/sample-project .*/hgs 0 htop\$" --dry-run htop sample-project
# `session-host` manages the session host only in Windows builds; here it stays a program.
check "session-host stays a command on Unix" 0 "new-session -e HGS_CLIENT=local -s session-host/sample-project .*/hgs 0 session-host install\$" --dry-run session-host sample-project -- install
check_not "codex --rc ignored"        "remote-control" --dry-run codex sample-project --rc

# Exact conversation IDs use provider resume arguments and their own stable session
# name; they never fall back to a fresh conversation or attach the project default.
conversation_id="11111111-2222-4333-8444-555555555555"
check "exact codex -c UUID uses isolated name" 0 "-s codex/sample-project/resume-$conversation_id " --dry-run codex sample-project -c "$conversation_id"
check "exact codex -c UUID has no fresh fallback" 0 "/hgs 0 codex resume $conversation_id\$" --dry-run codex sample-project -c "$conversation_id"
check "exact --resume UUID alias" 0 "/hgs 0 codex resume $conversation_id\$" --dry-run codex sample-project --resume "$conversation_id"
check "exact --continue UUID alias" 0 "/hgs 0 codex resume $conversation_id\$" --dry-run codex sample-project --continue "$conversation_id"
check "exact --resume=UUID form" 0 "/hgs 0 codex resume $conversation_id\$" --dry-run codex sample-project "--resume=$conversation_id"
check "exact claude provider selector" 0 "/hgs 0 claude --resume $conversation_id\$" --dry-run claude sample-project -c "$conversation_id"
check "exact kimi provider selector" 0 "/hgs 0 kimi --session $conversation_id\$" --dry-run kimi sample-project -c "$conversation_id"
check "exact resume preserves project launcher" 0 "/hgs 0 [^ ]+/run_codex.sh resume $conversation_id\$" --dry-run codex wl -c "$conversation_id"
check "exact resume forwards extra provider options" 0 "/hgs 0 codex resume $conversation_id --model example\$" --dry-run codex sample-project -c "$conversation_id" -- --model example
check "exact resume keeps explicit session tag" 0 "-s codex/sample-project/review " --dry-run codex sample-project -c "$conversation_id" -n review
check "legacy -c followed by project stays project selection" 0 "/hgs 3 codex resume --last codex\$" --dry-run codex -c sample-project
check "exact resume rejects --fresh" 1 "." --dry-run codex sample-project -c "$conversation_id" --fresh
# WhiteWh deliberately allows a new tracked session for an exact conversation.
# Keep both the exact native selector and the requested new session name pinned.
check "new exact resume keeps its new session tag" 0 "-s codex/sample-project/exact-new " --dry-run codex sample-project -c "$conversation_id" --new -n exact-new
check "new exact resume has no fresh fallback" 0 "/hgs 0 codex resume $conversation_id\$" --dry-run codex sample-project -c "$conversation_id" --new -n exact-new
check "new ambiguous resume is refused" 1 "." --dry-run codex sample-project -c --new
check "new session rejects fresh" 1 "." --dry-run codex sample-project --fresh --new
check "explicit invalid resume ID is rejected" 1 "." --dry-run codex sample-project --resume=not-a-uuid
check "explicit empty resume ID is rejected" 1 "." --dry-run codex sample-project --resume=

# --- @host ---
check "@self -> local"                0 "^tmux -u new-session -e HGS_CLIENT=local $hint_env -s claude/sample-project " --dry-run @testhost claude sample-project
check "@self alias -> local"          0 "^tmux -u new-session -e HGS_CLIENT=local $hint_env -s claude/sample-project " --dry-run @arch claude sample-project
# the marker ssh touches on connecting is what tells a dropped link from a peer that never answered
check "@peer -> ssh -t + link marker"  0 "^ssh -t -o PermitLocalCommand=yes -o 'LocalCommand=touch [^']+/up' nowhere ~/.local/bin/hgs --client testhost claude sample-project --dry-run\$" --dry-run @nowhere claude sample-project
check "@peer -d -> ssh without -t"    0 "^ssh -o BatchMode=yes nowhere ~/.local/bin/hgs --client testhost claude sample-project -d --dry-run\$" --dry-run @nowhere claude sample-project -d
check "@peer ls -> ssh without -t"    0 "^ssh -o BatchMode=yes -o ConnectTimeout=3 nowhere ~/.local/bin/hgs ls --local --dry-run\$" @nowhere ls --dry-run
check "@peer directories -> noninteractive ssh" 0 "^ssh -o BatchMode=yes -o ConnectTimeout=3 nowhere ~/.local/bin/hgs dirs '/work/a b' --dry-run\$" --dry-run @nowhere dirs '/work/a b'
check "@peer projects -> noninteractive ssh" 0 "^ssh -o BatchMode=yes -o ConnectTimeout=3 nowhere ~/.local/bin/hgs project ls --json --dry-run\$" --dry-run @nowhere project ls --json
check "@peer new folder session -> quoted path and name" 0 "--client testhost codex '/work/a b' --new -n work-test --dry-run\$" --dry-run @nowhere codex '/work/a b' --new -n work-test
check "@peer archive -> noninteractive ssh" 0 "^ssh -o BatchMode=yes -o ConnectTimeout=3 nowhere ~/.local/bin/hgs archive claude/p/one --dry-run\$" --dry-run @nowhere archive claude/p/one
check "@peer rename -> noninteractive ssh" 0 "^ssh -o BatchMode=yes -o ConnectTimeout=3 nowhere ~/.local/bin/hgs rename claude/p/one claude/p/review --dry-run\$" --dry-run @nowhere rename claude/p/one claude/p/review
check "@peer archived rename forwards identity" 0 "^ssh -o BatchMode=yes -o ConnectTimeout=3 nowhere ~/.local/bin/hgs rename claude/p/one claude/p/review --archive $conversation_id --dry-run\$" --dry-run @nowhere rename claude/p/one claude/p/review --archive "$conversation_id"
check_not "@peer rename never decorates calling terminal" "^tab " --dry-run @nowhere rename claude/p/one claude/p/review
check "rename requires source and destination" 1 "." rename
check "rename requires destination" 1 "." rename claude/p/one
check "rename rejects extra positional arguments" 1 "." rename claude/p/one claude/p/review unexpected
check "rename requires archive identity after selector" 1 "." rename claude/p/one claude/p/review --archive
check "@peer history -> noninteractive ssh" 0 "^ssh -o BatchMode=yes -o ConnectTimeout=3 nowhere ~/.local/bin/hgs history claude/p/one --json --dry-run\$" --dry-run @nowhere history claude/p/one --json
check_not "@peer history never decorates calling terminal" "^tab " --dry-run @nowhere history claude/p/one --json
check "@peer archived inspect forwards identity and cursor" 0 "^ssh -o BatchMode=yes -o ConnectTimeout=3 nowhere ~/.local/bin/hgs inspect claude/p/one --archive 11111111-2222-4333-8444-555555555555 --after 42 --dry-run\$" --dry-run @nowhere inspect claude/p/one --archive 11111111-2222-4333-8444-555555555555 --after 42
check "@peer archived forget forwards archive identity" 0 "^ssh -o BatchMode=yes -o ConnectTimeout=3 nowhere ~/.local/bin/hgs kill claude/p/one --archive 11111111-2222-4333-8444-555555555555 --dry-run\$" --dry-run @nowhere kill claude/p/one --archive 11111111-2222-4333-8444-555555555555
check "@peer archived resume preserves detached mode" 0 "^ssh -o BatchMode=yes nowhere ~/.local/bin/hgs --client testhost resume claude/p/one --archive 11111111-2222-4333-8444-555555555555 -d --dry-run\$" --dry-run @nowhere resume claude/p/one --archive 11111111-2222-4333-8444-555555555555 -d
check "@peer exact UUID resume forwards native selector" 0 "^ssh -o BatchMode=yes nowhere ~/.local/bin/hgs --client testhost codex -c $conversation_id -d --dry-run\$" --dry-run @nowhere codex -c "$conversation_id" -d

# --- link drop: the peer's tmux switches the terminal over and switches it back on detach;
# when the ssh link dies (rc 255) that teardown is lost and hgs must replay it. The isolated
# tmux server stands in for the terminal emulator: its pane flags say which modes the
# "terminal" is left in, pipe-pane records the raw bytes hgs wrote to it.
if command -v tmux >/dev/null 2>&1 || [ -x /opt/homebrew/bin/tmux ]; then
  mkdir -p "$tmp/fakebin"
  # ssh whose peer got as far as attaching (LocalCommand fires, tmux's start sequence lands),
  # then the link died: exit 255 with no teardown.
  cat > "$tmp/fakebin/ssh" <<'EOF'
#!/bin/sh
for a; do case "$a" in LocalCommand=*) "${SHELL:-/bin/sh}" -c "${a#LocalCommand=}" ;; esac; done
printf '\033[?1049h\033[?1h\033[?1003h\033[?1006h\033[?2004h'
exit 255
EOF
  chmod +x "$tmp/fakebin/ssh"
  # runs hgs in the pane once "go" appears, so pipe-pane can attach first
  cat > "$tmp/pane.sh" <<EOF
#!/bin/sh
while [ ! -e "$tmp/go" ]; do sleep 0.1; done
PATH="$tmp/fakebin:\$PATH" HGS_TAB=0 "$HGS" @nowhere claude sample-project 2>"$tmp/pane.err"; echo \$? > "$tmp/pane.rc"
sleep 30
EOF
  run_pane() {   # leaves the pane's mode flags in $flags, the raw bytes in $tmp/pane.raw
    rm -f "$tmp/go" "$tmp/pane.rc" "$tmp/pane.raw"
    stop_server t
    t new-session -d -s term -x 100 -y 30 "sh $tmp/pane.sh"
    t pipe-pane -t term -O "cat >> $tmp/pane.raw"
    touch "$tmp/go"
    for _i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do [ -e "$tmp/pane.rc" ] && break; sleep 0.2; done
    sleep 0.3   # the last bytes are still on their way through tmux
    # tmux 3.4 does not expose bracket_paste_flag. Keep a stable-width flag
    # projection there; the raw teardown below still verifies bracketed paste.
    flags="$(t display -p -t term '#{alternate_on}#{keypad_cursor_flag}#{mouse_any_flag}#{mouse_sgr_flag}#{?bracket_paste_flag,1,0}')"
    stop_server t
  }
  esc="$(printf '\033')"
  run_pane
  if [ "$(cat "$tmp/pane.rc" 2>/dev/null)" = 3 ] && [ "$flags" = 00000 ] && grep -q "link to nowhere dropped" "$tmp/pane.err" \
     && LC_ALL=C grep -q "${esc}\[?1003l" "$tmp/pane.raw" && LC_ALL=C grep -q "${esc}\[?1049l" "$tmp/pane.raw" \
     && LC_ALL=C grep -q "${esc}\[?2004l" "$tmp/pane.raw"; then
    pass=$((pass+1)); echo "ok   link drop: modes restored, alt screen left, rc 3"
  else
    fail=$((fail+1)); echo "FAIL link drop: rc=$(cat "$tmp/pane.rc" 2>/dev/null) flags=$flags"; sed 's/^/     /' "$tmp/pane.err" 2>/dev/null
  fi
  # ssh that never connected: same rc 255, but the terminal was never touched -- leaving the
  # alternate screen now would jump the cursor to a stale saved position, so nothing is sent.
  printf '#!/bin/sh\necho "ssh: connect to host nowhere port 22: No route to host" >&2\nexit 255\n' > "$tmp/fakebin/ssh"
  run_pane
  if [ "$(cat "$tmp/pane.rc" 2>/dev/null)" = 3 ] && grep -q "nowhere unreachable" "$tmp/pane.err" \
     && ! LC_ALL=C grep -q "${esc}\[?" "$tmp/pane.raw" 2>/dev/null; then
    pass=$((pass+1)); echo "ok   link never up: rc 3, terminal untouched"
  else
    fail=$((fail+1)); echo "FAIL link never up: rc=$(cat "$tmp/pane.rc" 2>/dev/null)"; sed 's/^/     /' "$tmp/pane.err" 2>/dev/null; cat -v "$tmp/pane.raw" 2>/dev/null | sed 's/^/     /'
  fi
  rm -rf "$tmp/fakebin" "$tmp/pane.sh"
else
  echo "skip link-drop tests (no tmux)"
fi

# --- the local tmux server dies under the client: the tty belongs to the SERVER (the client
# only hands it the fd), so a server that segfaults takes the teardown with it -- clicks keep
# reporting themselves into the prompt, the terminal stays in the alternate screen. Two
# isolated servers here: the outer one stands in for the terminal emulator (pane flags +
# pipe-pane are the evidence), the inner one is what hgs drives and what this test kills.
if command -v tmux >/dev/null 2>&1 || [ -x /opt/homebrew/bin/tmux ]; then
  esc="${esc:-$(printf '\033')}"
  mkdir -p "$tmp/tmux2"
  # Every tmux call against the inner server, with the same guard as t(): a missing dir would
  # send kill-server to the user's real socket.
  t2() {
    [ -d "$tmp/tmux2" ] || { echo "FATAL: $tmp/tmux2 missing — refusing to run tmux" >&2; exit 99; }
    TMUX_TMPDIR="$tmp/tmux2" "$TM" "$@"
  }
  cat > "$tmp/pane2.sh" <<EOF
#!/bin/sh
while [ ! -e "$tmp/go2" ]; do sleep 0.1; done
unset TMUX TMUX_PANE
TMUX_TMPDIR="$tmp/tmux2" HGS_TAB=0 "$HGS" \$(cat "$tmp/args2") 2>"$tmp/pane2.err"; echo \$? > "$tmp/pane2.rc"
sleep 30
EOF
  run_pane2() {   # $1 = hgs arguments; runs them in the pane that stands in for the terminal
    rm -f "$tmp/go2" "$tmp/pane2.rc" "$tmp/pane2.raw"
    printf '%s\n' "$1" > "$tmp/args2"
    stop_server t
    t new-session -d -s term2 -x 100 -y 30 "sh $tmp/pane2.sh"
    t pipe-pane -t term2 -O "cat >> $tmp/pane2.raw"
    touch "$tmp/go2"
  }
  wait_rc2() {
    for _i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do [ -e "$tmp/pane2.rc" ] && break; sleep 0.2; done
    sleep 0.3   # the last bytes are still on their way through tmux
  }
  flags2() { t display -p -t term2 '#{alternate_on}#{mouse_any_flag}#{mouse_sgr_flag}#{?bracket_paste_flag,1,0}'; }

  stop_server t2
  run_pane2 "sh wl"
  srv=""
  for _i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25; do
    srv="$(t2 display-message -p '#{pid}' 2>/dev/null)"
    [ -n "$srv" ] && [ "$(t2 list-clients 2>/dev/null | wc -l | tr -d ' ')" -ge 1 ] && break
    sleep 0.2
  done
  sleep 0.4                      # tmux's own setup sequences are still on their way to the pane
  before="$(flags2)"             # 1... = the "terminal" really is in tmux's hands now
  [ -n "$srv" ] && kill -9 "$srv" 2>/dev/null
  wait_rc2
  after="$(flags2)"
  if [ "$(cat "$tmp/pane2.rc" 2>/dev/null)" = 1 ] && [ "${before%???}" = 1 ] && [ "$after" = 0000 ] \
     && LC_ALL=C grep -q "${esc}\[?1000l" "$tmp/pane2.raw" && LC_ALL=C grep -q "${esc}\[?1049l" "$tmp/pane2.raw" \
     && LC_ALL=C grep -q "${esc}\[?2004l" "$tmp/pane2.raw" \
     && grep -q "tmux server died" "$tmp/pane2.err"; then
    pass=$((pass+1)); echo "ok   server death: modes restored, alt screen left, rc 1"
  else
    fail=$((fail+1)); echo "FAIL server death: rc=$(cat "$tmp/pane2.rc" 2>/dev/null) flags $before -> $after"
    sed 's/^/     /' "$tmp/pane2.err" 2>/dev/null
  fi
  stop_server t2

  # A client that fails while the server is alive never got the terminal: sending the teardown
  # now would jump the cursor to a stale saved position, so hgs must keep quiet.
  t2 new-session -d -s keepalive -c "$tmp" "sleep 60"
  run_pane2 "a nosuchsession"
  wait_rc2
  if [ "$(cat "$tmp/pane2.rc" 2>/dev/null)" = 1 ] && ! LC_ALL=C grep -q "${esc}\[?" "$tmp/pane2.raw" 2>/dev/null; then
    pass=$((pass+1)); echo "ok   failed attach, server alive: terminal untouched"
  else
    fail=$((fail+1)); echo "FAIL failed attach, server alive: rc=$(cat "$tmp/pane2.rc" 2>/dev/null)"
    cat -v "$tmp/pane2.raw" 2>/dev/null | sed 's/^/     /'
  fi
  stop_server t2
  stop_server t
  rm -f "$tmp/pane2.sh" "$tmp/args2"
else
  echo "skip server-death tests (no tmux)"
fi

# --- terminal tab (title = session name, colour = the box we hop to) ---
check "tab: local session -> bare name, no colour" 0 "^tab claude/sample-project color=$" --dry-run claude sample-project
check "tab: tag lands in the title"       0 "^tab claude/sample-project/review color=$" --dry-run claude sample-project -n review
check_not "tab: -d never decorates"       "^tab "                                    --dry-run claude sample-project -d
check "tab: peer -> host prefix + colour" 0 "^tab nowhere:claude/sample-project color=#[0-9a-f]{6}$" --dry-run @nowhere claude sample-project
check "tab: peer path project -> basename" 0 "^tab nowhere:claude/sample-project color=#" --dry-run @nowhere claude /some/where/sample-project
check "tab: peer tag"                     0 "^tab nowhere:codex/foo/rev color=#"      --dry-run @nowhere codex foo -n rev
check "tab: peer dotted name -> underscore" 0 "^tab nowhere:claude/example_com color=#"  --dry-run @nowhere claude example.com
check_not "tab: peer -d never decorates"  "^tab "                                    --dry-run @nowhere claude sample-project -d
check_not "tab: peer ls never decorates"  "^tab "                                    @nowhere ls --dry-run
out="$(HGS_TAB_COLORS="nowhere=#abcdef" "$HGS" --dry-run @nowhere claude sample-project 2>&1)"
if printf '%s\n' "$out" | grep -qE -- "^tab nowhere:claude/sample-project color=#abcdef$"; then pass=$((pass+1)); echo "ok   tab: HGS_TAB_COLORS wins over the hash"; else fail=$((fail+1)); echo "FAIL tab: HGS_TAB_COLORS"; printf '%s\n' "$out"; fi
out="$(HGS_TAB_COLORS="local=#123456" "$HGS" --dry-run claude sample-project 2>&1)"
if printf '%s\n' "$out" | grep -qE -- "^tab claude/sample-project color=#123456$"; then pass=$((pass+1)); echo "ok   tab: local key colours own sessions"; else fail=$((fail+1)); echo "FAIL tab: local colour key"; printf '%s\n' "$out"; fi
out="$(HGS_TAB=0 "$HGS" --dry-run claude sample-project 2>&1)"
if ! printf '%s\n' "$out" | grep -qE -- "^tab " && printf '%s\n' "$out" | grep -qE -- "^tmux -u new-session"; then pass=$((pass+1)); echo "ok   tab: HGS_TAB=0 disables it"; else fail=$((fail+1)); echo "FAIL tab: HGS_TAB=0"; printf '%s\n' "$out"; fi
# same peer -> same hue, every time
c1="$("$HGS" --dry-run @nowhere claude sample-project 2>&1 | sed -n 's/^tab .*color=//p')"
c2="$("$HGS" --dry-run @nowhere codex example.com 2>&1 | sed -n 's/^tab .*color=//p')"
if [ -n "$c1" ] && [ "$c1" = "$c2" ]; then pass=$((pass+1)); echo "ok   tab: peer colour is stable"; else fail=$((fail+1)); echo "FAIL tab: peer colour not stable ($c1 vs $c2)"; fi
# --- ls --json ---
json_ok() {   # json_ok <имя> <python-проверка> [аргументы hgs...]
  local name="$1" check="$2"; shift 2
  if ! command -v python3 >/dev/null 2>&1; then
    echo "skip $name (no python3)"; return
  fi
  local out rc perr prc
  out="$("$HGS" "$@" 2>&1)"; rc=$?
  perr="$(printf '%s' "$out" | python3 -c "
import json,sys
d = json.load(sys.stdin)
$check
" "$HGS" 2>&1)"; prc=$?
  if [ "$rc" = 0 ] && [ "$prc" = 0 ]; then
    pass=$((pass+1)); echo "ok   $name"
  else
    fail=$((fail+1)); echo "FAIL $name (rc=$rc)"
    printf '%s\n' "$out" | sed 's/^/     /'
    [ -n "$perr" ] && printf '%s\n' "$perr" | sed 's/^/     python: /'
  fi
}
json_ok "json: --local отдаёт объект этого бокса" "
assert d['host'] == 'testhost', d['host']
assert d['ok'] is True
assert d['hgs_version'] == __import__('subprocess').check_output([sys.argv[1], '--version'], text=True).strip().removeprefix('hgs ')
assert d['sessions'] == [], d['sessions']
" ls --json --local
json_ok "json: карта проектов внутри" "
assert d['projects']['sample-project'].endswith('/proj/sample-project'), d['projects']
assert 'example.com' in d['projects'] and 'wl' in d['projects'], d['projects']
assert '# comment' not in d['projects'], d['projects']
" ls --json --local

# --- RFC 8259 escaping through the public notify API --------------------------------
# Use the actual executable rather than sourcing a Bash implementation detail. A raw
# newline in the input must still produce exactly one parseable notification record.
if command -v python3 >/dev/null 2>&1; then
  jstest="$tmp/json-control-events"
  : > "$jstest"
  jsout="$(HGS_EVENTS="$jstest" "$HGS" notify claude/json "$(printf 'a\tb\nc\rd\001e')" 2>&1)"; jsrc=$?
  [ "$jsrc" = 0 ] && jsout="$(cat "$jstest")"
  jserr="$(printf '%s' "$jsout" | python3 -c "
import json,sys
lines = sys.stdin.read().splitlines()
assert len(lines) == 1, lines
d = json.loads(lines[0])
assert d['text'] == 'a\tb\nc\rd\x01e', repr(d)
" 2>&1)"; jsprc=$?
  if [ "$jsrc" = 0 ] && [ "$jsprc" = 0 ]; then
    pass=$((pass+1)); echo "ok   json_str: escapes tab/newline/CR/\\001 (RFC 8259)"
  else
    fail=$((fail+1)); echo "FAIL json_str: control-char escaping (rc=$jsrc)"
    printf '%s\n' "$jsout" | sed 's/^/     /'
    [ -n "$jserr" ] && printf '%s\n' "$jserr" | sed 's/^/     python: /'
  fi
else
  echo "skip json_str control-char test (no python3)"
fi

# --- projects file parsing: missing trailing newline, indentation, duplicates ----------
proj_edge_cfg="$tmp/cfg-edge"; mkdir -p "$proj_edge_cfg"
printf 'HGS_SELF="edgehost"\nHGS_PEERS=""\n' > "$proj_edge_cfg/config"
# CRLF + padded "k = v" + an indented (disabled) comment + a duplicate key + NO trailing \n
printf 'alpha=/tmp/alpha\r\nbeta = /tmp/beta \r\n  # disabled=/tmp/old\nalpha=/tmp/DUPLICATE\ngamma=/tmp/gamma' > "$proj_edge_cfg/projects.local"
_save_cfg="$HGS_CONFIG_DIR"; export HGS_CONFIG_DIR="$proj_edge_cfg"
json_ok "json: projects file — no trailing newline still keeps the last entry" "
assert d['projects']['gamma'] == '/tmp/gamma', d['projects']
" ls --json --local
json_ok "json: projects file — CRLF / padding around k=v trimmed" "
assert d['projects']['alpha'] == '/tmp/alpha', d['projects']
assert d['projects']['beta'] == '/tmp/beta', d['projects']
" ls --json --local
json_ok "json: projects file — indented comment is not a project" "
assert 'disabled' not in d['projects'], d['projects']
assert not any(k.lstrip().startswith('#') for k in d['projects']), d['projects']
" ls --json --local
json_ok "json: projects file — duplicate key: first occurrence wins (matches the CLI's awk resolver)" "
assert d['projects']['alpha'] == '/tmp/alpha', d['projects']
" ls --json --local
export HGS_CONFIG_DIR="$_save_cfg"

# --- ls --json: session parsing off real tmux -F output (needs tmux; isolated server) --
# Guards the tab/space swap this format is exposed to: on an EMPTY server, replacing the tab
# in the two `IFS=` reads (leaving -F untouched) still returns rc=0 with sessions=[] -- only
# real session/client rows expose it, so this needs actual tmux state, not a no-server run.
if command -v tmux >/dev/null 2>&1 || [ -x /opt/homebrew/bin/tmux ]; then
  stop_server t
  t new-session -d -s "claude/jsontest" -c "$tmp" "sleep 60"
  t new-session -d -s "codex/jsontest2/rev" -c "$tmp" "sleep 60"
  # A real (non-zero) `attached` can only be observed with an actual client -- a session that
  # was never attached is *correctly* 0, so it can't distinguish a good parse from a broken
  # one forcing everything to 0. Attach via tmux control mode: no pty/tty needed, just a fifo
  # kept open so the client's stdin never sees EOF.
  ctlfifo="$tmp/ctl.fifo"; mkfifo "$ctlfifo" 2>/dev/null
  exec 8<>"$ctlfifo"
  t -C attach-session -t "claude/jsontest" <&8 >/dev/null 2>&1 &
  ctlpid=$!
  for _i in 1 2 3 4 5 6 7 8 9 10; do
    [ -n "$(t list-clients -F '#{client_session}' 2>/dev/null)" ] && break
    sleep 0.2
  done
  now="$(date +%s)"

  json_ok "json: two-segment name -> cmd/project, tag=null" "
s = next(x for x in d['sessions'] if x['name'] == 'claude/jsontest')
assert s['cmd'] == 'claude', s
assert s['project'] == 'jsontest', s
assert s['tag'] is None, s
" ls --json --local

  json_ok "json: three-segment name -> tag set" "
s = next(x for x in d['sessions'] if x['name'] == 'codex/jsontest2/rev')
assert s['cmd'] == 'codex', s
assert s['project'] == 'jsontest2', s
assert s['tag'] == 'rev', s
" ls --json --local

  json_ok "json: attached session -> attached=1 (real int), a real client, created a real epoch" "
s = next(x for x in d['sessions'] if x['name'] == 'claude/jsontest')
assert s['attached'] == 1, s
assert len(s['clients']) == 1, s
assert isinstance(s['created'], int) and abs(s['created'] - $now) < 30, s
" ls --json --local

  json_ok "json: detached session -> attached=0, clients=[]" "
s = next(x for x in d['sessions'] if x['name'] == 'codex/jsontest2/rev')
assert s['attached'] == 0, s
assert s['clients'] == [], s
" ls --json --local

  kill "$ctlpid" 2>/dev/null; wait "$ctlpid" 2>/dev/null
  exec 8>&-
  rm -f "$ctlfifo"
  stop_server t
else
  echo "skip session-loop json tests (no tmux)"
fi

# --- ls --json --dry-run: truthful (both real tmux calls, not a fake "-F json") -------
check "json dry-run shows the real list-clients call"  0 "tmux list-clients -F.*client_tty" ls --json --local --dry-run
check "json dry-run shows the real list-sessions call" 0 "tmux list-sessions -F.*session_created" ls --json --local --dry-run
check_not "json dry-run drops the old fake -F json format" "-F json" ls --json --local --dry-run

# --- usage() documents the JSON contract -----------------------------------------------
check "usage documents ls --json --local" 0 "ls --json --local" -h
# HGS_SELF/HGS_PEERS -- без них не работают ни @host, ни флотовый ls, а узнать о них
# раньше можно было только из исходника.
check "usage documents HGS_SELF"  0 "HGS_SELF"  -h
check "usage documents HGS_PEERS" 0 "HGS_PEERS" -h
# Указатель на роль, из которой инструмент уехал, уже протухал однажды.
if grep -q 'roles/hgs_sessions' "$HGS" "$here/../tmux.conf"; then
  fail=$((fail+1)); echo "FAIL hgs/tmux.conf всё ещё ссылаются на roles/hgs_sessions"
else
  pass=$((pass+1)); echo "ok   no stale roles/hgs_sessions pointers in hgs and tmux.conf"
fi

# --- @peer ls --json: offline peer is a state, not an error --------------------------
check "json: @peer -> ssh с --local --json" 0 "^ssh -o BatchMode=yes -o ConnectTimeout=3 nowhere ~/.local/bin/hgs ls --local --json --dry-run\$" --dry-run @nowhere ls --json
json_ok "json: недоступный пир -> ok:false, выход 0" "
assert d['host'] == 'nowhere', d['host']
assert d['ok'] is False
assert d['error'] == 'offline', d['error']
assert d['sessions'] == [] and d['projects'] == {}
" @nowhere ls --json
# a peer that answers but not with a JSON object (e.g. an old hgs without --json
# support, echoing its human-readable list) must not be passed through as-is.
mkdir -p "$tmp/fakebin"
printf '#!/bin/sh\necho "ssh: connect to host nowhere port 22: Connection refused" >&2\nexit 255\n' > "$tmp/fakebin/ssh"; chmod +x "$tmp/fakebin/ssh"
PATH="$tmp/fakebin:$PATH" json_ok "json: SSH failure preserves the connection reason" "
assert d['ok'] is False and d['error'] == 'offline', d
assert 'Connection refused' in d['error_detail'], d
" @nowhere ls --json
printf '#!/bin/sh\necho "  claude/legacy-session   0 client(s)"\nexit 0\n' > "$tmp/fakebin/ssh"; chmod +x "$tmp/fakebin/ssh"
PATH="$tmp/fakebin:$PATH" json_ok "json: пир отвечает не-JSON -> ok:false, error:bad_response" "
assert d['host'] == 'nowhere', d
assert d['ok'] is False and d['error'] == 'bad_response', d
assert d['sessions'] == [], d
" @nowhere ls --json
rm -rf "$tmp/fakebin"

# --- bare ls --json (no @host, no --local): array over the whole fleet ----------------
json_ok "json: без --local -> массив по флоту" "
assert isinstance(d, list), type(d)
assert len(d) == 1, d          # HGS_PEERS пуст в тестовом конфиге
assert d[0]['host'] == 'testhost' and d[0]['ok'] is True
" ls --json

# --- existing session by bare name (needs tmux; isolated server in TMUX_TMPDIR) ---
if command -v tmux >/dev/null 2>&1 || [ -x /opt/homebrew/bin/tmux ]; then
  t new-session -d -s "claude/zzz_front" -c "$tmp" "sleep 60"
  check "existing session, name not in map -> attach" 0 "^tmux -u attach-session -t =claude/zzz_front$" --dry-run claude zzz_front
  check "existing session, dotted name -> attach"    0 "^tmux -u attach-session -t =claude/zzz_front$" --dry-run claude zzz.front
  check "existing session -> set-environment first"  0 "^tmux set-environment -t =claude/zzz_front HGS_CLIENT local$" --dry-run claude zzz_front
  check "existing session, --client -> peer name"    0 "^tmux set-environment -t =claude/zzz_front HGS_CLIENT mac$" --client mac --dry-run claude zzz_front
  # live: new-session -e really lands HGS_CLIENT in the session environment (a path project:
  # an unknown name with no session is an error)
  mkdir -p "$tmp/proj/clienttest"
  HGS_TAB=0 "$HGS" --client mac sh "$tmp/proj/clienttest" -d >/dev/null 2>&1
  envv="$(t show-environment -t "=sh/clienttest" HGS_CLIENT 2>/dev/null)"
  if [ "$envv" = "HGS_CLIENT=mac" ]; then pass=$((pass+1)); echo "ok   client: live -d session has HGS_CLIENT=mac"; else fail=$((fail+1)); echo "FAIL client: live -d session env ($envv)"; fi
  t kill-session -t "=sh/clienttest" 2>/dev/null
  check "tab: attach to an existing session too"    0 "^tab claude/zzz_front color=$" --dry-run claude zzz_front
  check "no session and not in map -> rc 1"          1 "unknown project .nope. and no session claude/nope" --dry-run claude nope
  stop_server t
else
  echo "skip existing-session tests (no tmux)"
fi

# --- tmux.conf: both `prefix d` and `prefix C-d` detach ---
if command -v tmux >/dev/null 2>&1 || [ -x /opt/homebrew/bin/tmux ]; then
  t -L hgscfgchk kill-server 2>/dev/null
  t -L hgscfgchk -f "$here/../tmux.conf" new-session -d -s c -x 80 -y 24 "sleep 30" 2>/dev/null
  binds="$(t -L hgscfgchk list-keys -T prefix 2>/dev/null)"
  rootcv="$(t -L hgscfgchk list-keys -T root 2>/dev/null | grep -E '^bind-key +-T root +C-v ')"; t -L hgscfgchk kill-server 2>/dev/null
  printf "%s\n" "$binds" | grep -qE "prefix d[[:space:]]+detach" && printf "%s\n" "$binds" | grep -qE "prefix C-d[[:space:]]+detach" \
    && { pass=$((pass+1)); echo "ok   prefix d and C-d both detach"; } || { fail=$((fail+1)); echo "FAIL prefix d/C-d detach binding"; }
  # root C-v -> hgs paste (the clipboard bridge; README "Картинка по Ctrl+V")
  if printf "%s\n" "$rootcv" | grep -q "hgs paste '#{session_name}' '#{pane_id}' '#{client_name}'"; then pass=$((pass+1)); echo "ok   tmux.conf: C-v -> hgs paste"; else fail=$((fail+1)); echo "FAIL tmux.conf: C-v -> hgs paste"; printf '%s\n' "$rootcv" | sed 's/^/     /'; fi
fi

# --- macOS Claude auth is owned by Claude, including isolated Keychain services ---
kc_home="$tmp/kchome"; mkdir -p "$kc_home/.local/bin"
printf '#!/bin/sh\necho Darwin\n' > "$kc_home/.local/bin/uname"
printf '#!/bin/sh\necho SECURITY_CALLED >> "%s/kc.log"\nexit 1\n' "$tmp" > "$kc_home/.local/bin/security"
printf '#!/bin/sh\necho CLAUDE_RAN\n' > "$kc_home/.local/bin/claude"
chmod +x "$kc_home/.local/bin/uname" "$kc_home/.local/bin/security" "$kc_home/.local/bin/claude"
for profile in native isolated; do
  rm -f "$tmp/kc.log"
  if [ "$profile" = isolated ]; then
    out="$(HOME="$kc_home" CLAUDE_CONFIG_DIR="$kc_home/work" "$HGS" --run 0 claude 2>&1)"; rc=$?
  else
    out="$(HOME="$kc_home" "$HGS" --run 0 claude 2>&1)"; rc=$?
  fi
  if [ "$rc" = 0 ] && printf '%s' "$out" | grep -q CLAUDE_RAN && [ ! -f "$tmp/kc.log" ]; then
    pass=$((pass+1)); echo "ok   macOS $profile Claude starts without HGS Keychain access"
  else
    fail=$((fail+1)); echo "FAIL macOS $profile Claude auth (rc=$rc): $out"
  fi
done

# --- locale: C locale is upgraded to UTF-8 before anything touches tmux ---
# Set the child's locale through env. Changing Bash's temporary locale inside a
# command substitution can crash Homebrew Bash in libintl/CoreFoundation on macOS.
out="$(env LANG=C LC_ALL=C "$HGS" --run 0 sh -c 'echo "LANG=$LANG LC_ALL=${LC_ALL:-unset}"' 2>&1)"; rc=$?
if [ "$rc" = 0 ] && [ "$out" = "LANG=en_US.UTF-8 LC_ALL=unset" ]; then pass=$((pass+1)); echo "ok   C locale -> UTF-8 LANG for children"; else fail=$((fail+1)); echo "FAIL C locale -> UTF-8 (rc=$rc): $out"; fi

# --- notify ---
ev="$tmp/events"
rm -f "$ev"
HGS_EVENTS="$ev" "$HGS" notify claude/sample-project "ждёт ответа" >/dev/null 2>&1
if [ ! -e "$ev" ]; then pass=$((pass+1)); echo "ok   notify: нет файла событий -> no-op"; else fail=$((fail+1)); echo "FAIL notify: создал файл, которого не было"; fi
: > "$ev"
HGS_EVENTS="$ev" "$HGS" notify claude/sample-project "ждёт ответа" >/dev/null 2>&1
HGS_EVENTS="$ev" "$HGS" notify --bell codex/sample-worker >/dev/null 2>&1
if [ "$(wc -l < "$ev")" -eq 2 ] && python3 -c "
import json,sys
lines = [json.loads(l) for l in open(sys.argv[1], encoding='utf-8') if l.strip()]
a, b = lines
assert a['session'] == 'claude/sample-project', a
assert a['text'] == 'ждёт ответа', a
assert a['bell'] is False and a['host'] == 'testhost', a
assert isinstance(a['ts'], int) and a['ts'] > 1700000000, a
assert b['session'] == 'codex/sample-worker' and b['bell'] is True, b
" "$ev" >/dev/null 2>&1; then pass=$((pass+1)); echo "ok   notify: пишет по строке JSON на событие"; else fail=$((fail+1)); echo "FAIL notify: содержимое файла событий"; sed 's/^/     /' "$ev"; fi
check "notify: без сессии -> usage rc 1" 1 "^hgs: usage: hgs notify" notify

# concurrent writers: short lines appended with O_APPEND must never interleave -- each of N
# parallel notify calls should land as exactly one whole, parseable JSON line.
: > "$ev"
if command -v python3 >/dev/null 2>&1; then
  nconc=25
  for i in $(seq 1 "$nconc"); do
    HGS_EVENTS="$ev" "$HGS" notify "claude/par$i" "concurrent $i" >/dev/null 2>&1 &
  done
  wait
  if [ "$(wc -l < "$ev")" -eq "$nconc" ] && python3 -c "
import json,sys
n = 0
for l in open(sys.argv[1], encoding='utf-8'):
    if not l.strip():
        continue
    json.loads(l)
    n += 1
assert n == int(sys.argv[2]), n
" "$ev" "$nconc" >/dev/null 2>&1; then
    pass=$((pass+1)); echo "ok   notify: параллельные вызовы не бьют строки"
  else
    fail=$((fail+1)); echo "FAIL notify: параллельные вызовы попортили файл событий"; sed 's/^/     /' "$ev"
  fi
else
  echo "skip notify: параллельные вызовы (no python3)"
fi

# --- project: оверлей и управление ---
plocal="$tmp/cfg/projects.local"
rm -f "$plocal"
json_ok "project: ignores Ansible entries" "
assert d == [], d
" project ls --json
check "project: add в пустой оверлей" 0 "" project add sample-tray "$tmp/proj/example.com"
json_ok "project: добавленное видно как local" "
byname = {p['name']: p for p in d}
assert byname['sample-tray']['src'] == 'local', d
assert byname['sample-tray']['dir'].endswith('/proj/example.com'), d
" project ls --json
check "project: add на занятое имя -> rc 1" 1 "already exists" project add sample-tray "$tmp/proj/example.com"
check "project: set переопределяет ansible-запись" 0 "" project set sample-project "$tmp/proj/withlauncher"
json_ok "project: переопределение выигрывает и помечено local" "
byname = {p['name']: p for p in d}
assert byname['sample-project']['src'] == 'local', d
assert byname['sample-project']['dir'].endswith('/proj/withlauncher'), d
assert len([p for p in d if p['name'] == 'sample-project']) == 1, d
" project ls --json
check "project: резолвер видит переопределение" 0 "new-session -e HGS_CLIENT=local $hint_env -s claude/sample-project -c $tmp/proj/withlauncher " --dry-run claude sample-project
json_ok "project: карта в ls --json тоже слита" "
assert d['projects']['sample-project'].endswith('/proj/withlauncher'), d['projects']
assert 'sample-tray' in d['projects'], d['projects']
" ls --json --local
check "project: rm своё" 0 "" project rm sample-tray
check "project: rm чужое -> rc 1 с объяснением" 1 "unknown project" project rm ansibleonly
check "project: add в несуществующий каталог -> rc 1" 1 "no such directory" project add bad "$tmp/nope"
check "project: имя со слешем -> rc 1" 1 "bad project name" project add "a/b" "$tmp/proj/example.com"
check "project: add accepts symlink directory" 0 "" project add alias "$tmp/proj/site-alias"
json_ok "project: stored directory keeps symlink alias" "
byname = {p['name']: p for p in d}
assert byname['alias']['dir'] == '$tmp/proj/site-alias', d
assert byname['alias']['exists'] is True, d
" project ls --json

# --- project: входы, которые не переживают построчный формат оверлея -------------------
# projects.local читается построчно и режется по первому '=' с trim по краям. Имя с '='
# уедет частью в поле каталога, каталог с переводом строки разорвёт запись и оставит в
# файле мусорную строку, а пробел по краям каталога молча потеряется при чтении. Всё это
# должно отвергаться ДО записи -- и, главное, файл после отказа обязан остаться
# байт-в-байт прежним (иначе «ошибка» и порча неотличимы).
rm -f "$plocal"
"$HGS" project add keepme "$tmp/proj/example.com" >/dev/null 2>&1
cp "$plocal" "$tmp/plocal.before"
nldir="$tmp/proj/nl"$'\n'"x"; mkdir -p "$nldir"   # $(printf '\n') не годится: подстановка съест перевод строки
spdir="$tmp/proj/sp "; mkdir -p "$spdir"
check "project: '=' в имени -> rc 1"             1 "bad project name" project add "a=b" "$tmp/proj/example.com"
check "project: set с '=' в имени -> rc 1"       1 "bad project name" project set "a=b" "$tmp/proj/example.com"
check "project: перевод строки в каталоге -> rc 1"       1 "bad directory" project add nl "$nldir"
check "project: set с переводом строки в каталоге -> rc 1" 1 "bad directory" project set nl "$nldir"
check "project: пробел по краям каталога -> rc 1"        1 "bad directory" project add sp "$spdir"
check "project: имя с '#' в начале -> rc 1"              1 "bad project name" project add "#hidden" "$tmp/proj/example.com"
if cmp -s "$plocal" "$tmp/plocal.before"; then
  pass=$((pass+1)); echo "ok   project: отказ не трогает projects.local"
else
  fail=$((fail+1)); echo "FAIL project: отказ испортил projects.local"; sed 's/^/     /' "$plocal"
fi
# и карта после всех отказов по-прежнему читается целиком
json_ok "project: после отказов оверлей цел" "
byname = {p['name']: p for p in d}
assert byname['keepme']['dir'].endswith('/proj/example.com'), d
assert not [p for p in d if p['name'] in ('a', 'a=b', 'nl', 'sp')], d
" project ls --json
rm -rf "$nldir" "$spdir"
rm -f "$plocal"

# --- clip: this box's clipboard image to stdout ---
# Platform-independent: a fake uname says Linux so the wl-paste branch runs on the Mac too.
mkdir -p "$tmp/fakebin" "$tmp/emptyrt"
printf '#!/bin/sh\necho Linux\n' > "$tmp/fakebin/uname"; chmod +x "$tmp/fakebin/uname"
cat > "$tmp/fakebin/wl-paste" <<'EOF'
#!/bin/sh
# FAKE_TYPES = what the clipboard offers; -t image/png streams the fake PNG
case "$1" in
  -l) printf '%s\n' $FAKE_TYPES; exit 0 ;;
  -t) [ "$2" = image/png ] && { printf 'PNGDATA'; exit 0; }; exit 1 ;;
esac
exit 1
EOF
chmod +x "$tmp/fakebin/wl-paste"
out="$(PATH="$tmp/fakebin:$PATH" WAYLAND_DISPLAY=wayland-test FAKE_TYPES="text/plain image/png" "$HGS" clip 2>&1)"; rc=$?
if [ "$rc" = 0 ] && [ "$out" = PNGDATA ]; then pass=$((pass+1)); echo "ok   clip: png -> stdout rc 0"; else fail=$((fail+1)); echo "FAIL clip: png (rc=$rc out=$out)"; fi
out="$(PATH="$tmp/fakebin:$PATH" WAYLAND_DISPLAY=wayland-test FAKE_TYPES="text/plain" "$HGS" clip 2>&1)"; rc=$?
if [ "$rc" = 1 ] && [ -z "$out" ]; then pass=$((pass+1)); echo "ok   clip: no image -> rc 1, silent"; else fail=$((fail+1)); echo "FAIL clip: no image (rc=$rc out=$out)"; fi
out="$(PATH="$tmp/fakebin:$PATH" WAYLAND_DISPLAY= XDG_RUNTIME_DIR="$tmp/emptyrt" FAKE_TYPES="image/png" "$HGS" clip 2>/dev/null)"; rc=$?
if [ "$rc" = 2 ] && [ -z "$out" ]; then pass=$((pass+1)); echo "ok   clip: no wayland socket -> rc 2"; else fail=$((fail+1)); echo "FAIL clip: no socket (rc=$rc out=$out)"; fi
rm -rf "$tmp/fakebin" "$tmp/emptyrt"

# --- paste: the tmux C-v binding ---
# Branches are checked with a fake tmux/ssh/wl-copy that log their calls; the live check
# below proves the local branch really lands C-v in a pane.
mkdir -p "$tmp/fakebin"
printf '#!/bin/sh\necho Linux\n' > "$tmp/fakebin/uname"; chmod +x "$tmp/fakebin/uname"
cat > "$tmp/fakebin/tmux" <<EOF
#!/bin/sh
echo "tmux \$*" >> "$tmp/paste.log"
[ "\$1" = show-environment ] && printf '%s\n' "\$FAKE_ENV"
exit 0
EOF
cat > "$tmp/fakebin/ssh" <<EOF
#!/bin/sh
echo "ssh \$*" >> "$tmp/paste.log"
case "\$FAKE_CLIP" in
  png)    printf 'PNGDATA'; exit 0 ;;
  none)   exit 1 ;;
  notool) exit 2 ;;
  *)      exit 255 ;;
esac
EOF
cat > "$tmp/fakebin/wl-copy" <<EOF
#!/bin/sh
echo "wl-copy \$*" >> "$tmp/paste.log"
cat > "$tmp/wl-copy.in"
EOF
chmod +x "$tmp/fakebin/tmux" "$tmp/fakebin/ssh" "$tmp/fakebin/wl-copy"
# Use a real executable, not an exported Bash function: Rust inherits PATH only.
# A private home also keeps Homebrew's tmux from overriding the stub on macOS.
mkdir -p "$tmp/paste-home/.local/bin"
ln -s "$tmp/fakebin/tmux" "$tmp/paste-home/.local/bin/tmux"
paste_hgs() (
  HOME="$tmp/paste-home" "$HGS" "$@"
)
# paste_case <name> <FAKE_ENV> <FAKE_CLIP> <grep -E pattern the log must match> <pattern it must NOT match>
# XDG_STATE_HOME points the paste log at $tmp, not the real ~/.local/state.
paste_case() {
  local name="$1" env="$2" clip="$3" want="$4" nowant="$5" out rc
  : > "$tmp/paste.log"; rm -f "$tmp/wl-copy.in"
  out="$(PATH="$tmp/fakebin:$PATH" WAYLAND_DISPLAY=wayland-test XDG_STATE_HOME="$tmp/state" FAKE_ENV="$env" FAKE_CLIP="$clip" paste_hgs paste sess %7 cli 2>&1)"; rc=$?
  if [ "$rc" = 0 ] && [ -z "$out" ] && grep -qE -- "$want" "$tmp/paste.log" && ! grep -qE -- "$nowant" "$tmp/paste.log" \
     && grep -q '^tmux send-keys -t %7 C-v$' "$tmp/paste.log"; then
    pass=$((pass+1)); echo "ok   paste: $name"
  else
    fail=$((fail+1)); echo "FAIL paste: $name (rc=$rc out=$out)"; sed 's/^/     /' "$tmp/paste.log"
  fi
}
paste_case "local -> only C-v"            "HGS_CLIENT=local" png  '^tmux send-keys' '^ssh|display-message'
paste_case "unset -> only C-v"            "-HGS_CLIENT"      png  '^tmux send-keys' '^ssh|display-message'
paste_case "peer png -> wl-copy, C-v"     "HGS_CLIENT=mac"   png  '^ssh -o BatchMode=yes -o ConnectTimeout=3 mac ~/.local/bin/hgs clip$' 'no image|unreachable|cannot set'
paste_case "peer png -> pulling message"  "HGS_CLIENT=mac"   png  '^tmux display-message -c cli -d 4000 hgs: pulling clipboard from mac$' 'unreachable'
paste_case "peer no image -> message"     "HGS_CLIENT=mac"   none '^tmux display-message -c cli -d 4000 hgs: no image in mac clipboard$' '^wl-copy'
paste_case "peer no tool -> message"      "HGS_CLIENT=mac"   notool '^tmux display-message -c cli -d 4000 hgs: mac has no clipboard tool' '^wl-copy'
paste_case "peer down -> message"         "HGS_CLIENT=mac"   down '^tmux display-message -c cli -d 4000 hgs: mac unreachable, pasting local clipboard$' '^wl-copy'
# paste log: one line per C-v, naming the branch taken (README "Картинка по Ctrl+V")
paste_log_case() {  # <name> <FAKE_ENV> <FAKE_CLIP> <grep -E pattern the paste log must match>
  local name="$1" env="$2" clip="$3" want="$4"
  rm -f "$tmp/state/hgs/paste.log"
  PATH="$tmp/fakebin:$PATH" WAYLAND_DISPLAY=wayland-test XDG_STATE_HOME="$tmp/state" FAKE_ENV="$env" FAKE_CLIP="$clip" paste_hgs paste sess %7 cli >/dev/null 2>&1
  if grep -qE -- "$want" "$tmp/state/hgs/paste.log" 2>/dev/null; then
    pass=$((pass+1)); echo "ok   paste log: $name"
  else
    fail=$((fail+1)); echo "FAIL paste log: $name"; sed 's/^/     /' "$tmp/state/hgs/paste.log" 2>/dev/null
  fi
}
paste_log_case "ok: rc/bytes/set"     "HGS_CLIENT=mac"   png  'who=mac rc=0 ssh=[0-9]+s bytes=7 set=ok$'
paste_log_case "no image"             "HGS_CLIENT=mac"   none 'who=mac rc=1 .*no-image-on-mac$'
paste_log_case "no tool"              "HGS_CLIENT=mac"   notool 'who=mac rc=2 .*no-tool-on-mac$'
paste_log_case "peer down"            "HGS_CLIENT=mac"   down 'who=mac rc=255 .*ssh-failed$'
paste_log_case "local passthrough"    "HGS_CLIENT=local" png  'who=local local-passthrough$'
: > "$tmp/paste.log"
PATH="$tmp/fakebin:$PATH" WAYLAND_DISPLAY=wayland-test XDG_STATE_HOME="$tmp/state" FAKE_ENV="HGS_CLIENT=mac" FAKE_CLIP=png paste_hgs paste sess %7 cli >/dev/null 2>&1
if [ "$(cat "$tmp/wl-copy.in" 2>/dev/null)" = PNGDATA ] && grep -q '^wl-copy -t image/png$' "$tmp/paste.log"; then pass=$((pass+1)); echo "ok   paste: png bytes reach wl-copy"; else fail=$((fail+1)); echo "FAIL paste: png bytes reach wl-copy"; sed 's/^/     /' "$tmp/paste.log"; fi
: > "$tmp/paste.log"
PATH="$tmp/fakebin:$PATH" WAYLAND_DISPLAY=wayland-test XDG_STATE_HOME="$tmp/state" FAKE_ENV="HGS_CLIENT=mac" FAKE_CLIP=png paste_hgs paste sess %7 >/dev/null 2>&1
if ! grep -q display-message "$tmp/paste.log" && grep -q '^tmux send-keys -t %7 C-v$' "$tmp/paste.log"; then pass=$((pass+1)); echo "ok   paste: no client arg -> no messages"; else fail=$((fail+1)); echo "FAIL paste: no client arg -> no messages"; sed 's/^/     /' "$tmp/paste.log"; fi
check "paste: no args -> rc 1"  1 "^hgs: usage: hgs paste" paste
check "paste: one arg -> rc 1"  1 "^hgs: usage: hgs paste" paste sess
rm -rf "$tmp/fakebin" "$tmp/paste.log" "$tmp/wl-copy.in" "$tmp/state"
# live: the local branch lands C-v (0x16) in a real pane. Raw mode, else the tty eats ^V as lnext.
t new-session -d -s pastelive -x 60 -y 10 -e HGS_CLIENT=local 'stty raw; cat -v'
pane="$(t list-panes -t =pastelive -F '#{pane_id}' | head -1)"
sleep 0.5
XDG_STATE_HOME="$tmp/state" "$HGS" paste pastelive "$pane"
sleep 0.5
if t capture-pane -p -t "$pane" | grep -q '\^V'; then pass=$((pass+1)); echo "ok   paste: live local -> pane got ^V"; else fail=$((fail+1)); echo "FAIL paste: live local -> pane got ^V"; t capture-pane -p -t "$pane" | sed 's/^/     /'; fi
t kill-session -t =pastelive 2>/dev/null

# --- graphical env of a session created over ssh (README "Буфер обмена в сессии, созданной с пира") ---
# An ssh login has no DISPLAY/WAYLAND_DISPLAY/XAUTHORITY, so a session created from the peer
# left the agent inside without a clipboard (codex/arboard falls back to X11 and times out).
# hgs fills them from the systemd user manager. A fake uname (Linux) + systemctl run this on
# the Mac too; XDG_RUNTIME_DIR points at an empty dir so the box's real socket stays out.
mkdir -p "$tmp/guibin" "$tmp/emptyrt"
printf '#!/bin/sh\necho Linux\n' > "$tmp/guibin/uname"
printf '#!/bin/sh\n[ -n "${FAKE_SYSENV:-}" ] && printf "%%s\\n" "$FAKE_SYSENV"\nexit 0\n' > "$tmp/guibin/systemctl"
chmod +x "$tmp/guibin/uname" "$tmp/guibin/systemctl"
touch "$tmp/gui-xauth"
"$HGS" project set sample-project "$tmp/proj/sample-project" >/dev/null
gui_case() {   # <name> <FAKE_SYSENV> <XDG_RUNTIME_DIR> <grep -E pattern> [must-not-match]
  local name="$1" sysenv="$2" rt="$3" want="$4" nowant="${5:-}" out rc
  out="$(env -u WAYLAND_DISPLAY -u DISPLAY -u XAUTHORITY PATH="$tmp/guibin:$PATH" \
         FAKE_SYSENV="$sysenv" XDG_RUNTIME_DIR="$rt" "$HGS" --dry-run claude sample-project 2>&1)"; rc=$?
  if [ "$rc" = 0 ] && printf '%s\n' "$out" | grep -qE -- "$want" \
     && { [ -z "$nowant" ] || ! printf '%s\n' "$out" | grep -qE -- "$nowant"; }; then
    pass=$((pass+1)); echo "ok   gui env: $name"
  else
    fail=$((fail+1)); echo "FAIL gui env: $name (rc=$rc; pattern: $want)"; printf '%s\n' "$out" | sed 's/^/     /'
  fi
}
gui_case "systemd values -> -e for all three" \
  "DISPLAY=:1
WAYLAND_DISPLAY=wayland-0
XAUTHORITY=$tmp/gui-xauth" "$tmp/emptyrt" \
  "new-session -e HGS_CLIENT=local $hint_env -e WAYLAND_DISPLAY=wayland-0 -e DISPLAY=:1 -e XAUTHORITY=$tmp/gui-xauth -s claude/sample-project "
gui_case "systemd quotes a value -> quotes stripped" \
  "WAYLAND_DISPLAY=\"wayland-0\"" "$tmp/emptyrt" "-e WAYLAND_DISPLAY=wayland-0 -s "
gui_case "stale XAUTHORITY (no such file) -> not passed on" \
  "DISPLAY=:1
XAUTHORITY=$tmp/gone/xauth" "$tmp/emptyrt" "-e DISPLAY=:1 -s " "XAUTHORITY"
gui_case "no systemd, no socket -> nothing added" "" "$tmp/emptyrt" \
  "new-session -e HGS_CLIENT=local $hint_env -s claude/sample-project " "WAYLAND_DISPLAY|DISPLAY=|XAUTHORITY"
# fallback when the systemd manager knows nothing: the compositor socket in XDG_RUNTIME_DIR
if command -v python3 >/dev/null 2>&1; then
  mkdir -p "$tmp/rt"
  python3 -c 'import socket,sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$tmp/rt/wayland-9" 2>/dev/null
  gui_case "no systemd -> WAYLAND_DISPLAY from the socket scan" "" "$tmp/rt" \
    "new-session -e HGS_CLIENT=local $hint_env -e WAYLAND_DISPLAY=wayland-9 -s " "DISPLAY=:|XAUTHORITY"
else
  echo "skip gui env socket-scan test (no python3)"
fi
# the local case: env already has them, tmux inherits them, hgs adds nothing
out="$(PATH="$tmp/guibin:$PATH" WAYLAND_DISPLAY=wayland-0 DISPLAY=:1 XAUTHORITY="$tmp/gui-xauth" \
       FAKE_SYSENV="WAYLAND_DISPLAY=wayland-9" "$HGS" --dry-run claude sample-project 2>&1)"
if printf '%s\n' "$out" | grep -q -- "-e HGS_CLIENT=local $hint_env -s claude/sample-project "; then
  pass=$((pass+1)); echo "ok   gui env: env already set -> nothing added"
else
  fail=$((fail+1)); echo "FAIL gui env: env already set -> nothing added"; printf '%s\n' "$out" | sed 's/^/     /'
fi

# --- guards ---
out="$(TMUX=/tmp/fake,1,0 "$HGS" claude sample-project 2>&1)"; rc=$?
if [ "$rc" = 2 ] && printf '%s\n' "$out" | grep -q "inside tmux.*; use --new -d or open a new tab"; then pass=$((pass+1)); echo "ok   nested tmux refused"; else fail=$((fail+1)); echo "FAIL nested tmux refused (rc=$rc)"; printf '%s\n' "$out"; fi
# A detached launch from inside a session starts beside it: no tab, no attach.
out="$(TMUX=/tmp/fake,1,0 "$HGS" --dry-run claude sample-project --new -n nested -d 2>&1)"; rc=$?
if [ "$rc" = 0 ] && printf '%s\n' "$out" | grep -q -- "^tmux -u new-session -d .* -s claude/sample-project/nested " \
   && ! printf '%s\n' "$out" | grep -qE "^tab |attach-session"; then pass=$((pass+1)); echo "ok   nested detached launch"; else fail=$((fail+1)); echo "FAIL nested detached launch (rc=$rc)"; printf '%s\n' "$out"; fi
check "no args -> usage rc 1"         1 "^usage:"
check "help"                          0 "^usage:" -h
check "ls with no server"             0 "no sessions" ls

# --- --run fallback (real execution, no tmux) ---
check "run: n=0 -> exec base"             0 "^fresh\$"          --run 0 echo fresh
check "run: fast failure -> fallback"     0 "starting fresh.*|^fresh\$" --run 1 false echo fresh
check "run: resume ok -> no fallback"     0 "^resumed\$"        --run 2 echo resumed echo fresh
out="$(HGS_FALLBACK_SECS=1 "$HGS" --run 3 sh -c 'sleep 2; exit 7' echo fresh 2>&1)"; rc=$?
if [ "$rc" = 7 ] && ! printf '%s\n' "$out" | grep -q fresh; then pass=$((pass+1)); echo "ok   run: slow failure -> no fallback"; else fail=$((fail+1)); echo "FAIL run: slow failure (rc=$rc)"; printf '%s\n' "$out"; fi

# A GUI group intent follows only the session created by its unique launch token.
# Use this suite's isolated server and plain sh; no real agent receives input.
launch_id=92812fe9-eed8-477c-921a-ef3949434722
other_launch=4400baad-7d90-480a-8451-5f3e456e2ae9
check "launch token requires new session" 1 "requires --new" --dry-run sh "$P" --launch-id "$launch_id"
check "launch token validates UUID" 1 "needs a UUID" --dry-run sh "$P" --new --launch-id invalid
check "launch token forwards over interactive SSH" 0 "ssh -t.*--launch-id $launch_id" --dry-run @nowhere sh "$P" --new -n group-test --launch-id "$launch_id"
# New sessions accept the same tags as rename and fork, so the GUI can create them.
check "new session accepts inner spaces" 0 "-s sh/sample-project/my plan -c " --dry-run sh "$P" --new -n 'my plan'
check "@peer new session quotes a spaced name" 0 "--new -n 'my plan' -d --dry-run\$" --dry-run @nowhere sh "$P" --new -n 'my plan' -d
for bad in ' plan' 'plan ' 'a/b' 'a.b' 'a:b' "$(printf 'a\tb')"; do
  check "new session rejects tag '$bad'" 1 "new session name" --dry-run sh "$P" --new -n "$bad"
done
if [ -x "$TM" ] && command -v python3 >/dev/null 2>&1; then
  spaced_launch=0c5a7f3e-6d1b-4c8e-9f2a-5b7d3e1c9a40
  check "detached launch creates a spaced session" 0 "started sh/sample-project/my plan" sh "$P" --new -d -n 'my plan' --launch-id "$spaced_launch"
  "$HGS" ls --json --local > "$tmp/spaced-list.json"
  if python3 - "$tmp/spaced-list.json" "$spaced_launch" <<'PY'
import json, sys
session = next(s for s in json.load(open(sys.argv[1]))['sessions'] if s['name'] == 'sh/sample-project/my plan')
assert session['launch_id'] == sys.argv[2], session
PY
  then pass=$((pass+1)); echo "ok   spaced session is listed with its launch token"; else fail=$((fail+1)); echo "FAIL spaced session is missing from the listing"; fi
  t kill-session -t '=sh/sample-project/my plan' 2>/dev/null || true
fi
if [ -x "$TM" ] && command -v python3 >/dev/null 2>&1; then
  check "launch token creates only its own session" 0 "started sh/sample-project/group-test" sh "$P" --new -d -n group-test --launch-id "$launch_id"
  check "launch collision cannot take ownership" 1 "already exists" sh "$P" --new -d -n group-test --launch-id "$other_launch"
  "$HGS" ls --json --local > "$tmp/launch-list.json"
  if python3 - "$tmp/launch-list.json" "$launch_id" <<'PY'
import json, sys
session = next(s for s in json.load(open(sys.argv[1]))['sessions'] if s['name'] == 'sh/sample-project/group-test')
assert session['launch_id'] == sys.argv[2], session
PY
  then pass=$((pass+1)); echo "ok   launch token survives listing and collision"; else fail=$((fail+1)); echo "FAIL launch token was lost or overwritten"; fi
  # A session that ends before a listing shows it, leaving no record, still ends the
  # GUI's launch placeholder: the listing reports its launch as ended (no supervisor ran,
  # so no exit code). The running group-test launch is not reported.
  gone_launch=5f0e3a1c-2b7d-4e8a-9c6f-1d2e3f4a5b6c
  out="$(SHELL="$(command -v false)" "$HGS" sh "$P" --new -d -n gone --launch-id "$gone_launch" 2>&1)"; rc=$?
  if [ "$rc" = 0 ] && printf '%s\n' "$out" | grep -q "started sh/sample-project/gone" \
     && python3 - "$HGS" "$gone_launch" "$launch_id" <<'PY'
import json, subprocess, sys, time
deadline = time.monotonic() + 10
while True:
    box = json.loads(subprocess.run([sys.argv[1], 'ls', '--json', '--local'], capture_output=True, text=True,
                                    check=True).stdout)
    ended = {launch['launch_id']: launch for launch in box.get('ended_launches', [])}
    if sys.argv[2] in ended or time.monotonic() > deadline:
        break
    time.sleep(0.1)
assert ended.get(sys.argv[2]) == {'launch_id': sys.argv[2], 'name': 'sh/sample-project/gone', 'code': None,
                                  'output': ''}, box
assert sys.argv[3] not in ended, box
assert all(session['name'] != 'sh/sample-project/gone' for session in box['sessions']), box
PY
  then pass=$((pass+1)); echo "ok   a launch whose session is gone is listed as ended"; else fail=$((fail+1)); echo "FAIL a launch whose session is gone is not listed as ended"; printf '%s\n' "$out" | sed 's/^/     /'; fi
  t kill-session -t '=sh/sample-project/group-test' 2>/dev/null || true
fi

echo "passed=$pass failed=$fail"
[ "$fail" -eq 0 ]
