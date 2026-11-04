# macOS distribution

[Install on macOS](installation.md#macos-homebrew) | [CI and Arch packages](ci-and-aur.md) | [Design](superpowers/specs/2026-10-10-homebrew-cask-design.md)

Apple Silicon Macs install Zerus from the tap
[ufna/homebrew-zerus](https://github.com/ufna/homebrew-zerus):

```sh
brew install --cask ufna/zerus/zerus
zerus-setup
```

Signed stable builds begin with
[0.43.0](https://github.com/ufna/zerus/releases/tag/v0.43.0).
For daily verified builds, install `ufna/zerus/zerus@nightly`; for Dev builds
from successful main CI, install `ufna/zerus/zerus@dev`.

| Cask | Built by | Version | Download |
| --- | --- | --- | --- |
| `zerus` | manual `release.yml` candidate, `publish.yml` | `VERSION` | `releases/download/vVERSION/` |
| `zerus@nightly` | daily `nightly.yml`, `publish-nightly.yml` | `VERSION.rCOUNT.gCOMMIT.nNUMBER` | `releases/download/nightly-RUN_ID/` |
| `zerus@dev` | successful main `ci.yml`, `publish-dev.yml` | `DEV_VERSION-dev.NUMBER` | `zerus.dev/downloads/dev-desktop-RUN_ID/` |

Stable and nightly casks ship with the AUR packages of the same candidate:
`zerus` with stable packages, `zerus@nightly` with `zerus-ade-nightly-bin`.
Dev follows successful main CI and uses the existing bounded website mirror,
without creating a GitHub Release for each push. All casks install the same
`Zerus.app`; only one channel can be installed. Intel Macs and a CLI-only formula
are not offered.

Mac casks are built only while the repository variable `MACOS_CASKS_ENABLED` is
`true`; until the signing credentials exist, stable and nightly candidates stay
Arch-only and publish as before.

## Bundle

For direct installation, GitHub Releases also carries
`zerus-VERSION-macos-arm64.dmg`. Open the image and drag `Zerus.app` to its
`Applications` shortcut. The read-only image contains the same stapled app as
the Homebrew ZIP; the image itself is separately signed, notarized and stapled.
See [DMG installation](installation.md#macos-dmg) for CLI and local-session setup.

`zerus-VERSION-macos-arm64.zip` contains one signed, notarized and stapled
`Zerus.app`:

| Path | Content |
| --- | --- |
| `Contents/MacOS/hgs-tray` | Zerus Next (Tauri); the executable keeps its name |
| `Contents/MacOS/hgs` | CLI, linked by the cask into Homebrew's `bin` |
| `Contents/MacOS/zerus-mobile-connector` | Rust mobile connector, signed with the other code; its LaunchAgent is opt-in through `zerus-setup --mobile-connector install` |
| `Contents/Resources/bin/zerus-setup` | Per-user setup, linked by the cask |
| `Contents/Resources/setup` | `hgs_state.py`, the desktop/session/recovery/swarm LaunchAgent installers, `launchd_agent.py`, the mobile connector's service installer and the tray LaunchAgent template |
| `Contents/Resources/licenses` | Zerus and Codex notices, CLI/connector Cargo notices in `rust/`, Tauri Cargo notices in `client-rust/`, and locked frontend notices in `frontend/` |

The bundle ID stays `com.hgdev.hgs-tray`, so settings, drafts and LaunchAgent
labels are shared with source installations. `CFBundleShortVersionString` is the
product version; `ZerusReleaseVersion` holds the stable, nightly or numbered Dev release version,
which the GUI compares to offer **Restart to finish update** after an upgrade, even
between nightlies of one product version. `CFBundleName` is `Zerus` and
`LSMinimumSystemVersion` is 14.0, the desktop deployment floor in
`client/src-tauri/tauri.switch.conf.json`. Every Mach-O file is arm64 only.
`scripts/check-macos-bundle.py` enforces the layout, notices, `Info.plist`
identity, minimum macOS version and that every library reference resolves inside
the bundle or the system. Public jobs use `--gui tauri --release RELEASE`, which
checks the bundled CLI, connector and all three dependency-notice inventories
and refuses Qt. Next uses the system WebKit and AVFoundation frameworks.

The casks have no install/uninstall hooks or `zap` action: upgrades and removal
never quit the GUI, unload agents or write to user homes. Per-user setup and
removal stay explicit through `zerus-setup` and `zerus-setup --remove`. Existing
settings, drafts, logs and connector journals remain on disk.

Explicit `zerus-setup` also registers `com.hgdev.hgs-desktop` in the current
user's Aqua session. It consumes the selected CLI's `desktop registration
--json` definition and checks the lifecycle ABI, configuration and loaded actor
PID before reporting IPC ready. A missing Aqua session saves the registration
for next login and reports readiness as unverified. IPC readiness does not mean
that background workers have started. Unmarked or linked files and definitions
for another configuration are kept. A marker for the same configuration and
service label identifies an owned registration even when its old CLI path or definition differs. The
installer checks a loaded job against that old argv before replacing or
removing it.

Native frontend requests, including ownership Grant/Rearm, and explicit service
mode changes share one macOS ancestry check. Every own-user process keeps its
full native identity, executable, environment and agent-marker checks. Foreign
ancestors are classified using short BSD metadata and the native executable
path: full BSD metadata can be inaccessible for root-owned processes.

The root-owned `/usr/bin/login` used by Terminal is only a bridge. It must be
the protected system image, have effective and saved UID zero, real UID equal
to the desktop user, and matching PID, process group and session ID. The walk
checks the original child-parent generation identities and continues through
Terminal; an agent above the bridge still refuses. It ends only at the verified
root `/sbin/launchd` PID 1 with parent 0. Native process generations, executable
UUIDs, versions, credentials and protected image metadata are re-read after the
walk, including the bridge and Terminal. Changed or unavailable evidence and
every other foreign ancestor refuse. In particular, an SSH ancestry containing
a root `sshd-session` is unsupported; use the actual installed frontend or a
foreground Terminal outside Zerus. Linux keeps its separate per-user-manager
boundary.

Identity comparisons include only the fixed SYSTEM, LP64, PSUGID and EXEC
flags. App Nap, scheduling/resource-control flags and live scheduler states may
change between reads. Tracing is a separate explicit refusal on both the first
and final read; unavailable identities and zombie processes still refuse.
This checks current ancestry: a same-user process reparented to launchd with its
own controlling terminal is outside its scope, as on Linux.

The Darwin caller fixture prints a bounded PID/UID/real-UID/path/session topology
before its positive case. Only on GitHub-hosted macOS may an observed unsupported
foreign ancestor skip that fixture with an explicit reason. Unreadable metadata
and failures of the recognized login/launchd checks still fail; the diagnostic
never grants native permission or establishes installed acceptance.
Agent-marker, agent-named executable and checker-impersonation refusal cases
run before that hosted-only skip is considered.

Source `install.sh` reloads a PATH-backed desktop service when its CLI bytes
change. An owned service with a separate runner keeps its registered backend;
the CLI-only install refreshes its ordinary runner without selecting a different
service image. After rollback to an existing unpaired app, a missing bundled CLI
and absent pair record restore the ordinary stable-CLI registration. A remaining
pair record, present-but-broken helper, missing app or link still refuses.
That repair preserves the owned service mode; it does not select passive mode
or create a new background grant.
Recovery, swarm and voice retain their existing installer/update rules.
`tray/install.sh` preflights it before the GUI swap and retains an
unchanged active backend during GUI-only updates.

### Explicit desktop service mode

Fresh LaunchAgent registration is passive. Start the maintained Next frontend
in service mode and complete its authenticated Grant against that passive
actor first; a retained revoke requires explicit Rearm. Then use the maintained
helper from a foreground terminal **outside Zerus** in the current Aqua login:

```sh
python3 scripts/install-macos-desktop-service.py "$APP/Contents/MacOS/hgs" \
  --runner-hgs "$RUNNER_HGS" --mode background --json
python3 scripts/install-macos-desktop-service.py "$APP/Contents/MacOS/hgs" --status --json
```

`APP` and `RUNNER_HGS` are the exact installed paired app and independent runner,
not a build or guessed sibling. Optional absolute `--config-dir` and `--state-dir`
select the same configuration as the frontend. Registration is rendered by the
signed bundled service CLI before the ordinary runner override is applied. A
mode change preserves that service/runner split; ordinary GUI installs and
CLI-only `--refresh-runner` also preserve the selected mode. The GUI's own
`--hgs` selection is unchanged.

The native shared caller guard requires the actual controlling foreground TTY
or installed frontend ancestor and rejects agent ancestry, including stripped
child environments. `HGS_SESSION`/`HGS_RUN_ID` in terminals opened inside Zerus
also refuse. A missing/stale native ownership grant, no Aqua session, custom
plist controls or an unknown original transition refuses. The helper does not
create a grant, clear revoke, change defaults or restart the GUI. It reloads
only the owned `com.hgdev.hgs-desktop` LaunchAgent; other services and agents
remain under their own lifecycle.

For rollback, use the same selections with `--mode passive --json`. The helper
checks the selected CLI's `desktop-revoke-hold-v1` help marker before recording
one original rollback UUID or offering its revoke. Before any unload or rewrite,
it requires that exact durable checkpointed runtime/resource drain and acquires
the original native hold child. Its stdin writer stays private and open through
bootout, plist/record writes, bootstrap and passive readiness. While held, every
manager and CLI command checks that same child before and after; manager children receive
DEVNULL stdin and cannot inherit the writer. Success requires its matching
release frame, actual exit and both pipe EOFs before clearing the transition.
See [the original hold contract](desktop-revoke-hold.md). An uncertain
result retains `transition.request_id`; inspect `--status --json` and resume only
the same passive transition. Do not replace the UUID or switch to background
while rollback is pending. After success `last_revoke_request_id`
identifies that original receipt. Returning to background needs authenticated
frontend Rearm. `selected_mode`, `running_mode` and `pending_restart` are separate
status fields; they do not grant permission or prove individual workers ready.
Mode changes require an already owned registration and the current Aqua login.
Fresh setup is passive; headless refresh preserves the existing selection and
does not establish active-background acceptance.

If a newer authenticated Grant/Rearm won before the hold, the old UUID refuses.
A pending record can also remain if the helper stopped before its original
revoke took effect. With a verified current Service grant, the owner can cancel
only that registration metadata using the original UUID:

```sh
python3 scripts/install-macos-desktop-service.py "$APP/Contents/MacOS/hgs" \
  --runner-hgs "$RUNNER_HGS" --cancel-rollback --request-id ORIGINAL_UUID --json
```

Cancellation repeats the native caller and current-grant checks under the same
installer lock, keeps the plist, selected mode, native grants/revoke receipts and
manager unchanged, and records `last_cancelled_rollback_request_id`. It performs
no launchctl operation. A later `--mode passive` is a separate explicit intent
with a new UUID; unknown originals are never automatically resent or rotated.
Do not delete records or the database to recover.

The signed app's fixed `--desktop-notification-host` entry keeps
`com.hgdev.hgs-tray` without a window, Dock tile or ordinary GUI instance.
Its sealed `HGSDesktopNotificationHostABI` capability lets the desktop service
refuse older apps before attempting that entry. This is an internal entry:
it requires the factory's inherited connected socket and private owner lock,
then verifies its own paired bundle and the actual service peer before reading
commands. A command-line invocation cannot establish that ownership. Native
completion blocks and outgoing receipts retain the original work until their
actual teardown; a timeout never starts a replacement host. Service activation
and signed native delivery acceptance remain separate from shipping this entry.

A custom or foreign `com.hgdev.hgs-desktop.plist` makes this preflight refuse
the GUI update before replacing the app. Review or migrate that definition
deliberately, then rerun the installer; it does not overwrite another
configuration's LaunchAgent to complete an update.

`zerus-setup --mobile-connector install` writes the
`com.hgdev.zerus-mobile-connector` LaunchAgent for the bundled connector
(`AbandonProcessGroup`, `KeepAlive` on failure only, logs in `~/Library/Logs/hgs`),
only when none exists or it is its own, and prints the `launchctl bootstrap`
command instead of loading it. `zerus-setup --remove` unloads and removes it with
the other agents of the bundle; the agents and tmux servers it launched keep
running.

Under this agent, macOS attributes the connector's file access, and that of its
`hgs` and `git` children, to Zerus (`com.hgdev.hgs-tray`) rather than to
`zerus-mobile-connector`. Folder browsing and worktree catalogs in Documents,
Desktop and Downloads therefore use Zerus's Files and Folders consent; the
connector needs no consent of its own and no Full Disk Access. The stored consent
carries Zerus's Developer ID requirement, which later builds signed with the same
identifier and team satisfy. This was rehearsed on 2026-10-17 with a signed main
build under a fixture label, on a Mac where Zerus already had that consent, and
no prompt appeared (`zerus-nbzl.23`).

## Legacy Qt bundles

The release, Nightly and Dev bundle jobs build Zerus Next without Qt. The
pinned Qt tooling below remains for legacy builds and their regression checks.

`packaging/macos/qt.json` pins official Qt **6.11.2** (`qtbase`, `qtsvg`,
`qtmultimedia`, `qttools`, `qtdeclarative`) by archive size and SHA-256. `qttools`
supplies the build-time `lrelease` compiler for the optional interface catalog;
it does not change the English default. `lrelease` loads QtQml, so the build
prefix also needs `qtdeclarative`; the GUI links neither module. Qt WebEngine and
the other modules only it needs (`qtwebchannel`, `qtpositioning`) are not pinned:
the embedded DeepSeek Native UI is an optional Linux plugin, so on macOS
Native UI opens in the browser. `check-macos-bundle.py` rejects any Qt WebEngine
file. `scripts/install-qt-macos.py` downloads the archives from
`download.qt.io` (mirrors may serve them), verifies every archive before any
extraction and extracts them with the system `bsdtar`, through
`scripts/qt_archives.py`, which the [Windows build](ci-and-aur.md#windows-client-desktop-and-portable-zip)
shares. Framework links must stay inside the prefix. Before any Qt tool runs,
the installer reads the Mach-O load commands of `qmake`, `lrelease`,
`macdeployqt`, `moc` and `rcc` and stops when one of them, or a framework it
loads, needs a Qt framework the prefix lacks. The official Qt is relocatable
and needs no path patching.

Qt 6.11.2 is the version macOS CI tests through Homebrew, supports macOS 14 and
has public sources (`qt-everywhere-src-6.11.2`, `v6.11.2` tags) for the LGPL
source pointer. On 2026-10-10 the Qt 6.11.3 binaries were available without
published sources, so they cannot be shipped. Its relevant QtNetwork and
QXmlStreamReader security fixes are already in the v6.11.2 sources. Qt 6.12.0 also
passed all Qt suites but requires macOS 14.4.

To move to another Qt version, change the version and package names in
`qt.json`, confirm that its exact sources are published, then refresh the pins:

```sh
python3 scripts/install-qt-macos.py --lock
python3 scripts/install-qt-macos.py --output /tmp/zerus-qt
```

Build and test the tray against that prefix (`ZERUS_QT_PREFIX=/tmp/zerus-qt bash
scripts/ci/macos-bundle.sh`) and review the module SBOMs before committing.

## Candidates

`macos-cask.yml` is a reusable workflow with one input, `channel`. `release.yml`
calls it with `stable` when `macos_cask=true` (the default; it then requires
`run_macos=true`) and `MACOS_CASKS_ENABLED` is `true`; `nightly.yml` calls it with
`nightly` whenever the variable is `true`. Trusted main CI calls it with `dev`
after the full gate succeeds, when both `DEV_UPDATES_ENABLED` and
`MACOS_CASKS_ENABLED` are enabled. All callers explicitly forward only
the five Apple secret names declared by the reusable workflow. GitHub requires
this forwarding even for environment secrets; without it their values are empty.
The values remain in the main-only `macos-signing` environment and are resolved
only by the signing job. The callers never use `secrets: inherit`. See
[GitHub's reusable workflow secret rules](https://docs.github.com/en/actions/how-tos/reuse-automations/reuse-workflows#using-inputs-and-secrets-in-a-reusable-workflow).
The same three stages remain required. Stable/Nightly start them after full
verification. Dev overlaps only the credential-free bundle with its core gate;
`macos-cask.yml` reuses that exact run's checked artifact and waits for both before
signing. An unsigned version from another Dev run is refused before key import.
The unsigned job builds the Release Tauri switch app with voice and video, then
packages that same output for all channels:

1. **macOS bundle** (`macos-15`, no secrets) selects the release version (the
   product version, the nightly version computed exactly like
   `prepare-nightly.py`, or `DEV_VERSION-dev.<CI run_number>`), installs the locked
   npm toolchain and runs `scripts/ci/macos-bundle.sh`. It builds the CLI, connector
   and Next with a macOS 14.0 target, runs frontend and Rust regression tests
   (only numbered Dev may opt out), and assembles the production bundle,
   runs the bundle checks and self-tests and uploads `Zerus-unsigned.zip`.
   Dev passes the same CI sequence to the Tauri build. The unsigned, mounted and installed
   apps report their embedded build identity through `--build-info`; validation
   requires the expected version, Dev sequence and clean source commit.
   Bundle and installation runners install `tmux` explicitly. Self-tests use a
   private home and existing private socket directory, with only tmux exposed
   alongside system tools; they cannot inspect a developer's running sessions.
2. **macOS signing and notarization** runs in the `macos-signing` environment.
   It compiles nothing and executes only scripts from the same main commit.
   `scripts/ci/macos-keychain.sh` creates a temporary keychain;
   `scripts/sign-macos.py` signs inside-out with the hardened runtime and a secure
   timestamp, submits the app to the notary service and staples its ticket.
   It then creates the DMG, signs and notarizes it separately, staples its ticket
   and writes both release assets with `macos-info.json` only after both succeed.
   The job verifies the Team ID, Gatekeeper and the stapled ticket, then deletes the
   keychain and key files even on failure. A team API key passes its issuer ID; an
   individual API key leaves `APPLE_NOTARY_API_ISSUER_ID` empty.
   Stapling and ticket validation retry a lost CloudKit connection
   (`NSURLErrorDomain -1005`, exit 68) at most four times, after 5, 15, 30 and
   60 seconds, on the same signed file. The bundle checker and cask image check
   use the same validation policy. Other failures stop immediately; an exhausted
   retry or invalid ticket prevents publication. Signing and notary submissions
   are not retried by this policy.
3. **macOS cask installation** (no secrets) checks the DMG signature and ticket,
   mounts it read-only and verifies the contained app, self-tests and Applications
   shortcut. It then renders the channel's cask, runs `brew style`,
   installs it from a local tap pointing at the candidate bytes, repeats the bundle
   checks on `/Applications/Zerus.app`, runs `zerus-setup` and `--remove` in a
   scratch home and uninstalls the cask.
   After validating the image and mounted app with Gatekeeper and their stapled
   tickets, CI installs the cask with Homebrew's default quarantine and assesses
   the installed copy again before execution. Only after it passes does CI clear
   that disposable copy's download attribute, so the normal first-open
   confirmation cannot block unattended self-tests. Public install commands,
   casks and release archives retain their normal quarantine behavior.

Only entitlements with a concrete need are granted, and only to the application:
`com.apple.security.automation.apple-events` with `NSAppleEventsUsageDescription`
because Terminal integration runs `osascript`, whose Apple Events TCC attributes
to Zerus, and `com.apple.security.device.audio-input` and
`com.apple.security.device.camera` with `NSMicrophoneUsageDescription` and
`NSCameraUsageDescription` because the hardened runtime blocks voice and video
message recording without them. Without Qt WebEngine there is no helper process
that would need JIT.

The Arch assembly job then adds the signed ZIP, DMG and rendered cask through
`prepare-release.py --macos` (`homebrew/zerus.rb`) or `prepare-nightly.py --macos`
(`homebrew/zerus@nightly.rb`). The candidate records release-info schema 2: every
schema 1 field is unchanged, plus `macos` (asset, legacy `qt_version`, Team ID, notarization
ID, minimum macOS, and `dmg: {asset, notarization_id}`) and `homebrew_casks`
(`["zerus"]` or `["zerus@nightly"]`). Older schema-2 releases without `dmg`
remain valid; all new signed builds require both formats. The `qt_version`
field remains a semantic-version compatibility marker for existing Qt and Next
feed parsers; it does not indicate a Qt dependency in Next.
`SHA256SUMS` covers both assets. New casks use the templates in
`packaging/homebrew/` with only the version, digest and, for a nightly, the
immutable `nightly-RUN_ID` tag substituted. Sealed-candidate validation also
accepts the exact reviewed pre-Next templates in `packaging/homebrew/previous/`
so publication retries preserve the original candidate without rebuilding it.
The cask continues to download the ZIP. Deploy DMG-aware `update_feed.py` on
the website mirror before publishing the first DMG candidate: both macOS files
remain on GitHub and are excluded from the existing desktop catalog artifact kinds.

When the variable is `true`, a skipped or failed cask fails the candidate: a stable
release needs `-f macos_cask=false` to stay Arch-only, and a nightly that cannot
build its bundle publishes neither AUR nor Homebrew.

## Publication

`publish.yml` and `publish-nightly.yml` check the candidate as before. For a schema 2
candidate they also require the tap deploy key. Before any GitHub write,
`publish-release.py` prepares the AUR changes and the tap commit, each with a
dry-run push. It then publishes the GitHub release or prerelease, verifies every
asset anonymously, pushes AUR and finally pushes the channel's cask file,
`Casks/zerus.rb` or `Casks/zerus@nightly.rb`; the other cask stays untouched. The
tap step refuses a lower version (stable versions compare numerically, nightlies by
Nightly run number) and a changed cask for an already published version, treats an
identical cask as done and never force-pushes; the tap's ruleset also blocks force
pushes and branch deletion. If any push fails, rerun the same publisher with the
same candidate run. Never rebuild or replace published assets.

The website mirror must run the current `scripts/update_feed.py` before
`MACOS_CASKS_ENABLED` is set: older versions reject a stable or nightly release
with a macOS asset. Like the Windows zip, the macOS zip stays on GitHub:
the mirror checks its sealed name and GitHub digest but neither stores it nor lists
it in catalog artifacts; see [public distribution](public-distribution.md).

Dev assembly adds the signed ZIP and DMG through `prepare-dev-update.py --macos`
to the existing desktop candidate. Its optional top-level `macos` object contains
the signing provenance plus exact `downloads` entries (`name`, `url`, `size`,
`sha256`); the source/Arch `artifacts` array stays compatible with older clients.
The mirror retains these bytes under the same three-build/72-hour grace/20 GiB
policy as other Dev files. Deploy `macos_release.py`, `dev_updates.py` and
`update_feed.py` together before the first Mac Dev candidate arrives.

After the mirror job succeeds, `publish-dev.yml` runs `publish-dev-cask.py` in
the protected release environment. It independently verifies the successful main
CI run, sealed candidate, exact public catalog and both public Mac file hashes,
then renders and pushes only `Casks/zerus@dev.rb`. Its `release-publication`
concurrency group serializes tap writes with stable/nightly publishers. An older
superseded candidate leaves the tap unchanged; changed bytes under an existing
version fail. Retry the publisher with the same CI run, without rebuilding.

Stable/nightly v1 catalogs expose the same optional `macos` download projection
with GitHub URLs. Existing catalogs may gain only this projection from their
already sealed release; all prior fields and release bytes stay immutable.
Settings > Updates recognizes actual Homebrew ownership and direct DMG installs,
defaults to the installed channel unless the user already chose another, compares
the full installed release identity and offers a copyable Homebrew command or
DMG download. A channel without a Mac build offers neither an upgrade command nor
a download. Installation and GUI restart remain explicit.

## Configuration and credentials

Configured on 2026-10-10:

- Public tap `ufna/homebrew-zerus` with README, MIT license and `Casks/`; issues,
  wiki and projects disabled; ruleset "Protect main" blocks deletion and
  non-fast-forward pushes.
- Write deploy key "Zerus publish.yml (release environment)"; its private key
  exists only as the `release` environment secret `HOMEBREW_TAP_DEPLOY_KEY`.
- Environment `macos-signing`, restricted to the `main` branch like `release`.
- `packaging/homebrew/known_hosts` pins GitHub's Ed25519 host key
  (`SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU`, from
  `api.github.com/meta` and GitHub's published fingerprints).

Provisioned and tested on 2026-10-15 (`zerus-6a2u.2`): a **Developer ID
Application** certificate issued by the G2 intermediate and a Developer-role
App Store Connect team API key for notarization. All five secrets below and
`MACOS_TEAM_ID` are configured in the main-only `macos-signing` environment.
Private encrypted exports and API key backups remain outside the repository.

For rotation, store values from private files or prompts, never through chat or
literal command arguments. Commands without `--body` or stdin prompt for the value:

```sh
base64 -i DeveloperIDApplication.p12 | gh secret set MACOS_DEVELOPER_ID_P12 --env macos-signing --repo ufna/zerus
gh secret set MACOS_DEVELOPER_ID_P12_PASSWORD --env macos-signing --repo ufna/zerus
gh secret set APPLE_NOTARY_API_KEY_P8 --env macos-signing --repo ufna/zerus < AuthKey_KEYID.p8
gh secret set APPLE_NOTARY_API_KEY_ID --env macos-signing --repo ufna/zerus
gh secret set APPLE_NOTARY_API_ISSUER_ID --env macos-signing --repo ufna/zerus   # team keys only
gh variable set MACOS_TEAM_ID --env macos-signing --repo ufna/zerus --body TEAMID
```

For initial activation, verify that the deployed mirror's `update_feed.py`
supports schema 2 and the macOS asset; redeploy it if needed. Then enable both
casks and start a manual nightly as the first signed build:

```sh
gh variable set MACOS_CASKS_ENABLED --repo ufna/zerus --body true
gh workflow run nightly.yml --ref main
```

If signing or notarization fails, that nightly publishes nothing; fix the
credentials and rerun, or unset the variable to return to Arch-only candidates. Rotate the certificate or
API key by testing its replacement before changing the secrets or revoking the
old credential. Never revoke an unrelated team certificate.

The original Developer ID intermediate expires on 2027-02-01. Xcode issued a
certificate from that old chain during setup, so CI was switched to a
portal-issued G2 replacement before any release. Its expiration is 2027-11-14.
Check the leaf certificate's issuer and expiration during each rotation; select
**G2 Sub-CA** when the portal offers a choice. See
[Apple's replacement procedure](https://developer.apple.com/help/account/certificates/replace-developer-id-certificates/).

### Local source builds

For the owner's feature evaluation, merge a trusted internal PR after a short
review, then build and update the Mac immediately from main. Hosted CI,
regressions and public Dev publication run independently. A clean persistent
checkout with retained Cargo and CMake build directories keeps subsequent
iterations incremental. Use the CI sequence of that exact main push as
`ZERUS_DEV_BUILD_NUMBER`; do not wait for its result.

From that clean checkout on the Mac, use the maintained installers. The default
GUI is Zerus Next; Node/npm and Rust must be installed:

```sh
export ZERUS_DEV_BUILD_NUMBER=NUMBER_FROM_THIS_MAIN_PUSH
export HGS_MOBILE_CONNECTOR=0
bash install.sh
bash tray/install.sh
```

The CLI and GUI builds can overlap before installation. Keep the build and
signature checks, then let the GUI updater save drafts, retain native processes
and verify the installed source/version. Skip full regression runs in this local
delivery path; investigate their CI results separately. The connector remains
unchanged unless its update is separately requested. These locally signed builds
are for evaluation; public distribution retains its packaging and notarization
flow below.

Raw CMake build bundles use `com.hgdev.hgs-tray.local`, so an uninstalled ad hoc
copy cannot replace the installed application's privacy identity. Do not grant
that copy permissions as a substitute for the installed app. Settings, native
profiles and session identities are unchanged.

For an owner-requested branch preview, keep main deployment paused in the other
sessions and build a clean branch commit with
`-DZERUS_DEV_BUILD_NUMBER= -DZERUS_LOCAL_PREVIEW_LABEL=task-board`. The displayed
identity is `DEV_VERSION-preview.task-board.SHORT_COMMIT`, with no public Dev
sequence. The same certificate signer and maintained GUI updater apply. This is
a local evaluation build, not a public update channel or a main publication.
Before returning the checkout to main, clear `ZERUS_LOCAL_PREVIEW_LABEL` and set
the exact main CI sequence again. Build labels must never hide dirty sources.

For Next, `tray/install.sh` prepares a disposable app/CLI pair through
`tray/prepare-local-macos.py`, then signs it through `tray/sign-local-macos.py`
before replacing or restarting
the installed application. Only after selecting a compatible certificate does the helper promote
the staged copy to the installed `com.hgdev.hgs-tray` identifier; failed preparation
restores the local identifier and the installer removes its temporary copy.
The build-tree bundle stays local. The helper uses a valid
application-signing identity already in the local keychain; it never imports or
unlocks credentials and never falls back to ad hoc signing. `mac-install.sh`
uses the destination Mac's keychain and forwards the optional
`HGS_MACOS_SIGN_IDENTITY` exact certificate name or SHA-1 selector.

The paired copy contains exactly the app and `Contents/MacOS/hgs`. The signer
signs the CLI first with identifier `hgs`, then seals it and
`Contents/Resources/zerus-local-pair.json` into the app. The record contains the
unsigned build artifact's SHA-256 and the signed CLI's SHA-256. Both signatures
must be genuine Apple application signatures from the same team, with stable
designated requirements; both retain mutual continuity with an existing pair.
Unexpected nested Mach-O files, links and special files refuse preparation.
The signer's default single-executable mode remains available for the explicit
`HGS_MAC_GUI=qt` fallback.

The independent `~/.local/bin/hgs` remains the ordinary CLI for agents, hooks and
session recipes. Before building or replacing the GUI, the installer requires
its bytes to equal the root build artifact. A mismatch asks you to run
`./install.sh` (or `mac-install.sh`) from the same checkout first. A custom Cargo
output can be selected with `HGS_MAC_CLI_BUILD=/absolute/path/to/release/hgs`;
`mac-install.sh` passes its retained native artifact explicitly. Signing changes
only the private bundle copy, never that artifact or the stable CLI.

After the app swap, the owned desktop LaunchAgent selects the signed bundled
CLI as its executable and sets `HGS_EXECUTABLE` to the independent stable CLI
for ordinary descendants. A changed backend reloads only this desktop service;
an unchanged backend stays running. The GUI keeps its explicit `--hgs` stable
CLI selection. The desktop-service client selects the bundled CLI only through
fresh native running-self and sealed-pair verification; a purported pair that
fails stays unavailable. Unpaired/source launches keep ordinary CLI discovery.
The read-only service client verifies once per connection, then checks the fixed
generation metadata before each CLI exchange; catalog pages do not rehash the
whole app. Changed generations refuse until an explicit reconnect verifies again.
Ancestors are compared by identity, mode and ownership, so unrelated home-folder
updates do not invalidate the app. The bundle directory's ctime is excluded
because last-used xattrs change it; code, plist and seal files keep full stamps.
The shared `desktop::relay::select` API also supplies the bundled service image
for the authenticated frontend-request sender; that still-unavailable sender is
a separate UI integration. The stable runner is not native authority.
Native host entry/cutover is a separate
change: pairing alone enables neither notifications nor a new permission.
`update-installed.py` still saves drafts, checks native processes and keeps one
rollback app; an old Qt copy continues to use the stable CLI.

When a certificate-signed app is already installed, selection follows its team
and signing class. A local rebuild of the published app therefore needs a
compatible Developer ID Application identity. Apple Development can retain its
own local identity, but has a different default designated requirement from
Developer ID. Without an installed certificate identity, selection prefers one
unambiguous Developer ID Application certificate, then Apple Development.
Missing or ambiguous identities stop installation with setup instructions.

The keychain must be unlocked in the signing context and allow `codesign` to use
the private key. An SSH login does not unlock it. For `errSecInternalComponent`
or a key-access error, test from Terminal on the Mac and respond to the system's
unlock or key-access dialog. Follow [Apple's signing troubleshooting](https://developer.apple.com/forums/thread/712005)
before testing SSH again. Do not put keychain passwords in environment variables,
repository files or command arguments; the installer never edits key ACLs.

The helper verifies the candidate signature and checks that each app satisfies
the other's designated requirement, as described in
[Apple TN3127](https://developer.apple.com/documentation/technotes/tn3127-inside-code-signing-requirements).
A shared bundle identifier or team alone is insufficient. An incompatible
signature stops before the maintained GUI updater can replace the installed app.
An old ad hoc installation can move to certificate signing with fresh consent
once; the installer does not reset TCC or grant permissions.

The source bundle contains one executable and links external Homebrew Qt.
It is signed without the hardened runtime or a network timestamp, for local
development. Unexpected nested code or symbolic links are refused. Official
packaging also promotes only its private staged copy to the unchanged public
identifier. Public distribution continues to use the separate inside-out hardened-runtime signer,
entitlements, timestamp and notarization checks above.

An opt-in native regression uses two different synthetic executables to verify
mutual designated-requirement compatibility, compares them to the installed app
read-only, and proves that an ad hoc replacement is rejected:

```sh
ZERUS_TEST_LOCAL_MACOS_SIGNING=1 python3 -m unittest discover -s tests -p test_macos_local_signing.py -v
```

### macOS privacy permissions after a signature change

macOS attributes agent-launched Apple Events, Accessibility and screen capture
to the containing `com.hgdev.hgs-tray` application, displayed as **Zerus**.
An enabled Settings switch is not proof that the current binary is trusted.
During qualification, the TCC log showed `SecStaticCodeCheckValidity -67050`:
the stored requirement still matched an older ad-hoc build's code hash. Toggling
the existing entry retained that stale requirement.

After verifying this exact mismatch, reset only the affected service for this
bundle, request access again through its public API, and let the user grant fresh
consent in System Settings:

```sh
tccutil reset Accessibility com.hgdev.hgs-tray
tccutil reset ScreenCapture com.hgdev.hgs-tray
```

These are separate services; run only the required reset. Confirm access with
`AXIsProcessTrusted()` and `CGPreflightScreenCaptureAccess()` respectively.
Retain a stable Developer ID designated requirement across updates. Do not reset
all privacy permissions, grant consent on the user's behalf or restart native
agents to repair GUI permissions.

## Tauri client switch

On the owner's decision of 2026-10-28, the Tauri client (`client/`) replaces the Qt GUI on the Mac as Zerus. It takes over Qt's identity, `com.hgdev.hgs-tray`, at the installed path `~/Applications/hgs-tray.app`. Qt is removed at the switch and kept as the one rollback copy; all of Qt's data and drafts stay where they are. The dev app keeps `dev.zerus.next`.

### The switch bundle

`client/scripts/switch-config.py` writes `client/src-tauri/target/switch.conf.json` from `client/src-tauri/tauri.switch.conf.json` and `VERSION`, plus a generated Info.plist with `ZerusReleaseVersion`. `--release` selects the distribution release; local numbered Dev builds derive it from `DEV_VERSION` and `ZERUS_DEV_BUILD_NUMBER`. The source template stays unchanged. `tauri build --config` with it produces `Zerus.app`:

- **Identity.** It has the executable `hgs-tray` and the build identifier `com.hgdev.hgs-tray.local`; `tray/sign-local-macos.py` promotes its signed copy to `com.hgdev.hgs-tray`, as for Qt's raw bundles.
- **Info.plist.** `Info.switch.plist` adds Qt's keys:
  - `CFBundleName` Zerus, with `CFBundleDisplayName` hgs-tray and `en.lproj` naming it Zerus;
  - `LSUIElement`;
  - Qt's icons and usage texts;
  - `ZerusGUI = tauri`, which `tray/update-installed.py` uses to tell the clients apart.
- **LaunchAgent template.** It ships in `Contents/Resources/setup/com.hgdev.hgs-tray.plist`. The installed agent is unchanged: `KeepAlive Crashed`, `ProcessType Interactive`, `--hgs` and the same label.

The client behaves as Qt does at the points other tools rely on:

- **Command line.** `--hgs`, `--sessions`, `--background`, `--version` and `--build-info` (Qt's JSON). Qt's other options are accepted and ignored.
- **Single instance.** It uses Qt's protocol: the lock `~/Library/Application Support/hgs-tray-start.lock`, the socket `hgs-tray` in the canonical per-user temporary folder, and the lines `show` and `sessions`. A start that launchd did not run waits for the service.
- **Quitting.** The Apple Event quit and SIGTERM, SIGINT and SIGHUP all end in the same exit path, which flushes what must reach the disk and removes the socket.
- **Menu-bar item.** It shows Qt's tile, tooltip and menu. Refresh and the update entries are left out. A primary click opens Zerus; closing the window keeps the app in the menu bar.
- **Notifications.** It posts Qt's attention notifications under the same identifiers (`zerus.attention.<token>`). Qt's saved `attention.observed` seeds the client's state once, so banners Qt delivered are withdrawn and stay clickable. Qt's update notices ("Zerus update available") are not ported yet (`zerus-5wmh.12.4`).
- **Main-thread watchdog.** Qt's: a main thread silent for 30 s aborts the client, so launchd's `KeepAlive Crashed` restarts it, with a hold around sleep and dark wakes. The menu and modal panels do not stop its heartbeat.

The client keeps its own data in `~/Library/Application Support/com.hgdev.hgs-tray/`: read marks, `attention-observed.json`, `menubar.json` and its drafts root, `drafts/`. It reads Qt's settings domain, `~/.config/hgs/tray.conf` and Qt's drafts, and never writes them. Its settings are seeded once, at its first start, from Qt's values (coordinator ruling: "Seed it once from Qt's values with Qt's types and defaults, and never write Qt's store"). The seed reads Qt's domain read-only through `defaults export`, records itself in `settings.json` (source, time, keys) and never runs again, so a later Qt edit is not imported. Three exceptions: the client's own `client/…` settings start at their defaults; the microphone (`speech/microphone`) must be chosen again, because Qt keeps a device id the client cannot use; and automatic approvals start off: Qt's choice is kept only as historical `automaticApprovals/qtEnabledAtSwitch`, never as consent. Next requires its own explicit confirmation before its automatic-approval worker runs (`zerus-5wmh.54`). Read marks start fresh, since the hosts' read state is authoritative; the one-time draft import (`zerus-5wmh.13.1`) copies Qt's drafts and the dev app's drafts into its drafts root, reading both read-only. AppKit writes its own keys into the shared defaults domain: the status item's position and visibility (`NSStatusItem …`) and, where used, window and panel state (`NSWindow Frame …`, `NSNav…`). The updater reports them after each client update.

### Building and installing

Numbered Dev installs need a clean checkout of the exact main commit, as for Qt:

```sh
export ZERUS_DEV_BUILD_NUMBER=NUMBER_FROM_THIS_MAIN_PUSH
bash tray/install.sh
```

- **Choosing the GUI.** Fresh installs and upgrades use Zerus Next, including when Qt is installed. `HGS_MAC_GUI=client` selects it explicitly; `HGS_MAC_GUI=qt` selects the temporary Qt fallback until Mac acceptance. A later install without that override selects Next again. `mac-install.sh` ships the client sources and forwards the same choice, but its tarball has no Git metadata and cannot make a numbered build. The maintained updater keeps Qt as the rollback copy after the switch.
- **The build.** It needs npm and Rust 1.99.1 on the Mac. With the `media` feature present, the build enables it. It always builds into `client/src-tauri/target`, whatever `CARGO_TARGET_DIR` says, since the installer takes the bundle from there, and the installer's job count bounds Cargo as it bounds the Qt build (`CARGO_BUILD_JOBS` overrides it).
- **The updater at the switch.** `tray/update-installed.py`:
  1. quits Qt as its Quit action does, so Qt saves its drafts: SIGTERM, which current builds handle, or the quit Apple Event for an older one;
  2. snapshots Qt's drafts and the client's drafts root;
  3. exports Qt's settings with `defaults export com.hgdev.hgs-tray` to `~/.local/share/hgs/desktop-previous/com.hgdev.hgs-tray.plist`;
  4. sets Qt aside, removed from LaunchServices, and installs the client; the rollback folder does not change yet;
  5. bootstraps the same LaunchAgent, waits on Qt's socket path and sends `sessions`;
  6. once the client runs, makes Qt the one rollback copy, `~/.local/share/hgs/desktop-previous/hgs-tray.app`;
  7. verifies the drafts, the native processes and Qt's settings keys.

  A failure or interrupt before step 6 puts Qt back, or keeps it in `~/Applications/.hgs-tray-kept-*/previous/hgs-tray.app` and prints where. Later updates list such folders and never remove them.
- **Later client updates.** Qt stays the rollback copy until the owner retires Qt; there is no second slot. The replaced client is kept while that update runs, so a new client that does not start is replaced by it automatically. After a successful update, the replaced client is removed only if Qt's rollback copy can be verified as complete. If that check fails or times out, both copies stay: the client in a `.hgs-tray-kept-*` folder beside the installed bundle, Qt in the rollback folder. The updater prints the kept client's path; later updates list it and never remove it.
- **Dev numbers.** The updater refuses to replace a numbered Dev install with a build that has neither a Dev number nor a local preview label, also for `HGS_TRAY_RESTART=0` (`docs/reference.md`). `HGS_TRAY_RESTART=0` never switches between Qt and the client.

### Rollback

```sh
python3 tray/update-installed.py --rollback ~/Applications/hgs-tray.app
```

The rollback reinstalls exactly the rollback copy through the same quit, snapshot, start and verification steps. After the switch, that copy is the last Qt bundle. It compares Qt's settings with the copy taken at the switch before Qt starts. If they differ, quit Zerus, run `defaults import com.hgdev.hgs-tray ~/.local/share/hgs/desktop-previous/com.hgdev.hgs-tray.plist`, then `launchctl kickstart gui/$(id -u)/com.hgdev.hgs-tray`, since KeepAlive Crashed does not restart a clean quit. The client then becomes the rollback copy. Drafts typed in the client after the switch stay in `~/Library/Application Support/com.hgdev.hgs-tray/drafts/`, where Qt does not see them. Use `HGS_MAC_GUI=qt` on later installs to stay on Qt.

### Privacy identity

TCC grants and the Notification Center entry belong to the bundle identifier and its designated requirement. The installed Qt is signed with a Developer ID Application certificate, without the hardened runtime. A switch bundle signed by `tray/sign-local-macos.py` with a Developer ID Application certificate of the same team satisfies Qt's requirement, so the grants carry over. An ad hoc or Apple Development signature would not, and the grants would be asked for again. Before the switch, compare the two requirements (the runbook below); `tray/sign-local-macos.py` already refuses an incompatible signature.

### Release signing

`scripts/check-macos-bundle.py --gui tauri` checks the switch bundle; `--selftest` runs only `--version` and `--build-info`. `scripts/sign-macos.py` signs it inside-out with the hardened runtime and `packaging/macos/Zerus.entitlements`. No JIT entitlement is needed, because WKWebView runs JavaScript out of process. Notarization runs only in the `macos-signing` environment. A public Tauri `Zerus.app` with the CLI, connector, setup scripts and notices, and its release archive, is phase 7 (`zerus-5wmh.12`).

### Switch runbook

The coordinator runs this on the owner's Mac after Tasks 1 to 9 are merged to main and their main push has a CI sequence `N`. Steps marked **display** run only in a window the owner gives: the implementer tells the coordinator when the work reaches step 4, and step 8 is the coordinator's own, done with the owner. Nothing here touches the hgs CLI, `com.hgdev.hgs-recovery`, `com.hgdev.hgs-voice`, native agents, tmux or the mobile connector. Signing needs the login keychain, so signing steps run as a `.command` in Terminal, like other Dev rollouts.

**Before the merge (Task 7).** The numbered Dev guard also refuses unnumbered `tray/install.sh` and `mac-install.sh` runs over a numbered Dev install on arch and on the Mac, including `HGS_TRAY_RESTART=0`. Tell the events and menu-parity sessions before Task 7 reaches main: their ad hoc installs then need `ZERUS_DEV_BUILD_NUMBER` or a local preview label (`ZERUS_LOCAL_PREVIEW_LABEL`, `docs/reference.md`), both from a clean Git checkout. `mac-install.sh` ships a tarball without `.git`, so it can make neither; on a Mac with a numbered Dev install it then updates the CLI and stops at the GUI with the refusal.

**What the switch carries over (coordinator rulings).** Settings are seeded once from Qt's, read-only, except the client's own `client/…` settings (defaults), the microphone (chosen again) and automatic approvals (off; Qt's choice is only recorded). Read marks start fresh; the hosts' read state stays authoritative. Qt's drafts and the dev app's drafts reach the client through its one-time import (`zerus-5wmh.13.1`) at its first start, which reads both stores read-only; step 7 checks its marker. In-app update notices are missing after the switch, accepted until `zerus-5wmh.12.4`; the coordinator rolls Dev out by hand meanwhile.

1. **Prepare the clean checkout** (no display). In the clean clone used for Dev rollouts, record its path and the commit: later steps cd to `$CLONE` and compare against `$SHA` (in another Terminal window, export all three again; the `${CLONE:?}` below stops a step whose `CLONE` is unset):
   ```sh
   cd PATH_OF_THE_CLEAN_CLONE
   git fetch origin && git switch --detach SHA_OF_THAT_MAIN_PUSH
   export ZERUS_DEV_BUILD_NUMBER=N CLONE="$PWD" SHA="$(git rev-parse HEAD)"
   ```
2. **Compare the privacy identity** (no display, Terminal, in the clean clone of step 1). The installed app is read only through `codesign`, and the switch bundle is checked and signed in a scratch folder. The `$(…)` that adds the `media` feature splits into words in zsh and bash alike, where a variable would not in zsh, and `CARGO_TARGET_DIR` set to the clone's own target, as `tray/install.sh` sets it, wins over any `build.target-dir` in the Cargo configuration, so the bundle lands where the next lines take it from:
   ```sh
   cd "${CLONE:?}/client" && npm ci --no-audit --no-fund
   python3 scripts/switch-config.py src-tauri/target/switch.conf.json
   CARGO_TARGET_DIR="$PWD/src-tauri/target" RUSTUP_TOOLCHAIN=1.99.1 npx tauri build --config src-tauri/target/switch.conf.json --bundles app \
     $(grep -q '^media = ' src-tauri/Cargo.toml && echo --features media)
   S="$(mktemp -d)"; ditto src-tauri/target/release/bundle/macos/Zerus.app "$S/hgs-tray.app"
   python3 ../scripts/check-macos-bundle.py --gui tauri --version "$(cat ../VERSION)" \
     --release "$(cat ../DEV_VERSION)-dev.$ZERUS_DEV_BUILD_NUMBER" --source-commit "$SHA" --selftest "$S/hgs-tray.app"
   python3 ../tray/sign-local-macos.py "$S/hgs-tray.app" ~/Applications/hgs-tray.app
   codesign -d -r- ~/Applications/hgs-tray.app 2>&1 | grep designated
   codesign -d -r- "$S/hgs-tray.app" 2>&1 | grep designated
   rm -rf "$S"; cd "${CLONE:?}"
   ```
   `sign-local-macos.py` must print "privacy identity matches the installed application". Report both `designated =>` lines to the owner privately; they contain the team. Equal requirements carry the TCC and Notification Center grants over. Different ones need the owner's decision, because the grants would be asked for again.
3. **Check the socket path** (no display, read-only):
   ```sh
   realpath "$(lsof -n -a -c hgs-tray -U -F n | grep '/hgs-tray$' | sed 's/^n//')"
   echo "$(realpath "$(getconf DARWIN_USER_TEMP_DIR)")/hgs-tray"
   ```
   The two paths must be equal: the client binds `hgs-tray` in the canonical per-user temporary folder (`TMPDIR`, else `_CS_DARWIN_USER_TEMP_DIR`, as Qt's `QDir::tempPath`). Both sides go through `realpath`, so `/var` and `/private/var` spellings of one folder compare equal; the updater reaches the socket through either. If they still differ, stop and report: the updater would time out on the new client and roll it back.
4. **Try the dev app** (**display**, in an owner-given window; the implementer tells the coordinator when the work reaches this step). Build the dev bundle under the heavy-build lock (**mac-lock**): in `~/zerus-dev/webui`, `git fetch origin && git checkout --detach origin/main`, then `CARGO_TARGET_DIR="$PWD/src-tauri/target" npx tauri build --bundles app` in `client/`, so the bundle lands in its own `src-tauri/target` whatever the Cargo configuration says; remove `~/zerus-dev/.heavy.lock` once the build is done. Start its executable from Terminal, in `~/zerus-dev/webui/client`:
   ```sh
   ZERUS_CLIENT_SINGLE_INSTANCE=1 ZERUS_CLIENT_NOTIFICATIONS=1 ZERUS_CLIENT_WATCHDOG=1 \
     "src-tauri/target/release/bundle/macos/Zerus Next (dev).app/Contents/MacOS/zerus-client"
   ```
   Then check:
   - **The tile:** counts, amber attention and the stale dot, on a light and a dark menu bar; the tooltip lines.
   - **The menu:** computers; sessions with the marker, "(paused)" and "(stopped)"; "Open in" with Terminal.app and Clipboard; Quit.
   - **The window:** a primary click opens and raises it. Closing hides it and the Dock icon goes. A Dock click restores a minimised window.
   - **`--sessions` while minimised:** minimise the window, then start the executable again with `--sessions`. The window comes back in front and active, as Qt's `activateWindow` brings it (`TrayAgent.cpp:781-783`).
   - **Closing in native fullscreen:** put the window in fullscreen with its green button, then close it. It hides, and no empty Space is left behind in Mission Control.
   - **Session entries:** a running session opens in Terminal.app. Clipboard copies its command and shows "Zerus: copied to the clipboard".
   - **Second start:** the same executable started again with `--sessions` raises the first window and exits with "already running".
   - **Notifications:**
     - allow the one-time prompt for the dev app;
     - a session waiting for approval posts "Zerus — Needs attention";
     - answering withdraws it;
     - clicking a banner opens that session (a click while Zerus is not running is step 8's: started by a click, the dev app runs without the variables above);
     - nothing posts while the window is focused.
   - **Defaults:** `defaults read dev.zerus.next` lists only Cocoa and WebKit keys (`NS…`, `Apple…`, `WebKit…`). Report the list; it shows what Cocoa or WebKit writes into a client's domain.
   - **Watchdog:** Terminal shows `main-thread watchdog on (30 s)`. Open the menu-bar menu and leave it open for 60 s, then close it: the app keeps running and no `main thread unresponsive` line appears (the heartbeat runs in the common run-loop modes).
   - **Ignored tile test:** if `the_tile_draws_amber_attention_and_the_stale_dot` was ignored over SSH, run it here with `--ignored`.
   - **Menu-bar checks from the Task 4 review:**
     - **A click after a rebuild.** Hold the menu open while a session appears above the target entry, then click the target: exactly that session opens, never a neighbour or Quit.
     - **The Dock badge after a closed window.** Let attention arrive while the window is closed. Opening the window shows the Dock badge with the count, and the badge survives closing and reopening the window.
     - **A fleet that cannot start.** Start with `ZERUS_CLIENT_HGS=/nonexistent/hgs`: a right-click still shows Open Zerus and Quit, and Quit works.
     - **`&` in a name.** A session or computer name containing `&` shows a single `&` in the menu.
     - **Other menus.** After using the app menu, a webview context menu and the Open in submenu, tile updates resume once each has closed.
     - **A theme switch with the menu open.** Switch light and dark while the status item's menu is open: the tile redraws in the new colour once the menu closes.
   Quit the dev app from its menu.

   **Scroll measurement (zerus-5wmh.36), in the same window.** Two instances must never share the client's data folder, so start this only after the dev app has quit.

   Conditions:
   - AC power;
   - Low Power Mode off;
   - a nominal thermal state;
   - the window fully visible and left untouched.

   Each run ends by itself after about 5 minutes:
   ```sh
   B="src-tauri/target/release/bundle/macos/Zerus Next (dev).app/Contents/MacOS/zerus-client"
   mkdir -p ~/zerus-dev/perf-36
   ZERUS_CLIENT_PERF=auto ZERUS_CLIENT_PERF_LABEL=mac-switch-live \
     ZERUS_CLIENT_PERF_OUT=$HOME/zerus-dev/perf-36/live.json "$B"
   ZERUS_CLIENT_PERF=auto ZERUS_CLIENT_PERF_LABEL=mac-switch-replay \
     ZERUS_CLIENT_REPLAY="$PWD/fixtures/replay.ndjson" \
     ZERUS_CLIENT_PERF_OUT=$HOME/zerus-dev/perf-36/replay.json "$B"
   ```
   Over SSH, the same runs use `open -n -W --env …` as in "Activity pass in replay mode" in `client/bench/README.md`.
   - **The live run** measures `list-scroll` on the real catalog.
   - **The replay run** measures `activity-scroll` on the fixture's long session. It always scrolls at least 2000 px; see "Activity pass in replay mode" in `client/bench/README.md`.
   - Each report records whether its Activity was live or replay.
   - Put both reports on one page with `client/scripts/measure-report.py`.

   If `list-scroll` or `activity-scroll` fails the harness criteria, zerus-5wmh.36 joins the switch bar. Otherwise it stays after the switch (coordinator, 2026-10-28).
5. **Agree the moment with the owner.** Qt flushes its drafts when it quits, but the owner should not be typing.
6. **Switch** (Terminal). From the root of the clean clone, not the dev app's clone of step 4, with `ZERUS_DEV_BUILD_NUMBER`, `CLONE` and `SHA` still set, run only the GUI installer, not the CLI `install.sh`:
   ```sh
   cd "${CLONE:?}" && HGS_MAC_GUI=client bash tray/install.sh 2>&1 | tee ~/zerus-switch.log
   ```
   Expected lines:
   - quitting Zerus;
   - kept a copy of the Qt settings;
   - `Zerus <DEV_VERSION>-dev.N runs as pid …; … nonempty drafts kept, … native processes untouched; the replaced GUI is the rollback copy in …`;
   - possibly `Cocoa settings written since the switch: …`.
   Any `hgs-tray update failed` line stops the runbook. The updater has already restored or kept the previous GUI and printed where it and the draft snapshot are.
7. **Verify** (no display):
   ```sh
   ~/Applications/hgs-tray.app/Contents/MacOS/hgs-tray --build-info | python3 -c 'import json, os, sys; i = json.load(sys.stdin); print(i["devBuildNumber"], i["sourceCommit"] == os.environ["SHA"], i["sourceDirty"])'
   cat ~/.local/share/hgs/build-provenance.json
   launchctl print gui/$(id -u)/com.hgdev.hgs-tray | grep -E 'state =|program ='
   ls ~/.local/share/hgs/desktop-previous/
   find ~/Applications -maxdepth 1 -name '.hgs-tray-kept-*'
   lsof -n -a -c hgs-tray -U -F n | grep '/hgs-tray$'
   python3 -c 'import json, pathlib; m = json.loads((pathlib.Path.home() / "Library/Application Support/com.hgdev.hgs-tray/drafts/.imported-v1.json").read_text()); print(m["version"], m["report"].get("unfinished"))'
   python3 -c 'import json, pathlib; s = json.loads((pathlib.Path.home() / "Library/Application Support/com.hgdev.hgs-tray/settings.json").read_text()); print((s["seed"]["source"], sorted(s["seed"]["keys"])) if "seed" in s else "no seed record")'
   ```
   Expected:
   - `--build-info` prints `N True False`: Dev number N, the commit of step 1 (`$SHA`), a clean tree;
   - the agent is running `~/Applications/hgs-tray.app/Contents/MacOS/hgs-tray`;
   - the rollback folder holds `hgs-tray.app` (Qt) and `com.hgdev.hgs-tray.plist`, and no `.hgs-tray-kept-*` folder exists;
   - the socket is at the path from step 3;
   - the draft import marker, `<data dir>/drafts/.imported-v1.json` with the data directory `~/Library/Application Support/com.hgdev.hgs-tray`, prints `1 None`: version 1, with no `unfinished`. The marker is written only once the import finished, so a missing one (the command fails) means it did not: it runs again at the next start; report its `drafts import: …` line from `~/Library/Logs/hgs-tray.log`;
   - the settings seed record names its source and the keys it carried over: those Qt has stored (for example `workspace/theme` once the owner chose a theme), never `speech/microphone`, and Qt's automatic approvals only as `automaticApprovals/qtEnabledAtSwitch`. Report the list; step 8 checks one of these keys in Settings. An empty list means Qt had stored none of them, and step 8 then checks only automatic approvals and the microphone. `no seed record` means the seed failed and runs again at the next start: report its `settings seed:` line from `~/Library/Logs/hgs-tray.log`.
8. **Confirm** (**display**; the coordinator's job, done with the owner):
   - one menu-bar item;
   - notifications post without a new permission prompt (when step 2 matched);
   - a click on a banner Qt delivered before the switch opens its session;
   - a click while Zerus is not running: quit Zerus from its menu (launchd leaves a clean exit alone), then click one of its banners in Notification Center. Zerus starts and opens that session. If it starts but the session stays closed, the click that launched it was lost: the notification delegate is set during setup, and the fix is to set it in `applicationWillFinishLaunching` (Task 6); file a bead and tell webui. The instance the click started runs outside launchd, so quit it and run `launchctl kickstart gui/$(id -u)/com.hgdev.hgs-tray` to bring back the supervised one;
   - the owner's sessions, and the drafts in Qt's store, are where they were; Qt drafts and the dev app's drafts appear in the client (the import of step 7);
   - the settings seed: a key from step 7's list shows Qt's value in Settings (for example the theme in Settings > Appearance), automatic approvals are off, and Voice input asks for the microphone again;
   - after the Mac's next sleep and wake, `launchctl print gui/$(id -u)/com.hgdev.hgs-tray | grep 'pid ='` shows the same pid as before, and `~/Library/Logs/hgs-tray.log` has no `main thread unresponsive` line.
9. **Later Dev rollouts.** Run the same `bash tray/install.sh` with the new `N` and without `HGS_MAC_GUI`. The installed client is kept, and Qt stays the rollback copy: there is no second slot. A new client that does not start is replaced by the client it replaced, automatically. The numbered Dev guard refuses a run without `ZERUS_DEV_BUILD_NUMBER` or a local preview label.
10. **Rollback.** From the clean checkout, in Terminal:
    ```sh
    cd "${CLONE:?}" && python3 tray/update-installed.py --rollback ~/Applications/hgs-tray.app
    ```
    Qt comes back with its drafts, and the client moves into the rollback folder, so a second `--rollback` returns to the client. If the updater reports changed Qt settings, quit Zerus, run `defaults import com.hgdev.hgs-tray ~/.local/share/hgs/desktop-previous/com.hgdev.hgs-tray.plist`, then `launchctl kickstart gui/$(id -u)/com.hgdev.hgs-tray` (KeepAlive Crashed does not restart a clean quit). Drafts typed in the client after the switch stay in `~/Library/Application Support/com.hgdev.hgs-tray/drafts/`, where Qt does not see them. Use `HGS_MAC_GUI=qt` on later installs to stay on Qt, and file a bead for the cause.

## Mac App Store

Mac App Store publication is deferred by the owner as of 2026-10-15, tracked in
`zerus-8reb`. The active scope is Homebrew and a downloadable signed macOS DMG
in GitHub Releases. The Homebrew Developer ID signature and notarization do not qualify
an app for the store. Apple requires App Sandbox, inherited sandbox constraints
for embedded command-line helpers, and store-managed application updates. See
[App Review Guidelines 2.4.5](https://developer.apple.com/app-store/review/guidelines/#hardware-compatibility)
and [sandboxed command-line helpers](https://developer.apple.com/documentation/xcode/embedding-a-helper-tool-in-a-sandboxed-app).

The current entitlements do not enable App Sandbox. Local provider executables,
tmux, per-user services and shared state need an explicit sandbox-compatible
design and isolated qualification before a store build can be submitted.
Do not silently remove local functionality or change the Homebrew bundle to
obtain a store build.

Apple Distribution and Mac Installer Distribution certificates were issued on
2026-10-15. Encrypted PKCS#12 exports were tested in a disposable keychain using
a synthetic app and installer package; the code and package signatures verified.
This establishes credential readiness only. Store-specific provisioning,
product scope, metadata, an upload and App Review remain outstanding.

## Validation record

On 2026-10-10, local checks on an Apple Silicon Mac covered the setup command
(also under `/usr/bin/python3` 3.9), bundled `hgs` discovery, Homebrew ownership
and restart detection, candidate schema 2, the tap publication contracts and the
mirror. All 46 Qt suites passed against the pinned official Qt.
`scripts/ci/macos-bundle.sh` produced a 136 MB unsigned zip that passed the
bundle checks and self-tests. Ad-hoc signing with the hardened runtime verified
signing order, nested entitlements and `codesign --verify --deep --strict`.
Pinned actionlint 1.7.12 reported no findings.

On 2026-10-12, after rebasing onto main with the Dev channel and adding
`zerus@nightly`: all macOS, publication, mirror, CI-security and privacy Python
contracts passed; all 54 Qt suites passed against Homebrew Qt and, inside
`macos-bundle.sh`, against the pinned official Qt. A nightly bundle built with
`ZERUS_RELEASE_VERSION=0.42.0.r999.gabcdef0.n77` passed the bundle checks and
self-tests and carried that `ZerusReleaseVersion`, product version 0.42.0, macOS
14.0 and `launchd_agent.py`. Pinned actionlint 1.7.12 reported no findings.

On 2026-10-15, the G2 certificate and intermediate were imported from the encrypted
PKCS#12 into a disposable keychain. A native probe passed hardened-runtime signing
with Apple's timestamp, strict signature verification and execution. Apple's
notary service accepted that signed probe using the team API key. The temporary
keychain was removed and the original keychain search list retained.
The deployed mirror already supported macOS schema-2 releases. Casks were enabled
and [Nightly run 38767851278](https://github.com/ufna/zerus/actions/runs/38767851278)
was started for the first full bundle qualification.
That run passed full CI and built the bundle, but its final self-test could not
find tmux in the restricted PATH. The workflow now installs that dependency,
and the checker exposes it through a private bin directory with an isolated
tmux socket directory. Fixture and real installed GUI/CLI checks verify this
environment without modifying the installed application or live sessions.

DMG qualification on 2026-10-15 used a synthetic native application and the
production G2 credentials in a disposable keychain. Apple accepted separate app
and DMG submissions; both tickets were stapled and validated. Gatekeeper accepted
the signed image and the app mounted read-only inside it, and that app executed
successfully. The drag-to-Applications link and nested framework links survived
image creation. Publication contracts cover both channels, immutable DMG
checksums, old ZIP-only releases, and exclusion from website mirror storage.

Production qualification later on 2026-10-15 used the exact unsigned artifact
from [Nightly 38776672013](https://github.com/ufna/zerus/actions/runs/38776672013),
source `8f7f68f67c6eadc0eb9bf6c4946ffa0e614a6bff`. Full CI and bundle tests
passed, but that run stopped before signing because the reusable workflow's
callers did not forward the environment secret names. Both callers now forward
the explicit five-name allowlist; credential values remain in `macos-signing`.
The production signing script independently qualified the saved Qt bundle in a
temporary local keychain: Apple accepted the app and DMG, both tickets validated,
and Gatekeeper accepted both. The read-only mounted app passed the CLI and GUI
self-tests under library validation and reported the exact clean source commit.
This local qualification was not published. Both rendered cask templates passed
`brew style` in disposable local taps.

The first published DMG comes from
[Nightly 38794972859](https://github.com/ufna/zerus/actions/runs/38794972859),
version `0.42.2.r1291.g5d2e5d0.n17`, from clean source
`5d2e5d0955ab34f1e81ebd237a9b7eed2761eb7f`. Its complete CI matrix and all
67 production Qt test suites passed. Apple accepted separate app and DMG
submissions; the signing job stapled and verified both. An independent download
matched the Actions artifact digest, and its read-only mounted app passed
signature, ticket, Gatekeeper, CLI/GUI self-tests and exact source metadata checks.
A transient CloudKit connection loss during CI ticket validation was resolved by
retrying installation verification on the same signed artifact, without a rebuild.
The clean macOS runner passed Homebrew style, installation, installed bundle and
Gatekeeper checks, CLI/GUI self-tests, isolated setup/removal and uninstallation.
[Publisher 38802915468](https://github.com/ufna/zerus/actions/runs/38802915468)
published the immutable nightly, synchronized AUR and committed the nightly cask
to `ufna/homebrew-zerus`. Anonymous downloads of the public DMG and ZIP match
their Actions artifact bytes, `SHA256SUMS` and GitHub digests; the public cask
matches the reviewed template, version and ZIP digest. `brew fetch --cask
ufna/zerus/zerus@nightly` also downloaded and verified the public package without
installing over the developer's app. The desktop/nightly mirror synchronized
successfully and serves the same release and source commit; macOS downloads
remain on GitHub.

The first public macOS Dev build, `0.42.3-dev.693`, was qualified on 2026-10-16
from clean source `f16d73bcd0730157b43a3ca2b8476f0319bf6193`.
[CI 38813489733](https://github.com/ufna/zerus/actions/runs/38813489733)
passed the full platform matrix, all 68 production Qt suites, Apple signing and
notarization, and clean Homebrew installation and removal. Both the read-only
mounted DMG and the installed cask passed bundle identity and CLI/GUI self-tests.
[Publisher 38820507139](https://github.com/ufna/zerus/actions/runs/38820507139)
published the sealed candidate through the bounded Dev mirror and verified its
public ZIP/DMG bytes before adding `zerus@dev` to the tap. No GitHub Release was
created for this Dev build.

Independent downloads on macOS matched the published sizes and SHA-256 values;
the public cask matched the reviewed template and ZIP digest. Gatekeeper and
stapler accepted the downloaded DMG, and its read-only mounted application passed
the self-tests with the exact clean source and Dev identity. `brew fetch --cask
ufna/zerus/zerus@dev` verified the public package without installing it. The
compiled desktop update controller also parsed the live Dev catalog and exposed
its correct ZIP and DMG links. The shared developer GUI was left unchanged;
its coordinated installation remained a separate rollout.

The first signed macOS Stable release, [0.43.0](https://github.com/ufna/zerus/releases/tag/v0.43.0),
was published on 2026-10-16 with hgs 1.52.0 from clean source
`48dff262f46be71bbd9cd114577239ee85f55573`.
[Candidate 38908051629](https://github.com/ufna/zerus/actions/runs/38908051629)
passed the complete platform matrix, all 75 production Qt suites, the pinned
real-agent matrix without skips, both Arch package builds, Apple signing and
notarization, and clean Homebrew installation. Retries kept the same source;
the CLI lifecycle and transcript-polling test races remain tracked separately
in `zerus-d84a`. The final source archive matched `git archive` exactly. Gitleaks
8.30.1 passed the complete Git history, tracked working tree and candidate source.

[Publisher 38920602130](https://github.com/ufna/zerus/actions/runs/38920602130)
published the candidate unchanged as the latest Stable release and synchronized
the AUR packages and `zerus` cask. Independent anonymous downloads matched the
candidate, GitHub asset digests and checksums. The public DMG and its read-only
mounted application passed signature, ticket, Gatekeeper, exact source identity
and isolated CLI/GUI self-tests. `brew fetch --cask ufna/zerus/zerus` verified the
public cask without installing it. The Stable mirror synchronized successfully;
the compiled desktop update controller accepted its live catalog and exposed the
correct ZIP and DMG links. Stable, Nightly and Dev are now published Homebrew
channels. Shared developer GUI replacement remains a separate coordinated action.

Still requiring interactive or subsequent release qualification:

- Terminal automation in the signed application.
- Installation from the public tap on a clean account, and an upgrade from one
  cask version to the next with running GUI, tmux session and native agent.
- The channel switch between stable and nightly from the public tap.

### Public cask upgrade qualification

The manual **macOS public cask upgrade qualification** workflow tests already
published bytes on a disposable standard `macos-15` runner. It has no signing,
publication, deployment or cache credentials. Supply three full commits from
`ufna/homebrew-zerus`: an older Stable recipe, a newer Stable recipe and the
signed Nightly recipe. The defaults retain the public 0.43.0 and 0.44.0 Stable
recipes; after publishing 0.45.0, repeat with the exact 0.44.0 and 0.45.0 commits.
The Nightly commit must also be supplied explicitly.

The existing ephemeral `contents: read` job token is used only for exact official
GitHub REST tap-commit and release-tag metadata GETs, avoiding the shared anonymous
rate limit. The requester rejects redirects and every other host, route or method.
The script removes the token from its environment before starting any subprocess;
raw casks and release payloads remain anonymous, and evidence never includes it.

Before Homebrew evaluates a cask, the script checks its complete bytes against
the current or retained pre-connector template, public release metadata and
seals, bounded ZIP paths and the native Developer ID signature, Gatekeeper and
stapled ticket. It installs from the real public tap, performs `brew upgrade`
between the Stable versions and switches Stable to Nightly and back using
ordinary uninstall/install, without `--zap` or `zerus-setup --remove`.

An official SHA-512-pinned Claude executable runs against the existing
localhost-only keyless fixture in a private tmux server. Its exact process start,
pane, run and conversation must survive every package operation, together with
synthetic saved preferences and an immutable attachment draft. The old private
GUI must remain running during package operations; the fixture then explicitly
restarts only that GUI and checks retained state. The receipt distinguishes this
from interactive editor input. Native-format preferences belong to the disposable
runner account; setting `HOME` alone is not claimed to isolate CFPreferences.

This job must never run on an owner's account. It does not grant or reset TCC
permissions and does not qualify Terminal automation. That remaining case needs
an actual signed application's explicit **Open in Terminal** action against an
owned synthetic session in a user-authorized interactive test: accept any normal
Apple Events consent explicitly, verify the new owned Terminal window attaches
the exact private tmux session, then close only that window. An `osascript`
response over SSH or a fake Terminal backend is not that evidence.
