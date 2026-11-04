# Zerus Next (Tauri client)

The new desktop client of Zerus, growing beside the Qt GUI (`tray/`). Design:
`docs/superpowers/specs/2026-10-27-tauri-web-ui-design.md`; plan:
`docs/superpowers/plans/2026-10-28-tauri-client-phase1.md`. The approved visual
reference is `design/webui`; its components moved here and are bound to live data.

| Path | What it is |
| --- | --- |
| `src/` | SolidJS frontend: `bridge.ts` (per-frame batches), `store/` (catalog, Activity, interface state), `ui/` (rail, session list, Activity, styles), `skin/` (skin format v1 and the built-in skins) |
| `src-tauri/` | Tauri 2 app `zerus-client`: window creation, commands (`start`, `next_batch`, `follow`, `unfollow`, `metrics`), the change pump and the replay |
| `core/` | `zerus-client-core`: watch streams, catalog state and the coalescer, pure Rust |
| `media/` | `zerus-client-media`: microphone and round video capture for the `media` and `video` features (see its README) |
| `fixtures/` | Synthetic replay streams (`scripts/make-fixture.py` writes them) |

The dev app is a separate application (`dev.zerus.next`, "Zerus Next (dev)"): it
never claims Qt's single-instance socket, never posts notifications and runs no
main-thread watchdog unless asked (`ZERUS_CLIENT_SINGLE_INSTANCE=1`,
`ZERUS_CLIENT_NOTIFICATIONS=1`, `ZERUS_CLIENT_WATCHDOG=1`), and it never starts,
stops or replaces the installed Zerus GUI, CLI, agents or services.
On the Mac the client also builds as Zerus itself, the switch bundle that takes
over `com.hgdev.hgs-tray`; see `docs/macos-distribution.md#tauri-client-switch`.

The canonical `com.hgdev.hgs-tray` GUI has one owner per login user on macOS,
Linux and Windows, independent of temporary directories, HOME/profile overrides
and hgs configuration. Unix retains a kernel lock in a private UID directory
under the OS `/tmp`; Windows retains a global kernel object keyed by the token
SID. A duplicate forwards public navigation to the canonical endpoint and exits
before GUI or desktop ownership setup. If that endpoint is not ready, it exits
cleanly after a bounded wait. Unsafe or unavailable exclusion fails closed.
Exclusion lasts through final GUI teardown until actual process exit. Crashes
release it in the kernel; only its next owner replaces a stale Unix
socket. The existing Qt activation endpoints remain available for updaters and
configuration-bound desktop requests. `ZERUS_CLIENT_DESKTOP_SERVICE=1` still
starts a GUI and receives the same exclusion. The fixed authenticated Mac
`--desktop-notification-host` entry runs separately before GUI admission and
creates no window or menu-bar item. Private canonical copies retain the published
bundle ID and participate in this guard.

The installed Linux and Windows switch uses the same identifier,
`com.hgdev.hgs-tray`, and the `hgs-tray` executable/instance name. Settings seed
once from Qt's `~/.config/hgdev/hgs-tray.conf` on Linux (an absolute
`XDG_CONFIG_HOME` replaces `~/.config`) or `%APPDATA%/hgdev/hgs-tray.ini` on
Windows. The destination is the platform data directory under
`com.hgdev.hgs-tray/settings.json`. Existing client choices win. The saved seed
record prevents a second import after rollback; Qt's files are never changed.
Development identities never seed, and a replay uses `settings-replay.json`
without reading Qt, including when launched from the installed switch.

G3 adds one versioned first import for each previously unhandled preference
family. Machine color/vivid choices, Projects' All swarm projects preference,
per-conversation collapsed session rows and Tasks' Only this session filter
use the same settings reader and writer, including on a switch that already has
its original seed record. Existing Next values win. `settings.json` keeps private
`g3.families` metadata (family/schema/source path/time/key names/exact byte hash)
and a source-mtime observation relative to the original switch. The timestamp
observation does not prove that bytes stayed unchanged. A failed family has only
private quarantine metadata pointing to its untouched source, stays unhandled
and retries next start. Handled families do not reread Qt after rollback. The
original settings seed, draft seed and run-layout seed retain their own existing
one-time records. Newer settings/G3 schemas are preserved without reseeding.
Keys recorded by the original switch seed stay handled even if a Next value is
later removed. A newly added family never imports those keys again.

Collapsed row preferences keep Qt's packed SHA-256 identities as at most 50,000
hex keys in the existing settings store. Rust projects exact/fallback keys using
the configured peer alias (empty for local), raw command, archive and conversation
identity; a displayed machine name never substitutes for them. A matching
conversation-less fallback upgrades only when that row is observed. Compact and
Expand choices persist through the same writer, and Expand all also clears
unloaded/filtered hashes. Malformed packing leaves this family unhandled for
retry, and an existing empty Next choice wins. Settings writes that would exceed
the store's read bound are refused before changing existing values.

## Menu bar, single instance and notifications (macOS)

| Piece | Where | Qt source it follows |
| --- | --- | --- |
| Command line (`--hgs`, `--sessions`, `--background`, `--version`, `--build-info`) | `src-tauri/src/cli.rs`, `build.rs` | `AppConfig.cpp`, `UpdateController::buildInfo` |
| Identity and data directory | `src-tauri/src/identity.rs` | owner decision of 2026-10-28 |
| Single instance (lock, socket `hgs-tray`, `show`/`sessions`) | `src-tauri/src/instance.rs` | `SingleInstance.cpp`, `main.cpp` |
| Hidden start, close hides, Dock icon only with a window, signals | `src-tauri/src/lifecycle.rs`, `signals.rs` | `main.cpp`, `MacAppVisibility.mm`, `TerminationSignals.h` |
| Tile, tooltip, menu, "Open in" | `core/src/menubar.rs`, `core/src/trayicon.rs`, `src-tauri/src/desk.rs`, `src-tauri/src/platform/macos_menubar.rs` | `IconFactory.cpp`, `FleetState.cpp`, `TrayMenu.cpp`, `QuietTrayIcon.h`, `MacBackend.cpp` |
| Attention notifications | `core/src/attention.rs`, `src-tauri/src/platform/macos_notify.rs` | `AttentionTracker.cpp`, `MacAttentionNotifier.mm` |
| Main-thread watchdog and its sleep hold | `src-tauri/src/watchdog.rs`, `src-tauri/src/platform/macos_power.rs` | `MainThreadWatchdog.cpp`, `MacSystemPower.cpp` |

The data directory is Tauri's `app_data_dir` formula with the running bundle's
identifier: `~/Library/Application Support/dev.zerus.next/` for the dev app,
`~/Library/Application Support/com.hgdev.hgs-tray/` for the switch bundle. The
switch bundle does not see what the dev app saved there. The attachment cache
follows the same rule with Tauri's `app_cache_dir` formula
(`Identity::cache_dir_now`): `~/Library/Caches/<identifier>/` on the Mac.

## Linux tray

Live Linux builds keep the same desk and menu models behind a native
StatusNotifierItem and DBusMenu (`src-tauri/src/platform/linux_tray.rs`). Tauri's
AppIndicator backend does not report primary clicks on Linux, so this adapter
exports the protocols directly, as specified in the approved design. It keeps
Qt's tile sizes, attention colour, tooltip and menu texts; updates wait while
the menu is open, and its actions keep their captured session identities.

`--background` starts without a window, `--sessions` presents the existing
window, and closing it keeps the tray alive. Single-instance names and the
`show`/`sessions` handshake stay in `instance.rs`. The Open in submenu offers
Automatic, Tab, Window and Clipboard, reads `open-mode`/`terminal` from
`tray.conf`, and saves changes in the client's own `menubar.json`.
Native protocol tests create a private D-Bus daemon and fake Plasma watcher;
they never connect to the desktop's bus.

A running local tray session captures its first attached client's TTY from the
existing listing, as Qt does. The existing terminal launcher raises a matching
Konsole tab when possible and otherwise opens the normal attach terminal.
Peer, detached and saved rows never use that TTY to select a local tab; a menu
action keeps the terminal it showed even if the listing changes before its click.

The same Linux connection exports Qt's launcher badge at
`/org/hgdev/Zerus/LauncherEntry`, using `com.canonical.Unity.LauncherEntry`
`Query`/`Update` for `application://hgs-tray.desktop`. Its signed 64-bit `count`
and `count-visible` follow the existing desk attention count even while a menu
is open; zero or unavailable counters hide it. The existing tray worker watches
Plasma/Unity registration and coalesces republication after one second, without
another poller or thread. Normal exit clears the badge and fences late offers.
The shell owns its appearance; the tray tile, menu and activation routes stay
unchanged. This remains the GUI's legacy authority until a G1 writer handoff.

Linux attention uses `org.freedesktop.Notifications`, with Qt's literal body
escaping, action labels, icon and hints. The existing desk decides attention;
the adapter adds no fleet reader or detector. It stores native IDs in
`linux-notifications.json`, bound to the session bus's authenticated
`org.freedesktop.DBus.GetId` and the notification daemon's exact unique owner.
It reads Qt's `attention/linuxOwner` and `attention/linuxTokens` on the switch
identity's first start. Owner-only Qt/older Next IDs cannot prove which bus
generation owns their integers: their exact tokens become uncertain suppression,
never numeric-ID activation or close authority. Clicking a Qt-era banner after
the switch therefore does nothing in Next; attention still shows in the list,
badge and tray, and the next new edge posts a bound notification. A daemon/bus
change also retires its old IDs to suppression; it never permits an automatic
repost. Only withdrawal of the original edge clears that suppression.
`attention/observed` supplies the existing edge seed, so historical alerts are
not reposted. Properly bound expired Plasma notifications stay clickable in
history; withdrawal removes their exact token's IDs, and late replies to a
withdrawn post close only on their verified original bus/owner. Clicks preserve the activation
token and use the desk's guarded session resolution; update clicks verify the
current update token separately. Unconfirmed native deliveries remain reserved
across restart instead of being posted again. Invalid client state is preserved
and posting stays disabled until it can be reviewed.

The pure `zerus_client_core::linux_notify` module shares the existing schema-1
saved-state decoder, namespace quarantine and literal body escaping. Its values
are retained evidence, not authenticated connections or permission to post. It
opens no files or buses in the default build. The GUI still owns its secure
reader, Qt seed, persistence and OS calls. Importers must retain original source
bytes and unknown fields separately from this projection. A future service
transport and writer handoff require G1 admission and current producer drain.

The explicit, default-off `linux-native-notify` core feature exposes a bounded
method transport for later service integration. It has no GUI or service caller,
worker, signal consumer, persistence or retry queue. The root CLI keeps it off.
See [the method-port contract](../docs/desktop-linux-notifications.md) for its
namespace, outcome and actual-drain boundaries.

## Build and run

```sh
cd client
npm ci
npm run check                  # TypeScript
npm test                       # Vitest: stores, transcript, screens
cargo +1.99.1 test --manifest-path src-tauri/Cargo.toml
scripts/check-licenses.sh      # a license notice for every shipped crate (Linux, macOS, Windows)
RUSTUP_TOOLCHAIN=1.99.1 npx tauri dev
```

`scripts/check-licenses.sh` needs the crates in Cargo's cache (`cargo +1.99.1 fetch
--locked --manifest-path src-tauri/Cargo.toml`). When it stops at a crate without a
notice, add the upstream text under `packaging/rust-licenses` as its README there
describes.

`npm run dev` alone serves the frontend in a browser; without Tauri it replays
`fixtures/replay-batches.json`, so the UI works without the app.

The menu component regression uses native WebKitGTK (Python GI, GTK 3 and
WebKit2 4.1), with synthetic delayed IPC and no real CLI or sessions. Start
`npm run dev -- --port 1437`, then from the repository root run:

```sh
timeout 45s dbus-run-session -- xvfb-run -a -s '-screen 0 1440x900x24' \
  env GDK_BACKEND=x11 WAYLAND_DISPLAY=zerus-nonexistent WEBKIT_DISABLE_DMABUF_RENDERER=1 \
  /usr/bin/python3 client/scripts/menu-smoke.py
```

It checks painted frames, button identity, focus, placement layout reads and
dismissal during preparation. A stalled inspection must paint after the 180 ms
preparation budget, then update late capabilities without moving or refocusing.
To use an authorized Arch display window, wrap
the command in `flock -w 1800 /tmp/zerus-arch-display.lock`, omit Xvfb and the
renderer/display overrides, pass the original desktop bus address as
`ZERUS_MENU_SCREEN_BUS` through `dbus-run-session`, and add `--real-display`.
That mode checks the screen lock before opening and throughout the run. Both
modes use private webview data and close their own window on completion.

Environment:

| Variable | Effect |
| --- | --- |
| `ZERUS_CLIENT_HGS` | The `hgs` to run (default: the first `hgs` on `PATH`, then in `~/.local/bin`, `/opt/homebrew/bin` and `/usr/local/bin`, since an app started from Finder or a launcher gets a short `PATH`) |
| `ZERUS_CLIENT_DESKTOP_SERVICE` | Exact value `1` selects the experimental desktop-service source at startup (default off). No legacy Fleet, read delivery, automatic approval, notification or recovery-sync worker starts in that mode; unavailable service stays unavailable. This does not activate the service. |
| `ZERUS_CLIENT_REPLAY` | Replay synthetic watch streams instead of running `hgs`: a path list (`:` on Unix), the first file is this computer, `alias=file` entries are peers, e.g. `fixtures/replay.ndjson:mac=fixtures/replay-mac.ndjson` |
| `ZERUS_CLIENT_REPLAY_INTERVAL_MS` | The pause per `heartbeat` frame of a replay (default 900) |
| `ZERUS_CLIENT_QUERY` | URL parameters for the window, e.g. `select=claude/sample-project/api-refactor` opens that session and `page=settings` that screen |

The service source validates lifecycle component readiness and reads bounded catalog
pages through one subscriber. After the initial insertion walk, a serial
`catalog_changes` reader catches up from its first revision, then reads only
bounded upserts/tombstones. It preserves exact decimal revisions and projection
identity, coalesces hints, and restarts a baseline on a gap, unknown cursor outcome
or scope replacement. Only a fully consumed delta walk establishes membership;
provisional/offline rows remain incomplete. Quit bounds the service join to five
seconds after its draft flush. Shared host attention counts remain separate from
this device's review marks; incomplete/null counts remain unknown. An unavailable
or invalid count projection does not disconnect a valid lifecycle or catalog.
Catalog bootstrap uses the bounded, epoch-bound runtime source roster instead
of the counts' computer list. Retained pre-cutover read marks are projected by
indexed exact-identity lookups into outgoing rows as device-local flags; the host
acknowledgement fields and shared counts remain unchanged. The GUI read-delivery
gate stays closed, and the webview does not mirror that entire legacy queue. Read intents
are acknowledged only after a durable actor receipt, retaining their request UUID
through an uncertain result. Activity, processes, project tasks and organization use
the existing subscriber's bounded detail lease. Each view retains its exact consumer
identity and validates the route generation, revision and contiguous detail sequence;
native frame sequences may skip. Native Gap frames retain the subscription and
allow the next snapshot to replace its staged data. Genuine route inconsistencies
retire only that view before bounded rearming; unknown follow outcomes stay
unavailable until explicit Retry, with no GUI watcher or request replay. An explicit
Retry resets that view's recovery budget; temporary control admission pressure uses
bounded retries before reporting it unavailable.

After the existing automatic-approval confirmation, the native sender uses the
dedicated authenticated `desktop frontend-request` route. It registers the original
child before writing stdin and retains that registration through child and pipe
drain. On a verified Mac installation it selects the paired bundled `hgs`; a failed
pair has no ordinary CLI fallback. One outstanding request per operation retains
its UUID through an uncertain result. **Check result** reads the original receipt
and never resubmits it. If the original relay's exit is confirmed and that lookup
proves no receipt exists, the request settles as not applied and a fresh intent
can be submitted. An unverified exit or failed lookup remains pending.
Maintenance has a native command using the shared full
configuration type, with both update and recovery explicit; it replaces GUI identity
with the running application's executable and build. An unknown recovery save
remains pending. Updates and Recovery use one shared maintenance intent: update
preference changes wait for the native preference worker before submitting both
subsystems, and Recovery Save snapshots its form/base and retains update preferences.
Only that Save's exact receipt acknowledges the edit; unrelated save counters do
not. Opening a page only reads status and any existing receipt. **Apply background
settings** explicitly sends current preferences and connects shared recovery policy
synchronization when no configuration has been supplied yet. Manual **Check now**
stays in the UI update worker. No generic request can change approval policy or
maintenance configuration.
Keep this switch off for normal installations;
this source is not a complete UI-only cutover.

A build without the `live` feature (`--no-default-features`) has no data source;
the frontend then uses its built-in replay.

## Headless runs

Development windows never open on a desktop someone is using. On Linux run the
app inside Xvfb and force X11: with `WAYLAND_DISPLAY` merely unset, GTK falls
back to the default Wayland socket (`wayland-0`) and the window opens on the
desktop. `ZERUS_CLIENT_HEADLESS=1` makes the app exit (code 3) instead of
opening a window anywhere but an X server display. WebKitGTK paints a black
window in Xvfb unless its DMA-BUF renderer is off:

```sh
env -u DISPLAY WAYLAND_DISPLAY=zerus-none GDK_BACKEND=x11 ZERUS_CLIENT_HEADLESS=1 \
  WEBKIT_DISABLE_DMABUF_RENDERER=1 xvfb-run -a -s "-screen 0 1440x900x24" \
  sh -c 'RUSTUP_TOOLCHAIN=1.99.1 ZERUS_CLIENT_REPLAY=$PWD/fixtures/replay.ndjson npx tauri dev'
```

## New session

The New session dialog (`src/store/newSession.ts`, `src/ui/sessions/NewSessionDialog.tsx`)
ports Qt's `NewSessionDialog`: direct folders, None or Auto worktrees, an existing
or manually created worktree, a Git repository clone, account and agent choice,
and OpenCode model and effort. It opens from the Rail, the Sessions header,
project and folder menus, and `session.new` (Ctrl+N, or Command+N on macOS).
`src-tauri/src/newsession.rs` derives the form and plan through the core. The
creation host (`src-tauri/src/creations.rs`) owns background work, its journal,
binding and project placement. `src/store/creations.ts` renders creation rows,
the progress panel, the status menu and strip tiles; no card opens a composer or
touches drafts.

Opening, browsing and choosing send only reads: `account ls` and usage inspects,
`worktrees --path` and `dirs` listings, and the folder/model reads. Creating calls
happen only after the click that names them: `creation_submit` after Start,
`worktrees create`, `dirs --create`, `account start` or `account favorite`.
Nothing creates a worktree while the dialog opens or is inspected. Start submits
once, and an unknown result is never replayed. Every guarded creation call checks
the expected machine. A capable watch stream binds by exact launch id, raw agent
and tag; older peers retain the bounded `ls --json` poll.

Creation cards offer Continue setup, Check result, Stop, Edit choices and Hide.
Check reads the original result and never repeats a step. A bound card adopts its
exact online session row, even if the bound event arrives first. Creation titles
and menus use the D6 layout without dot separators; older creations get headings
every 50, and a search matches card details in place. The D7 remembered choices
are `workspace/launchAgent`, `workspace/launchOpenTerminal` and
`workspace/launchAccounts` in `settings.json`, seeded once read-only from Qt.
Placement follows Q6: acceptance into the durable organization Controller draft
is enough, without waiting for `swarm apply`. A pre-launch refusal fails the step;
after binding, at most one AddFolder and one Move wait for readiness or member
arrival, and quit drops them without blocking.

State stays under the app data directory: `worktree-snapshots.json`,
`creations.json`, and those three remembered choices in `settings.json`. Replay
uses separate files. Bounds include 256 account rows, 10,000 OpenCode models
(5 MiB) with 64 favorites and 500 shown rows, four OpenCode catalogs, 64 verified
worktree catalogs, 128 creations and notices, 32 ended launches, 65,536 detail
characters, 200 folders per project and 512 saved-project placements. Watch and
listing reads stay bounded, and idle passes do not copy listings.

### New session smoke run

`ZERUS_CLIENT_QUERY=newsession-smoke=1` runs three passes with the replay and a
fake `hgs`: open plus read-only Cancel, None through Ctrl+N and Enter, then Auto
on a verified Git catalog. It needs replay mode, an explicit `ZERUS_CLIENT_HGS`
and `ZERUS_CLIENT_PERF`, and runs only headless. The fake appends ordinary calls
to `log.ndjson` and `[unix_ms, argv]` to `stamped.ndjson`; its receipts remember
the launch id and the created worktree's branch and destination so binding and
verification use dynamic identities.

Build the app, then run and assert:

```sh
S=$(mktemp -d)
cp core/tests/support/fake_hgs_creation.py "$S/hgs"
cp core/tests/fixtures/newsession-smoke.json "$S/config.json"
cp core/tests/fixtures/newsession-smoke-replay.ndjson "$S/replay.ndjson"
chmod +x "$S/hgs"
env -u DISPLAY WAYLAND_DISPLAY=zerus-none GDK_BACKEND=x11 ZERUS_CLIENT_HEADLESS=1 \
  WEBKIT_DISABLE_DMABUF_RENDERER=1 XDG_DATA_HOME="$S/data" \
  ZERUS_CLIENT_REPLAY="$S/replay.ndjson" ZERUS_CLIENT_HGS="$S/hgs" \
  ZERUS_CLIENT_QUERY=newsession-smoke=1 ZERUS_CLIENT_PERF=smoke \
  xvfb-run -a -s "-screen 0 1440x900x24" src-tauri/target/release/zerus-client \
  > "$S/app.log" 2>&1
python3 - "$S" <<'PY'
import json, re, sys
from pathlib import Path
S = Path(sys.argv[1])
text = (S / "app.log").read_text()
marks = {}
for p, kind, ms in re.findall(r"newsession-smoke (\d) (begin|start|end) (\d+)", text):
    marks.setdefault(int(p), {})[kind] = int(ms)
assert set(marks) == {1, 2, 3}, marks
for p in (1, 2, 3):
    assert {"begin", "end"} <= set(marks[p]), marks[p]
for p in (2, 3):
    assert "start" in marks[p], marks[p]
calls = [tuple(row) for row in map(json.loads, (S / "stamped.ndjson").read_text().splitlines())]
def between(p, start="begin", end="end"):
    lo, hi = marks[p][start], marks[p][end]
    return [args for ts, args in calls if lo <= ts <= hi]
def worktree(args, action):
    return (len(args) > 1 and args[:2] == ["worktrees", action]) or \
        (len(args) > 3 and args[0] == "--expect-machine" and args[2:4] == ["worktrees", action])
first = between(1)
for args in first:
    assert args[0] not in {"clone", "swarm"}, args
    assert not worktree(args, "create"), args
    assert args[:2] not in (["dirs", "--create"], ["account", "start"], ["account", "favorite"]), args
    assert "--new" not in args, args
second = between(2)
assert sum(worktree(args, "create") for args in second) == 0
assert sum(args[:2] == ["dirs", "--creation"] for args in second) == 1
assert sum("--expect-machine" in args and "--new" in args for args in second) == 1
assert sum(args[:2] == ["ls", "--json"] for args in second) >= 1
third = between(3, "start")
assert sum(worktree(args, "--creation") for args in third) == 2
assert sum(worktree(args, "create") for args in third) == 1
assert sum(worktree(args, "status") for args in third) == 1
assert sum("--expect-machine" in args and "--new" in args for args in third) == 1
assert sum(args[:2] == ["ls", "--json"] for args in third) >= 1
assert sum(worktree(args, "create") for _, args in calls) == 1
assert '"run":"session.new"' in text
assert text.count("bound row-present true adopted true machine workstation") == 2
print("PASS open+Cancel read-only; no worktree before Start; two catalogs/create/receipt/launch/binding")
print(f"executed stamped calls {len(calls)}; pass1 {len(first)}; pass2 {len(second)}; pass3-after-start {len(third)}")
PY
```

Pass 1 permits only reads: no clone, `swarm`, `worktrees create`, `dirs --create`,
`account start`, `account favorite` or argv containing `--new`. Pass 2 adds one
`dirs --creation`, one guarded launch and a binding `ls --json`, with no worktree
creation. Pass 3 adds two catalog reads (initial and verification), exactly one
`worktrees create`, its receipt read, one launch and a binding read. The log also
proves Ctrl+N routes to `session.new`, and both bound events adopt the exact row
with matching `machine`.

## Actions

The session menus, the header and the Rename and Terminate dialogs run guarded
actions through the `hgs` CLI (`core/src/actions.rs`, `src-tauri/src/actions.rs`),
with Qt's enablement, confirmations and texts (`actions-ui.ts`). A peer's call
is prefixed with `@alias` and uses the longer timeout. Every call is made with
the child's own environment: the agent attribution (`HGS_SESSION`,
`HGS_RUN_ID`) is never passed on.

`core::end_action` supplies the bounded confirmation snapshot and monotonic
five-second timing rules for the Undo integration. Its metadata check requests
no history; its end calls reuse the existing scoped lifecycle envelope and exact
receipt validator. A shell run may have no native conversation. An unidentified
run cannot become a delayed name-only mutation. This pure module performs no I/O.

Projects folder Add/Edit/Remove/Copy/Move use the same shared organization
writer. They compare the original folder identity and fields, preserve unknown
record fields, apply Qt's path/duplicate rules, and change links without creating
or removing files. The form reads the existing folder decoder; Browse and local
Open reuse the maintained directory browser and opener. Folder selection uses
its stable key. Local Windows folder management remains dependent on
zerus-5wmh.66.2.31; these controls do not introduce a Windows folder model.

The native `action_delay` adapter reserves Actions' existing write lane without
publishing a send-block fact. Main-webview registration starts a five-second
monotonic window; its dedicated waiter never reads a source or runs a command.
Undo and window destruction discard reservations; close-to-tray, hide and
minimize retain their deadline. Consuming an
elapsed reference repeats metadata equality and uses the same action, cleanup
or Clear dispatcher, with exact receipt handling. The queue holds at most five
windows of 50,000 targets, with at most 16 MiB of serialized captured metadata
across all windows. Old settled UUID tombstones roll over FIFO at 16,384; active
windows are never evicted. A batch shares
one lane for same-name archives but admits only one active member at a time.
Confirmed Archive, Terminate, Clear session and Forget now use these native
reservations through a lazy countdown controller. Selection Archive/Terminate/
Forget has one window and one toast for the whole batch; Clear archive keeps its
existing immediate confirmed flow. Native timing releases the original writer,
which rechecks the exact captured identity. Browser timers only paint the Undo
label. Ordinary sends and Stop stay outside the grace period.

Five pending Undo notices remain visible and focusable. A sixth action adds the
agreed transient refusal line to the newest one without reserving anything.
Identity changes cancel a single window; a batch refuses only the changed
member. Hiding, minimizing and close-to-tray preserve deadlines. Under the
coordinator's budget ruling for this slice, real Quit/destruction drops pending
windows without effects or durable state; Quit logs one bounded line per dropped group
with at most five session names. Durable quit delivery and next-start settlement remain
in zerus-5wmh.66.2.34.3.

| Action | Arguments (`NAME` is the session name) | Standard input | Timeout here, peer |
| --- | --- | --- | --- |
| Stop (interrupt) | `interrupt NAME --json` | JSON: `request_id`, `expected_run_id`, `expected_conversation_id`, `expected_turn_started` | 15 s, 45 s |
| Mark as read | `read NAME --json` | JSON: `run_id`, `conversation_id`, `reply_id`, `attention_signature` | 15 s, 35 s |
| Mute or Unmute, probe (always on this computer) | `--help` | none | 5 s |
| Mute or Unmute | `notifications NAME --json` | JSON: `run_id`, `conversation_id`, `muted`, `expected_revision` | 15 s, 35 s |
| Pause | `pause NAME` | none | 45 s, 45 s |
| Resume | `resume NAME -d` | none | 30 s, 30 s |
| Start fresh | `resume NAME --fresh [--expected-run-id RUN] -d` | none | 30 s, 30 s |
| Pause all, one computer | `pause --all` | none | 15 s + 30 s per live session, same on a peer |
| Resume all in background, one computer | `resume --all -d` | none | 15 s + 15 s per saved session, same on a peer |
| Rename | `rename NAME NEW [--archive ID]` | none | 5 s, 15 s |
| Move to archive | `archive NAME` | none | 30 s, 45 s |
| Terminate a tracked Codex, Claude or Kimi session | `terminate NAME --expected-run-id RUN` | none | 45 s, 60 s |
| Terminate any other live session | `kill NAME` | none | 5 s, 15 s |
| Forget saved session | `kill NAME` | none | 5 s, 15 s |
| Forget the OpenCode binding | `session-action NAME --json` | JSON: `request_id`, `action` (`forget`), `expected_run_id`, `expected_conversation_id` | 15 s, 45 s |
| Restore session | `resume NAME --archive ID -d` | none | 30 s, 30 s |
| Forget archived session | `kill NAME --archive ID` | none | 5 s, 15 s |
| Archive page (a read) | `archives --json` | JSON: `from`, `until`, `limit` (50), `cursor`, `query` | 30 s, 30 s |

Outcome rules:

- A call is made once and never repeated. A timeout, a failed hand-over or an
  abnormal end is an unknown outcome, shown as a failure and never replayed.
  Stop words them "Request timed out. Check Terminal before retrying." and
  "Transfer failed. Check Terminal before retrying."; the other actions show
  "timed out" or the trimmed standard error `hgs` printed.
- Stop succeeds only when the receipt echoes the request id, session name, run
  id and conversation id of the call (status `submitted`); anything else is
  "Invalid acknowledgement. Check Terminal before retrying." Its notices are
  Qt's: "Interrupt sent to NAME (COMPUTER). Waiting for the agent to stop…",
  "Stop NAME (COMPUTER): the turn already ended or is waiting for input. No
  action needed." when `hgs` answers "This turn is no longer working", and
  otherwise a sticky "Stop NAME (COMPUTER) failed: …" with "Open Terminal to
  check the agent before trying again."
- Pause, Resume, Rename and kill succeed on exit 0. Move to archive and
  terminate also read the single worktree report line on standard error: a
  removed worktree gives an information notice, a kept or unconfirmed one a
  sticky warning with its reason. Otherwise a failure shows the trimmed
  standard error.
- An entry acts on the row as it was when its menu, button or dialog was
  built. If the listed row has another run id by then (or, for a row without
  one, is gone), nothing runs and the notice says "This session changed. Open
  its actions again." Stop, Mark as read and terminate need the row's run id
  (Stop and Mark as read also its conversation id); the other actions name
  the session only.
- One write per session at a time. While it runs the session's entries show
  "Updating…"; another write for that session is a silent no-op (the Rust side
  answers `busy`). Stop, Mute and the Rust worker's read marks each run in their own
  lane: Stop waits only for its own interrupt, as in Qt, and neither a Mute nor
  a read mark holds up a write. Mark as read stays available while a write runs.
- Terminate stops a tracked Codex, Claude or Kimi session into Archive and
  kills any other live session; the confirmation says which. After the
  confirmation (and after a menu's Pause or Resume) the session must still be
  running, or still not running, as it was; otherwise nothing runs and the
  notice says "The session state changed. Open its actions again."
- Rename edits only the tag after the agent and project, with Qt's tag rule
  ("Enter a session name.", "Remove spaces at the beginning or end.", "Names
  cannot contain /, : or . or control characters.") and "This name is already
  in use on COMPUTER." for a live session of that name on the same computer.
- Stop is shown while the selected session's turn works (`working`, `tool` or
  `compacting`) and enabled when its computer is online and the details say
  `interrupt_supported`; it sends the details' run, conversation and
  `turn_started`. Hosts never list `interrupt` among the allowed actions.
- Details (allowed actions and their reasons, `auth_required`, Stop's fields)
  come from the followed session's Activity `state` value when it names the
  row's run and conversation. Only when the subscription answered without one
  does the client run one read-only `inspect`, refetched when the run,
  conversation or state changes, and never while the computer is offline.
- Read state is local first, as Qt's FleetState keeps it (`src/store/marks.ts`).
  This device keeps attention marks (marked for review, the acknowledged
  attention) and, for hosts without shared read state, the replies it read.
  Each row is shown with them applied: a reply is unread until the host's
  read mark or a pending read mark of the same run and conversation names it
  (on an older host, until this device read it); an attention the host signs
  is acknowledged by the host's mark or a pending one, and one without a
  signature by this device's mark for its fingerprint. Acknowledged attention
  shows its acknowledged status, and an unread reply blanks the current action
  unless the session needs action. A mark names the reply, request and
  attention it was made for, so a newer one shows again. The store's Mark as
  read, Mark needs attention and Mark all as read change the rows at once, save
  only the changed entries (`update_attention_marks`, after the stored marks
  have loaded) and queue read marks for hosts with shared read state
  (`queue_read_marks`). A host that starts sharing read state gets this
  device's older marks for its current replies and attention queued once.
- Pending read marks are kept in `read-marks.json` in the app data directory
  (`app_data_dir`, e.g. `~/.local/share/<bundle id>/` on Linux; a replay uses
  `read-marks-replay.json`), so a restart keeps them. A mark is settled, and
  dropped, once a row shows another run or conversation, or each of its parts
  unset, no longer current or read by the host; a complete snapshot of its
  computer that no longer lists the session drops it too (an empty one never
  does). The window keeps a copy for the rows: loaded at start, replaced by
  each `queue_read_marks` answer (an older answer never undoes a newer mark)
  and settled by the same rules. Nothing in the window sends a mark again.
- Read marks reach their hosts through a worker in the Rust app, as Qt's
  `ReadMarkSync` does. `queue_read_marks` merges marks in memory and wakes it;
  otherwise it wakes every 5 s. Each wake first writes new marks to
  `read-marks.json`, so a mark is saved before any call. At most four
  `hgs read` calls run at once, each free slot going to the computer with the
  fewest. Until a snapshot settles a mark, a delivered one is sent again after
  120 s (doubling to 960 s while its host applies only part of it) and a
  failed one after 10 s, doubling to 300 s; a changed mark goes at once. Each
  call takes its session's read lane; a busy lane leaves the mark for the next
  wake. A delivery shows no notice. Once this computer's listing names its
  peers, marks for a computer it no longer names are dropped (this computer's
  stay). A replay without `ZERUS_CLIENT_HGS` sends nothing.
- This device's attention marks and the read replies of hosts without shared
  read state are kept in `attention-marks.json` (a replay uses
  `attention-marks-replay.json`). `update_attention_marks` writes or removes
  entries in memory; each map keeps its 20 000 newest written entries, in the
  window too: `attention_marks` answers each map's keys oldest written first,
  and the window writes one update's entries in key order as the Rust side
  does, so both drop the same entries. A Mark needs attention made before the
  stored marks arrive changes only `review`. A writer thread reads the file at
  start and saves the latest state after each change, listing entries in write
  order, so the window never waits for the disk. The computers whose older
  marks were queued are remembered in the webview's local storage. Outside the
  app (a browser) there is no pending list, and the window keeps its own.

- Start fresh is offered for a paused or stopped Codex, Claude or Kimi session
  of an online computer with no write pending. The dialog "Start fresh?" says
  that the agent starts in the same folder with the same account and model
  without the current conversation context, and its default button is Cancel.
  On confirmation the row must still qualify and be the same run and
  conversation; otherwise nothing runs and the sticky notice says "The session
  changed. Open its actions again." Otherwise the call passes the run as
  `--expected-run-id` (omitted when the row has none) and the notice says "Starting NAME with a
  new conversation…". The header offers it in the main context only, in place of
  Clear session, and not at all while a subagent is open.
- Forget saved session ends a saved session that is not running, or an OpenCode binding (usually running). The dialog
  "Forget saved session?" says "Remove NAME from HGS? The agent's native
  conversation history is kept." and, for an OpenCode session that only keeps
  a binding, "Forget NAME in Zerus? The conversation stays in OpenCode; only
  Zerus forgets it." with the computer named below; Cancel is the default.
  On confirmation the session must be as when the dialog opened (still not
  running unless it is an OpenCode binding, the same binding-only case and, for a binding, the same run and
  conversation); otherwise nothing runs and the sticky notice says "The
  session state changed. Open its actions again." A binding with a run goes
  through `session-action`, any other session through `kill`; an offline
  computer ends silently; a vanished row raises the same notice.
- Forget archived session needs the archive id and an allowed forget. The
  dialog "Forget archived session?" says "Remove this archived instance of NAME
  from HGS? Other sessions with this name and the agent's native conversation
  history are kept." with the computer named below; Cancel is the default. On
  confirmation the archived row must still be listed as the instance the dialog
  named (its archive id and name, and `created` when known), otherwise nothing runs and
  the sticky notice says "This session is no longer available. Refresh the list
  and open its actions again."; an offline computer or a running batch ends
  silently. The call is `kill NAME --archive ID`; after a success the archive
  pages reload, and a pending restore's follow is cancelled.
- The binding forget counts only when the receipt echoes the request id,
  session name, run id and conversation id of the call with status
  `completed`; anything else is a failure and is never replayed. Success shows
  "Zerus forgot NAME. The conversation stays in OpenCode."
- Start fresh and Forget saved session first recheck the listed row when the
  entry is clicked: a session that is gone or whose conversation or `created` changed opens
  no dialog and shows "This session is no longer available. Open its actions
  again." Restore session and Forget archived session do the same, and also
  for an archived row listed again under another name (the client keys an
  archived row by its archive id; Qt's key also holds the name).

Restore session is an archived row's change entry. It is enabled when the
computer is online, no write is pending, the row is resumable and has its archive
id, and no live row of that computer has its name. As in Qt's header and the
menu of the selected row (a right-click selects the row), it does not read the
row's allowed operations; a batch's Resume / restore does. The call is
`resume NAME --archive ID -d`. While the same session's restore is pending, another single restore runs or
64 restores are pending, nothing runs and the sticky notice says "Finish or
refresh the earlier restores before restoring another session." A restore stays
pending until its live (running) row appears, or until its call fails.

- After a success the archive pages reload, and the Archive filter becomes All
  when the restored row is still selected there; the search, machine and project
  filters stay. The archived row stays selected until its live row appears.
- When the live row appears with the restored conversation, the selection moves
  to it (also when it carries no conversation at all). With another
  conversation the selection clears with the sticky notice "The restored
  conversation could not be verified." Qt adds that the archived draft remains
  in Saved drafts; the client has no saved drafts before phase 4
  (`zerus-5wmh.9`).
- A change of the state filter or the search, a Rename, a Forget archived
  session or a batch cancels the follow: the verified live row then only clears
  an old selection of the archived row.

A successful Rename or Move to archive also reloads the archive pages, as Qt's
`writeDone` does. Sending, the Mark read chip of the composer toolbar and the
Open terminal entries arrive with phase 4. The copy entries below are live.

### Batches

Selecting more than one row shows the selection bar above the list (the client
has no single-row bulk mode). The same actions are in the context menu of a
selected row, under the heading "N selected sessions", followed by "Deselect all".

- Tools, in this order: Compact session rows, Expand session rows, Mark as
  read, Mark needs attention, Pause, Resume / restore, Move to archive, Move to
  project, Forget, Terminate. The tool text is "ACTION (COUNT)" and the hint
  "ACTION — COUNT of N selected sessions". A tool with a count of 0 is hidden
  from the bar; the others that do not fit move into "More actions for selected
  sessions". The close button has the hint "Deselect all".
- A tool is enabled when its count is above 0, and, except Compact and Expand,
  when no batch is running. The count is the number of selected rows the action
  applies to, by Qt's rules: Compact and Expand follow the row preference; Mark
  as read needs a row that is not archived and needs attention, Mark needs
  attention one that does not; every other action needs the computer online, no
  write pending for the session and an operation the row allows (Resume / restore
  is Resume for a live-listed saved session and Restore for an archived one);
  Move always applies. Only listed rows count: a search or filter hides rows from the selection, and vanished sessions drop out. An archived Resume
  also needs the archive id and no live row of that computer with its name,
  checked against one index of the live names.
- Move to project is shown with its count, disabled, with the hint "Arrives with
  project organisation." until the organisation stream's move lands.
- Forget and Terminate first confirm. The title is the action, the text
  "Forget N selected sessions?" or "Terminate N selected sessions?", then
  Qt's suffixes: what is removed or stopped and what is kept, the OpenCode line
  when bindings are forgotten, and "K unavailable or inapplicable sessions will
  be left as they are." when some selected rows do not apply. "Show details"
  lists "MACHINE / NAME" per target. The buttons are Cancel (focused and the
  default) and "Forget N sessions" or "Terminate N sessions"; Cancel runs nothing.
  Qt's "Remove working folders after stopping" checkbox and its "Stop N
  sessions" labels arrive with `zerus-5wmh.10`.
- Mark as read and Mark needs attention run through the marks store at once and
  report "ACTION: C of N sessions updated."; an error notice that stays when
  the counts differ.
- The other actions run one session at a time. Before each call the row is
  read again by its key and must be the same instance as when the menu opened
  (name, archive id, `created`, run, conversation and state) and still
  apply; otherwise the item fails with "MACHINE / NAME: session changed or is
  unavailable" and no call is made. Each item shows "ACTION: I / N…"; the end
  shows "ACTION: D of N completed." and, when items failed, "F failed or
  changed; refresh before retrying." with the first three errors, as a notice
  that stays. A call is made once and never retried, and a failure does not stop
  the next item, also when it throws. The calls are the single-session ones of
  the table above, with their own success and failure notices replaced by the
  batch's. An archived row restores (past the restore limit, also for a second
  instance of a name restored earlier in the batch, it fails with "NAME: earlier
  restores still need a refresh") or forgets its archived instance. When a write batch ends, the archive pages reload
  (Qt's own reload after a batch never runs, so this is a deviation).
- While a batch (or a Pause all or Resume all) runs, every write entry is
  disabled, the header shows "Updating…" and a second batch does not start. Stop
  and the copy entries are not writes.

Remove loaded archived sessions… in the batch menu is Qt's Clear archive. It is
enabled when no batch runs and some loaded archived row in the machine and
project filters can be forgotten. It runs Forget on the loaded archived rows in
those filters and the date range, with the title and button "Clear archive" and
the text "Remove N loaded archived sessions in the current date and machine
scope? Unloaded entries are kept." plus the Forget suffixes.

Pause all and Resume all are in the batch menu, one group per computer: a
heading with the machine name, "Pause all (LIVE)" and "Resume all in background
(SAVED)". LIVE counts the running sessions, SAVED the resumable ones that are
not running; archived rows are skipped. Pause all is enabled when the computer
is online, LIVE is above 0 and every live session is idle and resumable and
whose process has not exited; otherwise "Some sessions are busy or context is not
tracked" appears. Rows of OpenCode or a host without bulk lifecycle are not
counted and show "OpenCode uses individual Pause / Resume controls" with the
hint "OpenCode sessions are excluded; use their individual Pause / Resume
controls." The calls are `pause --all` and `resume --all -d` (table above), take
the computer-wide write claim and name no session. They show "Pausing sessions
on MACHINE…" or "Resuming sessions on MACHINE…", then "Session updated.
Refreshing…" or the trimmed standard error as a notice that stays, and are never
retried.

### Mute and Unmute

The entry changes the notification preferences a host shares with all devices.
It first runs `hgs --help` on this computer (5 s) and refuses unless a line
of its output is exactly `session-notifications-v1`; it then runs
`hgs [@alias] notifications NAME --json` with `run_id`, `conversation_id`,
`muted` and `expected_revision` on standard input (15 s here, 35 s on a peer).
The change counts only when the reply is exactly `ok`, `applied`, `run_id`,
`conversation_id` and `preferences`, names this session's run and
conversation, carries the requested `muted`, and has revision
`expected_revision + 1` when applied or the same revision when not. The
entry is disabled, with its reason in this order, when the computer is offline,
the host does not share notifications, the row's identity is missing, the
preferences conflict or a change is already pending. There is one pending
change per conversation and 16 in all. The change has its own lane: it never
waits for a write, Stop or a read mark. A confirmed answer is kept as an
override of the listed row until a listing shows a higher revision; a late
answer for a replaced run or conversation is dropped.

### Mark all as read

The entry "Mark all as read (N)" is in the batch menu, the list footer button
and the Rail Sessions menu. N counts every unread reply on every computer,
hidden sessions included, except sessions marked for review; at 0 the entry is
disabled. A run marks those replies read and shows "1 reply marked as read."
or "N replies marked as read."

### Viewed reply

As Qt's `checkViewedReply`, the selected session's unread reply is read once
it stayed in view for a second (`src/store/viewedReply.ts`, checked every
400 ms while the window shows). The window must be shown, not minimised and
focused, the Sessions page current and no modal dialog or popup open; a
subagent's Activity, an archived row and a computer that is not online never
count. The followed inspection must have succeeded and name the row's
conversation and reply. Activity must be within 30 px of its end, show no
search result or deferred layout, and have the reply's row start in the
viewport (when that row is not rendered: an empty final Stop of an idle
session, or loaded events at or past the reply). Terminal must be connected,
at its bottom and not blank. Any change of the session, conversation or reply
starts the second again. Only that reply is read (`markReplyRead`): the
attention and Review later stay, and a read mark is queued as for Mark as
read. Nothing scrolls. The open row then stays under Needs attention and
Active until another row is selected, as it does after Mark as read and Mark
all as read.

### Copy actions

The copy entries build text locally or ask `hgs` for it and put it on the
clipboard. They are not writes: they need no row lock and show no "Updating…".

| Entry | Text |
| --- | --- |
| Copy connect command | `hgs [@alias] a NAME` |
| Copy restore command (archived row) | `hgs [@alias] resume NAME --archive ID` |
| Copy session reference | the `zerus://session?` URI printed by `hgs [@alias] reference NAME --json --run RUN --conversation CONVERSATION [--archive ID]` (15 s here, 25 s on a peer) |
| Copy folder path | the `cwd` of the selected session's details, else the row's; disabled when empty |
| Copy session name | `NAME` |

- The commands are quoted with Qt's SessionCommands rules (POSIX shell; PowerShell
  on Windows) and name the `hgs` the client runs: `ZERUS_CLIENT_HGS`, else the
  one found on `PATH` or in the install folders.
- The reference takes the run and conversation of the selected session's
  details, else the row's. A reply must be a `zerus://session?` URI of at most
  8192 characters; otherwise the notice says "Could not read a session
  reference. Update hgs on this computer." Success shows the toast "Copied",
  "The session reference is on the clipboard".
- The clipboard is written from Rust. Linux tries Klipper's
  `org.kde.klipper.klipper.setClipboardContents`, then `wl-copy`, then the
  native clipboard plugin with exact read-back. Calls run off GTK's thread;
  Klipper and the front helper have Qt's three-second timeout. The helper
  gets exact text on anonymous memory-backed stdin, including leading dashes
  and trailing newlines for verbatim copies; no copied body goes to argv or
  disk. Its donor keeps null output streams and survives until the next
  clipboard selection. No Qt clipboard or service is required. A text
  that is empty or holds a line break is refused with Qt's texts. A failure
  shows the sticky toast "Not copied" with the error. A success shows no
  notice, except for Copy session reference and the header's ID copy ("Copied",
  as the design's toasts carry a title). The browser build (no Tauri) copies
  through `navigator.clipboard` with the same two checks.
- Each copy rechecks its row when it runs, as Qt does: a session that is gone
  or whose conversation changed copies nothing and shows "This session is no
  longer available. Open its actions again."; otherwise the current name,
  archive id, folder and run are copied.

### Archive

The state filter Archive lists archived sessions page by page, as Qt's
`ArchiveBrowser` does. The rows are loaded on demand and are never part of the
live catalog.

- Each live computer inside the machine filter has one page of at most 50 rows.
  Older and Newer replace the page (a history never grows), and Refresh archive
  drops the pages and loads the first. Pages load only while the Archive filter
  shows the Sessions list and are kept when it is left, as Qt keeps them.
  Leaving the filter, a change of the computers or their names, a change of
  the search text, a change of the project filter or date range and Refresh archive cancel the
  reads that are running, also one sent before the cancel that has not started
  yet; an answer that arrives afterwards is dropped. Each read carries the
  store's request number, so a late older read never cancels a newer one.
- The archive metadata query is applied 300 ms after the last text change.
  Public-content search adds matching message excerpts after 350 ms; while it
  is active, the date picker stays visible and content results replace the
  metadata page controls. Clearing the query returns to the paged archive.
- The date picker offers All dates (the default), Today, Last 7 days, Last 30
  days and a custom range, which starts at the last 30 days. A range whose
  start is not before its end loads nothing.
- The status line reads "Loading archive…" while a page loads, then
  "{a}–{b} of {total}" or "{shown} shown of {total}" for a single computer
  with a window, "{shown} loaded of {total}" once every total is known and
  "{shown} loaded" before. Each computer's window is in its tooltip. An
  error reads "{machine}: {error}" (240 characters shown, the full text in
  the tooltip). A read that fails reads as Qt words it: "timed out",
  "cannot start '{hgs}': {error}", the standard error (4000 characters at
  most) or "exit code {n}". Older and Newer lose their text below 310 px. The
  count chip of the filter shows "…" while some computer's total is unknown and
  no archived row is counted, otherwise the count with a "+" while more can be
  loaded; its tooltip says how many are loaded.
- The list groups loaded archive identities by project, in organization order,
  newest archived first inside each project (SessionsWindow.cpp:3238-3242).
  Machine and project choices count the loaded archive rows inside the date
  range before their corresponding filter applies (SessionsWindowRows.cpp:266-284).
  Machine choices come from the fleet, including machines with no loaded rows.
  Selecting an archived row shows its header
  and its details, read with `inspect --archive ID`, and its recorded Activity
  (below). Restore session, Forget archived session, the copy entries and
  Rename work on a loaded archived row (see Actions).
- The page is checked as Qt checks it. An invalid reply shows one of three
  texts: "Invalid archive page. Update the computer CLI and refresh." (version,
  shape, cursors, total or echo of the query), "Invalid archive window. Refresh
  the archive." (position or previous cursor) and "Invalid archived session
  identity. Refresh the archive." (state, name, lowercase hyphenated UUID or
  `archived_at` outside the range).

The selected archived row's Activity is read as Qt reads it
(`SessionsWindow::inspect` and `acceptInspection`), by its own store
(`src/store/archiveActivity.ts`): an archived row is never streamed, so while
it is selected and the Sessions page shows in a shown window it is read with
`inspect NAME --after CURSOR --skip-processes --archive ID` (5 s, 15 s on a
peer) 2500 ms after the previous read started, one read at a time; a read of
the same row that is still running answers "busy", which is ignored and tried
again 500 ms later. A new selection starts at cursor 0. A reply with
`replace_events` replaces the events; otherwise events at or below the cursor
or already shown are skipped, the rest are appended in the reply's order and
the cursor becomes the reply's. A reply for another conversation than the shown
one (unless its `cleared_conversations` lists the shown one) replaces the
events and is read again from cursor 0 at once. At most 500 events are kept,
the oldest dropped first, as Qt keeps them. A failed read shows "Could not
refresh activity: {error}" as a notice until the next answer; that notice is
for read-only sources only, and the live pane shows no follow error. The row of
a pending restore stays selected and read while the archive reloads, as Qt's
`selected()` keeps it. While the row's computer is offline nothing is read and
the events stay, as Qt's `inspect()` waits for it; the reads resume once it is
back. Leaving the row, another page or a hidden window stops the reads and
drops the events. The pane renders through the same Activity view, read-only:
the transcript without the live tool line, the attention, error and offline
banners or a composer, and no Terminal tab, as Qt shows none for an archived
row.

### Actions smoke run

`ZERUS_CLIENT_QUERY=actions-smoke=1` (hook in `src/ui/PerfHost.tsx`, beside the
perf hooks) checks the actions end to end without the real `hgs`. It starts only
when the native side reports replay mode (`ZERUS_CLIENT_REPLAY`), an explicit
`ZERUS_CLIENT_HGS` and `ZERUS_CLIENT_PERF` (any value but `auto`; `exit_app` is
allowed only in a measurement run); otherwise it logs why and does nothing. In
replay mode `run_action`, `inspect_session` and `session_reference` refuse unless
`ZERUS_CLIENT_HGS` is set ("Actions in replay mode need ZERUS_CLIENT_HGS."), so
a replay never drives the real CLI.

Put `core/tests/support/fake_hgs_actions.py` as `hgs` in a scratch folder with a
`config.json` beside it containing a `replies` list (the first entry whose
`match` is a prefix of the arguments answers; `reply` answers the rest):
`reference` gets a URI, `session-action` gets `echo_session_action` with status
`completed`, and every other call is a plain success (the write actions ignore
standard output, and exit 0 is a success for them; `echo_interrupt` parses
standard input and fails on writes that have none). The fake logs each call to
`log.ndjson` beside it. Run it only with the headless environment:

```sh
S=$(mktemp -d); cp core/tests/support/fake_hgs_actions.py $S/hgs
echo '{"reply":{"exit":0},"replies":[{"match":["reference"],"exit":0,"stdout":{"uri":"zerus://session?machine=m&name=claude%2Fsample-project%2Freview"}},{"match":["session-action"],"exit":0,"stdout":"echo_session_action","receipt_status":"completed"},{"match":["archives"],"exit":0,"stdout":{"version":1,"from":0,"until":253402300800,"query":"","next_cursor":"","previous_cursor":"","total":1,"position":0,"sessions":[{"name":"claude/sample-project/archived","state":"archived","resumable":true,"archive_id":"00000000-0000-4000-8000-000000000001","archived_at":1700000000}]}},{"match":["inspect","claude/sample-project/archived"],"exit":0,"stdout":{"events":[{"seq":1,"type":"UserPromptSubmit","source":"claude_hooks","detail":"Summarize the sample notes","at":1700000000},{"seq":2,"type":"Stop","source":"claude_hooks","detail":"The sample notes are summarized.","at":1700000030}],"cursor":2}},{"match":["resume","claude/sample-project/archived"],"exit":0},{"match":["kill","claude/sample-project/archived"],"exit":0}]}' > $S/config.json
env -u DISPLAY WAYLAND_DISPLAY=zerus-none GDK_BACKEND=x11 ZERUS_CLIENT_HEADLESS=1 \
  WEBKIT_DISABLE_DMABUF_RENDERER=1 ZERUS_CLIENT_REPLAY=$PWD/fixtures/replay.ndjson \
  ZERUS_CLIENT_HGS=$S/hgs ZERUS_CLIENT_QUERY=actions-smoke=1 ZERUS_CLIENT_PERF=smoke \
  xvfb-run -a -s "-screen 0 1440x900x24" src-tauri/target/release/zerus-client
cat $S/log.ndjson
```

The `archives` entry answers the Archive step with one archived row (the page
echoes the All dates range and an empty query); the `inspect` entry for that
row answers its details and its Activity reads (an object with no run or
conversation, as the row has none, and two synthetic events). The `resume` and
`kill` entries for that row answer the Restore and Forget archived steps with a
plain success.

The hook first switches to the Archive filter, waits for the page and logs
`actions-smoke archive-page: <status text> (<row count> rows)`. It then opens
the date panel while the list panel carries a transform (as the strip's peek
does) and logs `actions-smoke archive-date-panel: done (...)` when the panel is
in the page's body at the picker button and Escape closed it with the focus
back on the button. Over a hover-opened strip peek it then logs
`actions-smoke archive-date-panel-peek: done (...)` when Shift+Tab and Tab wrap
inside the panel, Escape with the focus outside it closes only the panel (the
peek and a selection stay), leaving the list with the panel open keeps the peek
(as Qt keeps a hover peek under a popup) and, once it closed, the next leave
closes the peek. It selects the archived row, waits up to 15 s for its
details and logs `actions-smoke archive-inspect: done <archive id>` (or
`failed`); the fake's `log.ndjson` then holds `archives --json` (with the page
query on standard input) and an `inspect NAME ... --archive ID` call. It then
waits up to 15 s for the row's Activity to show the fake's events and logs
`actions-smoke archive-activity: done (N events)`, or `failed (...)` when no
event shows, the row is followed live or the pane shows a notice or a live line
(`log.ndjson` holds `inspect NAME --after 0 --skip-processes --archive ID` for
the details and the first Activity read, and after the hook waits one poll the
same call with `--after 2`). On the same archived row it then runs the Restore
session entry's path and logs `actions-smoke restore: done (state filter now
all)` (the Archive filter becomes All, as the row is selected and followed;
`log.ndjson` holds `resume NAME --archive ID -d`), Forget archived session
through the dialog's confirmation path and logs `actions-smoke forget-archived:
done` (`kill NAME --archive ID`), and Clear archive: it takes the batch menu's
entry, presses the confirmation dialog's "Clear archive" button and logs
`actions-smoke clear-archive: done (...)` with the notices of the batch (a
second `kill NAME --archive ID`), or `failed (... is disabled)` when the entry
is disabled, since the fake's row can be forgotten. The fake keeps answering
the archived row; the hook shows the Archive filter again after the restore, as
the pages load only there, so each step finds the row after the archive
reloads. It returns to the All filter and the replay session. The hook selects
`claude/sample-project/review` (its details come from the replay's Activity
state value or, as here, one `inspect` of the fake), first marks it as read,
marks all as read (logging `actions-smoke mark-all-as-read: N marked`) and
marks it as needing attention, through the marks store the menus use
(`actions-smoke mark-as-read: done`, `mark-needs-attention: done`; they run
no `hgs`), then copies the session reference, the connect command, the session name and the
folder path through the store the menus use (the first calls `reference`; the
fake's URI is the clipboard text), logging each as
`actions-smoke copy-<entry>: done <the exact clipboard text>`, then runs start
fresh (`resume NAME --fresh --expected-run-id RUN -d`), forget (`kill NAME`) and
forget binding (`session-action NAME --json`, answered with a completed
receipt) through `perform`, which the dialogs' confirmations call on a row that passed their
rechecks (the replay row is running and not an OpenCode binding, so `forget` and the binding
forget skip the recheck; `actions-smoke start-fresh: done`,
`forget: done`, `forget-binding: done`), then runs pause, resume, rename (to `claude/sample-project/review-renamed`) and archive,
then the end call the Terminate dialog picks for the row through the dialog's
confirmation (`kill`, since the replay row is not tracked), then the other end
call (`terminate`), once each through the store the menus use. It logs each
outcome to stderr (`zerus-client perf: actions-smoke ...`) and exits 0, or 1
when an outcome failed.

Then the batch steps, before the marks: `run("review_later", [row])` and
`run("mark_read", [row])` through the batch store the selection bar uses
(`actions-smoke batch-review-later: ...` and `batch-mark-read: ...` log the
notice text, or that the step was skipped because the action does not apply to
the replay row by Qt's rules), and Pause all and Resume all for the row's
computer, enabled by the same `machineEntries` rules as the batch menu and
started through `runMachine` (`actions-smoke pause-all: ...` and `resume-all:
...`; a started call puts `pause --all` or `resume --all -d` into `log.ndjson`).
A step the rules disable logs `skipped (...)` instead of being forced. In the
replay fixture the row already needs attention (Mark needs attention is
skipped), Pause all is disabled because some sessions are not idle, and Resume
all has no saved session, so only `batch-mark-read` runs; the two calls are
covered by the core's fake-hgs test.

The marks must survive a restart. Mark needs attention runs last of the three
so the first run ends with the session marked for review, and the replay keeps
its marks in `attention-marks-replay.json` in the app data directory. A second
run with `ZERUS_CLIENT_QUERY=actions-smoke=marks` (same command, the same
`XDG_DATA_HOME` or app data directory as the first) only waits for the replay
session and logs `actions-smoke marks-after-restart: review true` (exit 0) or
`review false` (exit 1) once the stored marks have applied. Give both runs one
scratch `XDG_DATA_HOME` so a smoke run never touches the real marks files.

## Terminal

The Terminal tab follows Qt's `TerminalView` and `PtyProcess` lifecycle. Showing
it connects one attach per view. Selecting another session retargets it, leaving
the Sessions page or hiding the window disconnects it, and hiding only the tab
keeps the attach alive for an exact redraw. A rename of the same run keeps the
attach. Rust retains at most 1 MiB of delivered output per view, delivers raw
bytes over the attach channel, and allows at most two unacknowledged messages;
the webview acknowledges after xterm parses each one. Resizes are coalesced to
one per 100 ms plus the trailing size, and unchanged sizes never reach the PTY.
Input is bounded to 4 MiB and Terminal output messages to 1 MiB plus one read
batch. Paste is cleaned as Qt's `TerminalScreen::pasteText` does.

File drops follow `SessionFileDrop` and `SessionsWindow.cpp:2618-2659`. A local
session gets quoted paths pasted without an automatic Enter. A peer's files are
staged first with `Action::StageFiles`; the paste still names the attach
generation and is refused if that attach changed or the Terminal is hidden.
Other tabs attach dropped files to the current main or child composer. The
drop must stay over the detail panel, retain its entering target
and contain only regular files. Windows keeps the T1b refusal text until native
Terminal support lands.

## Processes

Processes is off by default, as in Qt. Settings > Processes stores
`processes/enabled`, `activeSeconds`, `backgroundSeconds`, `unknownSeconds` and
`loadingSeconds` in the app's ordinary settings store. When enabled, the tab
follows only the selected session's bounded `processes` topic. It never follows
an archived row, an offline or paused session, an exited process or a failed
inspection. Output is read only while the tab shows, at Qt's state-dependent
cadence (2.5 s for active work and 10 s for unknown work). Durations and busy
state tick four times a second, and up to 30 session views are retained.

Stop requests run sequentially and are never retried. Selection and stop calls
name the view, selected ids and session identity, so a late click cannot stop a
process in a newer view. The stop queue is cancelled by a changed session,
conversation or host generation. These paths follow `ProcessView.cpp` and the
core's `process_view` rules.

## Composer and Send

The composer keeps the text protocol in Rust and the webview in sync: edits
carry a growing revision and the current text epoch after a 250 ms coalescing
window, on blur and before attach, paste, remove, Undo or Send. Rust applies
only newer revisions of that epoch. Its own text changes advance the epoch and
hand text and selection back to the webview. Window hiding and page exit flush
pending edits too. `next_batch` adds the open composer view, optional process
view, bounded draft keys and at most 16 composer events. Rust recomputes block
reasons when details or action facts change and refreshes the cache chip once a
second. The hub keeps at most 64 composers and 128 MiB of their attachments,
500 followed events, 50 local rows for each of at most 512 sessions, and draft
marks from the 200 newest drafts plus the composers in memory.

Send follows `MessageComposer.cpp` and `SessionsWindow.cpp`: session and global
write blocks, attachment locks, a cold-cache choice, resume or start fresh,
compact then send, and local submitted or failed rows. A continuation's waiting
draft is saved before its start or compaction call, and every step checks the
same session, run, conversation and draft revision. Each delivery gets one new
request id immediately before its one dispatch. Message bytes go only to the
standard input of `hgs send --json`, never argv. An uncertain delivery keeps the
draft and its warning and is never sent again automatically, including after a
reload, restart or another question's answer. Allow retry is explicit, and a
definite failure's retry uses a new request id.

Drafts live in the app's one store at `<app data>/drafts`, or `drafts-replay`
in replay mode, shared with voice. The previous Qt store is read but never
written, including recovery. The one exit hook closes voice and the composers,
then flushes this store on quit and termination signals. A send whose durable
save has not started cannot dispatch during shutdown.

### Subagents

The catalog retains at most 256 native children per session, never the
hook-profile fallback. The list follows Qt's completed-child rule, retaining
the selected child and nongroup children with drafts. A child Activity is one
bounded replacement page of at most 200 events from `inspect --subagent`, read
only while that exact child is visible and online; it never inherits the
parent's live state or grows a history. Child attachments carry the same exact
agent identity.

A child with a matching inspection and `send_supported` gets the ordinary
composer layout and durable draft under
`computer\nsession\nparent-conversation\nagent-id`. Send names the parent run
and conversation plus `agent_id`, rechecks that identity and support before
dispatch, and keeps its local delivery row under the same parent identity.
Read-only children hide the editor and actions. Parent lifecycle, model, Stop,
cache/account, question, queue and push controls never cross into a child.
Navigation preserves parent and child drafts independently.

## Account usage and automatic recovery

The session header shows the bound account's usage meters and Refresh action.
The screens worker reads `hgs [@host] account inspect --session NAME --refresh`
on the same five-minute schedule and two-slot queue as profile usage, retaining
at most 64 run-scoped snapshots. The composer's Usage limit chip shows exhausted
limits and the current Claude recovery notice; its popover keeps cached values
visible during refresh and offline states. The Context chip opens the Session
usage section in a popover, with no side panel required.

The Recovery chip and panel use only the followed inspection's recovery job and
provider error. Immediately before Retry now or Cancel retry, the action lane
reads a fresh full inspection and refuses unless it still names the exact
session, run, conversation and waiting job. It then makes Qt's
`hgs [@host] recovery action` call with `{name, id, action}` and accepts only
the matching updated job. A failed, uncertain, timed-out or malformed outcome is
never replayed. Attempts, Open Terminal, Refresh usage and Settings follow Qt's paths.
No additional background poller runs for these chips; the durable fanout remains
G1's work.

## Attachments

A message can have up to eight nonempty attachments, at most 10 MiB each and
20 MiB together. Pasted images must be below 32 megapixels. Attachment bytes
never cross the IPC as JSON: reads and pasted images use raw bodies, and views
open files through the `zerus-file` scheme. The scheme serves only canonical
regular files in the private cache or bytes of an open draft, to the main
webview, with HTTP Range support for seeking.

Activity reads share one job per file, with at most three reads at once and Open
ahead of thumbnails. The last 256 read paths can answer from their copies. The
cache is bounded to 512 MiB, least recently used first; visible previews,
playing media and files needed by a failed Retry are pinned. Image thumbnails
need a known size of at most 64 million pixels. A vanished preview is fetched
once more before showing its failure.

Activity Open marks the cached file as downloaded from another machine. Draft
Open keeps this computer's origin and refuses while the draft sends. A file
whose type runs programs is revealed in the file manager instead of opened.
These rules follow `AttachmentViewer.h`, `MessageComposer.cpp` and
`ExternalFile.cpp`.

## File references

Activity's existing File reference dialog keeps destinations behind a Rust
id. Download… uses the native Save local copy dialog; Download and open uses
Settings > Files' configured DownloadCache root. Automatic requests join by
(root, machine, remote path), with a requested refresh winning over an older
cache lookup. The existing cache worker owns lookup, reserve, publication and
trim; one timer repeats trim after the opening grace period plus five seconds.

Downloads continue after Close or Escape. A successful requested open still
runs, a manual save reports its destination, and a failed closed download posts
a sticky warning through the window's notice bus. Local opens retain this
computer's origin; downloaded copies use the shared external-file safeguards.
The established dialog frame follows the mockup; the missing file actions and
states follow Qt, as approved for the #521 shell. SSH terminal stays with its
separate action owner.

On Linux, after building the frontend and the media/custom-protocol app, run
from the repository root:

```sh
python3 client/scripts/file-reference-smoke.py --app target/tauri/debug/zerus-client
```

This walk creates private D-Bus, Xvfb, HOME, XDG, temporary and tmux directories
from a fresh environment, selects a fake hgs, and mocks open/show effects. It
walks the real native save chooser, cached open/show, failure, Escape/focus and
completion after close. Captures and logs remain in its printed scratch root.

## Questions and the native queue

Question cards keep the native question id, hash, run, conversation, schema and
form revision. Their drafts use Qt's `question\n…` keys in the same store as
message drafts. A required card replaces the editor; an optional one leaves the
ordinary composer available. Submit saves first, then sends once through the
guarded answer path. A changed card never submits the old answers. A broader
approval needs its second Confirm, and Approve once waits out its three-second
review while the card is active. Cold-cache consent names the exact card.
Uncertain answers remain marked and never replay automatically.

The native queue panel offers Send now only for the current queue id and an
online, answerable session. It reports the requested send and then refreshes
the queue. Send and push now uses the composer chord, Ctrl+X Ctrl+S, or
Command+X Command+S on macOS. It sends the message once, then only the queue
snapshot that contains it; it never sends the text again. Another selection,
20 seconds, failure or an uncertain outcome ends the push. OpenCode's inbox and
its guarded Remove action arrive with A2-6.

## Phase 4 smoke run

DeepSeek's Native UI follows the mockup's session detail tab and existing
question, local-message, recovery, sign-in and session-menu routes. Linux
embeds a bounded cache of four incognito launch profiles in the main window;
macOS, Windows and unsupported web engines use the existing browser route
from the session menu. Opening the tab never opens a browser automatically.
Reload reconnects the view through `hgs native-ui NAME --json`, without
reloading or restarting a DeepSeek bridge.

The native child has no app or plugin IPC. Linux checks the actual WebKit
navigation action before handing a clicked external link to the guarded
opener, keeps same-origin popups in place, and refuses new native windows.
User-initiated `window.open` uses the same guarded browser handoff.
Downloads require the exact foreground session and a native Save dialog.
Its GTK overlay follows the measured frame and hides before menus or dialogs.
The GLib FFI block replaces Wry's single earlier navigation-policy
handler on that owned child before its first URL load; an unexpected handler
graph uses browser fallback. The primary view's policy is unchanged.

Run the Native UI proof from the repository root with a switch-identity build:

```sh
TAURI_CONFIG='{"identifier":"com.hgdev.hgs-tray"}' cargo build --locked \
  --manifest-path client/src-tauri/Cargo.toml --features media,tauri/custom-protocol
python3 client/scripts/native-ui-smoke.py \
  --app client/src-tauri/target/debug/zerus-client \
  --output artifacts/validation/native-ui-proof
```

The script always creates a private D-Bus, Xvfb and HOME/XDG/TMPDIR/TMUX_TMPDIR
from a fresh environment, and uses only a local HTTP fixture and fake CLI.
It records IPC and private-protocol refusals, native pointer gestures,
download cancellation and exact saved bytes, profile eviction, explicit
browser fallback, resize and activation, then the complete Phase 4 smoke.
The ignored `native_ui::linux::tests` case separately qualifies the expected
handler graph, primary-handler preservation and reload on that same private
bus/display; run it explicitly with `--ignored --test-threads=1`.

`scripts/make-fixture.py` deterministically generates the replay, including the
warm and cold cache states, a paused resumable session, JSON and one-pixel PNG
attachments, a question, an approval and a native queue. The six driven
sessions do not participate in the replay's ordinary delta loop.
`src-tauri/tests/support/fake_hgs_phase4.py` answers only synthetic calls,
writes `log.ndjson` beside the fake, and advances the paused inspection after
Resume. The guarded `smoke_resume` command then advances the replay's catalog
and Activity to the same fresh run. `scripts/test_phase4_smoke.py` checks those
fixtures, receipts and state transitions.

Build and test from `client/`:

```sh
npm ci
npm run check
npm test
python3 -m unittest scripts/test_phase4_smoke.py
cargo +1.99.1 test --locked --manifest-path src-tauri/Cargo.toml
cargo +1.99.1 clippy --locked --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
npm run build && RUSTUP_TOOLCHAIN=1.99.1 npx tauri build --no-bundle
```

Run the smoke with a new scratch directory (choose another name on a rerun):

```sh
python3 scripts/phase4-smoke.py --scratch "$PWD/target/phase4-smoke-run"
```

Use `--only variable-scroll` for the long Markdown/append regression. On Linux,
add `--native-wheel` to replay the queued-scroll/layout ordering and drive
trusted wheel input while the provider window rotates. Its XTest driver verifies
that the display server is the original sibling Xvfb owned by `xvfb-run`.
Use `--static-wheel --static-shape mixed` with those flags for separate gestures
and 900 ms pauses without event-window changes. This also checks that remounting
static rows does not change their scroll range. `--renderer shm|dmabuf` selects
the Linux renderer configuration; Xvfb does not qualify hardware DMA-BUF.

The runner supplies the release binary, replay and fake, isolates `HOME`,
`XDG_DATA_HOME` and `XDG_CACHE_HOME`, and forces the safe headless environment:
no `DISPLAY`, a nonexistent `WAYLAND_DISPLAY`, `GDK_BACKEND=x11`,
`WEBKIT_DISABLE_DMABUF_RENDERER=1` and Xvfb. It keeps `app.log`, `log.ndjson`,
the fake configuration and the synthetic dropped file in that directory. Its
guard also allows the frontend's `smoke_log` command only in this measurement
setup, never against a real `hgs` directory. The same guard pins the composer
clock to fixture time, so the deterministic warm cache has not expired;
production always uses the real clock.

The Part A steps are Terminal output, Saved drafts first-open focus and focus
return, draft navigation, attach, one Send, cold-cache full-context Send,
Resume and send, process groups and output, and
attachment fetch plus a decoded `convertFileSrc` thumbnail. The A2 steps answer
the question, Approve once after its review delay and Send now, checking exact
ids and receipts in `log.ndjson`. Additional WebKitGTK checks paste a PNG and
decode its draft thumbnail and prove that the Clear dialog closes before its
delayed command finishes. Every step logs `phase4-smoke <step>: done`; a failure
logs the step and exits 1. Message and answer text appears only in the fake's
standard input records, never in argv. Keep the raw logs, their SHA-256 and the
executed test counts with the review.

## Screens

Overview, Projects, Machines and Accounts show live data and read only;
every write keeps its place, label and Qt enablement shape, disabled with the
phase that brings it ("Arrives with machine editing.", "... account
management.", "... project editing.", "... the New session dialog.", "... the
worktree flows.", "... the copy actions.").

| Data | Source |
| --- | --- |
| Machine cards (host, version, platform, metrics, console fields) | `catalog.meta`, projected in the core (`machine.rs`) and sent as `Batch.machines` |
| Connection state | `Batch.computers`: only `live` is fresh; `probing` and `reconnecting` connect (amber), `ended` is offline, `unsupported` reads "Update hgs" |
| Accounts | `hgs [@host] account ls` per host (every 30 s while Overview or Accounts shows, once per new host otherwise) and `account inspect ID --refresh` per installed profile (every 5 min, two at once, offline hosts skipped) |
| Machines | `hgs machine ls` on show and Refresh, `machine resolve ALIAS` on selection, `machine check ALIAS` only on Test connection |
| Projects | No read of its own: the organization the Sessions list groups by (the `organization` topic, or the core's `swarm get` poll for a CLI without it), this device's default project and the project sync status line |

The reads run in `src-tauri/src/screens.rs` on the blocking pool with the
core's timeouts and output limits (`core/src/reads.rs`); when each read is
due is decided in `core/src/views/schedule.rs`, and the core projects every
reply into a view model (`core/src/views`). A changed view emits
`screens-changed` with the screen's name, and the frontend pulls it again.
In replay mode the screens that read refuse unless `ZERUS_CLIENT_HGS` is set
("Reads in replay mode need ZERUS_CLIENT_HGS."); Projects keeps the project
sync status line. Peers without the event stream get no polling fallback yet
(see the phase table).

### Screens smoke run

`ZERUS_CLIENT_QUERY=screens-smoke=1` (hook in `src/ui/PerfHost.tsx`) shows
the four screens on the replay with a fake `hgs` answering the reads from
`core/tests/fixtures/views`, and exits 0 once each has loaded:

```sh
S=$(mktemp -d); cp core/tests/support/fake_hgs_reads.py $S/hgs; chmod +x $S/hgs
python3 - "$S" "$PWD/core/tests/fixtures/views" <<'EOF'
import json, sys
replies = {
    "account ls": {"file": "accounts-local.json"},
    "@mac account ls": {"file": "accounts-studio.json"},
    "account inspect claude-work --refresh": {"file": "inspect-claude-work.json"},
    "account inspect native-codex --refresh": {"file": "inspect-native-codex.json"},
    # The replay's other installed profiles have no fixture: their inspect fails.
    "account inspect native-claude --refresh": {"stderr": "no fixture", "exit": 1},
    "@mac account inspect claude-studio --refresh": {"stderr": "no fixture", "exit": 1},
    "@mac account inspect kimi-studio --refresh": {"stderr": "no fixture", "exit": 1},
    "machine ls": {"file": "machines.json"},
    "swarm get": {"file": "swarm.json"},
}
json.dump({"fixtures": sys.argv[2], "replies": replies}, open(sys.argv[1] + "/config.json", "w"))
EOF
cat > $S/run.sh <<'EOF'
src-tauri/target/release/zerus-client 2> "$S/app.log" &
app=$!
for page in overview machines accounts projects; do
  until grep -q "screens-smoke $page: shown" "$S/app.log" || ! kill -0 "$app" 2>/dev/null; do sleep 0.2; done
  sleep 1 # the frame after the page switch
  import -window root "$S/$page.png"
done
wait "$app"
EOF
env -u DISPLAY S="$S" WAYLAND_DISPLAY=zerus-none GDK_BACKEND=x11 ZERUS_CLIENT_HEADLESS=1 \
  WEBKIT_DISABLE_DMABUF_RENDERER=1 ZERUS_CLIENT_REPLAY="$PWD/fixtures/replay.ndjson:mac=$PWD/fixtures/replay-mac.ndjson" \
  ZERUS_CLIENT_HGS=$S/hgs ZERUS_CLIENT_QUERY=screens-smoke=1 ZERUS_CLIENT_PERF=smoke \
  xvfb-run -a -s "-screen 0 1440x900x24" sh $S/run.sh
echo "exit $?"; grep screens-smoke $S/app.log
python3 -c 'import collections, json, sys; print(collections.Counter(" ".join(json.loads(l)["argv"]) for l in open(sys.argv[1])))' $S/log.ndjson
```

The screenshots stay in `$S`; they are never committed.

### Updates replay screenshots

With `ZERUS_CLIENT_REPLAY`, the update worker never starts and makes no network
requests. `ZERUS_CLIENT_UPDATES_FIXTURE` may name one JSON `UpdatesView` of at
most 64 KiB; `updates_view` returns it and the service publishes it once. The
screenshot driver uses this seam for unchecked, available, cached-with-error,
restart-required and full status-bar states in both built-in themes. It checks
the real controls before each capture and isolates the single-instance runtime
of every case:

```sh
npm run build && RUSTUP_TOOLCHAIN=1.99.1 npx tauri build --no-bundle
S=$(mktemp -d)
python3 scripts/updates-smoke.py --scratch "$S/artifacts"
```

The ten PNGs and per-case logs stay in `$S/artifacts`; they are never committed.

## Projects in the Sessions list

The Sessions list groups sessions by the shared project organization, as the
Qt GUI does, and its menus edit that organization (plan:
`docs/superpowers/plans/2026-10-28-tauri-client-organization.md`).

Data path:

- The source is this computer's `organization` topic (`core/src/organization.rs`);
  a CLI without the topic is polled with `hgs swarm get` every 5 s by the core,
  with the same states. Only this computer is followed: the catalog is shared.
- The organization hub (`src-tauri/src/organization.rs`) feeds each state to the
  write controller (`core/src/swarm/sync.rs`, a port of `SwarmController`) and
  hands the presented organization to the pump's grouping
  (`core/src/swarm/grouping.rs`).
- The pump puts `Batch.groups` (each row's project and rank, only for rows
  whose project or rank changed) and `Batch.grouping` (the headers and whether
  edits can run, when they changed) on its batches, and strips
  `Batch.organizations`: the organization itself never reaches the webview.
- `src/store/organization.ts` keeps the assignments and headers, and
  `src/ui/sessions/model.ts` groups rows by their ids and assignments only, so a
  status update never regroups. The header counts are computed there, beside the
  webview's state and machine filters.

Qt's grouping rules (`SessionOrganization.cpp`, `SessionsWindowRows.cpp`):

- Projects follow the shared `project_order`. Members are
  `organization.projects[].sessions[]`, keyed `<host>\n<session name>`, where the
  host is this computer's host name or the peer's alias (the row's `machine`). A
  session listed by two projects belongs to the first.
- A row no project lists belongs to the default project: this device's choice
  when it is listed, else the catalog's `default_project` when it is listed, else
  the first project, else the Inbox (`ungrouped`).
- With one project the list shows no header and nothing collapses. A project
  without visible members is hidden ("Hide projects without active sessions",
  on by default). A header's counts cover members the filters hide; muted
  members never count as attention or unread. Its colour is the project's
  `color`, else its palette slot.

This device's layout (collapsed projects, the order inside each project and the
default project) stays local, as in Qt. It lives in `organization-layout.json`
in the app data directory, remembers at most 100,000 members and is saved at
most once a second and as the app quits. Pending edits live beside it in
`organization-draft.json`, written through a temporary file and a rename
whenever they change, before their apply, and removed once they settle. A
replay keeps its own `organization-layout-replay.json` and
`organization-draft-replay.json`, and removes a leftover replay draft at start.

Edit commands (one `swarm apply` each, answered `done` with the conflicts it
recorded, `kept` with Qt's status text, or `refused` with the reason):

| Command | Menu entry |
| --- | --- |
| `move_to_project(members, project, before)` | Move to project, a drop |
| `create_project(name, members)` | New project… (in the Move to project dialog) |
| `rename_project(project, name)` | Rename project… |
| `move_project(project, before)` | Move project up, Move project down |
| `sort_project(project)` | Sort sessions by name |
| `delete_project(project, destination)` | Delete project… |
| `set_project_collapsed(project, collapsed)` | a header click, Collapse project, Expand project (layout only, no `hgs`) |

`move_to_project` takes a set of members, `<host>\n<session name>` from the
row's own machine (`memberOf`): every member goes into the project as one
`swarm apply` from one snapshot. The session menu passes one member; the batch
Move to project (`zerus-5wmh.20`) passes the selection. `before` places them in
this device's order only. Sort sessions by name makes every listed row an
explicit member; when they all are already, it changes only the layout and runs
no `hgs`.

Apply rules (`SwarmController.cpp`, ported in `core/src/swarm/sync.rs`):

- `hgs swarm apply` runs on this computer only, without `@host`, with
  `{base, desired, versions}` as compact JSON on standard input and a 10 s
  timeout; one runs at a time. Edits are sent 120 ms after the last one.
- Base and versions are the state the user saw, from one snapshot. A state is
  not adopted while edits are pending or an apply runs, and after an
  acknowledged write a state that does not show it yet is held back for 10 s.
  Only the fields an apply wrote (`written`) advance their versions, so another
  writer's change surfaces as a conflict, never as an overwrite.
- Nothing is sent before the first state. A catalog with `initialized: false`
  keeps the entries disabled ("Project sync is not set up on this computer yet.
  Start Zerus here once to import its projects."); this client never runs
  `swarm initialize`.
- A failed or timed-out apply keeps its edits: the list keeps showing them, the
  answer and the status line say "Project sync unavailable: <error>" (or "Project
  sync needs an updated HGS installation."), and the next tick sends one apply
  with the same base, desired state and versions. The tick is 5 s after the
  failure or the topic's next state, whichever comes first. After a resend fails
  again, only the 5 s tick sends it, so a refused apply goes out at most once per
  5 s. Edits made meanwhile ride on it.
- After a restart the saved draft is shown at once and sent once after the first
  state, from its saved base and versions, so a catalog that changed meanwhile
  gives a conflict. A draft that cannot be used is moved aside, never applied,
  and the list says why until the first state.
- The status line keeps Qt's texts and reaches the Projects screen.
- In a replay, edits need `ZERUS_CLIENT_HGS` ("Actions in replay mode need
  ZERUS_CLIENT_HGS."); the browser replay refuses them ("Project changes need
  the desktop app.").

Deviations from Qt (the plan's Decisions):

1. The layout lives in `organization-layout.json`, not in QSettings
   (`workspace/organization`).
2. Sort sessions by name folds case, then compares as written, then by member;
   Qt uses `localeAwareCompare`.
3. No `swarm initialize`: a computer without the Qt GUI's import keeps edits
   disabled.
4. Edits made before the first ready catalog are refused, not queued.
5. Persisted live-run identities follow closed-client renames (`zerus-5wmh.47`);
   their separate file is bounded at 100,000 tokens and 24 MiB.

Archive placement follows Qt's archive identity, `machine\narchive\n<archive id>`.
The page reply carries its group and rank; later `Batch.groups` deltas carry
`archive_request`, so the archive store applies shown-request deltas immediately,
holds at most 50 for the in-flight request (newest per key), and merges those
after its reply. Other request tags are discarded. The live assignment store
never retains archive identities. Project edits, default-project changes and
verified post-Terminate placement keep the loaded window current.

Leaving Archive only cancels in-flight reads and keeps loaded placements live.
Scope changes and Refresh use `reset_archive_pages` to clear both sides. An empty
next page raises "Archive changed. Refresh to start a new view." while keeping
the shown rows and their placement updates, as ArchiveBrowser.cpp:37 does.
Clear archive respects the project filter and resets after the confirmed batch;
Move to project and drag-and-drop use archive identities, including a destination
row's archive identity, so a live session with the same name stays in place.

### Organization smoke run

`ZERUS_CLIENT_QUERY=organization-smoke=1` (hook in `src/ui/PerfHost.tsx`, with
the actions smoke's environment and guard) waits until the replay's organization
is ready and lists `claude/sample-project/review`, then runs one edit of each
menu through the store the menus use: Move to project (`p-aurora`), New project
("Smoke project", with the session), Rename project (`p-docs-site`), Sort
sessions by name (`p-sample-project`), Move project (`p-orders-api` before
`p-sample-project`) and Delete project (`p-docs-site` into the Inbox). It logs
`organization-smoke <step>: <outcome>` and exits 0 when every outcome is `done`.
The fake `hgs` answers `swarm get` and `swarm apply` with
`fixtures/replay-organization.json`, so each step's project is listed again
after every apply. The sort is layout only, so the log holds five `swarm apply`
calls and no `swarm get`, each from the fixture's organization (in its shared
form) and versions, and the layout file holds the sorted project. The screens
hub's account warm-up (`account ls`) reaches the fake too, which refuses it, so
the check counts only the `swarm` calls. `XDG_DATA_HOME` keeps the app data
directory in the scratch folder:

```sh
S=$(mktemp -d); cp core/tests/support/fake_hgs_swarm.py $S/hgs; chmod +x $S/hgs
python3 -c 'import json, sys; o = json.load(open(sys.argv[1])); json.dump({"get": [{"stdout": o}], "apply": [{"stdout": o}]}, open(sys.argv[2], "w"))' \
  fixtures/replay-organization.json $S/config.json
env -u DISPLAY WAYLAND_DISPLAY=zerus-none GDK_BACKEND=x11 ZERUS_CLIENT_HEADLESS=1 \
  WEBKIT_DISABLE_DMABUF_RENDERER=1 XDG_DATA_HOME=$S/data \
  ZERUS_CLIENT_REPLAY="$PWD/fixtures/replay.ndjson:mac=$PWD/fixtures/replay-mac.ndjson" \
  ZERUS_CLIENT_HGS=$S/hgs ZERUS_CLIENT_QUERY=organization-smoke=1 ZERUS_CLIENT_PERF=smoke \
  xvfb-run -a -s "-screen 0 1440x900x24" src-tauri/target/release/zerus-client 2> $S/app.log
echo "exit $?"; grep organization-smoke $S/app.log
python3 - "$S" fixtures/replay-organization.json <<'EOF'
import json, sys
S, fixture = sys.argv[1], json.load(open(sys.argv[2]))
calls = [c for c in map(json.loads, open(f"{S}/log.ndjson")) if c["argv"][:1] == ["swarm"]]
assert [c["argv"] for c in calls] == [["swarm", "apply"]] * 5, [c["argv"] for c in calls]
base = dict(fixture["organization"], session_assignments_explicit=True)
for call in calls:
    sent = json.loads(call["stdin"])
    assert sent["base"] == base and sent["versions"] == fixture["versions"]
layout = json.load(open(f"{S}/data/dev.zerus.next/organization-layout-replay.json"))
members = next(p for p in fixture["organization"]["projects"] if p["id"] == "p-sample-project")["sessions"]
title = lambda member: member.rsplit("/", 1)[-1]
assert layout["order"]["p-sample-project"] == sorted(members, key=lambda m: (title(m).casefold(), title(m), m))
print("5 swarm apply calls from the fixture's base and versions; p-sample-project sorted in the layout")
EOF
```

### Organization measurement

`src/store/organization.perf.test.ts` measures 20,000 rows: one replace into a
catalog (phase 2's reconcile), then 20,000 assignments in 50 projects with the
grouped members and list rows, then 1,000 status upserts, which must not group
the members again. It is skipped unless `ZERUS_CLIENT_MEASURE=1`:

```sh
ZERUS_CLIENT_MEASURE=1 NODE_OPTIONS=--expose-gc npx vitest run src/store/organization.perf.test.ts
```

The Rust grouping's own pass over 20,000 rows has an ignored timing test:
`cargo +1.99.1 test --manifest-path core/Cargo.toml --lib twenty_thousand_sessions_group_within_five_seconds -- --ignored --nocapture`.

## Measurement run

`ZERUS_CLIENT_PERF=auto` runs the Diagnostics scenarios unattended (`src/perf`),
writes the report to `ZERUS_CLIENT_PERF_OUT` and exits; every stage goes to
stderr as `zerus-client perf: ...` (no session names). It picks a session whose
Activity has events and scrolls, bounds each scenario at 60 s and the run at
8 minutes, and ends with 10 s idle windows: as displayed, with Animate working
sessions off, with Reduce motion (both only tried, not stored), with the window
hidden and with it minimised (`perf_away`; a bare Xvfb has no window manager, so
there the window stays shown and the log says `minimized false`), each of the
last two followed by one as displayed again. On Linux, endless motion also pauses
on toplevel focus loss, covering minimised Wayland windows that keep reporting a visible document;
visible one-shot entrances still complete, including new questions and toasts.
the away log and report record the animation state at each pause and resume.
`ZERUS_CLIENT_PERF_LABEL` names the platform, `ZERUS_CLIENT_PERF_HZ`
overrides the display rate (`display_hz`), `ZERUS_CLIENT_PERF_REPRESENTATIVE=0`
marks Xvfb runs and `ZERUS_CLIENT_PERF_PREFERENCE=default` keeps WebKit's
near-60 fps preference on macOS and Linux (the control run). The page is cross-origin
isolated (COOP and COEP headers) for precise timers.

Screenshots of real sessions are never committed; the fixtures are synthetic
(example domains, `/home/example` paths).

## Data path

Rust reads every watch frame (`client/core`), keeps the catalog and the followed
Activity, and folds changes into one batch per display frame. The frontend calls
`next_batch` from `requestAnimationFrame` with one call in flight; the command
parks up to 250 ms while nothing is pending and answers with raw JSON bytes. A
batch is one reactive transaction: rows reconcile by `computer + "\n" + key`, so
unchanged rows keep their identity and DOM. Only the selected session's Activity
is followed (newest 100 events); scrolling to the top reads earlier events with
`after`.

While the window is hidden or minimised the pump asks for nothing and schedules
no frame; a batch that arrives meanwhile waits, the Rust side keeps folding
changes into the next one, and the first frame after the window returns applies
it. The Working clocks stop too, as Qt stops its timers when a window hides.

## Session content search

Session search shows immediate name, metadata and identity matches and reads
public message contents after 350 ms with two or more characters. Live and
archive excerpts share the virtual list, with state, machine, project and
archive-date scopes. Click, arrow navigation and Enter open the exact matched
run/conversation/archive, with a clearable highlighted preview. A stale result
never falls back to the latest session of the same name.

The parity path uses Qt's existing `hgs search --query … --limit 100` contract;
older peers are help-gated before that verb is sent. Four cancellable CLI reads
run at most, including children of a retired query until they exit. Replies are
limited to 8 MiB and 100 hits per computer, with 32 computers, 200 retained
content hits and 500 visible metadata/content rows overall, plus one selected
archive/excerpt. Exceeding coverage shows “Limited history searched”; offline,
unsupported or failed computers stay explicit. There are no transcript caches
per session. The cursor-backed content index is tracked by
`zerus-5wmh.66.2.2.1`; the current CLI's own scan limits remain visible meanwhile.
The `content-search-smoke=1` replay query requires an explicit fake hgs and
`ZERUS_CLIENT_PERF`; run it only under isolated data, Xvfb and a private D-Bus
session, as the other native replay smokes.

## Settings and motion

The Rail pin and Appearance's “Keep Zerus above other windows” checkbox share
`workspace/alwaysOnTop`. Native confirmation drives both controls, including
changes from KDE's window menu. macOS, X11 and Windows apply the saved native
level before the first show. KDE Wayland uses Qt's temporary KWin script path:
one normal window matched by PID, application ID and caption, then its UUID;
no global window rules or installed scripts. Unsupported desktops show Qt's
explanation, and refusals use the sticky notice bus. Pending changes leave the
focused controls enabled. GTK publishes the app identifier for this exact scope.

The opt-in replay query `window-layer-smoke=first` exercises both controls,
focus and hint geometry, then leaves the preference on; `window-layer-smoke=restart`
verifies the next launch restores it and clears it. Use a private data directory,
Xvfb, `GDK_BACKEND=x11`, a nonexistent `WAYLAND_DISPLAY`, a private D-Bus session
and `ZERUS_CLIENT_PERF=1`, as for the other native replay smokes. Ordinary
KWin protocol tests create their own fake compositor bus.

The Settings screen has, so far, the two motion settings of the Qt GUI with its
QSettings keys and defaults: Animate working sessions
(`workspace/animateActivity`, on) and Reduce motion (`workspace/reduceMotion`,
off). They are stored per user in the webview's local storage, one JSON object
under `zerus-client-settings` keyed by the QSettings names (`src/store/settings.ts`).
`src/ui/motion.ts` turns them into root attributes, and `src/ui/styles/motion.css`
lists every endless animation: Animate working sessions off stops the working
indicators (halos, the project count, status and typing dots), Reduce motion or
the system's reduced-motion preference stops all of them (also the Zerus mark,
shimmer and spinners) and makes entrances instant, and a hidden or minimised
window pauses everything. `src/ui/styles/motion.test.ts` checks that every
keyframe animates only transforms and opacity and that every endless animation
is in those lists.

## Renderer (Linux)

WebKitGTK picks its renderer when GTK starts, so `main` calls
`renderer::choose()` first, before Tauri, GTK or any thread (`src-tauri/src/renderer.rs`).
On NVIDIA Wayland the shared-memory renderer repaints the whole window every
frame (about 30% app CPU at idle with motion); DMA-BUF with
`__NV_DISABLE_EXPLICIT_SYNC=1` costs about 6%, and DMA-BUF without that variable
dies at start with Wayland Error 71. Evidence and the full policy:
`bench/results/spike-29-webkitgtk-idle-2026-10-28.md`.

`ZERUS_CLIENT_RENDERER` is the only switch; there is no setting.

- `auto` (default): with `WEBKIT_DISABLE_DMABUF_RENDERER` absent, NVIDIA Wayland gets
  `__NV_DISABLE_EXPLICIT_SYNC=1` (never overwritten if you set it) and everything else is
  left to WebKit. With the variable set to `0` it behaves as `dmabuf`; set to anything
  else it is respected and the client stays on shared memory ("set by the system").
- `dmabuf`: `WEBKIT_DISABLE_DMABUF_RENDERER=0`, plus the NVIDIA variable on NVIDIA Wayland.
- `shm`: `WEBKIT_DISABLE_DMABUF_RENDERER=1`.

NVIDIA means `/proc/driver/nvidia/version`, a DRM card bound to the `nvidia` driver and
`nvidia_drm` modeset `Y`. Wayland is decided as GDK does: `GDK_BACKEND` unset or
`wayland*` and a compositor socket; `GDK_BACKEND=x11` changes nothing. An unknown value
counts as `auto`.

The DMA-BUF paths write a crash marker, `renderer-marker.json` (`renderer`, `driver`,
`written_at`), in the client's private data directory (next to the read marks, for
example `~/.local/share/dev.zerus.next/`), before GTK starts. The webview clears it
through `renderer_frame` after the second animation frame following load. A marker
found at start means the last DMA-BUF start never reached a frame: that start uses
shared memory and the marker stays. A marker of another driver version is dropped and
DMA-BUF is retried once. An early exit before the first frame keeps shared memory until
the driver changes; deleting `renderer-marker.json` in the app data folder retries DMA-BUF.
The decision (`renderer`, `reason`, `nvidia`, `wayland`,
`marker_state`) is logged at start and in the `renderer` field of `perf_config`.
A blank window is not detected. Known gap: on the GL path a minimised window keeps painting.

## Rules

- Part B adds voice and round video and the external
  terminal, shell and folder entries; their current disabled slots stay in Qt's
  places until then.
- The UI never separates values with dots (U+00B7, U+2022, U+2219, U+22C5,
  U+2027, U+2981, U+30FB): use layout, commas or line breaks.
- Motion uses transform and opacity only and honours Reduce motion, Animate
  working sessions and the system's reduced-motion preference (`motion.css`).
- `src/ui/lib/os-icons.ts` embeds two Tabler Icons outlines (MIT, see
  `licenses/`); `src/ui/lib/icons.generated.ts` is markup generated from
  `lucide-solid` (ISC).

The Terminate dialog offers **Remove the working folder after stopping** for a
freshly verified tracked agent in a linked worktree. The choice admits background
cleanup only after the exact scoped termination and archive receipt. The statusbar
chip opens the mockup's recorded cleanup review; opening never checks or removes
anything. A kept folder needs **Check again** and the displayed manual consents;
an unknown outcome cannot be retried. Branches and archive restore identity stay.
The remembered checkbox imports once through the existing G3 settings importer;
an explicit Next false wins and no maintenance policy is changed.

On Linux, run `python3 client/scripts/termination-cleanup-smoke.py --app target/tauri/debug/zerus-client`
from the repository root. It walks the
built app using a private D-Bus/Xvfb, private state and fake hgs. It covers recorded
outcomes, explicit discard, unknown non-replay, source-change refusal, inspection,
Escape and Keep, with captures of the approved chip/dialog states. The fixture
never deletes even its own synthetic worktree files.

An archived session whose folder stays for review has a sticky **Session
archived** notice with structured counts, **Review…** and **Restore session**.
Restore rechecks the exact archive and physical computer, then uses the existing
archive writer. It never retries cleanup. Admitted rows stay bounded at 64;
overflow contributes to the chip counts and uses the existing Worktrees review
route. Its exact archive restore identity remains available independently of the
cleanup row. A refused restore leaves the warning and recorded outcome in place.
On an older CLI whose successful help reply lacks the computer-guard capability,
Archive and Terminate keep their original calls. Their notice offers Review only;
an unguarded Restore is never offered. Failed or timed-out capability probes do
not fall back to a weaker call.

For the recovery walk, first run `npm run build` in `client`, then build the
Tauri app with `--features media,tauri/custom-protocol` so it embeds those assets.
Add `--recovery` to the command above. Use `--legacy` for the older-CLI Archive,
Review-only, singular-row copy and Escape walk. The normal recovery walk covers
both Restore surfaces, changed
archive identity, refusal, unknown outcomes, overflow, and Escape focus return,
using a private D-Bus, Xvfb, HOME/XDG/TMPDIR/TMUX_TMPDIR and a fake hgs.
