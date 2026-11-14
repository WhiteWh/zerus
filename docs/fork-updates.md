# Fork builds and updates (WhiteWh/zerus)

This fork adds a Windows portable build with self-update. None of this is part of
upstream Zerus; the branch `whitewh/main` carries it on top of upstream `main`.

## Builds

Builds run only on a manual dispatch (owner, 2026-11-09: "сборка только по
пинку"). A push to `whitewh/main` builds nothing and a pushed tag builds nothing.
Start one in `WhiteWh/zerus` under **Actions -> Fork Windows build -> Run
workflow**, from `whitewh/main`, and pick `channel`:

- `numbered` (the default) publishes release `whitewh-<build>` with
  `zerus-windows-x86_64.zip` and `zerus-fork-feed.json`, marked as the latest
  release; that is the stable feed installed fork builds update from. It builds
  the commit selected at dispatch and embeds `<build>` as `ZERUS_FORK_BUILD`.
  `<build>` is the run number unless the dispatch names one in `build`; a named
  build must exceed the latest numbered release, and the run stops before
  building otherwise. Name one when local builds have passed the run numbers:
  an installed build only offers a release with a higher number.
- `latest` refreshes the single rolling `Latest` prerelease (see below): fixed tag
  `latest`, one Windows ZIP, no feed, never the latest release.

A dispatch from another branch builds and self-tests its zip but publishes
nothing; that package carries neither a fork build number nor a feed.

On a standard `windows-2025` runner a dispatch:

1. runs the publication contract tests (`tests/test_fork_latest_publication.py`);
2. runs the Rust tests and the mobile connector's;
3. builds, packages and self-tests the zip through
   `scripts/release-candidate.py`, the same release candidate our local packages
   use: the machine-dependent test scan, the release `hgs.exe`, the fork update
   tests and the Qt-to-Tauri handoff (`client/core/tests/fork_handoff.rs`), the
   Tauri desktop as upstream does (`npm ci`, `tauri build --no-bundle` with
   `tauri.windows.switch.conf.json` and the `video` feature) as `hgs-tray.exe`,
   the frontend runtime license notices, the portable zip
   (`scripts/package-windows.py --fork`: the fork identity, the relay and the fork
   README section) and its self-test (`scripts/check-windows-portable.py --run
   --selftest`, which also checks that `hgs-tray.exe --build-info` embeds the fork
   build and feed `build-info.json` names);
4. publishes the channel the dispatch picked.

The workflow keeps one running build and one pending run (GitHub's default
`queue: single`, with `cancel-in-progress: false`); the `queue` key is omitted for
compatibility with the pinned actionlint version. A newer dispatch replaces the
pending run without stopping the running build, and the slot covers both building
and publishing.

### Rolling Latest

`channel: latest` refreshes **one** prerelease named `Latest`, with the fixed tag
`latest` and the asset `zerus-windows-x86_64.zip`; it creates no numbered release.
Download it from `/releases/tag/latest` or the stable asset URL
`https://github.com/WhiteWh/zerus/releases/download/latest/zerus-windows-x86_64.zip`.
It is a prerelease, never the stable latest release, and carries no update feed,
so a build taken from it does not update itself.

A Latest dispatch checks out the current head of `whitewh/main` once it gets its
slot, including commits that arrived while it waited; that SHA, not the
dispatch's, identifies the package, the `latest` tag and the release notes, and
the run summary records both. `scripts/publish-fork-latest.py` publishes it and is
its only writer: it uploads the new ZIP under a staged name and verifies its size
and SHA-256 before renaming the old asset aside, advances only the `latest` tag,
reuses the same release on a retry, and refuses to touch a stable, immutable or
unrecognized release. Numbered `whitewh-*` releases and their assets stay as they
are, and a failed dispatch leaves the previous Latest in place.

The zip's `build-info.json` says `"desktop": "tauri"` and names the fork:
`"fork": {"repository": "WhiteWh/zerus", "build": 42, "feed": "<feed URL>"}`
(`build` null and `feed` empty for a Latest build and for a probe from another
branch).

### No Qt (task #388)

Only the Tauri desktop is supported (owner, 2026-11-08). The fork's build runs
no Qt configure, build or ctest step and the zip carries no Qt: no Qt runtime,
no `qt\` folder and no `"qt_fallback"` in `build-info.json`. The one-release
fallback `qt\zerus-qt.exe` of task #65 (fork builds up to 164) is retired, and
the portable check refuses it. Upstream's Qt sources stay in the tree,
untouched, so upstream merges stay simple; the Qt-to-Tauri handoff test still
runs, because an installed Qt build still updates to the Tauri zip.

A local dry run uses the same release candidate from a clean checkout (the Tauri
`--build-info` check refuses a dirty source):

    python scripts/release-candidate.py --commit <sha> --fork-build <run number> --fork-feed <feed>

It runs the builds, the Tauri build with `ZERUS_FORK_BUILD` and
`ZERUS_FORK_FEED_URL`, `scripts/package-windows.py --fork ...` and
`scripts/check-windows-portable.py <zip> --run --selftest`, and publishes
nothing.

A failing step publishes nothing. The release build is embedded in `hgs-tray.exe` as
`ZERUS_FORK_BUILD`, together with the feed address `ZERUS_FORK_FEED_URL`
(`client/src-tauri/build.rs`; the app accepts only release assets of
`WhiteWh/zerus` for the latter). Until task #63 the fork shipped the Qt
desktop; every build since is the Tauri app, and task #28 removed the Qt
sources and their packager.

## Feed

`https://github.com/WhiteWh/zerus/releases/latest/download/zerus-fork-feed.json`,
schema 1:

```json
{"schema": 1, "build": 7, "commit": "<40 hex>", "size": 123456789,
 "url": "https://github.com/WhiteWh/zerus/releases/download/whitewh-7/zerus-windows-x86_64.zip",
 "sha256": "<64 hex>"}
```

## Updates

`tray/src/ForkUpdater.cpp` supports a portable Windows installation (a folder
with `build-info.json` next to `hgs-tray.exe`) and the per-user Linux source
installation `~/.local/bin/hgs-tray` with its `hgs` CLI. In either installation,
a build that embeds the feed checks two minutes after start, then hourly. On
Windows, when `build` is newer than its own, it downloads the zip into
`<installation>.update`, beside the installation. Only HTTPS release URLs of
this fork are accepted, up to 1 GiB. The zip must match the feed's size and SHA-256, and its `build-info.json` must name
the feed's commit. Windows `tar.exe` unpacks it. The feed is read hourly even
while a build waits for the restart: a newer build downloads into
`<stage>-next` and replaces the waiting one only once it is verified, so the
restart always installs the newest build found.
**Settings -> Updates -> Check now** reads the feed at once.

**Settings -> About** names `WhiteWh/zerus` as the active repository beside
upstream `ufna/zerus`, with **Check for updates from WhiteWh/zerus** (then
**Restart to install fork build N** once a build is verified). In a supported
installation of a build made locally, without an embedded feed, this button is
the only check: it reads the feed of the fork's latest release,
`https://github.com/WhiteWh/zerus/releases/latest/download/zerus-fork-feed.json`,
with the same rules, and never checks by itself. Outside those installations the
button stays off.

The feed's `commit` is compared with the running build's `build-info.json` or
compiled source commit first: the build of the running commit is never offered.
A build without a fork build number (made locally, a probe dispatch from another
branch, or a `Latest` build) cannot tell whether the feed's build is newer, so it
never prepares it by itself: the status names both commits and the button becomes
**Replace this build with fork build N...**, which asks for confirmation before
the download. Restart then installs it as an ordinary update, keeping the
running build as the rollback copy.

A portable Windows build of the fork never takes an upstream zip: the upstream
catalog check (`UpdateController`) does not offer the Windows portable build of
`ufna/zerus` over it, since installing that would replace the fork.

The update button sits with the version at the top right of the title bar
(owner, 2026-11-13; it used to be at the bottom right of the status bar). Once
the feed publishes a newer fork build, the button beside the version reads
**Build N available** and opens Settings > Updates, also while that build
downloads or after its download failed; an offered replacement of an unnumbered
build shows there too. A verified build turns it into **Restart for build N**
(`client/src/ui/components/title-update.ts`). Outside a fork build it follows
upstream's channel: **Update available** and **Restart to finish update**.

A verified build shows **Restart to finish update** in the tray menu and
**Restart for build N** beside the version, and posts one notification.
Restarting quits Zerus as Quit does, so
drafts are saved. A PowerShell helper without a console window then shows a small
banner in the lower right corner of the screen with each step and its progress
(a click hides it) and:

1. waits until no program runs from the installation folder, naming the ones it
   waits for;
2. moves the folder to `<installation>.previous` (the single rollback copy);
3. moves the new build in and starts it again.

The banner leaves once the new build's window is up. A failure stays on it for
ten seconds and goes to the log.

Only tests pass the helper `-Quiet`; neither updater ever does. A quiet helper
shows no banner and starts every build with a hidden window, so
`client/core/tests/fork_handoff.rs` runs the real helpers without anything
appearing on the desktop, and checks that no window of theirs appeared.

If the new build exits within 15 seconds, the helper restores the previous build,
keeps the failed one as `<installation>.failed` and starts the previous build. If a
program still runs from the folder after two minutes, the update waits for the
next restart. The helper logs to `zerus-fork-update.log` beside the installation.

Native agents and the session host run from `%USERPROFILE%\.local\bin\hgs.exe` and
the staged host image, never from the portable folder, so updates do not stop
them. On its first start, the new build refreshes the staged CLI through
`session-host install` as any newer portable build does.

The first fork build is installed by hand. Builds made elsewhere, including
upstream releases, have no fork build number and never update this way.

### Linux source installation

The Linux updater deliberately supports only the per-user layout installed by
`install.sh` and `packaging/desktop/install.sh`: `~/.local/bin/hgs-tray` beside an executable
`~/.local/bin/hgs`. It does not replace package-managed, system-wide or unknown
installations.

A newer feed starts `packaging/desktop/linux-source-update.sh`, installed under
`~/.local/lib/hgs-tray/`. In `~/.cache/hgs/fork-source-update` it creates a clean
clone of `WhiteWh/zerus`, checks out the feed's exact commit and verifies both
`HEAD` and a clean tracked tree. It builds the release CLI and GUI there, with
the feed's build number and update URL. No checkout, build product or installer
from the developer's working tree is used.

**Restart to install fork build N** verifies that source identity again, then
runs the maintained `install.sh` and `packaging/desktop/install.sh`. The old GUI remains
usable during any remaining build work. Only the ordinary GUI replacement quits
it, saves drafts, keeps one rollback copy and starts the verified build; native
agents and tmux sessions are not stopped. Preparation and installation logs stay
in the stage as `prepare.log` and `install.log`. A newer release replaces the
waiting source tree only after its own build succeeds.

### Versioned root

The plain layout above puts `.update`, `.previous`, `.failed` and the log next to
the installation, that is, in whatever folder holds it. A versioned root keeps all
of it inside one folder and never moves the running build:

```
C:\Tools\Zerus\
  current\      junction to the build that starts; shortcuts point here
  app-24\       the previous build, kept for rollback
  app-25\       the current build
  .update\      a newer build while it downloads and waits for the restart
  update.log
```

The updater uses it when `<root>\current` is a junction to the folder Zerus runs
from, started either as `current\hgs-tray.exe` or from that build's own folder.
After **Restart to finish update** the helper waits for Zerus to quit, moves the
verified build to `<root>\app-<build>`, re-points `current` at it and starts it.
Nothing has to stop running from the old build, so the two-minute wait of the
plain layout never applies. A build that exits within 15 seconds hands `current`
back to the previous one. After a successful start the two newest `app-*` builds
stay and older ones are removed.

When `current` points to another build than the one running, that build does not
update at all: start Zerus from `current`.

## The Tauri desktop and the move from Qt (task #63)

The Tauri desktop has the same fork channel as the Qt GUI
(`client/core/src/fork_update.rs`, `client/src-tauri/src/fork_updates.rs`): the
fork build number and the feed from the build environment, the same feed and zip
checks (schema 1, a newer build, an HTTPS release asset of this fork up to 1 GiB,
the published size and SHA-256, `tar.exe`, `hgs-tray.exe`, `hgs.exe` and a
`build-info.json` naming the feed's commit), the same `<stage>-next` rule, and on
Restart the Qt updater's own PowerShell helper, byte for byte
(`client/core/src/fork_update/*.ps1`, which a test compares with
`tray/src/ForkUpdater.cpp`): the idle wait, the banner, the one rollback copy, the
15-second start check and the restore. Restart quits Zerus as Quit does, so
drafts are saved. A fork build checks two minutes after start and hourly; a
locally made portable build checks only on request and offers the feed's build
as a replacement it asks about first.

Settings > Updates shows **Fork updates from WhiteWh/zerus**: the running fork
build (or its commit), the status, **Check for updates from WhiteWh/zerus**,
**Replace this build with fork build N...** (confirmed in a dialog) and
**Restart to install fork build N**. A verified build also sets the restart
flag the tray and sidebar show. Upstream's channels never replace a fork build:
the upstream release check stays informational and says so.

The move needs nothing new on the Qt side. The installed Qt build's updater
reads the feed, verifies the next zip (whose `hgs-tray.exe` is the Tauri app) and
installs it with its helper, into the same layout: `<root>\app-<N>` with
`current` re-pointed, or the plain folder with `.previous`. The Tauri app then
finds that layout and updates itself to the following build.
`client/core/tests/fork_handoff.rs` proves it on a real versioned root:

1. a Tauri build that exits within 15 seconds hands `current` back to the Qt
   build, which starts again;
2. the Qt helper installs the Tauri zip: `current` -> `app-101`, Qt stays in
   `app-100`;
3. re-pointing `current` at `app-100` by hand starts the Qt build again;
4. the Tauri updater installs the next zip: `current` -> `app-102`, `app-101`
   stays as the rollback copy.

Two builds are kept, so the Qt build is removed at the second Tauri update;
roll back to Qt before that, or keep a copy of its `app-<N>` folder.

### What the move carries over

The Tauri app imports the Qt GUI's settings once, on its first start, from the
same file the Qt build writes (`%APPDATA%\hgdev\hgs-tray.ini`; the Qt file is
never changed, so a rollback finds it as it was), and the composer drafts once
(`hgs-tray.ini.drafts`; a draft that clashes is kept under Saved drafts). Nothing
on the Qt side has to run first. Accounts, `tray.conf` and the mobile
connector's `connector.json` live in the hgs configuration both apps read, so
they are shared, not copied.

Carried over: theme, language, scale and the other workspace settings,
notifications, processes, downloads, update channel, keyboard shortcuts,
recovery policy, speech settings (not the microphone), new-session choices,
window geometry and splitters, machine and project colours, collapsed rows,
the composer drafts, the account list, the inspector width, visibility,
tab and folded cards (`workspace/inspectorWidth`, `workspace/inspectorOpen`,
`workspace/inspectorTab`, `workspace/inspectorClosed`), and the WhiteWh skin
(`workspace/skin`: Tauri ports it as `whitewh-dark` and `whitewh-light`).

Also carried over once, on a Tauri client that switched before them too
(task 102; Qt's store is only read, never written):

- the mobile services (`mobile/localRelay`, `mobile/localRelayPort`,
  `mobile/runConnector`): Qt keeps them only in its store, not in
  `connector.json` or `tray.conf`, so Tauri has its own copies and the same
  switches under Settings -> Mobile connection (see below);
- the new-session worktree choice (`workspace/launchWorktree`) as Tauri's
  `newSession/folderMode`, the New session dialog's default;
- the microphone (`speech/microphone`) as its name only
  (`speech/microphoneName`, read from Windows' device registry for Qt's
  endpoint id): the Voice page chooses the listed microphone with that name.
  Qt's device id itself is never stored, and a device this computer no longer
  knows imports nothing;
- `pathMappings/*`: the WSL path presets (at most 64) into
  `<data_dir>/wsl-path-mappings.json` at the first start, when that file does
  not exist yet;
- `privateStores/notified`: the private-store notices already raised (at most
  512) into `<data_dir>/private-store-notices.json` at the first start, when
  that file does not exist yet; Tauri keeps that set there from then on.

Not carried over, and why:

- task title, new-card and note drafts (`tasks/titleDraft/*`, `tasks/newCard/*`,
  `tasks/noteDraft/*`): the Tauri task board keeps no draft fields, so there is
  nowhere to put them; the owner loses only unsent board text;
- the expanded and collapsed sessions and teams (`ui/collapsed/*`,
  `ui/expanded/*`): Tauri keeps its own collapsed-row store, not Qt's keys;
- automatic approvals: off after the move by design; turn them on again in
  Settings;
- the Teams UI preferences (`teams/*`, `team/*`, `ui/catalogHideUnusedRoles`):
  the Tauri Teams screen has no matching keys for heat mode, hide finished,
  per-team plan mode or the catalog role filter, so those visible choices reset;
- `machines/residentHost`, `machines/otherTransport`: the resident host and
  transport choice are not implemented in Tauri;
- `socraticode/nativeQdrant`: SocratiCode/native Qdrant is not in Tauri;
- `workspace/cacheKeepWarm`: cache keep-warm is not implemented in Tauri;
- the extensions' switches (`extensions/*`): Tauri has no extensions surface;
- `dataPlane/eventStream`: the event-stream data plane is not in Tauri;
- pending reads, local read marks and the project organization, unless hgs
  recovers the latter itself.

The Startup shortcut keeps working while it points at `current\hgs-tray.exe`.
In the plain layout the Qt updater stopped the mobile connector before the
helper ran; the Tauri updater does not, so a connector running from the
installation folder makes the helper wait for it (up to two minutes, then the
update waits for the next restart). The versioned layout never waits for it.

Switching an existing installation, with Zerus closed (`app` is its folder):

```powershell
cd C:\Tools\Zerus
New-Item -ItemType Junction -Path current -Target app
```

Then start `C:\Tools\Zerus\current\hgs-tray.exe` and point shortcuts, including the
Startup one, there. The first update goes to `app-<build>`; the original `app`
folder is not an `app-*` build and stays until it is removed by hand.

## Log

Zerus writes its warnings, every status-bar notice and each change of the
connection line to `~/.local/state/hgs/zerus.log`, beside the relay's
`relay.log`. **Open log** in the tray menu and **Log** in the status bar open it
in the system's text viewer. Past 4 MiB the file moves to `zerus.log.1`,
replacing the older copy.

A launch writes `Zerus launched (pid N)` first, so every launch has its own
line. A launch that finds the tray already running writes `Already running
(pid N): …` and exits; the process that takes the single-instance lock writes
`Single instance: pid N holds <lock>`; and only the process that becomes the
tray writes `Zerus <version> started (pid N)`. Counting `started` lines
therefore counts windows, not launches, and a crash timeline names the process
that owned the tray. `HGS_TRAY_HOME` (log and single-instance socket) and
`HGS_TRAY_LOCK_DIR` (the lock) point a launch at another home and lock folder;
the launch-log test uses them so it never touches the real tray.

## Optional extensions

Integrations with optional third-party tools stay outside the core as
extensions. Each has an id that names both its cargo feature (`src/extensions.rs`)
and its Settings → Extensions switch (`tray/src/Extensions.h`, QSettings key
`extensions/<id>`). Both are off by default: a plain `cargo build` keeps the
command but answers that the extension is absent, and a GUI built without
`ZERUS_FORK_BUILD` (or `-DZERUS_EXTENSIONS_ON_BY_DEFAULT=ON`) shows the switch off.
The fork workflow builds `hgs` with `--features socraticode`, and a fork GUI
turns extensions on by default.

- `socraticode`: `hgs socraticode …` and the SocratiCode strip in Details, with
  its native-Qdrant option.

## Windows mobile gateway additions

The step-by-step setup with Tailscale, from install to pairing a phone and the
usual problems, is in [fork-mobile-tailscale.md](fork-mobile-tailscale.md).

Upstream's Windows gateway (`docs/superpowers/specs/2026-10-20-windows-mobile-gateway-design.md`)
manages the bundled connector through `zerus-mobile-connector --manage`. The fork
adds, under Settings -> Mobile connection (`tray/src/WindowsMobileServices.h`;
in Zerus Next the same two switches and startup, task 102,
`client/src-tauri/src/mobile.rs`, `client/core/src/mobile/local_services.rs`):

- **Start the connector when Zerus starts** (`mobile/runConnector`, on by
  default): at startup Zerus asks `--manage status` and, for a configured
  connector that is not running, `--manage start` with that revision. It never
  stops or kills the connector.
- **Run a relay on this computer** (`mobile/localRelay`, off by default): the
  bundled `zerus-relay.exe` serves `127.0.0.1:8787` with its SQLite database in
  `%USERPROFILE%\.local\state\hgs\relay`, from a per-build copy under
  `%USERPROFILE%\.local\share\hgs\mobile\<hash>` so the portable folder stays
  replaceable. A phone needs HTTPS: publish it inside the tailnet with
  `tailscale serve --bg --https=443 http://127.0.0.1:8787` (Tailscale's HTTPS
  certificates must be enabled; Serve stays tailnet-only, unlike Funnel).
  Provision the computer once with the relay stopped:
  `zerus-relay --database <db> provision --name <workspace> --computer-name <name>`,
  then save the printed computer credential in Mobile connection.

## Windows SSH peers

The desktop on one Windows PC can manage another Windows PC over SSH; see
[fork-windows-peers.md](fork-windows-peers.md).
