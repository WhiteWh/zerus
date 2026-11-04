# Install Zerus

[Product overview](../README.md) | [Detailed reference](reference.md#installation)

Zerus runs on Linux and macOS. Arch Linux x86_64 users can install the complete
CLI and desktop bundle from AUR; Apple Silicon Macs can use the Homebrew cask.
Other installations build from source. The CLI keeps the name `hgs`; the desktop
executable is `hgs-tray`. Windows has a portable [control console](#windows) for
remote Linux and macOS machines; it can also run agents on the Windows computer
itself.

## Requirements

For the CLI: the newest stable Rust, a C compiler, Python 3, tmux 3.7+ and OpenSSH. Install the
agents you want to use on the machines where they will run.

The ADE's verified answer/message delivery needs tmux 3.7's input-mode inspection.
Some distributions ship older tmux packages; use a supported upstream release.
Installing a newer binary does not replace an already running tmux server. Check
the server with `tmux display-message -p '#{version}'`; schedule any server
migration deliberately after saving your sessions. Never kill a live server or
native agents as part of a Zerus GUI/package update.
[Official tmux releases](https://github.com/tmux/tmux/releases).

For the Linux desktop: Rust, Node.js/npm, GTK3, WebKitGTK 4.1, ALSA and
GStreamer. The source installer and AUR packages build Zerus Next by default;
standard autostart uses user systemd. `HGS_LINUX_GUI=qt` keeps the explicit
Qt fallback until Linux acceptance and needs CMake, a C++17 compiler, Qt 6
Widgets/Multimedia/SVG and KDE Frameworks KStatusNotifierItem, KWindowSystem
and KIdleTime. Qt WebEngine is optional for that fallback's embedded DeepSeek
[Native UI](reference.md#deepseek). On macOS, the Qt source-build path uses
Homebrew Qt and requires macOS 14 or later.

## Local installation

```sh
git clone https://github.com/ufna/zerus.git
cd zerus
./install.sh
./tray/install.sh
```

On macOS, install the dependencies first:

```sh
brew install rust python tmux cmake qt qtmultimedia
```

The macOS source installer also needs a valid application-signing certificate
and its private key in your keychain. It selects a certificate compatible with
the installed app, including its team and distribution class. For a first install
it prefers Developer ID Application, then Apple Development. If selection is
ambiguous, choose an exact name or SHA-1 fingerprint from
`security find-identity -v -p codesigning` with `HGS_MACOS_SIGN_IDENTITY`:

```sh
HGS_MACOS_SIGN_IDENTITY='Developer ID Application: Example (ABCDE12345)' ./tray/install.sh
```

Compatible certificate signatures keep the privacy identity across local
rebuilds. The installer verifies both designated requirements before replacing
an existing certificate-signed app and stops if they are incompatible. It never
falls back to ad hoc signing. Moving from an older ad hoc installation can require
fresh consent once; no permissions are reset automatically. Source builds link
Homebrew Qt and are signed locally without the hardened runtime or notarization;
use the published app for distribution. See [macOS signing](macos-distribution.md#local-source-builds).

On Arch Linux, install the Next build and runtime dependencies before a source
install (AUR recipes declare them automatically):

```sh
sudo pacman -S --needed base-devel rust nodejs npm python tmux openssh \
  webkit2gtk-4.1 gtk3 alsa-lib libappindicator dbus gstreamer gst-plugins-base-libs \
  gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly gst-libav
```

GStreamer supplies the camera, H.264/AAC encoding and media playback plugins.
See [Linux packages and source installation](linux-distribution.md).

The desktop starts in English on every operating system. Settings > Appearance >
Interface language offers an optional Russian preview; restart only Zerus to
apply it. Agent sessions keep running, and untranslated text stays in English.
See [translation coverage and maintenance](../tray/translations/README.md).

Add `~/.local/bin` to `PATH`. The CLI installs to `~/.local/bin/hgs`; SSH calls
use that exact path. The desktop installs to `~/.local/bin/hgs-tray` on Linux or
`~/Applications/hgs-tray.app` on macOS. The installer configures autostart through
systemd or launchd.

On Linux, a normal Next source install also registers `hgs-desktop.service`
from the selected CLI's definition. For AUR packages, run
`zerus-setup --desktop-service` as the desktop user. Startup imports the current
graphical-session environment before creating the backend child; a headless
registration waits for the next graphical login. UI-only updates keep an
unchanged backend alive. Deliberate source CLI upgrades refresh only an already
managed desktop registration; a CLI-only installation creates no new service.
Pacman upgrades never change user services. See the
[Linux background-service commands and bare-WM setup](linux-distribution.md#desktop-background-service).
IPC readiness and background worker ownership are separate; registration alone
does not enable background workers.

For a local Linux Zerus Next source build, an explicit private WebKitGTK/GTK3
stack can be selected at installation:

```sh
HGS_LINUX_GUI=client HGS_LINUX_WEBKIT_PREFIX=/absolute/private/prefix ./tray/install.sh
```

The prefix must contain `lib/libwebkit2gtk-4.1.so.0`, its JavaScriptCore library,
GTK3 under `gtk/lib`, and the WebKit processes under `libexec/webkit2gtk-4.1`.
The installer saves the selection in `~/.local/share/hgs/local-webkit.json`
(`$HGS_INSTALL_PREFIX/share/hgs/local-webkit.json` for an isolated install).
Later preview installs retain it without the variable. The local service and
desktop entry use `~/.local/libexec/hgs-tray-launch`; it selects these libraries
and processes only for Zerus Next, leaving the real `hgs-tray` binary available
to the maintained updater and its rollback checks. If the stack disappears or
is incomplete, the next launch uses system libraries. Qt and older Zerus Next
rollback builds also use system libraries. Published packages continue using stock WebKitGTK.

Run `HGS_LINUX_WEBKIT_PREFIX= ./tray/install.sh` to clear the selection. Changing
the setting takes effect at the next GUI start; if the binary is unchanged, the
installer leaves the current GUI running. A changed build still uses the normal
draft-preserving update. The opt-in does not modify global library paths or
the environment of native agents. The launcher records the original library
variables; Zerus restores them when starting CLI commands, watches, Terminal
clients, desktop terminals and external applications. WebKit's own subprocesses
keep the private stack.

`HGS_MOBILE_CONNECTOR=1 ./install.sh` also builds the Rust mobile connector
(`services/mobile/connector`) into `~/.local/bin/zerus-mobile-connector`. Later
runs update that copy when it changed; `HGS_MOBILE_CONNECTOR=0` always skips it.
The installer never enables, starts or restarts the connector's service: when the
running service uses the replaced binary, it prints the restart command to run
after phone actions have drained. `mac-install.sh` passes the variable to the
Mac's build. [Connect a computer](mobile-deployment.md#connect-a-computer) sets up
its service, and [Move a computer to the Rust connector](mobile-deployment.md#move-a-computer-to-the-rust-connector)
switches one that runs the Python connector.

To install an already qualified standalone connector without building or updating
the CLI, use `./install.sh --connector-only /absolute/path/to/zerus-mobile-connector SHA256`.
Pass the artifact's verified lowercase SHA256. This mode checks the artifact and
installation directories, replaces only the connector atomically and performs no
service action. `HGS_INSTALL_PREFIX` selects the prefix as usual. Coordinate any
subsequent connector service transition separately; the running process retains
its previous executable until it exits.

Open the workspace from the command line:

```sh
hgs-tray --sessions                         # Linux
~/Applications/hgs-tray.app/Contents/MacOS/hgs-tray --sessions  # macOS
```

The repository's `tmux.conf` is a suggested configuration. If you already have
your own, merge the settings you need manually: `hgs` uses the machine's shared
tmux server.

## Arch Linux

Choose one package; all include the CLI and desktop and conflict with one another:

| Package | Installation | Contents |
| --- | --- | --- |
| [zerus-ade-bin](https://aur.archlinux.org/packages/zerus-ade-bin) | `yay -S zerus-ade-bin` | Stable prebuilt Arch x86_64 bundle |
| [zerus](https://aur.archlinux.org/packages/zerus) | `yay -S zerus` | Stable release compiled locally |
| [zerus-git](https://aur.archlinux.org/packages/zerus-git) | `yay -S zerus-git` | Current upstream main compiled locally |
| [zerus-ade-nightly-bin](https://aur.archlinux.org/packages/zerus-ade-nightly-bin) | `yay -S zerus-ade-nightly-bin` | Prebuilt nightly after full Linux/Arch/macOS/Windows checks |

Keep Arch fully updated before installing. Binary library version requirements
come from the verified release builder; source packages compile against your
installed libraries. The [0.42.0 release](https://github.com/ufna/zerus/releases/tag/v0.42.0)
includes immutable source/binary archives, checked pacman packages, AUR recipes
and SHA-256 checksums. Native agents are installed separately.

Nightly packages update through ordinary `yay -Syu`; source-based `zerus-git`
updates need `yay -Syu --devel`. Nightly releases use immutable build URLs and
checksums and leave the latest stable release unchanged. See
[automatic nightly publication](ci-and-aur.md#automatic-nightly-publication).

After installation, run this once as your normal user, **without sudo**:

```sh
zerus-setup
```

It creates the `~/.local/bin/hgs` compatibility links used by SSH. Existing
source-install files are preserved: inspect and move them aside deliberately if
you want to migrate to the system package. Keep your configuration and agent state.

Services and autostart are opt-in. Inspect old user unit files before enabling
packaged services; local definitions can shadow the system package. Package
installation and upgrades do not restart native agents, tmux or the GUI.
[Service setup and migration](ci-and-aur.md#package-contents-and-compatibility).

## macOS (DMG)

For Apple Silicon Macs running macOS 14 or later, download
`zerus-VERSION-macos-arm64.dmg` from the [latest stable release](https://github.com/ufna/zerus/releases/latest)
or a selected [nightly release](https://github.com/ufna/zerus/releases?q=nightly-&expanded=true).
Open the image, drag **Zerus** into **Applications**, then eject the image and
open Zerus from Applications. The application and disk image are signed with
Developer ID and notarized by Apple. Signed stable macOS downloads begin with
[0.43.0](https://github.com/ufna/zerus/releases/tag/v0.43.0), and nightly downloads with
[nightly 0.42.2.r1291.g5d2e5d0.n17](https://github.com/ufna/zerus/releases/tag/nightly-38794972859);
older releases may not contain a macOS download.

To use the bundled CLI and SSH session bridge, run the per-user setup once,
without sudo:

```sh
/Applications/Zerus.app/Contents/Resources/bin/zerus-setup
```

Local tmux sessions require `tmux` (`brew install tmux`). Agent providers are
installed separately. The Homebrew cask below installs tmux and links the CLI
automatically. For a manual upgrade, quit Zerus from its menu, replace the app
with the new DMG's copy and open Zerus again; native agents keep running.
**Settings > Updates** checks the selected channel and offers **Download DMG**
with replacement instructions. Dev DMGs come from the verified
[desktop Dev catalog](https://zerus.dev/updates/v2/desktop/dev.json) on zerus.dev;
stable and nightly DMGs come from GitHub Releases. Updates are installed manually.

## macOS (Homebrew)

Apple Silicon Macs running macOS 14 or later can install the signed and notarized
application and CLI from the Zerus tap:

```sh
brew install --cask ufna/zerus/zerus
zerus-setup
```

The tap supports three channels. `zerus` follows stable releases; `zerus@nightly` follows
the daily build from main that passed the full CI, published at the same time as
the AUR nightly. `zerus@dev` follows successful main CI builds, with the same
Apple signature and notarization. All install the same `Zerus.app`, so only one can be installed.
Settings, agents and conversations are shared. Switch between Stable and Nightly with:

```sh
brew uninstall --cask zerus && brew install --cask ufna/zerus/zerus@nightly
brew uninstall --cask zerus@nightly && brew install --cask ufna/zerus/zerus
```

For Dev, install `brew install --cask ufna/zerus/zerus@dev`. To switch from nightly:

```sh
brew uninstall --cask zerus@nightly && brew install --cask ufna/zerus/zerus@dev
```

Zerus checks its installed channel unless you choose another one in
**Settings > Updates**. That page shows the complete installed version, a copyable
Homebrew update/channel-switch command and a separate DMG download.

The cask installs `/Applications/Zerus.app`, links `hgs` and `zerus-setup` into
Homebrew's `bin` directory and installs `tmux` as a dependency. It starts nothing
and does not write to your home directory. Run `zerus-setup` once as your normal
user, **without sudo**. It links `~/.local/bin/hgs` for SSH protocols and installs
the on-demand session bridge that lets agents started over SSH use your login
Keychain. Options:

```sh
zerus-setup --autostart   # start Zerus at login
zerus-setup --workers     # recovery and swarm background workers
```

Through 0.42.2, `--autostart` wrote a LaunchAgent that launchd runs as a background
daemon: whenever other programs keep every core busy, the Zerus window stops
responding and is restarted after 30 seconds. A Zerus that its login item started at
background priority shows **Fix login item** in the Sessions window's status bar.
Nothing changes until you choose: **Update Only** (the default) runs
`zerus-setup --autostart` and applies at your next login while Zerus keeps running;
**Update and Restart** also quits and reopens Zerus now. Saved drafts, sessions and
agents stay; a voice or video message that is still recording stops and is not
kept. You can also run `zerus-setup --autostart` again yourself. It applies at your
next login; to apply it now, quit Zerus from its menu and reload the agent:

```sh
launchctl bootout "gui/$(id -u)/com.hgdev.hgs-tray"
launchctl bootstrap "gui/$(id -u)" ~/Library/LaunchAgents/com.hgdev.hgs-tray.plist
```

`brew upgrade --cask zerus` (or the installed nightly/dev cask) replaces the application but never
quits it, unloads its LaunchAgents or stops tmux and native agents. When the
installed bundle is another build than the running GUI, Zerus shows **Restart to
finish update**; drafts are kept. Background workers finish their current round
and exit when an upgrade replaces their executable, and launchd starts the new
version. A worker that `launchctl print "gui/$(id -u)/com.hgdev.hgs-swarm"` shows
without a `pid` and with `last exit code = 78: EX_CONFIG` cannot be started: the
system log reports `Unable to get updated LWCR`, a launch constraint from its Login
Items record that names an earlier build, which launchd's retries do not renew. Running
`zerus-setup --workers` (or `install.sh` for a source installation) again loads such
a worker again, as does:

```sh
launchctl bootout "gui/$(id -u)/com.hgdev.hgs-swarm"
launchctl bootstrap "gui/$(id -u)" ~/Library/LaunchAgents/com.hgdev.hgs-swarm.plist
```

Use `com.hgdev.hgs-recovery` the same way.

To uninstall, remove the per-user integration first:

```sh
zerus-setup --remove
brew uninstall --cask zerus   # use zerus@nightly for Nightly, or zerus@dev for Dev
```

`brew uninstall --zap --cask zerus` (or `zerus@nightly` / `zerus@dev`) additionally removes the Zerus LaunchAgents,
including ones created by a source installation, the logs and saved window state.
`~/.config/hgs`, agent state and conversations are never removed.

### Migrating from a source installation

`zerus-setup` changes nothing while it finds a source installation:
`~/Applications/hgs-tray.app`, a regular file at `~/.local/bin/hgs`, or Zerus
LaunchAgents that run a program outside `/Applications/Zerus.app`. Keep your
configuration and agent state, then migrate deliberately:

1. Quit Zerus from its menu bar icon. Native agents and tmux keep running.
2. Unload the old agents, ignoring ones you never installed:

   ```sh
   for label in com.hgdev.hgs-tray com.hgdev.hgs-recovery com.hgdev.hgs-swarm com.hgdev.hgs.user-session; do
     launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true
   done
   ```

3. Move `~/Applications/hgs-tray.app`, `~/.local/bin/hgs`, `~/.local/bin/hgs_state.py`
   and the matching `~/Library/LaunchAgents/com.hgdev.hgs*.plist` files into a
   backup folder.
4. Run `zerus-setup` with the options you used before.

To return to the source installation, run `zerus-setup --remove` and restore the
backup.

<a id="windows"></a>

## Windows

Stable releases after 0.42.2 and nightly releases include a portable Windows
x86_64 zip with the desktop app (`hgs-tray.exe`) and the CLI (`hgs.exe`). It starts
as a control console: it manages agent sessions on your Linux and macOS machines
through the Windows OpenSSH client. You can also
[enable local sessions](#local-sessions-on-windows), and agents then run on the
Windows computer itself, in a session host of your user account on ConPTY, without
WSL ([Windows plan](superpowers/specs/2026-10-10-windows-support-design.md),
[session host](superpowers/specs/2026-10-13-windows-native-host-design.md)).

Requirements:

- Windows 10 version 1903 or later, or Windows 11, on x86_64. Local sessions are
  tested on Windows 11 and on the Windows Server 2025 CI runners.
- Zerus Next packages require the [Microsoft Edge WebView2 Evergreen Runtime](https://developer.microsoft.com/microsoft-edge/webview2/).
  Winget installs this dependency. For portable/manual installs, install it first
  if absent; Microsoft Edge itself is not a substitute. Zerus does not remove
  this shared runtime when uninstalled.
- For remote machines, the OpenSSH client. `ssh -V` in PowerShell shows whether it
  is installed; otherwise add **OpenSSH Client** in Settings → Optional features.
- For remote machines, at least one Linux or macOS machine with `hgs` installed at
  `~/.local/bin/hgs` (see [local installation](#local-installation)) and key-based
  SSH access from Windows. Status polls run SSH in batch mode, so the key must work
  without a passphrase prompt, for example through the Windows `ssh-agent` service
  (disabled by default; enabling it needs an administrator). Host aliases from
  `%USERPROFILE%\.ssh\config` work as usual.
- For local sessions, the agents you want to run, installed on the Windows
  computer ([below](#local-sessions-on-windows)).

Install and start:

1. From the [latest stable release](https://github.com/ufna/zerus/releases/latest)
   or a [nightly release](https://github.com/ufna/zerus/releases?q=nightly-&expanded=true),
   download `zerus-<version>-windows-x86_64.zip` and `SHA256SUMS`. Each release
   carries the zip its own CI run tested; stable 0.42.2 and earlier have none.
2. Check the zip in PowerShell. Both commands must show the same hash
   (`Get-FileHash` prints it in upper case):

   ```powershell
   Get-FileHash .\zerus-<version>-windows-x86_64.zip -Algorithm SHA256
   Select-String -Path .\SHA256SUMS -Pattern windows-x86_64.zip
   ```

3. Extract the whole zip and run `zerus-windows-x86_64\hgs-tray.exe`. Keep the
   files together: the app uses the `hgs.exe` next to it. The executables are not
   code-signed, so SmartScreen may show **Windows protected your PC** on the first
   start; choose **More info → Run anyway** only for a zip whose hash matched.
   The Zerus window opens. Closing it keeps Zerus running in the notification
   area, where Windows 11 first hides a new icon behind the **^** arrow; run
   `hgs-tray.exe` again to bring the window back.
   Alternatively, download `zerus-<version>-windows-x86_64-setup.exe` from the
   same Stable or Nightly release, verify its own `SHA256SUMS` entry and run it
   as your normal desktop user. The per-user installer contains the same tested
   ZIP payload, checks the WebView2 prerequisite and
   offers **Start Zerus at login**, off by default. It preserves configuration,
   drafts and native host images when upgrading. Its login choice applies only
   to the desktop app; local agent hosting remains a separate opt-in. Service
   setup invokes the maintained passive registration command, then registers the
   owned Windows Toast identity. Neither step starts the service or grants
   background consent. UI-only uninstall removes the separate UI login entry
   and preserves the staged CLI, service task and Toast identity.
4. To manage remote machines, add them in **Machines**. Accept a new host key once
   with **Open SSH terminal**, then use **Test connection**.
   **Install / update HGS…** builds `hgs` on the remote machine from a Zerus source
   checkout on this computer; the remote machine needs the
   [CLI requirements](#requirements). A checkout whose `install.sh` or
   `hgs_state.py` has CRLF line endings is refused.

In PowerShell, run the CLI from the folder with `.\hgs.exe` and quote a machine
argument, because `@` is special there: `.\hgs.exe '@build-01' ls`. Commands
copied from the desktop are quoted for PowerShell the same way. From Git Bash, set
`MSYS_NO_PATHCONV=1` before passing remote paths such as `/workspace/orbit`;
otherwise Git Bash rewrites them into Windows paths.

Configuration and state use the same layout as on Linux and macOS under
`%USERPROFILE%`, or under `HOME` when it is set: `.config\hgs` holds `config`,
`machines.local.json` and `tray.conf`, and `.local\state\hgs` holds the CLI state.
`HGS_CONFIG_DIR` and `HGS_STATE_DIR` move only the CLI's configuration and state;
the desktop app always reads `tray.conf` from `.config\hgs` under `HOME`, or
else under the account's profile folder, whatever `USERPROFILE` says. Desktop
settings are in `%APPDATA%\hgdev\hgs-tray.ini`, and drafts in the
`hgs-tray.ini.drafts` folder next to it. The computer's name in the fleet is
`HGS_SELF` when set, otherwise its lowercase `COMPUTERNAME`.

### Local sessions on Windows

Local sessions stay off until you enable them. Open **Machines** (**Open
Machines** in New session while no computer is available), select this computer
(*This computer: control console*) and choose **Enable local sessions**, or run
`.\hgs.exe session-host install` in the extracted folder. Either way, `hgs`
stages these files in your profile (under `HOME` instead when it is set):

- `%USERPROFILE%\.local\bin\hgs.exe`, the stable CLI that sessions, agent hooks and
  the recovery worker run, so they never depend on the extracted folder;
- `%USERPROFILE%\.local\share\hgs\host\<hash>\` with `hgs.exe` and the zip's ConPTY
  pair (`conpty.dll`, `OpenConsole.exe`), the image the session host starts from;
- the marker `%USERPROFILE%\.config\hgs\local-sessions`, which alone decides that
  local sessions are on (`HGS_CONFIG_DIR` moves it with the configuration).

Zerus then lists this computer's sessions, offers it in New session and Accounts,
and runs the stable CLI from its next start. When Zerus starts from another zip of
the same or a newer version, it first stages that zip's `hgs.exe` again (a small
*Starting Zerus…* note may show meanwhile); running sessions stay in the host that
runs them, and the next host starts from the new image. **Machines** shows the
session host's state for this computer, and `hgs.exe session-host status` reports
the marker, the staged files and the host.

When testing a local build with an explicit `--hgs` argument or `hgs=` in
`tray.conf`, run `.\hgs.exe session-host install` from that build's extracted
folder first. The explicit path selects the GUI's CLI and skips automatic
staging; native sessions and their hooks still run the stable CLI above. Keep
both CLIs on the same build before saving native permission selections.
Staging updates the CLI for new launches without restarting a running host or
its agents.

The session host starts with the first launch, resume or fork and exits a few
seconds after its last session has ended; listings never start it. Sessions keep
running when Zerus closes or updates and when the terminal or SSH connection that
started them ends. They end when you sign out of Windows or restart it; tracked
sessions stay saved and can be resumed, as after a reboot on Linux.

Install the agents from **Accounts** with **Install agent…**, or yourself. On
Windows, Codex installs with npm (`npm.cmd`, so Node.js must be installed), and
Claude Code and Kimi Code with their vendors' PowerShell installers (`install.ps1`),
which verify the binaries they download. Zerus looks the agents up on `PATH` (with
`PATHEXT`), adds `%USERPROFILE%\.local\bin`, npm's `%APPDATA%\npm` and Kimi Code's
`%USERPROFILE%\.kimi-code\bin`, and runs the script of an npm command shim with
Node.js directly; other batch files are refused.

- **Claude Code** and **Codex** run tracked, with Zerus hooks in their
  configuration. Every automatic CI run tests their
  [reviewed](ci-and-aur.md#native-agent-compatibility-and-upstream-review) Windows
  builds through the session host, and both were also checked on a Windows 11 PC.
  Claude Code before 2.1.296 needs Git for Windows for its hooks: without Git Bash
  Zerus refuses to track it, and `HGS_TRACKING=0` runs it untracked.
- **Kimi Code** runs tracked as well. It was checked on a Windows 11 PC and has no
  Windows CI suite yet.
- **Run PowerShell** in New session (`hgs sh`) opens an untracked PowerShell 7
  (`pwsh`), or Windows PowerShell when PowerShell 7 is missing.
- **DeepSeek Harness** runs on this computer in the WhiteWh fork (official
  `@deepseek-ai/dsh@0.2.0-rc.2` from npm): its resident host starts outside the
  CLI's job, as the OpenCode host does on other systems, and the Zerus bridge
  listens on a named pipe instead of a Unix socket. OpenCode is not offered.

Start Zerus and `hgs.exe` without elevation: a session host started from an
elevated terminal refuses every client that is not elevated, Zerus included, with
*the session host on this computer runs elevated*. A host started over SSH runs in
another Windows session, where agents cannot open windows on your desktop, such as
a browser sign-in; Zerus asks before it starts or resumes a session there
(**Launch anyway**).

A session host must not run in a job that ends it early, such as an agent's
sandbox or a terminal that starts it, so `hgs` refuses one that cannot leave such
a job (*the session host runs inside a job that does not let it break away*).
Some desktops run every process in a job, Explorer included; there the host shares
Explorer's job and ends with the desktop at sign-out, as sessions do anyway, and
`hgs.exe session-host status` reports it *in the desktop's job*.

To disable local sessions again, end the sessions on this computer and run
`hgs.exe session-host uninstall`, which refuses while one runs. It removes the
marker, so the computer is a control console again; the staged CLI and host images
stay in place. **Machines** has no button for this. `hgs.exe session-host stop`
stops an idle session host without disabling anything.

### Windows limitations

- Until local sessions are enabled, the computer has no local sessions, agent
  accounts or hooks, and `hgs.exe` exits with code 2 for local session commands.
  Commands for another machine (`hgs.exe '@build-01' …`) work as on Linux.
- No OpenCode sessions on the Windows computer. Its own DeepSeek Native UI opens
  in the default browser; another machine's needs SSH connection sharing, which
  Windows OpenSSH lacks, so a remote DeepSeek session cannot be opened externally
  from Windows. Its Activity still works; DeepSeek sessions have no Terminal tab
  on any platform.
- Voice messages cannot be sent to sessions on the Windows computer.
- Mobile connection runs the bundled Rust connector on this PC. Configure its
  HTTPS relay and computer credential, choose Start connector, then refresh
  Phones and pair by QR code. No other gateway is needed. Start the connector
  again after login; closing the GUI keeps it and native agents running.
  The first Windows gateway controls existing local Codex/Claude/Kimi sessions;
  mobile creation, folder/worktree/Git/file operations, peer routing and
  voice/speech are unavailable. Stop drains the current request and keeps
  the independent session host and native agents running.
- Other machines cannot connect to the Windows computer as a peer: its local
  sessions appear only in Zerus on that computer, and its folders in Projects are
  not shared with other machines.
- No installer, code signing, autostart or self-update. **Settings → Updates**
  starts on the Stable channel; choose Nightly to hear about nightly builds too.
  It reports a release with a newer product version than the running zip, with
  a link to its release page. Stable 0.42.2 and earlier have no
  Windows build, so they are never reported; Dev has none either, so on that
  channel it says so and reports no update. To update, quit Zerus from the
  tray menu and replace the folder with the new one; configuration and state
  stay in your profile. To start Zerus at sign-in, put a shortcut to
  `hgs-tray.exe` in the Startup folder (`shell:startup`) and add a space and
  `--background` at the end of its **Target** field, so Zerus starts in the
  notification area without opening its window.
  Zerus Next also supports an explicit per-user login registration:
  `hgs-tray.exe --install-login-item` enables background startup and
  `hgs-tray.exe --remove-login-item` removes it. Pass the same `--hgs` selector
  when registering a custom CLI. It is never enabled automatically. Existing
  Startup-folder shortcuts remain valid; duplicate background starts keep the
  window hidden.
- The separate `hgs desktop` service can be registered for the current user's
  next interactive sign-in with `hgs.exe desktop registration install --json`.
  Use `hgs.exe desktop registration status --json` to inspect it and
  `hgs.exe desktop registration remove --json` to remove only its registration.
  These commands never start or stop a running service or native agent. The
  task uses `InteractiveToken` and least privilege, never SYSTEM or Session 0;
  it runs the passive service with the selected configuration/state paths.
  Registration does not grant background writer ownership or enable automatic
  approvals. An explicit `HGS_EXECUTABLE` is preserved; otherwise installation
  stages only the stable `%USERPROFILE%\.local\bin\hgs.exe`, without enabling
  local sessions. A foreign or changed task is kept for manual review. If a
  command reports an uncertain outcome, inspect `status` before trying again.
  This service task is separate from the UI's Startup shortcut or Run value.
  Registration uses native Task Scheduler COM in a bounded helper of the same
  `hgs.exe`; it does not launch PowerShell or load user scripts. User shell
  sessions and vendor installers can still use PowerShell.
  The installer owns the Start menu shortcut and gives it the same
  `com.hgdev.hgs-tray` AppUserModelID and Toast activator as the native registry
  registration. `hgs` does not create or modify shortcuts; portable ZIP users
  use the existing registry-based notification registration.
- For service-owned Windows alerts, install the passive service registration
  first, then run `hgs.exe desktop notification-registration install --json`.
  `status --json` reports only registration, not service readiness or delivered
  alerts. These commands keep the `com.hgdev.hgs-tray` identity and bind its COM
  activation registration to the exact selected stable CLI/configuration. A
  foreign, partial or changed registration is kept. The worker never repairs it.
  The three current-user keys are `HKCU\Software\Classes\AppUserModelId\com.hgdev.hgs-tray`,
  `HKCU\Software\Classes\CLSID\{0F0F70A2-1D40-47D8-AC5E-D6A9B789605F}`,
  and `HKCU\Software\Classes\CLSID\{0F0F70A2-1D40-47D8-AC5E-D6A9B789605F}\LocalServer32`.
  After an interrupted install, inspect all three before manually removing
  only an owned partial set; the
  commands refuse partial sets rather than repairing or deleting them.
  Cold COM activation is passive and refuses without acquiring a service owner.
  Root service attachment and native alert/click acceptance remain separate.

  UI-only uninstall preserves this backend registration. Explicit backend
  removal first needs a settled manual revoke with a fresh UUID retained for
  that operation: `hgs.exe desktop revoke --json --request-id UUID --reason manual`.
  Then use `hgs.exe desktop notification-registration remove --json --request-id UUID`
  with that **same** UUID, followed by `hgs.exe desktop registration remove --json`.
  The Toast command only queries the original revoke; it does not issue or replay
  it. It requires the selected configuration, original runtime epoch and all
  accepted/durable/revoked/checkpointed fields. Busy, changed, missing or unknown
  evidence keeps the registration and stable CLI. An absent service with retained
  state is unavailable rather than substituted with a new baseline. Verified
  no-service/no-state is a separate path, not a live-runtime drain claim. Active
  removal currently refuses while Windows Runtime drain attachment is missing;
  a generic service receipt, component fixture or CLI exit zero is insufficient. Native notification-history
  deletion is not asserted.
- Without SSH connection sharing, every poll of every machine is a full SSH
  handshake: about 150 ms per machine on a LAN. The app lists this computer at
  most once every 15 seconds, or every 5 seconds with local sessions enabled.

[Windows details in the reference](reference.md#windows) |
[Building on Windows](../CONTRIBUTING.md#windows)

## Checking for updates

Open **Settings > Updates** to see the installed and published desktop versions,
the last successful check and any check error. **Update checks** contains the
automatic-check switch, release channel and **Check now** action. Automatic
checks run at most once a day for Stable/Nightly and every 15 minutes for Dev; manual checks remain available when
automatic checks are disabled.

When a newer release is verified, the workspace shows a compact **New version**
button with the Zerus mark. Its tooltip identifies Zerus and the release version;
clicking it opens Updates. The tray menu also offers Updates, and fresh newer
releases can show a system notification.

**View release** opens the published release page. Expand **Show installation
details** for package ownership and copyable Arch or Homebrew commands. Mac
downloads also offer a DMG button and manual replacement instructions. There, **Installation
instructions** opens this guide at the section for your system:
[Arch Linux](#arch-linux) on Arch-based Linux, [local installation](#local-installation)
on other Linux systems, [Homebrew](#macos-homebrew) on macOS and [Windows](#windows)
on Windows. These actions do not install an update automatically. A local build
remains separate from an optional AUR installation.

## Other machines

Add an SSH connection in **Machines**. The agent's machine needs `hgs` installed
at `~/.local/bin/hgs`, the selected agent and the project folder. SSH settings and
keys stay local.

To install on a Mac from another machine:

```sh
./mac-install.sh <ssh-alias>            # CLI and Zerus Next
./mac-install.sh <ssh-alias> --no-tray  # CLI only
```

Fresh installs and upgrades use Zerus Next, including upgrades from Qt.
Until Mac acceptance, `HGS_MAC_GUI=qt ./mac-install.sh <ssh-alias>` selects
the Qt fallback; use it on each install that should stay on Qt. Without the
override, the next install selects Next again. The maintained GUI updater
keeps the replaced Qt app as its rollback copy and preserves drafts.

Desktop signing uses the Mac's existing keychain. An optional
`HGS_MACOS_SIGN_IDENTITY` selector is forwarded to the remote installer; private
keys and signing credentials are never copied from the calling machine.

The CLI configuration lives at `~/.config/hgs/config`. For terminal use, you can
set SSH aliases directly:

```sh
HGS_SELF="workstation"
HGS_PEERS="macbook build-01"
```

Use **Projects** to collect folders from different machines into one project.
The shared project catalog syncs between connected members; local SSH access is
managed in **Machines**.

## Agents

Use **Accounts** to configure accounts and install supported agents.
On Arch, installing Codex or DeepSeek from Accounts requires both the optional
`nodejs` and `npm` packages. For a fresh npm setup, choose a writable user prefix
before installing agents; Zerus already recognizes `~/.local/bin`:

```sh
sudo pacman -S --needed nodejs npm
npm config set prefix "$HOME/.local"
```

Keep an existing working npm prefix if you already manage native agents there.
The current official DeepSeek Harness adapter is pinned to
`@deepseek-ai/dsh@0.2.1-rc.2`:

```sh
npm install -g @deepseek-ai/dsh@0.2.1-rc.2
hgs account login native-dsh
```

Then choose **DeepSeek Harness** in **New session**.
[Adapter capabilities and limitations](reference.md#deepseek).

See the [reference](reference.md) for commands and operational details, and
[CONTRIBUTING.md](../CONTRIBUTING.md) for build checks.
