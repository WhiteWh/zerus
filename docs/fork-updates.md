# Fork builds and updates (WhiteWh/zerus)

This fork adds a Windows portable build with self-update. None of this is part of
upstream Zerus; the branch `whitewh/main` carries it on top of upstream `main`.

## Builds

In `WhiteWh/zerus`, `.github/workflows/fork-windows.yml` runs on every push to
`whitewh/main` and on manual dispatch. On a standard `windows-2025` runner it:

1. runs the Rust tests and builds the release `hgs.exe`;
2. builds the desktop app with the pinned Qt 6.10.3 and runs its offscreen Qt suites;
3. packages and self-tests the portable zip (`scripts/package-windows.py`,
   `scripts/check-windows-portable.py`);
4. publishes the tested ZIP to the selected release channel.

### Rolling Latest on push

Every branch push refreshes **one** prerelease named `Latest`, with the fixed tag
`latest` and asset `zerus-windows-x86_64.zip`. It creates no numbered release.
Download it from `/releases/tag/latest` or use the stable asset URL
`https://github.com/WhiteWh/zerus/releases/download/latest/zerus-windows-x86_64.zip`.
This prerelease is separate from GitHub's latest stable release and carries no
automatic update feed.

The workflow keeps one running build and one pending run (GitHub's default
`queue: single`, with `cancel-in-progress: false`). The `queue` key is omitted for
compatibility with the pinned actionlint version. New pushes replace the pending
run without stopping the running build. The slot covers both building and
publishing. After acquiring it, a Latest run checks out the current head of
`whitewh/main`, including commits that arrived while it waited. That checked-out
SHA, not the triggering event's SHA, identifies the package, rolling tag and
release notes. The run summary records both SHAs.

A failed build leaves the previous Latest in place. The publisher uploads and
verifies the next ZIP before replacing the previous asset, then advances only
the `latest` tag. Existing numbered releases and their tags are preserved.

### Manual numbered releases

Use **Actions -> Fork Windows build -> Run workflow**, leaving `channel` set to
`numbered` (the default), to publish `whitewh-<run number>` with the ZIP and
`zerus-fork-feed.json`. These releases retain the stable GitHub Latest designation
and feed used by existing WhiteWh installations. A numbered build keeps the exact
commit selected at dispatch; it does not silently advance while waiting.

Manual dispatch also offers `channel: latest` to refresh the rolling prerelease.
Runs are accepted only from `whitewh/main` or an experimental tag. The internal
build number remains available for diagnostics, even though the rolling release
has no number. Only numbered WhiteWh builds embed `ZERUS_FORK_FEED_URL`; Latest
and experimental builds embed no automatic update feed.

### Experimental builds

An experimental build of any commit: push a tag `whitewh-exp-<name>` for it. The
same workflow builds, tests and packages that commit and publishes the zip as a
prerelease named after the tag. It is never the latest release and has no feed,
and its `hgs-tray.exe` embeds neither a fork build number nor a feed, so the
updater neither offers it nor replaces it. It is installed by hand, and going
back to the regular builds is a hand install of a `whitewh-<build>` zip.

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
A build without a fork build number (made locally, or an experimental
`whitewh-exp-*` zip) cannot tell whether the feed's build is newer, so it never
prepares it by itself: the status names both commits and the button becomes
**Replace this build with fork build N...**, which asks for confirmation before
the download. Restart then installs it as an ordinary update, keeping the
running build as the rollback copy.

A portable Windows build of the fork never takes an upstream zip: the upstream
catalog check (`UpdateController`) does not offer the Windows portable build of
`ufna/zerus` over it, since installing that would replace the fork.

A verified build shows **Restart to finish update** in the tray menu and the
sidebar, and posts one notification. Restarting quits Zerus as Quit does, so
drafts are saved. A PowerShell helper without a console window then shows a small
banner in the lower right corner of the screen with each step and its progress
(a click hides it) and:

1. waits until no program runs from the installation folder, naming the ones it
   waits for;
2. moves the folder to `<installation>.previous` (the single rollback copy);
3. moves the new build in and starts it again.

The banner leaves once the new build's window is up. A failure stays on it for
ten seconds and goes to the log.

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
`install.sh` and `tray/install.sh`: `~/.local/bin/hgs-tray` beside an executable
`~/.local/bin/hgs`. It does not replace package-managed, system-wide or unknown
installations.

A newer feed starts `tray/linux-source-update.sh`, installed under
`~/.local/lib/hgs-tray/`. In `~/.cache/hgs/fork-source-update` it creates a clean
clone of `WhiteWh/zerus`, checks out the feed's exact commit and verifies both
`HEAD` and a clean tracked tree. It builds the release CLI and GUI there, with
the feed's build number and update URL. No checkout, build product or installer
from the developer's working tree is used.

**Restart to install fork build N** verifies that source identity again, then
runs the maintained `install.sh` and `tray/install.sh`. The old GUI remains
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
adds, under Settings -> Mobile connection (`tray/src/WindowsMobileServices.h`):

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
