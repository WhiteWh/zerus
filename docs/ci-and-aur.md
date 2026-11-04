# CI and Arch Linux publication

This document describes the checks, package layout and publication workflow.
[Zerus 0.37.0](https://github.com/ufna/zerus/releases/tag/v0.37.0) is the first
public Arch x86_64 bundle. Stable release builds and GitHub/AUR publication remain
separate manual workflows; verified nightlies publish automatically. Stable
releases after 0.42.2 and nightlies also carry a Windows x86_64 portable zip of
the control console.

## Continuous integration

Hosted checks run automatically on pushes to `main` and pull requests targeting
`main`, including Dependabot proposals. External forks still require maintainer
approval. Dev main checks shared Rust/protocol contracts, Linux CLI, Arch,
macOS and Android. Windows and iOS run in the full nightly/stable matrix and on
PRs affecting their code: desktop changes select Windows, iOS changes select
iOS, and native/shared/build/workflow or unknown paths select both. An unknown
PR inventory also selects both; a partial or unavailable diff never removes
coverage. Android-only and documentation-only changes need no Windows/iOS jobs.

Manual CI defaults to all platforms, with independent `run_ios` and `run_macos`
flags. `ci.yml` selects the cache trust boundary and `checks.yml` respects the
explicit coverage. The resolver records the exact test commit and selected
suites. PR checks test the merge commit and use the base/head merge ancestry for
conservative change selection, including both sides of renames.

| Job | Automatic CI | Checks |
| --- | --- | --- |
| Resolve commit | Always | Exact test commit and suite selection for every other job |
| Quick checks | Always | Privacy, Gitleaks 8.30.1 on the tree and every fetched branch and tag, Python/Bash syntax, version consistency, actionlint, stable/nightly publication contracts, locked Rust 1.99 unit tests, the replica crate (format, clippy, tests, golden vectors), Rust license notices |
| Rust relay | Always | Rust 1.99 format, clippy, and every storage contract (cross-worker ones included) on its own PostgreSQL database; every `services/mobile/tests` module, with the Rust relay fixtures on per-test PostgreSQL databases and no skips; the speech suites against the Python reference relay; the deployment image and its 256 MiB envelope |
| Rust mobile connector | Always | Rust 1.99 format, clippy, vector, binary and journal handoff tests; the black-box suites against the Python and Rust connectors concurrently with differential traces compared against the reviewed allowlist, and a 300 s soak of the Rust build (`services/mobile/tests/BLACKBOX.md`) |
| Rust mobile connector on macOS | Always | Cold Rust 1.99 build, unit, vector and binary tests, and the black-box modules that need no real `hgs`, journal handoffs with the Python connector included |
| iOS core tests and simulator build | Affected PR; full nightly/stable | The project task replica built with Rust 1.99.1 for the iPhone, the simulator and macOS (`mobile/ios/scripts/build-replica.sh`, without the release profile's LTO in this check), then `swift test` for the ZerusKit core and an unsigned Release build of the app for the iOS simulator, with the newest installed Xcode 26 or later and no signing identity |
| Android unit tests and lint | Always | `testDebugUnitTest` and `lintRelease` of the Firebase variant that Dev ships, with a synthetic `google-services.json` and no credentials |
| Linux CLI | Always | Stable Rust unit tests, terminal smoke tests, Python integration modules, release build |
| Native desktop client | Linux with the CLI; macOS/Windows with their desktop jobs | Linux `client/media` audio/video tests, including synthetic GStreamer recordings, and `client/src-tauri` tests under headless Xvfb plus clippy with video; native macOS `client/core` tests, `client/media` video checks and `client/src-tauri` clippy (its test suite runs on the local Mac gate until `zerus-5wmh.78` is fixed); native Windows `client/core` tests, `client/media` video checks and `client/src-tauri` clippy (its test suite stays pending `zerus-5wmh.52`), all on standard runners with Rust 1.99.1 |
| Arch desktop | Always | Qt build and all CTest suites |
| Arch package | Always | Production package with the recipe's `check()`, namcap, staged and installed bundle checks; the only producer of the `arch-x86_64-<commit>` candidate artifact |
| macOS CLI and desktop | Always | Native CLI integration and all Qt CTest suites on two standard macOS jobs |
| Windows client and desktop | Affected PR; full nightly/stable | Rust tests, Rust 1.99 clippy for the Windows target, Windows client and session host integration tests, MSVC Qt build with all offscreen CTest suites, and the portable zip checked from an isolated extraction |
| Windows real agents | Affected PR; full nightly/stable | One latest `win32-x64` Claude Code and Codex release in isolated session hosts of the release build, each against a local fake API; skipped or empty suites fail |
| CI gate | Always | Requires every selected suite to succeed; fails on unexpected skips, cancellation and errors |

PR #430's first native-client run at `b3716dd8` (run `40898847326`) measured an
11-minute-19-second green Linux check run, a 65-minute macOS check-run span that
ended in cancellation at the 60-minute job timeout, and a 1-minute-51-second
Windows run that stopped at the first core-test failure. To bound cold builds
with registry-only caches, macOS and Windows disable dev/test debug info and use
`client/src-tauri` clippy with all targets instead of a redundant cargo check
with all targets. The separate media/video check remains because the app's
default features do not include media.

Native macOS checks are split into two parallel standard-runner jobs with
90-minute timeouts: `client-native-macos-core` runs core tests and the
media/video check, while `client-native-macos-tauri` runs clippy with a
25-minute step timeout. Hosted src-tauri tests are disabled until
`zerus-5wmh.78` is fixed; the local Mac gate continues to run them. Every cargo
command has its own named workflow step and calls
`scripts/ci/client-native.sh macos` with its stage. Only the Tauri job saves the
shared registry cache, so the two jobs cannot race to save one key. Linux keeps
45 minutes and Windows 60.

Quick checks and the full suites start together once the commit is resolved, so
a quick-check failure no longer holds back the other suites. New PR/manual runs cancel
obsolete runs for the same branch/PR and workflow. Trusted public Dev main pushes
use separate bounded qualification and signing/sealing queues; each keeps its active
phase and coalesces pending revisions to the newest one. PR/manual checks and main
before Dev rollout keep their existing whole-run concurrency. PR jobs have read-only
permissions and no publication credentials; fork PRs use `pull_request`, never
`pull_request_target`. Publishing is confined to trusted upstream `main` and the
branch-restricted `release` environment. Authenticated model-turn tests remain
explicit opt-ins.

The Android job runs in every CI run, including PR checks before merge, and needs
no credential. It builds with `-PzerusFirebase=true`, like the signed Dev APK,
from a `google-services.json` written in the job with placeholder values (project
number zero, package `app.zerus.mobile`). The google-services and Crashlytics
plugins only turn that file into resources; tests and lint never contact
Firebase, and the mapping upload follows R8, which these tasks do not run.

The macOS CLI unit/debug job finishes before two independent consumers start.
One continues the original smoke, Python integration and release build; the other
runs the unchanged latest-agent contracts from that exact debug CLI. A three-file
handoff contains only `hgs`, `fake_agent` and a manifest bound to the selected
source, run, producer attempt, actual OS/architecture, producing Rust toolchain
and source-helper/binary hashes. Consumers use the completed producer's exact
artifact ID and manifest digest, including its original attempt on a partial
rerun; there is no live-producer polling or name/latest fallback. The CLI consumer
selects the producer's exact Rust release and verifies the compiler identity.
The public `CLI (macos-15)` result and `Suite results` require every selected
component. The producer saves tested unit/debug dependencies under a `debug`
content key; the integration/release consumer uses a distinct `complete` key.
Both keep the legacy registry, Git and target paths and inherit the caller's
cache authority: only requested trusted-main saves run after successful work and
an exact-key miss. A cold core first restores the completed debug cache; a cold
producer first tries the complete cache, then both accept legacy lock/family
entries. The profile suffix stays inside the final key segment, so housekeeping
still retains only two total entries in that existing compiler/month family.
The native consumer has no cache access. Linux retains the default complete CLI path.
Separate runner setup, dependency restoration and artifact transfer add overhead;
this split does not establish an observed speedup without hosted evidence.

On upstream `main` pushes with the [Dev channel](public-distribution.md#verified-main-dev-channel)
enabled, `ci-dev-qualification.yml` builds the Arch package and parallel unsigned
Mac bundle. Dev uses explicit build-only modes: no Rust package `check()` or full
Qt regression run is on this publication path. Package layout, linked libraries,
source/version identity, installation checks and bundle self-tests still apply.
The build gate requires the Arch package and selected Mac bundle to succeed; a
disabled Mac bundle must be skipped. Its independent build lock does not wait for
an older full regression run.

After the public **CI gate** admits the successful build workflow,
`ci-dev-tail.yml` holds one separate noncancelling CI slot through Android/Mac
signing, installation checks, candidate sealing and the monotonic Dev tag. The next
revision can build while this tail runs. Signing remains in the existing protected
workflows; source/version checks, Developer ID, notarization and disposable cask
installation remain required. Build and delivery retain the same CI run ID,
number, SHA and artifact namespace. Only trusted main builds write caches; PR checks read
them and signing/publication have no cache access.

The existing **Publish Dev** workflow is the third, independently serialized lane.
It starts after the build/sign/seal CI run succeeds and validates that run's
immutable candidate. The separate **CI regression** workflow starts on the same
completion event, runs the full main test matrix against that exact commit and
reports its own failures. Regression failures remain visible and become follow-up
work; they do not block or retract a usable Dev build. It has read-only cache
access and no signing or publication credentials. PR/manual, Stable and Nightly
retain their existing test matrices. Internal same-repository PRs need no extra
manual trust step for Dev integration. Active signing and publication are never cancelled by a
new main push. All queues retain GitHub's default single pending entry, so newer
pending work can replace older pending work; every revision is not promised a
published build. Existing monotonic candidate/catalog checks reject stale delivery.

The short signed `dev-android.yml` build starts after the CI gate alongside Mac
signing/installation. It builds `assembleRelease`, seals the APK/source and removes
private inputs on every outcome, then uploads `zerus-dev-android-a<attempt>`.
No runner polls the gate and no Actions token reaches this signing job. Its
credential-free Android unit/lint checks run in the independent regression lane;
the signed build uses the same Firebase variant. Publication and the `dev` tag still wait
for all selected candidates. With Mac enabled, the Android build overlaps its
post-gate delivery work; with Mac disabled it adds its own short build duration.

Nightly retains all Windows/iOS checks and uploads iOS Dev to TestFlight only
after full verification. The upload retains the exact source's successful main
CI number, not Nightly's smaller sequence. A bounded read-only App Store Connect
preflight skips a known valid/processing existing build and refuses failed,
expired or unknown records/API results before archiving. Stable iOS uploads run
full verification too. These uploads remain separate from the desktop catalog.

### Incoming PRs and publication trust

Running a PR compiles and executes code supplied by its author, including Rust
build scripts, tests and packaging commands. GitHub-hosted runners provide an
ephemeral environment; passing CI does not establish that the proposed code is
safe to merge.

Repository settings require approval for **all external contributors** and full
commit SHA pins for actions. The workflow token defaults to read-only and cannot
approve PRs. Review the complete diff before approving a fork workflow, especially
workflow changes, dependency/build scripts and attempts to change cache permissions
or runner labels. A previous harmless contribution does not grant automatic runs.
Keep development machines off the runner list; secrets, SSH credentials, account
configuration and authenticated model sessions do not belong in CI caches/artifacts.

PR checks and every manual CI run receive **read-only cache tokens**, including a
manual run of `refs/pull/NUMBER/head` from the upstream workflow. Only upstream
`main` pushes and scheduled checks of that exact `main` commit can populate caches.
Scheduled upstream-main Nightly checks also refresh these families, including
Windows now absent from Dev. Manual Nightly uses a separate read-only call; its
verification gate requires exactly one full matrix to pass.
This restriction is enforced by GitHub's fixed `cache-mode` on the reusable call,
so checked-out build code cannot bypass it by invoking the cache API itself.
Publishers use `cache-mode: none` and execute trusted upstream publication scripts.
They accept only successful main-only stable/Nightly candidates, check workflow
identity, repository, commit, run identity, archive safety and digests, and never
execute downloaded AUR recipes. Ordinary PR CI artifacts cannot become releases.
The `release` environment permits the `main` **branch** only, with no tag allowance.

The current actionlint 1.7.12 release predates `cache-mode`. Source checks validate
its fixed enum and placement; actionlint suppresses only its unknown-key warning
for that field. Trust-boundary tests exercise the actual routing conditions and
failure gate. GitHub validates the cache policy and reusable-workflow limits. Remove
the narrow exception when a released actionlint supports the key.

These boundaries limit credential theft, repository writes and compute abuse.
Maintainer approval still matters: a PR can change its workflow, and granting a
write-capable cache mode to untrusted code would bypass the secure cache default.
See [fork workflow approvals](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/approve-runs-from-forks),
[Actions security](https://docs.github.com/en/actions/reference/security/secure-use)
and [cache access limits](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching).

### Build caches

`ci.yml` passes `save_caches: true` only to the trusted main checks, the one
caller with a write-capable cache token. `checks.yml` uses explicit
`actions/cache/restore` and `actions/cache/save` steps, and every save requires
that input: PR, manual, nightly and release runs restore caches and never try to
save them.

| Family | Job | Key after the prefix | Saved when | Bound |
| --- | --- | --- | --- | --- |
| `msrv-v2-` | Quick checks | OS/arch, hash of `rustc -Vv`, month, `Cargo.lock` hash | Exact key missed; Rust checks passed | Shared Cargo target, incremental state and debug symbols disabled; monthly restart |
| `cli-`, `windows-cli-` | CLI, Windows | OS/arch, hash of `rustc -Vv`, month, `Cargo.lock` hash | Exact key missed; Rust build passed | Monthly restart |
| `connector-` | Rust mobile connector | OS/arch, hash of `rustc +1.99.1 -Vv`, month, `services/mobile/connector/Cargo.lock` hash | Exact key missed; connector tests passed | Monthly restart; about 400 MB before compression, without incremental state |
| `client-native-` | Native desktop client | OS/arch, hash of `rustc +1.99.1 -Vv`, month, hashes of the three client lockfiles | Exact key missed; native checks passed | Monthly restart; Cargo registry and Git checkouts only, so multi-GB native target directories are never cached |
| `arch-cargo-v1-` | Arch package | Month, hash of `Cargo.lock` and the connector's lock | Exact key missed; package built | Cargo registry only |
| `android-gradle-v1-` | Android | Month, hash of the Gradle wrapper properties and `*.gradle.kts` | Exact key missed; tests and lint passed | Monthly restart; about 700 MB (`~/.gradle/caches` and `wrapper`) |
| `arch-ccache-v1-`, `arch-package-ccache-v1-`, `mac-desktop-ccache-v1-` | Arch desktop, Arch package, macOS | Architecture (macOS), run ID | Build passed with ccache misses | `CCACHE_MAXSIZE` 150, 100 and 100 MB |

Quick and relay checks reclaim only unused Android, .NET, Haskell and CodeQL installations on disposable hosted Ubuntu runners. Disk checkpoints record free bytes and build-directory sizes. Quick shares one Cargo target across its crates; the new cache family avoids restoring the former duplicated debug and incremental trees. Relay removes its completed host Cargo targets before Docker compiles the deployment image; the remaining memory-envelope checks run that image. PostgreSQL containers and volumes, Cargo registries and the Python test environment stay available. Runner classes and the repository cache limit are unchanged.

The Windows real-agent job restores the `windows-cli-` family of the Windows
desktop job for its release build and never saves it, so two jobs cannot race to
save one key.

When `Cargo.lock` changes, a Rust cache restores the newest target directory of
the same compiler and month. A new compiler or month starts empty, so stale
dependency builds cannot accumulate. The Gradle cache does the same when the
wrapper or a build script changes. The signed Dev build has no cache access and
downloads its dependencies afresh. Compiler caches use one key per saving run
and restore the newest entry by prefix, so each main build that compiles
something refreshes them. Statistics are reset before and printed after each
build. Before saving, `ccache --evict-older-than` drops every entry the build did
not use (ccache updates an entry's time on each hit). Every new commit
recompiles the update controller, which embeds the source commit, so main pushes
normally save new ccache entries; GitHub evicts the least recently used entries
above the 10 GiB limit. Desktop builds use Ninja and every runner vCPU (`ZERUS_BUILD_JOBS`
4 on Arch and Windows, 3 on macOS), and the AUR recipes let Ninja choose the job
count.

The owner decided on 2026-10-13 that objects compiled by trusted main checks may
feed published builds: the Arch package job produces the Dev, nightly and stable
Arch candidates, and the Windows job the portable zip of stable and nightly
releases. Cache directories stay outside the checkout
(`/home/user/.cache/ccache`, `~/Library/Caches/ccache` and `RUNNER_TEMP`), so
they never enter packages, the zip or uploaded artifacts.
The Arch package job enables ccache for makepkg only through
`/etc/makepkg.conf.d/ccache.conf` in its disposable container, after checking that
the image's default `BUILDENV` differs only in `ccache`. The published recipes and
their `check()` are unchanged. The package build uses different compiler flags
from the desktop build, so it has its own cache family.

### Manual checks

```sh
# Full Linux, Arch, macOS and Windows coverage:
gh workflow run ci.yml --ref main
# Full coverage of a PR head with the current upstream workflow:
gh workflow run ci.yml --ref main -f ref=refs/pull/123/head
# Source/MSRV checks plus the aggregate gate only:
gh workflow run ci.yml --ref main -f run_linux=false -f run_arch=false -f run_macos=false -f run_windows=false -f run_ios=false
```

An explicit test `ref` changes the checked-out commit, not GitHub's workflow run
metadata. Use the exact tested SHA recorded in the job summary when reviewing
such a manual run. Automatic PR and push runs produce CI gate without a dispatch.
Review and merging remain deliberate; successful CI does not auto-merge PRs.

Arch is the Linux GUI build environment because the application uses Qt 6 and KDE
Frameworks 6. Ubuntu remains useful for portable CLI and minimum-Rust checks.
Linux CLI CI builds the pinned official tmux 3.7c archive with an exact SHA-256;
Ubuntu 24.04's tmux 3.4 lacks `bracket_paste_flag`, introduced in tmux 3.7. That
format is required by the existing verified agent input transport. Unknown input
mode must keep rejecting delivery; upgrading the test toolchain preserves that
guard. The package requires tmux 3.7+. A binary upgrade must not restart live
servers; see the [installation requirements](installation.md#requirements).
Release packaging covers **Arch x86_64** and, through the `zerus`,
`zerus@nightly` and `zerus@dev` Homebrew casks, **Apple Silicon macOS**; see
[macOS distribution](macos-distribution.md). Ordinary macOS CI checks build/test
compatibility with Homebrew Qt; stable, nightly and successful main Dev candidates build, sign and
notarize the bundle, and only once the repository variable `MACOS_CASKS_ENABLED`
is `true`. Stable releases and nightlies add the Windows x86_64 portable zip
described below (owner decision of 2026-10-14; 0.42.2 is the last stable release
without it); Dev builds have none. Intel Macs and Arch ARM are outside this
package matrix.

All CI jobs have bounded timeouts. Checkout credentials are not persisted and
Actions are pinned to commit SHAs. Dependabot proposes monthly grouped action/Cargo
updates. Rust, C++ and Gradle caches reduce repeated build time; their repository
limit stays at 10 GiB. Ordinary test artifacts expire after seven days and stable
candidates after fourteen. Scheduled logs, Arch packages, Windows zips and nightly
candidates expire after three days.
Standard Ubuntu/macOS/Windows runners and public artifacts are free while the repository
is public. Larger runners and explicitly increased cache storage have separate
billing rules. No self-hosted runner is attached to a development machine.
[Actions billing](https://docs.github.com/en/billing/concepts/product-billing/github-actions).

After the trusted `main` checks, the `cache-trim` job keeps the two newest `main`
cache entries of each key family (the key without its last segment, which is a
run ID or a content hash) and deletes pull-request entries older than a day. It
logs each deleted key and size and the totals. The job has only `actions: write`
and `cache-mode: none`, checks out nothing and runs one inline `gh api` script.
An entry deleted concurrently is not an error, and a failed trim never fails the
run or holds back Dev. Its first `main` run confirms whether `cache-mode: none`
permits REST cache deletion; refused deletions appear in its log.

The Arch base image is digest-pinned, but `pacman -Syu` intentionally uses current
Arch packages. This tests rolling-distribution compatibility; it does not promise
bit-for-bit reproducibility months later. Rust dependencies are locked, fetched
in `prepare()` and built/tested with `--frozen` inside source packages. The CI
image, tmux and Windows Qt pins need periodic
maintainer review.

### Dependency updates and cost

Dependabot checks version updates on the first day of each month. Each ecosystem
has one version-update group and at most one open version-update PR: two ordinary
PRs across GitHub Actions and Cargo. Automatic rebasing is disabled; refresh a
stale proposal before testing and merging it. Updates still require review and
checks appropriate to the active CI policy; there is no automatic merge.

Cargo proposals update only `Cargo.lock` within the reviewed `Cargo.toml`
constraints. This also avoids unreviewed breaking minor upgrades of `0.x` crates.
Manifest migrations are deliberate maintenance work, including raising the pinned
Rust to the newest stable release and existing state/configuration compatibility
checks. Actions keep their exact
commit pins and may propose major upgrades in their group for manual review.

Dependabot vulnerability alerts and security updates are enabled separately in
repository settings. Security proposals have their own per-ecosystem groups;
they are not delayed by the monthly version schedule or counted against its PR
limit. A fix requiring a manifest migration still needs maintainer attention.
The historical DeepSeek fixture lockfile has no version-update entry. Ordinary
CI resolves a fresh exact source lock for the official latest release and retains
it with that run's evidence; it never silently falls back to the historical lock.
Do not disable security alerts to reduce routine version-update noise.

Dependabot jobs on standard GitHub-hosted runners do not consume included Actions
minutes. Standard runner minutes are also free while this repository is public;
larger runners and storage have separate billing rules. There is no monthly
standard-runner minute quota to exhaust for a public repository. Schedule useful
coverage rather than artificial runs. Keep artifact retention bounded and use
standard runner labels; private-repository checks consume the account allowance.
See [Dependabot runner billing](https://docs.github.com/en/code-security/concepts/supply-chain-security/dependabot-on-actions),
[Actions billing](https://docs.github.com/en/billing/concepts/product-billing/github-actions)
and [Dependabot options](https://docs.github.com/en/code-security/reference/supply-chain-security/dependabot-options-reference).

### Test isolation and local commands

```sh
python3 scripts/ci/check-source.py
bash scripts/ci/cli.sh
bash scripts/ci/gui.sh
cargo +1.99.1 test --locked
# Arch only, as an ordinary user with the documented build dependencies:
bash scripts/ci/arch.sh
```

The CLI wrapper clears inherited `HGS_*` account/session selection before setting
its own `HGS_TEST_BIN`. Python integration modules run in separate processes so
module patches do not leak between suites. Terminal fixtures use private socket
paths and synthetic homes. No check runs against existing agent sessions.
Authenticated native-client tests remain explicit opt-ins; baseline CI needs no
model API keys. Linux resolves one official latest `@deepseek-ai/dsh` release for
its keyless local-model contract fixture. `scripts/ci/dsh_fixture.py` verifies the
official package identity and SHA-512 integrity, then records the complete exact
registry-only dependency lock before `npm ci`. Private npm configuration disables
Git, file, directory and remote dependency sources during both resolution and
installation. Metadata, source lock, installation identity and logs remain in the
test evidence. The original ten-minute end covers selection through publication
of the install receipt, reserving fifteen seconds for owned npm-group cleanup.
Missing or changed receipts and a version mismatch fail mandatory native admission.
The fixture is not a package-script sandbox. A new release can expose an adapter
incompatibility and fail CI; selecting it does not itself change product support.
Native agent CLIs are not included in the product package.

The CI CLI jobs set `ZERUS_PYTHON_JOBS` to twice the runner vCPUs (8 on Ubuntu,
6 on macOS): the Python test classes mostly wait on terminals and subprocesses.
The desktop jobs keep per-function QtTest JUnit results in `qt-functions/` of
their test-results artifacts. The sessions window's tests run as area suites
(`test_sessionswindow_<area>`, a few minutes each; U62, #712), so no suite is
sharded today. A suite registered with `tray/tests/RunQtShard.cmake` would be
balanced by `tray/tests/qt-shard-durations.txt`, the largest duration of each
function, or data row of its slowest functions, across the measured runs; after
tests are added or become slower, download the Arch, macOS and Windows desktop
artifacts of one main run and rebuild that file with
`scripts/ci/qt-shard-seed.py`; its usage lists the commands. A test the file
does not list still runs exactly once.

`tests/test_claude_input_native.py` checks exact message acceptance by the real
Claude TUI with a private home/socket and a localhost fake API; it never reads a
user's account. Full Linux and macOS CI run it with the other Claude native
suites against one latest Claude Code package (see below); `HGS_CLAUDE_TEST_BIN`
selects another executable for a manual run. The coverage verifies
literal trailing backslashes, multiline messages and durable request replay.
Terminal submission alone is insufficient: the test waits for the unchanged user
message in native history. `tests/test_claude_settings_native.py` drives the
real `/model` picker with a synthetic model catalog and history. It changes
model and effort in a fresh session, accepts Claude's own cache
notice in a resumed conversation, and requires the session-only transcript
receipt while `settings.json` stays unchanged. A session-only change made in
Terminal must reach hgs through `PostModelSwitch` with the exact model ID.
`tests/test_claude_statusline_native.py` starts the real TUI through a tracked
`hgs claude` launch, with the private home, state folder and socket and the
loopback API of `tests/model_api_fixture.py`. Claude 2.1.296 and 2.1.308 coverage requires the
user's own status line on screen and its `settings.json` unchanged, and the window
from Claude's status line input in `hgs inspect` for the exact run, conversation and
model: before the first reply, after a reply, after a session-only model change, a
compaction and a clear, and in a resumed run. In a linked worktree whose repository's
local settings set a status line, Claude keeps that status line and no window is reported.
`tests/test_task_context_native.py` takes the exact bytes `hgs __state hook` prints
for a start that a fake agent's tracked launch recorded, returns them from the real
agent's SessionStart hook, and requires the task instructions in the request the
real Claude Code (`claude -p`) or Codex (`codex exec`) sends to its loopback model;
it runs with the Claude and the Codex suites.
`tests/test_claude_permission_native.py` leaves Claude's one-time auto-mode offer
enabled for a fixture account whose default mode is manual. Claude 2.1.296 and
2.1.308 coverage requires an answerable Activity card under the launch prompt's reply
that holds messages back, then answers it once: keeping the mode records Claude's own
`hasSeenAutoDefaultNudge` with `defaultMode` unchanged, and accepting sets it to `auto`.

### Unix tmux census

The Windows Phase 2 refactor moves every tmux call site behind a terminal backend
and must leave Unix behaviour byte-for-byte unchanged. `scripts/ci/tmux-census.py`
(Python 3, standard library only) records the tmux and `ps` calls that hgs makes
while the shell and Python suites run, and compares two recordings. It is a local
check, not a CI job. Record each tree with its own fresh debug build, on the same
machine, tmux version and `ZERUS_PYTHON_JOBS`, without `HGS_*`, `TMUX` or
`TMUX_PANE` in the environment:

```sh
census() {  # census OUT HGS: record both suites with that binary into OUT/
  HGS_TEST_BIN="$2" python3 scripts/ci/tmux-census.py record --out "$1/shell" -- \
    bash tests/test_hgs.sh </dev/null
  HGS_TEST_BIN="$2" ZERUS_PYTHON_JOBS=8 python3 scripts/ci/tmux-census.py record \
    --out "$1/python" -- python3 scripts/ci/python-tests.py </dev/null
  python3 scripts/ci/tmux-census.py summarize "$1/shell" "$1/python" --out "$1.json"
}
census "$scratch/main" "$main_build/debug/hgs"      # in a checkout of main
census "$scratch/branch" "$branch_build/debug/hgs"  # in the refactor checkout
python3 scripts/ci/tmux-census.py compare "$scratch/main.json" "$scratch/branch.json" \
  --rules scripts/ci/tmux-census-rules.json
```

Record both trees from the same kind of process: on Linux inside a user service
manager unit, a `new-session` that may start the server is preceded by
`list-sessions`, and without a server it reaches the shim through
`systemd-run --scope` (see
[Terminal, SSH and clipboard](reference.md#terminal-ssh-and-clipboard)).

`record` puts private `tmux` and `ps` shims first on `PATH`. `ZERUS_TMUX_CENSUS`
names the record directory, and `ZERUS_TMUX_CENSUS_TMUX` and `ZERUS_TMUX_CENSUS_PS`
name the real programs, resolved before the shims were added. Each call writes one
JSON record: argv, working directory, whether `TMUX` and `TMUX_TMPDIR` are set
(and whether that directory exists), `LC_ALL`, `LC_CTYPE`, `LANG` and `TZ`, what
stdin, stdout and stderr are connected to (terminal, pipe, `/dev/null`, file), the
nearest hgs ancestor process, the stdin bytes of `load-buffer ... -`, the exit
status and stderr. The shim then behaves like the real program, with the same
argv, environment, stdin, stdout, stderr and exit status; on Linux it undoes the
`LC_CTYPE` that Python's C-locale coercion would otherwise pass on.
`attach`, a `new-session` without `-d`, control mode and any call with a terminal
stdin exec tmux directly instead. No other call has its stdin read. A test's own
tmux wrapper reaches the shim through the path that the test resolved at import
(`tests/test_input.py:73`), so each call is recorded once, with the wrapper's
`-S` and `-f` kept apart from the command. Fakes that never run tmux, such as the
paste stubs in `tests/test_hgs.sh`, stay outside the census; those tests check
their own call logs.

`summarize` groups the calls into one operation per hgs process, in call order,
keyed by the test class and the normalized hgs argv. Temporary and home paths,
UUIDs, digests, PIDs, timestamps and generated names become placeholders numbered
within the operation, so equal values stay equal. tmux `%`/`$`/`@` IDs stay as
they are: every test owns a fresh server, so they repeat between recordings, and a
placeholder would hide a call that targets another pane. Hooks and supervisors
started inside a test's tmux server belong to the class that uses that socket.
Calls without an hgs ancestor, the fixtures' own tmux and `ps` calls, are listed
apart and compared only with `--include-direct`. `summarize` warns about tmux
calls that reach the recording user's default server (no `-S`, `-L`, `TMUX` or
existing `TMUX_TMPDIR`): a test that makes one is not isolated and must give hgs
a private socket or an existing `TMUX_TMPDIR`.

`compare` exits 1 on any difference, and 2 when the two files come from different
tool versions. It reports operations whose call sequences differ, what differs
(order, argv, exit status, environment, stdio, stdin, stderr), and call shapes
present in only one recording, including the calls of operations that an
`optional` rule drops. `scripts/ci/tmux-census-rules.json`
lists the nondeterminism measured on an unchanged commit, with the source of
each rule: back-to-back repeats of read-only observations in timed poll loops
(`answer`, `clear-context`, `compact-context`, `effort`, `settings`, `interrupt`,
`pause`), the error of the last `pause` poll when it meets the exiting server,
hooks and supervisors racing a test's `kill-server`, and tests that poll `hgs ls`
or emit hooks a timing-dependent number of times. Rule kinds are `replace` (a
pattern in a call's line), `optional` (drop an operation with a matching call),
`collapse`, `unordered` and `any_count` (ignore how often a distinct sequence
occurs); each applies to the operations whose key and test class match. Commands
that change tmux state keep their exact order and count. Add a rule only after two
recordings of one commit differ, never to absorb a difference that the refactor
introduced. Both test runs must pass: a failing test changes the census. When the
rules are in doubt, record main twice and compare the two recordings first.

### Terminal backend contract

`tests/test_backend_contract.py` states once what every terminal backend must do:
tmux on Linux and macOS, and the Windows session host. It is a black box at
the CLI: tests drive only `hgs` commands and observe only their output and JSON
and the records of the native fake agent (`examples/fake_agent.rs`, linked or
copied as `claude`), never the backend or the internal Rust `TerminalBackend`
trait. It covers listing without starting the backend; launches (folder,
arguments, launch ID, one session per name, untracked commands, a launch whose
session ended without a conversation); `a --existing [--run-id]` resolution and
attach from a terminal client (input, client size, several clients,
`Ctrl+B Ctrl+B`, detach, the session ending); the exact bytes of
`send` and Terminal input (one bracketed paste, then one Enter, no replay, keys in
both cursor modes); state read from the screen (styled capture, wide glyphs,
cursor, size, bracketed paste, an echoed draft against a dim placeholder) and
raw-mode evidence; rename; pause and resume; terminate and kill, with what each
does to the agent process. Signal expectations accept the Unix signal or the
Windows console event each operation sends: on Windows a pause is Ctrl+Break for
the supervisor's process group, terminate pauses first and then closes the
console, and kill closes it at once (spec D11).

`HGS_TEST_BACKEND` selects the backend: `tmux` (the default on Linux and macOS) or
`host` (the default on Windows, skipped elsewhere). Each test has a private home,
state and configuration directory. hgs puts the home's `.local/bin` first on its
`PATH` (ahead of Homebrew and `/usr/local/bin`; on Windows ahead of
`%APPDATA%\npm`), so the agent stand-ins live there. Only the backend classes in
the module differ: setup and teardown, `running()`, `describe()` (the backend's
state in failure messages), `alive()` (whether an agent process with a recorded
start time still runs) and the attach client.

- `tmux`: a private server through a `tmux` wrapper in that folder, a private
  `TMUX_TMPDIR` that keeps any other tmux off the user's server, the fake agent
  linked as `claude` and `fake_agent`, `ps` start times and a pseudo-terminal
  client.
- `host`: the test folder is also `USERPROFILE`, and `hgs session-host install`
  enables local sessions there, as `tests/test_windows_lifecycle.py` does: the
  build under test launches as a portable copy, the staged host image serves and
  the staged stable CLI supervises. The isolated `HGS_STATE_DIR` keeps the host's
  pipe away from any live host. The fake agent is copied as `claude.exe` and
  `fake_agent.exe` and answers hgs's version probe as Claude Code 2.1.296, whose
  hooks hgs writes in their exec form into a private `CLAUDE_CONFIG_DIR` (spec
  D12); `APPDATA` is private too. `running()` reads `hgs __host status`,
  `alive()` compares creation times through a process handle (spec D9), and attach
  runs `hgs a` on a pseudoconsole of the test (`PseudoConsole` of the lifecycle
  suite, spec D13). Teardown kills the sessions and stops the host through `hgs
  __host call`, then waits for the host and every agent and supervisor through
  handles opened while they ran, and ends and reports any that outlive it. Waits
  allow 30 s and commands 60 s, as in the lifecycle suite: a freshly copied
  executable may be scanned when it first starts.

The fake agent of a test has one configuration, set before its first launch,
because a backend may keep the environment of the launch that started it. The
module runs in the Linux and macOS CLI jobs in about 7 s, skips without tmux, and
passed 20 runs with 8 in parallel. The `windows-desktop` job runs it against the
host in its session host step
([below](#windows-client-desktop-and-portable-zip)).

### Windows client, desktop and portable zip

The `windows-desktop` job runs on a standard `windows-2025` runner. It disables
Git's CRLF conversion before checkout, so fixtures and scripts keep LF, and every
Python step uses one exact CPython 3.13 from the SHA-pinned `actions/setup-python`.
It runs `cargo test` with stable Rust and `cargo +1.99.1 clippy --all-targets -- -D
warnings`: clippy is pinned like the relay lints, so a new stable release cannot
fail an unchanged commit, and Windows-only code, compiled nowhere else, is checked
against Rust 1.99. It builds the release `hgs.exe` and the `fake_ssh` and
`fake_agent` examples and runs `tests/test_windows_client.py` and the session host
suites (`tests/test_windows_host.py`, `tests/test_windows_host_conpty.py`,
`tests/test_windows_host_survival.py`, `tests/test_windows_lifecycle.py`,
`tests/test_windows_hooks.py`, [testing the host](windows-host-testing.md)) and the
[terminal backend contract](#terminal-backend-contract) against them; real agents
run in a job of their own ([below](#windows-real-agents-through-the-session-host)).
Before the host suites, `scripts/fetch-conpty.py` fetches the
[ConPTY pair](#the-bundled-conpty-pair) once into `RUNNER_TEMP/conpty`; the host step names that folder in
`HGS_TEST_CONPTY_DIR`, and the portable zip takes the same files. Nothing caches
or uploads the folder. The runner puts every step in a job that forbids breakaway, where
hgs refuses to start a host (spec D4), so `scripts/ci/outside-job.py` starts the
host suites outside it through WMI. That copy restores the step's environment
without credentials and runs the suites in a kill-on-close job of its own that
allows breakaway, as an sshd session does. The step follows the log and exit
status, and after the suites or the 15-minute timeout it ends whatever they left,
including hosts of the tested `hgs.exe`. This is a CI harness only: the product
never escapes a job through WMI. The Rust cache uses its own
`windows-cli-` key with line-table debug information to bound its size. The runner
gives each step a synchronous anonymous pipe as stdin, and Windows OpenSSH never
exits after using one (the fake client refuses it), so steps that start `hgs.exe`,
`ssh` or the desktop app read stdin from `/dev/null`.

The Windows desktop is Zerus Next, built from the locked frontend and Tauri
Rust package with `tauri.windows.conf.json` and the `video` feature. The output
is `target/windows-next/release/hgs-tray.exe`, with the installed identity
`com.hgdev.hgs-tray`. This job does not install Qt, run `windeployqt` or cache a
Qt/C++ desktop build. Native Windows shell contracts also run in the dedicated
client-native job.

WebView2 Evergreen is a shared prerequisite. The app installer checks the
Microsoft-documented user and machine registration and stops before changing
Zerus files when it is missing. Winget declares `Microsoft.EdgeWebView2Runtime`
as a dependency; portable users install it separately. Only the disposable
Windows CI runner uses `scripts/ci/ensure-webview2.py`: it first checks for an
existing runtime, otherwise downloads the official HTTPS bootstrapper within a
4 MiB/time bound, verifies a valid Microsoft Authenticode signature, runs it
with a fixed timeout and checks the registration again. It refuses owner and
non-Windows environments and never removes the shared runtime. See Microsoft's
[WebView2 distribution guidance](https://learn.microsoft.com/microsoft-edge/webview2/concepts/distribution).

`scripts/package-windows.py` copies the exact tested `hgs.exe`, Tauri
`hgs-tray.exe` and `zerus-mobile-connector.exe` into
`zerus-windows-x86_64.zip`, preserving the top-level
`zerus-windows-x86_64/hgs-tray.exe` path winget uses. It adds the app-local MSVC
runtime from Visual Studio's default redistributable directory and the pinned
ConPTY pair next to `hgs.exe`. No Qt runtime or plugins are deployed.

The zip carries the project, Codex and ConPTY notices, the host-target Rust
notices for the CLI/connector and Tauri desktop (including its selected video
feature), and the exact installed production frontend packages' license texts.
Frontend collection uses the shared `client/scripts/collect-notices.py` and npm
lockfile, excludes development tools except Vite's emitted preload runtime, and
refuses missing notices or mismatched installed versions. `build-info.json`
records `desktop:tauri`, `webview2_runtime:evergreen`, versions and exact source.
The portable inspector still accepts immutable published Qt archives with their
original layout and notices, so retries never rebuild or alter older releases.

`scripts/check-windows-portable.py` checks safe regular-file names, the pinned
ConPTY hashes, notices, manifest identity and the existing 512 MiB compressed /
500 MB unpacked bounds. `--run --selftest` on Windows uses private temporary
configuration and a minimal environment to check CLI/connector startup and the
exact clean desktop `--build-info` identity. It stages the bundled ConPTY pair
through `hgs.exe session-host install` without starting a host. New Tauri builds
do not use Qt's old `--selftest` option.

CI builds the per-user Inno Setup installer from the tested zip without changing
payload bytes. Installer survival tests install, upgrade and uninstall in their
isolated fixture while preserving staged hosts and sessions. Login start is an
unchecked installer choice and invokes the desktop's guarded login-item command;
uninstall asks that command to remove only its own registration. Obsolete Qt
files are removed only by the existing owned-app-folder upgrade sweep.
Windows service registration/removal is reserved for the maintained G1 command
from the Windows service lane. Explicit installer hooks currently log that it is
pending; they create no ad-hoc scheduled task or claim active background duties.
`hgs desktop registration --json` previews the shared definition/readiness interface.

The job uploads the zip, installer and logs in `windows-x86_64-<commit>` for seven
days (three for schedules). Stable/nightly assembly continues to use the tested
zip unchanged; publishing the installer is separate work (.66.7.4). Windows Dev
builds remain absent.

#### The bundled ConPTY pair

The session host prefers Microsoft's ConPTY from the NuGet package
`Microsoft.Windows.Console.ConPTY` to the inbox one (spec D6 of
[Windows Phase 2](superpowers/specs/2026-10-13-windows-native-host-design.md)): it
loads `conpty.dll` by absolute path from its image folder, and only when
`OpenConsole.exe` sits there too. `conpty.dll` runs the `OpenConsole.exe` next to
it, else one in an `x64` subfolder (where the package's own build targets put it),
else silently System32's `conhost.exe`; the zip checks therefore allow no copy
of either file outside the top folder.
`packaging/windows/conpty.json` pins version 1.25.260930003: the package by its
nuget.org URL, size and SHA-256
(`02b07b349af66d801159bdf9e440d4a1ce78bb951f37fc8609731665afdae7ee`), and the x64
`runtimes/win-x64/native/conpty.dll`
(`feeef341d891643c62d30b6b07800bc70f0bc148f44cb8c3bee84aa557ae805a`) and
`build/native/runtimes/x64/OpenConsole.exe`
(`3d66b23d0a71bb8eed2b77edc8b9df9bf54ce6c8fb74c863a30e760997f80586`) inside it.
The package attached to the microsoft/terminal release `v1.25.2733.0` differs only
in its signature file and holds the same two files byte for byte.
`scripts/fetch-conpty.py --output DIR [--nupkg FILE]` downloads the package (or
reads a copy), checks it before opening it, and writes only those two files once
they match; nothing in the package runs. The package declares MIT and carries no
license file: `docs/licenses/conpty-LICENSE.txt` is the `LICENSE` of
[microsoft/terminal](https://github.com/microsoft/terminal) (tag `v1.25.2733.0`,
unchanged since its first release), shipped as `licenses/conpty-LICENSE.txt`.
`OpenConsole.exe` also contains third-party code, such as Chromium's numerics
library (BSD-3-Clause), so `docs/licenses/conpty-NOTICE.md`, that repository's
`NOTICE.md` from the same tag, ships as `licenses/conpty-NOTICE.md`. Both are
stored with LF line endings; the text is otherwise unchanged.

To move to another version, read its `.nuspec` and build targets and the
exports `src/host/conpty.rs` resolves, record the new package, paths, sizes and
digests in the pin, refresh both notice files from the release tag that ships
that package, change the version named in `src/host/conpty.rs` (a contract
test compares them) and run the host suites, including
`tests/test_windows_host_conpty.py`, on Windows.

From Linux or macOS, `scripts/ci/check-windows-target.sh` type-checks all targets
for `x86_64-pc-windows-msvc` without the MSVC toolchain (`clippy` instead of the
default `check` runs `cargo +1.99.1 clippy -D warnings`). It needs `rustup target add
x86_64-pc-windows-msvc` for the toolchains it uses. Build scripts that compile C get a
stand-in compiler that writes empty objects, since nothing is linked; only the
Windows job builds and tests the real binary.

On a Windows development PC the same steps run from Git Bash;
[CONTRIBUTING.md](../CONTRIBUTING.md#windows) lists the requirements and commands.

### Windows real agents through the session host

The `windows-host` job runs one latest `win32-x64` Claude Code and Codex release in
isolated session hosts of the release `hgs.exe` on a standard `windows-2025` runner
(M7 of [Windows Phase 2](superpowers/specs/2026-10-13-windows-native-host-design.md)).
It is selected with the Windows desktop job and the gate requires both, but it is a
job of its own: a failure points at the host, and the desktop job keeps its time
margin. Like that job it checks out text files unchanged and uses the same exact
CPython; it builds the release binary from the `windows-cli-` Rust cache, which it
restores and never saves, and fetches the [ConPTY pair](#the-bundled-conpty-pair)
the same way.

It then runs `scripts/ci/agent-compatibility.py` with `HGS_TEST_BIN` and
`HGS_TEST_CONPTY_DIR` ([native agent compatibility](#native-agent-compatibility-and-upstream-review)):
the runner verifies each package against its selected SHA-512 integrity, extracts only `claude.exe` or
`codex.exe` and runs that agent's class of `tests/test_windows_native_agents.py`,
whose agents reach only a local fake API (Anthropic's for Claude Code, OpenAI's
for Codex) and keep their homes private. The suites confirm what spike S6 ran by
hand with these versions (a tracked launch to a ready composer, hook events in
listings, records and the journal, key input, pause, resume and terminate,
non-ASCII prompts) and assert what had run only with the fake agent: exact
`hgs send` acceptance with a trailing backslash and multiline messages, question
and approval answers, and clear. Compaction and the model and effort pickers are not
reviewed on Windows and stay out. Codex runs without its Windows sandbox: the
package's sandbox setup program and command runner, its code-mode host, `rg` and
voice libraries are never extracted.

The suites start session hosts, so `scripts/ci/outside-job.py` runs the whole
runner outside the step's job, as the desktop job does for the host suites, with a
45-minute bound inside the 50-minute step. Afterwards it ends what is left by
executable path: hosts of the tested `hgs.exe`, anything run from the suites'
`hgs-host-*` folders (copies and staged images) and the extracted agents in the
runner's `zerus-native-compat-*` folder. The step runs Python in UTF-8 mode, so a
non-ASCII prompt in a log cannot fail on the runner's code page. The harness log,
`report.json`, `summary.md` and each suite's log are kept as `windows-host-results`
like the CLI job's logs: for seven days, or three after a failed scheduled run.

### Real-peer smoke on a Windows PC

Hosted CI runs `hgs.exe` only against the fake SSH client. Changes to SSH handling,
the embedded terminal or swarm transport also need a manual check against a real
Linux peer from a Windows development PC. The names below (`winpeer`, user `peer`,
`/workspace/sample-project`) are placeholders; keep real host names, addresses and
accounts in private configuration.

1. **Peer.** Use a disposable Linux container or virtual machine with its own user,
   home, tmux server and hgs state, never a machine that runs live agents. Install
   the tested tree's `hgs` at `~/.local/bin/hgs`, add a project and start a
   detached shell session (`hgs sh sample-project -d`). Make its SSH server
   reachable from the PC, for example through a loopback-only tunnel.
2. **Isolation on the PC.** Use the executables from the extracted zip and point
   `HOME`, `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`, `TEMP`, `TMP`, `HGS_CONFIG_DIR`
   and `HGS_STATE_DIR` at a scratch folder. Verify the resolved CLI state, desktop
   `tray.conf`, Qt settings, drafts, single-instance lock and pipe identities before
   launching the desktop. In the Windows 0.45.0 qualification, a per-process
   `USERPROFILE` override placed Qt settings and the lock below the scratch profile
   while the corresponding real files stayed unchanged. That observation does not
   establish full-account isolation or how every Windows/Qt build resolves known
   folders; verify the actual paths instead of assuming environment overrides are
   always honored or ignored. Use a separate Windows test account if those desktop
   identities cannot be verified private; an unisolated second tray may only
   activate the running one. Before changing
   the variables, back up `hgdev\hgs-tray.ini` and the `hgs-tray.ini.drafts`
   folder next to it in the real Roaming AppData folder, and `hgdev\hgs-tray` in
   the real Local AppData folder, when they exist
   (`[Environment]::GetFolderPath('ApplicationData')` and
   `'LocalApplicationData'` name them). `ssh.exe` ignores `HOME` and
   `USERPROFILE` and keeps using the real `%USERPROFILE%\.ssh\known_hosts`. Back up
   that whole file before the first connection and restore it byte for byte
   afterwards: OpenSSH's `UpdateHostKeys` appends the peer's other host keys, so
   removing the one line you added is not enough. Alternatively point `HGS_SSH` at
   a small wrapper executable (not a batch file) that inherits its standard handles
   and runs `%SystemRoot%\System32\OpenSSH\ssh.exe -o UserKnownHostsFile=<scratch
   file> -o UpdateHostKeys=no` followed by its own arguments.
3. **Checks.** Run them from PowerShell, or from Git Bash with `MSYS_NO_PATHCONV=1`:
   - register the peer's host, port, user and key with
     `hgs.exe machine set winpeer --json '{…}'` (Windows PowerShell 5.1 strips
     unescaped double quotes from native arguments, so write them as `\"` there),
     then run `hgs.exe machine check winpeer`;
   - `hgs.exe '@winpeer' ls --json`, then `hgs.exe ls --json` (the control-only
     local box and the peer) and plain `hgs.exe ls`. Time ten `@winpeer ls --json`
     calls: about 150 ms each over a LAN on 2026-10-11, almost all of it the SSH
     handshake;
   - forwarded read-only commands: `inspect`, `project ls --json`, `dirs`,
     `git-status --path /workspace/sample-project --json` and `worktrees`;
   - join the peer's swarm and run `hgs.exe swarm sync`: `hgs.exe swarm get` and
     `hgs.exe '@winpeer' swarm get` must report the same `digest`. With
     `hgs.exe swarm worker` running, a preference change reaches the peer within
     seconds;
   - agents, when a change touches agent control: install the reviewed Claude,
     Codex, Kimi and DeepSeek CLIs on the peer and point them at a loopback-only
     fake model (`ANTHROPIC_BASE_URL`, a Codex `model_providers` entry, a Kimi
     `providers` entry and the DeepSeek test model from
     `tests/fixtures/dsh-model.mjs`), never at an account. For each agent, run
     from `hgs.exe`: a `--new -d --launch-id` launch, `answer` for the startup
     dialogs, `send` with Cyrillic, several lines and a trailing backslash plus an
     image attachment, `history`, `terminal` snapshot, `interrupt`,
     `compact-context`, `clear-context`, and the `session-action` steps the
     session offers. Run the same sequence from a Linux client; every step must
     give the same result. Attach through a pseudoconsole with
     `a <session> --existing --run-id <run>`, type, resize and detach with
     `Ctrl+B d`. On 2026-10-12 both clients matched for Claude 2.1.296, Codex
     0.162.1, Kimi 2.1.1 and dsh 0.2.0-rc.2;
   - when the PC's desktop is free: attach the desktop app's Terminal to the peer's
     session, type, resize, interrupt a running command with Ctrl+C and
     disconnect. `hgs.exe` and `ssh.exe` must end, and the session must stay on
     the peer.
4. **Cleanup.** End every `hgs.exe`, `ssh.exe` and `hgs-tray.exe` started for the
   smoke, restore `known_hosts`, and delete the scratch folder and the disposable
   peer. In the real Roaming and Local AppData folders, remove
   `hgdev\hgs-tray.ini`, `hgs-tray.ini.drafts` and `hgdev\hgs-tray` written by
   the smoke, or restore their backups.
   Never attach to, stop or restart live sessions or tmux servers.

### Native agent compatibility and upstream review

Full Linux and macOS CLI CI also run `python3
scripts/ci/agent-compatibility.py`, and the Windows real-agent job runs it on
Windows ([above](#windows-real-agents-through-the-session-host)). Each ordinary
compatibility invocation resolves the official npm `latest` tag once for each
selected agent, then resolves its exact platform package and SHA-512 integrity.
All selected packages must resolve before any native execution. The report saves
their exact versions, URLs and digests before the first test; a missing release,
platform package or valid digest fails without falling back to historical pins.
Every terminal case uses that same selected image. Historical package records in
`scripts/ci/agent-compatibility.json` remain evidence for separately selected old
fixtures; ordinary CI and the daily upstream report do not execute those versions.
Targets are `linux-x64`, `linux-x64-baseline`, `darwin-arm64` and `win32-x64`.
From 2.1.304 Claude also
records each API request as an `api-request-blob` transcript row and can defer
WebFetch behind ToolSearch (its approval fixture sets `ENABLE_TOOL_SEARCH=false`).
Reviewed 2.1.314 against 2.1.312: its unattended approval countdown says
"automatically block" and "avoid holding up" instead of "automatically deny"
and "avoid blocking progress". Either exact advisory is outside question identity
because it counts down and disappears on navigation; the command, warning and
choices remain bound, and altered wording still refuses delivery.
Claude 2.1.317 adds a Bash reliability check after approval: macOS's system
Bash 3.2 refuses some compound commands with exit code 126. The native safety
fixtures retain the variable-path rm warning and exact directory fence using
`T=$PWD; test . -ef <fixture-work>`; the former `$(pwd)` and quoted-variable
comparison form was refused. Timeout diagnostics retain bounded native tool
results beside the screen so execution errors remain visible when Claude
collapses a completed tool call.
Claude offers a one-time default permission mode change; fixtures set
`hasSeenAutoDefaultNudge` except the suite that answers that offer. OpenCode has
a supported minimum of 2.0.32; CI tests its latest release on Linux and macOS.
On x86_64 Linux it uses the
`@opencode/cli-linux-x64-baseline` package, which runs without AVX2 on every x86_64
host. macOS therefore runs Claude Code and OpenCode, and Windows, where Zerus has no
OpenCode adapter, runs Codex and Claude Code. Only the native executable is
extracted into a disposable directory (`claude.exe` and `codex.exe` on Windows,
never Codex's sandbox setup program or its other helpers; OpenCode's `bin/opencode`);
agent binaries are neither committed nor bundled. The runner treats
skipped or empty native suites as failures. It writes each suite's output to
files rather than pipes, so a process a suite leaves behind cannot keep the run
waiting, and a suite that times out still leaves its log. `--agent claude`,
`--agent codex` or `--agent opencode` limits a local run to one matrix. `--hgs`
names the build under test (default: `HGS_TEST_BIN`, else the debug build). The
suites get a private home; of the caller's `HGS_` variables only
`HGS_TEST_CONPTY_DIR` reaches them, and none of its `OPENCODE_` variables do. The
runner's OpenCode version and help probes also get private XDG folders. On
Windows the runner's own version, help and schema probes also get a private
`USERPROFILE`, `APPDATA` and `LOCALAPPDATA`, while the suites keep the real
profile: each case gives its hosts and agents a profile of its own, and Codex
prints folders relative to the real one, which the suites place projects by.

On Linux and macOS, Claude Code runs every `tests/test_claude_*_native.py` suite
plus the Claude clear and compaction fixtures: exact input, questions, approvals,
trust, resume, clear, compaction, session-only model settings, the status line
context window and the session-start task instructions. Each test launches the real TUI under `env -i`, or through a tracked
`hgs` launch without the caller's `ANTHROPIC_` and `CLAUDE` variables, with a fixture
API key, a loopback or local fake API and no nonessential traffic, so no account,
telemetry or model request leaves the runner. On Windows each agent runs its class of
`tests/test_windows_native_agents.py` through the session host instead.

OpenCode runs `tests/test_opencode_native.py` with `ZERUS_TEST_OPENCODE_NATIVE=1`,
where a missing OpenCode, or one other than 2.x at or above the minimum, fails
rather than skips. The suite gives OpenCode and `hgs` a private HOME, private XDG
config, data, cache and state folders, private `hgs` state and tmux folders, and an
offline catalog (`OPENCODE_DISABLE_MODELS_FETCH`, `OPENCODE_DISABLE_AUTOUPDATE` and
`OPENCODE_DISABLE_LSP`). Before anything can start a service it runs `opencode
service set port` with a free private port, never OpenCode's default 49374, so
the background service that `hgs` starts registers only in the private state folder
and the developer's own service is never contacted. Its provider is a keyless local
OpenAI-compatible fixture that holds turns open, calls the `question` and `shell`
tools and answers HTTP 401 on request. The tests cover a stopped service (unknown
state, never started by inspection, listing or sending), a start that replaces only
a dead registration, Zerus session IDs and metadata, lost launch and message
responses without duplicates, a queued message through Stop and Remove, history,
question forms and permissions, model and effort changes, provider errors, OpenCode
1 bindings (adopted when migrated, otherwise stopped with only Forget), an older
version, a missing required route, a live registration that never answers, and the
explicit Start OpenCode action with its allowlisted catalog. Each test stops its
private service with `opencode service stop`; the suite then fails if any process
carrying its private root outlived it.

The macOS Claude matrix schedules its unchanged named unittest suites across two
private Python workers under the same original 900-second wall end, including
discovery, both starts, collection and a ten-second cleanup reserve. Each worker
gets short private HOME/XDG/TMP/state/config paths before importing fixtures and
cannot write shared source bytecode. The workers read same-byte, mode-0500 copies
of the already reviewed CLI and Claude image; neither caller binary is chmodded.
Both workers must discover the same full ordered suite inventory before either
runs. Whole original suite subtrees keep every occurrence, including imported
duplicate Clear coverage. Qualification requires the exact assigned START/END
orders, zero failures/errors/skips, agreeing result artifacts, and both direct
worker exits. Per-worker raw logs and `default.workers.json` retain this proof;
only the validated aggregate uses `ZERUS_NATIVE_RESULT`.

Timeout, cancellation, partial startup, incomplete reports and cleanup errors fail
the gate without replay. Cooperative cancellation runs the fixtures' existing
owned cleanups inside that same wall end. Killing a Python worker never proves
that a detached tmux/provider descendant exited; failed or unknown private roots
are retained. This scheduling change identifies no particular cause of slow
native startup, establishes no opaque provider-global-service isolation, and
changes no Linux, Windows, Codex or OpenCode suite or assertion.

Real native checks run at 95x47 and 110x35 terminal sizes with animations explicitly
enabled and disabled. Animated 95x47 checks require the actual released settled
idle logo before inspecting a Terminal clear; unknown or partial logo frames do
not confirm a reset. Assertion failures retain the synthetic native panel,
cursor and lifecycle state. The tests resume synthetic
history, then verify GUI-command clear, Terminal clear, the deferred native
identity transition, stale usage suppression, a single durable clear boundary,
preserved process/run identity, unsent native drafts and foreign-directory
rejection. A resumed conversation whose `SessionStart` is still deferred must
pause, and clear from the GUI and from Terminal, where the next message's hook
names the new conversation. A new session in a folder outside the fixture home,
which Codex prints in full in its header, answers a first message from the localhost
provider; idle polls must not treat that header above the history as a reset,
and a later clear must still be confirmed. A separate fixture caches a newer
version for the startup update picker and verifies its Activity identity,
**Skip** and **Skip until next version**. It selects **Update now** only after
checking that a fake `npm`, first on Codex's own `PATH` and with a private npm
prefix, stands in for the installer; the announced command must deliver the
choice while that fake still runs, and no input may reach it.
Nested real `codex exec --ephemeral`
reviews verify separate child Activity under Claude and Codex parents, preserved
parent identity/model/history and the child's recorded result without a rollout.
Codex async questions (feature `send_message_to_user_async`) come from a
localhost provider that holds the following turn open, because Codex shows the
panel only while a turn works. `hgs inspect` and `hgs answer` must recognize each
pinned panel (`ctrl+] skip` in 0.160, `^] skip` in 0.162, both with a `1 of 2`
line), keep a native draft, answer an option, Other and free text, and skip
without a reply. Codex 0.162 steers each answer into the working turn; 0.160
queues it until the turn ends, so the answer stays submitted until recorded.
Every fixture owns a private home
and tmux socket and uses only a localhost provider. No authenticated inference
or existing session is involved.
Fixture shutdown waits for its owned panes to exit before deleting the private
home, so a shell's final history write cannot race with filesystem cleanup.
Codex fixture homes turn plugins off, and each fixture checks the effective value
with `codex features list`: plugin sync clones the curated plugins from the
network through a git child that can outlive Codex, `codex exec` included, and
write into the home while it is deleted.

The separate **Agent upstream compatibility** workflow runs daily at 04:41 UTC
and is also available manually on upstream main. It discovers published Codex,
Claude Code, OpenCode, Kimi Code and DeepSeek Harness versions from their official
package indexes, links their official release notes and tests the latest Codex,
Claude Code and OpenCode, once per agent, using the same latest-only selection as
ordinary CI. OpenCode follows a minimum version
policy: Zerus accepts any 2.x at or above the reviewed minimum whose service offers
every operation it uses. CI qualifies the newest `@opencode/cli` release and reports
an incompatibility without changing the product's minimum. The report labels the newest OpenCode as newer than, the same as
or older than the reviewed minimum, and flags another major version or a prerelease
for review. Kimi discovery uses the current `@moonshot-ai/kimi-code` package,
not the archived Python `kimi-cli` project. The workflow does not install
anything on development machines. Its hosted job has read-only repository
access, no cache access and no publication credentials. Reports, native logs
and public protocol schemas expire after three days. A discovery or
native-contract failure fails the run; successful unittest skips do not hide
missing coverage.

Codex protocol schemas are captured separately with and without experimental
APIs, together with the selected release's CLI help and schema hashes. Reports
can be compared as data; the workflow does not execute an older agent to generate
a comparison. A detected addition is a review candidate, not evidence that Zerus supports it or that a
signed-in account can use it. For Kimi this workflow currently discovers
versions only. Its separate native and approval workflows qualify one latest
Kimi release for affected PRs or manual runs. DeepSeek's latest-release keyless
contract fixture runs in full Linux CLI CI.

The Kimi approval gate covers CreateGoal in both regular and fullscreen mode.
The fullscreen panel shows only the selected choice's description; inspection
checks that visible text without navigating. An explicit answer verifies every
choice's description through guarded navigation before submitting the requested
option. Changed or clipped descriptions, a changed request or a layout transition
refuse further input. Both modes retain native decision, permission-mode and
single goal-creation checks.

The Kimi native inventory includes launching with an initial message, waiting
at folder trust without sending it, then delivering it exactly once through
guarded input and preserving the conversation across pause/resume without
replaying that message. The former send-after-start-only coverage missed the
unsupported positional launch prompt. Help snapshots from native 2.1.1, 2.2.0
and 2.3.3 cover argument construction without running historical releases in CI.
Declining trust also verifies cancellation without delivery and bounds the
retained inspection preview independently of the full initial message size.

To test the latest releases or inspect current upstream changes locally:

```sh
cargo build --locked
python3 scripts/ci/agent-compatibility.py
python3 scripts/ci/agent-compatibility.py --agent claude
python3 scripts/ci/agent-compatibility.py --agent opencode
python3 scripts/ci/agent-compatibility.py --latest
```

The unshipped static coordination hint has a separately selected source candidate:
`--contract agent-hint`. This historical, separately granted fixture consumes its
frozen Claude and Codex pins and targets from `agent-compatibility.json`; it is
not part of ordinary CI's latest-only matrix. Native acceptance requires retained successful
results on the exact reviewed source. It selects
exactly four methods per pin: actual hgs lifecycle (launch, compact, clear, two
resumes, rename, fork), lost-mark output, real pre-feature first resume, and
global/account exclusions with instruction and personal-hook preservation. The
Unix launcher sentinel covers supported project shell launchers; Windows starts
the real executable directly and makes no shell-launcher claim. Existing default
UI method and geometry/animation coverage remain unchanged.

The source inputs are the candidate hgs, a real CLI built from pre-feature main
`ad3285c19013cf2ff78ec8eaf6ab2fbdd5253dce`, the `session_start_observer` Rust example,
and cached public official package tarballs whose exact filenames and SHA-512
pins are already in `agent-compatibility.json`. The hint selection never downloads
or retries missing packages. Build receipts must bind each declared Git source to
its binary hash; a version string cannot establish that association. A future
separately authorized run has this shape (all paths are examples, not inputs):

```sh
python3 scripts/ci/agent-compatibility.py --contract agent-hint --agent claude \
  --hgs /fixture/candidate/hgs --hgs-source <reviewed-40-digit-source> \
  --pre-feature-hgs /fixture/pre-feature/hgs \
  --hook-observer /fixture/observer/session_start_observer \
  --observer-source <reviewed-40-digit-observer-source> \
  --native-package-cache /fixture/public-package-cache --results /fixture/fresh-results
```

Each selected pin has one original 1200-second row end including preparation and
a 60-second cleanup reserve, with four fixed methods and first failure stopping
the selected campaign. Startup and deferred-resume gates keep their original
120-second ends, including late native consent answered only through `hgs answer`.
The native five-second hook timeout is unchanged. No prompt or count precedes the
actual public gate and native-process proof. Pre-feature migration launches and
pauses through the old CLI, then resumes through the candidate; Windows records a
resident old host separately from the newly installed candidate stable CLI.
Admission requires typed `tracked: true`, an explicit question list, and the
selected first/resume gate exactly `true` with its reason exactly empty. A known
already-bound steady conversation uses its exact run/conversation and idle
activity; its normal false first-message field has no first-message reason.
The exact observed read-only `Hooks need review` frame with 13 new or changed
hooks may wait within that same original gate for an answerable
`codex_hooks_trust` card. Its current run, hash, title, disclosure and all three
choices must match. This read-only card authorizes no answer or prompt; an altered
card is refused, and a card that never upgrades expires at the original end.
The product hook-review parser is unchanged.

Hint cases own cleanup before partial setup or launch can return. Their process
queries, private socket/endpoint stops, pinned-handle reaping, model shutdown and
post-stop image/observer checks share the same original row end. The ordinary
fixtures' cleanup defaults are not used or changed. Hint Windows profiles and the
outer hint scratch root have no automatic TemporaryDirectory deletion. Unknown
identity, a failed/expired cleanup or a parent timeout retains the namespaces and
fails qualification. Forced native stops use only a verified private socket or
pinned owned handles; no numeric-PID fallback is added. A parent timeout is not
proof of descendant exit. Files are removed only after every cleanup check has
verified within the original end. This bounds the fixture's waits, not arbitrary
host filesystem or logging stalls.
Windows cleanup queries the image through the already-pinned process handle.
If that image is unavailable, one zero-time wait on the same handle must prove
exit; otherwise cleanup remains UNKNOWN without a forced stop. Private cleanup
evidence retains at most 16 exception records, each with at most eight bounded
arguments and eight source frames, with explicit truncation/overflow metadata.
It records no frame locals. Private-stop failures still fail cleanup.

The fixture intentionally seeds an observed retained owned hook entry at a
separate private executable named `hgs` or `hgs.exe`. It leaves the real stable
CLI, staged images, supervisor/native argv and real answer channel intact. The
normal installer must retain that entry; this does not qualify fresh registration
as having generated the observer path. Each observed call starts one standalone
phase-sealed real hgs. Bounded capture never truncates forwarded streams; stdin,
stdout and stderr preserve their own byte order, without a cross-stream ordering
claim. Missing identity, incomplete capture/forwarding, overflow or unknown output
fails the case. Pipes add interposition and can change fault-path behavior.
Recorded output and completed forwarding do not prove customer receipt or
crash-atomic exactly-once delivery. The lost-mark control requires a second
distinct actual SessionStart output, then empty output on the next marked resume;
marks, model counts and transcripts alone cannot prove that, particularly with
Claude's identical-context deduplication. This is no OS/network-isolation claim,
native execution grant, shipped feature or release-dependency waiver.
On macOS, the observer child and fixture native process use `proc_pidpath` for
their absolute image paths and retain the existing `ps` birth identity. A `ps`
basename cannot establish executable identity.

Each call may add five fixed, exclusive stage files: before spawn, child identity,
first stdin write progress, child exit and drained streams. A stdin prefix is
bounded captured data and may be incomplete; these diagnostics never substitute
for complete `begin.json`/`result.json`, forwarding or cleanup proof. Their file
writes and syncs add observer timing overhead under the same five-second hook
limit. A missing result or recording failure remains UNKNOWN and fails the case.
Private Codex profiles set `sandbox_mode="danger-full-access"` and
`approval_policy="never"` for the keyless fixture, as the ordinary Windows native
suite does. Hook and folder trust still require their exact public hgs cards and
answers; the fixture never answers a Terminal-only sandbox setup card.

The finite allocation is seven pin/OS rows: retained Claude 2.1.296 and Claude
2.1.308 on Linux x86_64, Claude 2.1.308 on macOS arm64 and Windows x86_64;
retained Codex 0.160.1 on Linux x86_64, and
Codex 0.162.0 on Linux x86_64 and Windows x86_64. A disposable Linux job needs
Python, tmux and private short TMPDIR; macOS needs Python, tmux and its native
arm64 build; Windows needs Python and the reviewed ConPTY pair supplied through
`HGS_TEST_CONPTY_DIR`. Candidate and pre-feature hgs and the observer must be built
for that job's OS/architecture, with separate source/command/toolchain/hash
receipts. Linux Codex and Linux Claude selections run their two manifest pins
sequentially; all other provider/OS selections run one row. Each selection uses the command above
with `--agent claude` or `--agent codex` and fresh results, its OS-native binary
paths and official public cache. Existing version/help/schema probes remain part
of the runner. These focused disposable hosted or local jobs can be allocated
after review; ordinary CI defaults, suites and pins are unchanged.

#### Opt-in hosted hint gate

`agent-hint.yml` provides five disposable standard public-runner jobs. It runs
only in the public upstream repository, through `workflow_dispatch` or a PR to
main carrying the `native-agent-hint` label and changing one of its scoped source
paths. A PR run checks out the exact head commit. External-contributor workflow
approval still applies. Manual dispatch becomes available once the workflow is
registered on the default branch; select the reviewed source ref explicitly:

```sh
gh workflow run agent-hint.yml --ref <reviewed-source-branch-or-tag>
```

| Job | Manifest pins, in order | Required methods |
| --- | --- | --- |
| Linux Claude | 2.1.296 then 2.1.308 linux-x64 | 8 |
| Linux Codex | 0.160.1 then 0.162.0 linux-x64 | 8 |
| macOS arm64 Claude | 2.1.308 darwin-arm64 | 4 |
| Windows Claude | 2.1.308 win32-x64 | 4 |
| Windows Codex | 0.162.0 win32-x64 | 4 |

Every row must report its exact four raw IDs, four passes, zero skips/errors and
verified owned cleanup (eight for the two-pin Linux Codex and Claude rows). Each
stops before its second pin if the first fails. Jobs collect evidence
independently; all five must succeed for the 28-occurrence campaign to qualify. The gate does not publish or release, and
the full standard CI matrix remains required before merging the product change.

Before any Python interpreter starts, the workflow creates a short private Unix
`TMPDIR` (at most 24 bytes), with separate fresh private Cargo/rustup caches and
home. It uses no shared Actions cache, provider credential or signing environment.
Linux reuses the pinned tmux installer, macOS the existing Homebrew setup, and
Windows the reviewed ConPTY fetcher and MSVC environment helper. Rust 1.99.1 is
installed explicitly; preparation uses direct cargo/rustc paths with at most two
compiler jobs. Public dependency downloads belong to this hosted preparation
scope and never target an owner's installation.

`scripts/ci/prepare-agent-hint.py` has one original 2400-second preparation end:
public Cargo/npm fetches share at most 600 seconds, then candidate and pre-feature
builds each get at most 900 seconds, clamped to that original end. Builds are
locked and offline after fetching; dependencies must have crates.io checksums.
Only the selected official npm URLs, filenames and SHA-512 values already in
`agent-compatibility.json` are fetched (512 MiB maximum per package), without
latest discovery, redirects, retries or missing-cache fallback. Candidate hgs
and `session_start_observer` are built together from the candidate; real
pre-feature hgs is built from exact main
`ad3285c19013cf2ff78ec8eaf6ab2fbdd5253dce` on each OS. Preparation stops on its first
failure and starts no provider or hgs process.

The retained `build-receipt.json` binds clean pre/post Git commit/tree and lockfile
hashes, build argv, OS/architecture, direct tool hashes, raw-log hashes, exit codes
and all three binary hashes. Windows also records selected MSVC/SDK executable
hashes, versions and include/library search inputs; verbose build logs retain
compiler/linker invocation details. This associates source with build output;
it makes no reproducible-build or kernel-isolation claim. The row runner
independently hashes the binary inputs again.

Preparation and native rows have separate finite bounds. Dependency setup steps
have 5–10-minute caps, preparation has 45 minutes, each Unix selection 45 minutes,
and each Windows host step 28 minutes. Every job has a 120-minute whole-job cap,
including upload. Each native row keeps its original 1200 seconds, including the
60-second fixture cleanup reserve, original 120-second startup gates and unchanged
five-second hook timeout.

The Unix hint fixture records the exact private tmux server's socket, PID, birth
and kernel image before each launch when live, then registers each new run's
native and supervisor identities before consenting to a startup card. It retains
observed descendants while their recorded parents are live. The ledger permits
64 launch intents, 2,048 owned identities and 16 private panes; process snapshots
permit 16,384 rows and query output is checked against 2 MiB after capture. A
printable delimiter frames server, socket and pane fields: tmux may render TAB
control bytes as underscores. Owned socket paths containing that delimiter or
non-printable characters refuse before the query; malformed rows never qualify.
A stale or missing socket is insufficient: a zero-byte connection to only that
reserved Unix stream path must return ENOENT or ECONNREFUSED, all launch custody
must be complete, and all recorded birth identities must have exited. Unknown
connectivity, images, partial registration or exhausted bounds retain the runtime
and fail cleanup. No discovered numeric PID is signalled. Windows first waits
at most five seconds for the selected host's sessions to drain after pane kills,
clamped before the original row end's last five seconds, then keeps the same
pinned-handle exit and executable-release requirements. The 60 seconds is a row
cleanup reserve. If image lookup becomes unavailable, one passive wait of at
most five seconds on the same pinned handle may prove exit; the original end is
checked again afterward. Timeout, unknown status or late completion still fails,
without a forced stop, image-query retry or process-ID reopen. Cleanup uses
the existing absolute row end even after an early
body failure, and these checks do not create a renewed cleanup allowance.

An identity-bound, bounded read-only `startup_terminal_choice` may be observed
until a supported typed card appears within the same original 120-second gate.
It grants no answer, prompt or readiness proof. Unknown answerable cards still
refuse, and a forever read-only panel times out. This does not prove that a native
provider will expose a typed card; fresh native qualification is required.

Both qualification builds pass the per-command Cargo override
`--config profile.dev.package.sha2.opt-level=3`. The candidate hgs and observer
share that dependency profile; the real pre-feature hgs uses it as well. Normal
Cargo profiles, manifests and dependency pins remain unchanged. Both full
subject hashes and all stream joins remain required under the same five-second
hook limit. Incomplete diagnostics cannot distinguish an unfinished stream join
from an unfinished post-exit subject hash.

Focused helper qualification uses the same override in each command:

```sh
cargo test --locked --offline --example session_start_observer \
  --config profile.dev.package.sha2.opt-level=3 -- --nocapture
cargo clippy --locked --offline --example session_start_observer \
  --config profile.dev.package.sha2.opt-level=3 -- -D warnings
cargo build --locked --offline --bin hgs --example session_start_observer \
  --config profile.dev.package.sha2.opt-level=3
```

Windows reuses `outside-job.py` unchanged: WMI creation has a 120-second cap,
followed by a 1320-second monitor cap allowing the 1200-second row and up to 120
seconds of ordinary host cleanup. A monitor timeout may trigger the launcher's
separate shared 120-second cleanup allowance (30 seconds per action). Thus the
launcher permits up to 1560 seconds of bounded waits, apart from filesystem or
logging stalls, under the 1680-second host-step cap. This outer allowance is
separate from the row's 60-second reserve. Executable-pattern cleanup is confined
to these disposable Windows jobs; it cannot establish verified fixture cleanup
or turn UNKNOWN into PASS.

Three-day artifacts retain raw row logs/IDs and the full hook-output tree
(stdout and forwarded-stream hashes, exact phase/pre-send gates, known-consent
receipts and actual cleanup records), source/build receipts and the ConPTY fetch
log. Failed or unknown runtime roots remain on the disposable host until it is
discarded. The workflow neither deletes nor uploads those complete roots, build
targets or dependency caches. A canceled host may prevent artifact upload and
provides no successful qualification evidence.

The ordinary Windows compatibility matrix takes the release build and ConPTY pair as
[testing the session host](windows-host-testing.md#real-agents) shows. The
generated `artifacts/test-results/agent-compatibility/summary.md` and
`report.json` contain the coverage result and review candidates. Before changing
a supported integration, read the provider's release notes and compare CLI flags, hook events,
native history and protocol schemas. Classify each relevant feature as already
supported, requiring an adapter change, or requiring a separately reviewed
integration; track implementation work in Beads. Add an isolated native contract
for every adopted lifecycle/input change. CI tests one latest release per supported
agent and does not add retained resident versions. Historical frozen fixture
records retain their original target and integrity checks; they are not a CI
version-selection policy. Qualification does not update an installed agent or
restart a resident session.

Zerus receives activity from structured hooks, conversation messages and usage
from native history, and DeepSeek controls from its native API. Terminal checks
verify input ownership, empty composers and native dialogs, including Codex's
reset before its deferred SessionStart. Prefer a structured provider protocol
when it can address the same exact live process/run/conversation. A separate
app-server process must not be assumed to own an existing terminal session.
Windows cannot enumerate a process's open files. Deferred Codex fork verification
instead completes a bounded scan of only the selected account's `sessions`
directory and uses the existing Restart Manager query for each eligible child
file. It retains the exact supervised PID/birth, distinct child ID, parent,
interactive source, launch directory and empty-composer requirements. Links and
reparse points, missing or unreadable entries, uncertain ownership and multiple
eligible held histories refuse input. The scan allows at most 16,384 entries,
eight nested directory levels, 256 KiB per first-line header and 16 MiB of total
returned header data, within a two-second verification window. Exhaustion is
unknown; it never selects the newest file or accepts a partial inventory. This
window does not interrupt an arbitrary filesystem or Restart Manager stall and
does not extend the original startup, hook or cleanup budgets. Pure adapter
controls cannot prove that a pinned native Windows fork creates and holds its
child history before its first turn; that still requires native qualification.
See [Codex App Server](https://developers.openai.com/codex/app-server) for its
JSON-RPC protocol and version-specific schema generation. Migration of live
terminal-backed sessions requires separate ownership/resume verification.

No blanket formatting/clippy gate was added over legacy source: it would reject
unrelated existing formatting rather than verify this change. Existing unit,
integration and Qt suites provide the initial gate. Add stricter linting with a
separate, reviewed baseline cleanup. The Windows job is the exception: it runs
the pinned Rust 1.99 clippy with `-D warnings` over every module compiled for the
Windows target, which is clean today; Linux-only modules are not linted.
Logs are under ignored
`artifacts/test-results/`; packaging outputs are under ignored `artifacts/` and
`dist/`.

## Automatic nightly publication

`nightly.yml` runs daily at 00:17 UTC (03:17 Europe/Moscow) and supports manual
launches. It verifies one exact upstream `main` commit with the complete Linux,
Arch, macOS and Windows CI matrix, then repackages its already tested Arch binary
and copies the tested Windows zip unchanged. It checks the generated pacman
package, namcap output and installed bundle in an isolated home. There is no
second Rust/C++ compilation during nightly assembly.

The package is `zerus-ade-nightly-bin`; `zerus-git` remains a source package that
tracks upstream `main`. Nightly versions have the form
`0.37.0.r62.gabcdef0.n7`: product version, Git revision count, short source SHA
and monotonically increasing Nightly run number. The application keeps its
product/CLI versions. Rebuilding an unchanged commit creates a new package
version, allowing daily Arch ABI/dependency refreshes without replacing assets.

After a successful Nightly run, `publish-nightly.yml` automatically verifies the
upstream repository, workflow path/event, main branch, completion status, exact
source SHA, build number, GitHub artifact digest, sealed AUR recipes and checksums.
Only then does it receive the dedicated AUR key from the `release` environment.
Downloaded scripts/PKGBUILDs are never executed with publication credentials.

Each build creates an immutable `nightly-RUN_ID` GitHub prerelease and updates only
`zerus-ade-nightly-bin` in AUR. It never becomes the latest stable release or changes
`zerus`/`zerus-ade-bin`/`zerus-git`. Its public binary URL and SHA-256 stay fixed.
With `MACOS_CASKS_ENABLED=true` the Nightly run also builds, signs and notarizes the
Apple Silicon bundle with the same nightly version, adds it to the prerelease
(candidate schema 2) and updates only `Casks/zerus@nightly.rb` in the tap after AUR.
A failed macOS bundle fails the nightly, so AUR and Homebrew nightlies stay equal.
Binary library floors come from the verified source package's `.BUILDINFO`.
Stable and nightly publishers share a serialization group, preventing AUR races.

The Windows zip from the same CI run becomes `zerus-<nightly version>-windows-x86_64.zip`.
`prepare-nightly.py` reads only its zip directory and a bounded `build-info.json`,
requires the nightly's commit and product version, and records the name as
`windows_portable` in `release-info.json`. The zip is sealed in `SHA256SUMS`, and the
publisher checks its layout and provenance again before uploading it to the same
immutable prerelease; nothing in it is extracted or executed. A candidate whose
manifest names a zip must contain exactly that file, while retries of candidates
built before the Windows job remain valid. Because nightly publication requires the
whole CI run to succeed, a Windows failure also holds back the Arch nightly.

The zerus.dev mirror accepts the zip in the nightly asset family and checks its
GitHub digest, but neither mirrors it (the mirror never deletes and has a fixed
disk budget) nor lists it in the v1 desktop feed: deployed GUIs reject unknown
artifact kinds. Deploy the updated `scripts/update_feed.py` to the mirror before
the first nightly with a zip: older mirror code fails closed on the new asset
family and stops updating the nightly feed.

```sh
# Build and automatically publish a new fully verified nightly:
gh workflow run nightly.yml --ref main
# Retry partial publication using the same successful retained candidate:
gh workflow run publish-nightly.yml --ref main -f candidate_run=123456789
# Install the latest prebuilt nightly (conflicts with the other Zerus flavors):
yay -S zerus-ade-nightly-bin
# Source-based main builds require devel update checks:
yay -Syu --devel
```

Failed builds never publish. GitHub or AUR publication failures leave stable
releases untouched. Retry only the publisher with the same run ID; do not rerun a
published build to produce different bytes under its existing tag. If GitHub
refuses the job token's draft (seen once, for 0.42.0, as `403 Resource not
accessible by integration`), the publisher stops before any public write and prints
the release notes and the exact `gh release create ... --draft --target <commit>`
command. The owner creates that draft with their own token and reruns the same
publisher for the same candidate, which verifies the draft's target and uploads
only the sealed assets. Artifacts are
retained for three days and logs for three; public GitHub release assets remain
available after artifact expiry. Nightlies run even without new commits to test
current rolling Arch dependencies and produce fresh, uniquely versioned binaries.
After an accepted AUR push, RPC metadata and yay may briefly show the previous
version. Check the AUR Git repository's `.SRCINFO` and wait for metadata to refresh;
do not rebuild or republish the candidate to work around that delay.
Stable candidates/publication remain separate manual workflows; macOS checks are
now selected by default for new candidates and can still be explicitly disabled.

## What yay actually installs

`yay` is an AUR helper. AUR hosts a public `PKGBUILD` and generated `.SRCINFO`,
which tell users how to obtain and build a pacman package. Binary release archives
live upstream; they are not uploaded into AUR Git.
[AUR submission guidelines](https://wiki.archlinux.org/title/AUR_submission_guidelines),
[PKGBUILD manual](https://man.archlinux.org/man/PKGBUILD.5.en).

The owner selected the stable/source family on **2026-10-08** and added automated nightly binaries on **2026-10-09**:

| Name | Source | Purpose |
| --- | --- | --- |
| [zerus](https://aur.archlinux.org/packages/zerus) | Versioned source archive with SHA-256 | Stable release compiled locally |
| [zerus-ade-bin](https://aur.archlinux.org/packages/zerus-ade-bin) | Versioned Arch x86_64 binary archive with SHA-256 | Stable release without Rust/C++ compilation |
| [zerus-git](https://aur.archlinux.org/packages/zerus-git) | Public upstream `main`, version derived by `pkgver()` | Development build compiled locally |
| [zerus-ade-nightly-bin](https://aur.archlinux.org/packages/zerus-ade-nightly-bin) | Immutable verified nightly binary with SHA-256 | Current main without local compilation |

Primary owner/maintainer: **ufna**, **Vladimir Alyamkin <ufna@ufna.dev>**.
The stable/source AUR package pages confirm the approved maintainer. The publisher
rechecks ownership and SSH write access before writing. All flavors install the
same files and conflict with one another. The binary/VCS flavors provide versioned `zerus`.

Open-source prebuilt packages require the `-bin` suffix. `zerus-bin` belongs to
an unrelated project, so our binary package uses `zerus-ade-bin`.
[AUR naming rules](https://wiki.archlinux.org/title/AUR_submission_guidelines),
[existing zerus-bin package](https://aur.archlinux.org/packages/zerus-bin).

Stable recipes are templates until candidate assembly inserts the release version,
archive hashes and binary library floors. The VCS recipe tracks `main` explicitly;
its seed version is updated by makepkg. `SKIP` is only for its Git source.
Generate `.SRCINFO` with `makepkg --printsrcinfo`; never edit it manually.
[Rust package guidelines](https://wiki.archlinux.org/title/Rust_package_guidelines),
[VCS package guidelines](https://wiki.archlinux.org/title/VCS_package_guidelines),
[.SRCINFO](https://wiki.archlinux.org/title/.SRCINFO).

### Dependency declarations

| Class | Packages | Reason |
| --- | --- | --- |
| Desktop runtime | `webkit2gtk-4.1`, `gtk3`, `gdk-pixbuf2`, `glib2`, `cairo`, `libsoup3`, `libappindicator`, `dbus`, `hicolor-icon-theme` | Zerus Next webview, GTK integration, tray and icons |
| Media runtime | `alsa-lib`, `gstreamer`, `gst-plugins-base-libs`, `gst-plugins-base`, `gst-plugins-good`, `gst-plugins-bad`, `gst-plugins-ugly`, `gst-libav` | Voice capture, camera/round video, H.264/AAC and playback |
| CLI/runtime tools | `tmux>=3.7`, `openssh`, `python`, `curl`, `procps-ng`, `bash`, `tar` | Persistent sessions, remote access, compatibility helper, account HTTP, process inspection, installers and remote source transfer |
| Native library runtime | `glibc`, `libgcc`, `libstdc++` | Dynamically linked CLI/desktop binaries |
| Source build only | `rust`, `nodejs`, `npm`; additionally `git` for `zerus-git` | Locked Rust client/CLI/connector and frontend builds; Git source/version discovery |
| Standard build environment | `base-devel` | Required by Arch's makepkg/AUR build convention; its tools are not repeated in makedepends |
| Optional features | `nodejs`, `npm`, `git`, `konsole`, `wl-clipboard`, `xclip`, `systemd` | Native agent installation/runtime, worktrees, external terminals, clipboard and user services/scopes |

Both source recipes build `client/` with
`tauri build --no-bundle --config tauri.switch.linux.conf.json --features video`.
The output remains `hgs-tray`. Binary packages need no Rust, Node.js/npm or Qt
build toolchain. GTK/WebKit, media and C/C++ library floors come from the
**verified builder's `.BUILDINFO`**, never the publication runner;
`release-info.json` records them. Arch users should keep a fully updated system.
A fresh binary/ABI rebuild gets a new product version and immutable assets.

Node.js/npm are runtime options for separately installed agents. Credentials,
agent executables and user configuration are never bundled. Systemd integration
is detected at runtime; packaging never enables services.

### Package contents and compatibility

Each flavor installs the same ADE bundle:

- `/usr/bin/hgs`, `/usr/bin/hgs-tray`, the `hgs_state.py` compatibility shim and
  the explicit `zerus-setup` migration helper, whose `--mobile-connector
  {protect,migrate,rollback}` runs `/usr/lib/zerus/install-mobile-connector-service.py`.
- Zerus Next's `hgs-tray`, with embedded frontend assets. `check-package.py`
  requires the Next build identity and GTK/WebKit linkage, rejects Qt/KDE
  linkage and rejects a packaged Qt native UI plugin.
- `/usr/bin/zerus-mobile-connector`, the Rust mobile connector, and its user
  unit `zerus-mobile-connector.service` (`KillMode=process`, configuration
  `%E/hgs/mobile/connector.json`, skipped while that file is missing). The
  package never enables it, and an existing
  `~/.config/systemd/user/zerus-mobile-connector.service`, such as the Python
  connector's, keeps precedence; switching an installation over is the
  owner-approved cutover in the connector design. `check-package.py` requires
  both, a release build without test hooks that links only the C runtime, and
  the unit's `ExecStart` and kill mode.
- A desktop entry, existing application icons and system-provided **user** unit
  definitions for `hgs-tray`, `hgs-swarm` and `hgs-recovery`.
- Documentation and a sample tmux configuration under `/usr/share/doc/zerus`.
- Zerus MIT, the Codex data notice, and the host-target Cargo dependency
  notices for `hgs`, the mobile connector and the client with its shipped
  media/video features under `/usr/share/licenses/<package-name>`.

The Arch desktop dynamically links system GTK3/WebKitGTK and GStreamer; packages
ship no Qt/KDE runtime or native UI plugin. Existing sealed Qt candidates remain
valid for publication retries without changing their original bytes. The Qt
source installer remains an explicit fallback until Linux acceptance.
The runtime also declares curl for native account HTTP requests and procps-ng
for process inspection. Optional Node.js/npm enable agent installation from
Accounts; Git enables repository/worktree features. Arch's Node.js package does
not include npm, so both optional dependencies are listed.
The package does not bundle their distributions, native agent executables,
accounts, credentials or configuration. Package installation does not start or
stop services, edit user homes or replace the user's tmux configuration.

The product package version follows `VERSION`; the client embeds it in
`--build-info`. The compatible CLI has its own version in `Cargo.toml`.
Packaging records both versions and the source commit in `build-info.json`
when built from Git. For source-archive builds, the release manifest and archive
SHA-256 provide commit provenance.
Stable fixes use patch releases (`0.42.1`, `0.42.2`); feature releases use a new
minor version. `DEV_VERSION` names the next planned stable release and trusted
main CI embeds its growing sequence (`0.42.1-dev.215`). Android follows the same
rule with its independent `mobile/android/DEV_VERSION`. See the
[version rule](public-distribution.md#version-rule) for identity and channel
ordering. Bump both stable and next Dev bases when preparing a stable release;
ordinary main commits advance only the Dev sequence, never stable release tags.
Increase the product/desktop version for each new public bundle, including a
CLI-only bundle change, so published archive URLs and hashes remain immutable.
A recipe-only `pkgrel` bump can reuse immutable upstream assets. A newly compiled
binary needs a new product version under the prepared archive naming scheme;
do not replace an existing release archive during an ABI rebuild.

Existing runtime paths and IDs remain `hgs`/`HGS_*`/`hgs-tray`.
The desktop entry keeps the `hgs-tray.desktop` ID so existing launchers remain
associated with the same application. A local source-install entry can shadow
the system entry; inspect it deliberately during migration.
Remote SSH protocols call `~/.local/bin/hgs`. Pacman cannot install into arbitrary
user homes, so each user runs this once, **without sudo**:

```sh
zerus-setup
```

It creates compatibility symlinks to `/usr/bin` only after validating every
target and conflict. It is idempotent, preserves existing files and dangling
symlinks, and refuses root execution. An existing source installation needs an
intentional migration: inspect and move aside conflicting executables before
running it. Do not remove `~/.config/hgs` or agent/session state.

The package also ships the desktop-service installer. Explicitly register the
per-user backend with `zerus-setup --desktop-service`; it consumes the selected
CLI's configuration-scoped definition and IPC readiness probe. It imports the
current graphical login environment before service creation and enables
`hgs-desktop.service` for `graphical-session.target`. IPC readiness does not
enable background workers. Custom definitions and drop-ins are preserved.
The default `zerus-setup` command still creates compatibility links only.
Pacman install/upgrade hooks never register, start or restart user services;
after a package backend upgrade, run the explicit service setup again. See
[Linux desktop background service](linux-distribution.md#desktop-background-service).

Old user unit files in `~/.config/systemd/user` can shadow packaged definitions;
inspect `systemctl --user cat hgs-tray.service` and the worker units before
switching. Back up old definitions deliberately. Enable background services only
when wanted:

```sh
systemctl --user daemon-reload
systemctl --user enable --now hgs-tray.service
systemctl --user enable --now hgs-swarm.service hgs-recovery.service
```

`hgs-tray`, `hgs-recovery` and `hgs-swarm` use `KillMode=process`, both as
packaged and as the source installers write them (`install.sh` also writes
`hgs-voice`). A restart, or a worker exiting after an upgrade replaced `hgs`, then
ends only the unit's main process: native agents, tmux servers and DeepSeek hosts
that it started keep running. Under systemd's `control-group` default the same
events kill them. Units installed by earlier releases, and a
`zerus-mobile-connector.service` made from earlier
[mobile setup](mobile-deployment.md#protect-running-agents) instructions, may still
use that default. Before restarting or upgrading one, add a user drop-in with
`[Service]` and `KillMode=process`, run `systemctl --user daemon-reload` and verify
`systemctl --user show <unit> -p KillMode`. `install.sh` rewrites the worker units
and reloads the user manager before it restarts them. Prefer `restart` to `stop`:
tmux builds with systemd support put panes in `tmux-spawn-*.scope` units that are
`PartOf=` the unit holding the tmux server, so stopping that unit ends those panes
whatever its kill mode; `systemctl --user show <unit> -p ConsistsOf` lists them.
Package upgrades do not perform a restart. Review the old unit's kill policy before
restarting an existing installation; never use a broad process kill or restart all
native clients.

`hgs` starts a new tmux server from any user unit in a transient scope of its own
(see [Terminal, SSH and clipboard](reference.md#terminal-ssh-and-clipboard)), so
the panes of that server are not part of `hgs-tray`, the mobile connector or a
worker. A server that an earlier release started inside one of those units stays
there until it exits, and so does one started where systemd could not make the
scope: until then, restart that unit rather than stop it.

The swarm worker is the exception: it holds no agents. A current `hgs-swarm`
exits when an upgrade replaces its executable and `Restart=always` starts the new
version. A worker started by an earlier release keeps running and cannot read a
catalog that holds the phone notification timing, so the next `hgs swarm get`
(Zerus reads it every five seconds), `hgs swarm preference` or inbound swarm
exchange runs `systemctl --user try-restart hgs-swarm.service`, at most once
every ten minutes and only for the default `~/.config/hgs`. It never restarts
`hgs-tray`, `hgs-recovery`, agents, tmux servers or DeepSeek hosts. If Settings
still reports an outdated sync service, run
`systemctl --user restart hgs-swarm.service`.

## Manual build and publication

The public upstream is [ufna/zerus](https://github.com/ufna/zerus). There are two
independent manual workflows. Neither runs on pushes, pull requests, tags,
GitHub releases or a schedule.

### 1. Build and review a candidate

`release.yml` runs quick checks plus full Linux CLI, Arch, macOS and Windows
validation from one exact commit. macOS can be explicitly disabled; Windows
cannot, because the candidate carries the portable zip that this run tested. It
consumes that run's Arch package and Windows zip, verifies their embedded
version/source commit, assembles source and binary archives, rebuilds both stable
recipes, runs namcap and seals the result.
Stable and nightly candidates call the common `checks.yml` workflow directly
with read-only cache access. Their verification never includes the main-push dev
publisher or requests its tag-writing permissions.

```sh
# Only when the owner requests a hosted build:
gh workflow run release.yml --ref main -f version=0.42.0
# After this manual run succeeds:
gh run download RUN_ID --name zerus-0.42.0-candidate --dir /tmp/zerus-candidate
cd /tmp/zerus-candidate
sha256sum -c SHA256SUMS
```

The artifact contains source/binary archives, two checked pacman packages, the
Windows portable zip `zerus-VERSION-windows-x86_64.zip`, `release-info.json`, all
three concrete `PKGBUILD`/`.SRCINFO` pairs, their sealed `zerus-VERSION-aur.tar.gz`
archive and `SHA256SUMS` covering every release asset.
Git export attributes omit tracker, agent instructions/skills, workflows and
prototype artwork from the source archive; runtime artwork and notices remain.

The zip is the `windows-x86_64-<commit>` artifact of the same run, copied
unchanged under its stable name, exactly as nightlies take theirs.
`prepare-release.py` reads only its zip directory, a bounded `build-info.json` and
the pinned ConPTY pair, requires the candidate's commit and version (the bundled
`hgs-tray.exe` then reports that stable version with its commit and no Dev build
number) and records the name as `windows_portable` in `release-info.json`.
Validation and publication repeat these checks; nothing in the zip is extracted or
executed. A manifest that names the zip requires exactly that file, and an
undeclared zip fails. Candidates sealed without one, such as 0.42.2 and every
earlier stable release, remain valid for publication retries; a later candidate
without it fails validation, because portable Windows apps announce every stable
release after 0.42.2 as a Windows update.

Review the source commit, package logs, dependency/license inventory and recipes.
`create_draft=true` additionally creates a GitHub draft; by default everything
remains in the candidate artifact. Candidate artifacts expire after fourteen days,
logs after seven. Old 0.36.2 artifacts predate the new names/schema and cannot be
published by the new workflow. Never substitute a package built from another SHA.

Equivalent local assembly on Arch, with a clean committed checkout and its exact
verified package, remains available:

```sh
python3 scripts/prepare-release.py --version "$(cat VERSION)" \
  --package artifacts/zerus-git-EXACT_VERSION-x86_64.pkg.tar.zst \
  --windows-zip artifacts/zerus-windows-x86_64.zip --output dist
bash scripts/check-release-packages.sh dist
```

The Windows zip must be the one CI built and tested from the same commit; stable
releases after 0.42.2 carry it, and desktop apps rely on that. The helper requires
an empty output directory. Build artifacts are ignored and are never committed to
the application or AUR repositories.

Once the repository variable `MACOS_CASKS_ENABLED` is `true`, the candidate run
also builds the Apple Silicon bundle against the pinned official Qt, signs and
notarizes it in the main-only `macos-signing` environment and installs the result
through a local tap before assembly (`macos-cask.yml`). The candidate then uses
release-info schema 2 and contains the `zerus-VERSION-macos-arm64.zip` asset and
`homebrew/zerus.rb`. Until then, and with `-f macos_cask=false`, the candidate has
no macOS asset. Details, secrets and retry rules are in
[macOS distribution](macos-distribution.md).

### 2. Publish the reviewed candidate and update AUR and Homebrew

`publish.yml` takes the **successful candidate run ID** and its version. It uses
one Ubuntu runner, does no compilation and allocates no Arch/macOS runners:

```sh
# Only after review and an explicit owner publication request:
gh workflow run publish.yml --ref main \
  -f version=0.42.0 -f candidate_run=RUN_ID
```

The publisher verifies the same-repository manual run came from `main`, succeeded,
and contains exactly one unexpired versioned artifact. It checks GitHub's artifact
SHA-256, the manifest's exact source commit, all asset checksums, sealed AUR files
and public recipe URLs. Candidate source/recipe code is never executed with the
publication credential.

Before any GitHub release write it authenticates to AUR, checks ownership of all
names, prepares Git changes and refuses recipe downgrades. It creates or finishes
a draft for the exact commit, publishes it, verifies **every asset anonymously**,
then pushes the stable source, binary and VCS recipe updates. AUR Git receives only
`PKGBUILD` and `.SRCINFO`; release archives stay on GitHub. A macOS candidate also
stages the cask in `ufna/homebrew-zerus` with a dry-run push before the GitHub
write and pushes it after the AUR recipes.

The Windows zip reaches the same release unchanged, and the publisher checks its
layout and provenance again first. Release notes the publisher creates describe
it; a `create_draft` draft keeps its own notes. Like the nightly zip it stays on
GitHub: the zerus.dev mirror checks its sealed name and GitHub digest but neither
stores it nor lists it in the v1 desktop feed. Deploy the updated
`scripts/update_feed.py` to the mirror before publishing the first stable release
with a zip: older mirror code accepts a zip only in nightlies and fails the whole
`desktop/stable` job on that release. It keeps the previous stable catalog and,
where the same job also runs `--dev-platform desktop`, the previous desktop Dev
catalog too.

A separate `winget` job then proposes that zip to `microsoft/winget-pkgs` as
`ufna.Zerus` ([winget](public-distribution.md#winget)). It runs after the release
is public, reads only published assets and never proposes a version twice, so a
failed submission is retried with **Re-run failed jobs** without repeating the
release, AUR or Homebrew steps.

The VCS flavor follows `main` automatically when users rebuild. Release-time sync
updates its recipe when dependencies/build/package logic change; pure `pkgver`
bumps (including the corresponding versioned provides) do not create AUR commits.
Arch's submission rules explicitly disallow version-only VCS updates.

Publication runs are serialized across versions. Existing public assets must
match exactly; missing or different public assets stop publication. Uploads never
use `--clobber`, tags never move and AUR pushes never force. An interrupted draft
can resume only with matching existing assets and the exact commit. If an AUR push
fails after GitHub publication, **rerun Publish release with the same candidate
run and version**. Completed recipes become no-ops; the remaining updates retry.
The two services are not an atomic transaction, so inspect the failed job's output.
Do not rebuild or replace published archives to recover a partial AUR update.

Description-only AUR maintenance updates both `PKGBUILD` and generated `.SRCINFO`
without changing `pkgver`, `pkgrel`, dependencies or release assets. It does not
trigger package upgrades. Retrying a retained candidate preserves the explicitly
approved description refresh; every other recipe difference still follows the
normal version and provenance checks. Older sealed nightlies accept their exact
previous description, with the rest of the trusted binary template unchanged.

Publication retries require the same retained candidate artifact. After expiry,
recover the exact published assets/recipes for local review; do not regenerate
archives and assume identical checksums. A functional packaging-only stable update
needs an explicit `pkgrel` bump and a separately reviewed recipe change.

### Publication credentials

The dedicated AUR SSH public key is registered to **ufna** and SSH authentication
was verified on **2026-10-08**. Its private key is configured as the GitHub
**release environment** secret `AUR_SSH_PRIVATE_KEY`. That environment permits
only the `main` branch. CI/candidate jobs receive no AUR key. Publication jobs use
`actions: read` and `contents: write`; only the `winget` job needs a personal token:
the `release` secret `WINGET_TOKEN`, a classic token with the `public_repo` scope of
the account that owns the `microsoft/winget-pkgs` fork. Without it the job warns
and stable publication still succeeds.
The owner starts publication manually; environment reviewers may be added later.

`packaging/aur/known_hosts` pins AUR's Ed25519 host key, independently verified
against the [official AUR homepage](https://aur.archlinux.org). SSH uses strict
host checking, batch mode and only the dedicated identity. Rotate a host key only
after verifying a newly announced official fingerprint. Rotate the client key by
registering its replacement, updating the environment secret and revoking the old
public key. No password, private key or account session is stored in the repository.

### Release maintenance

For each new bundle, increase the product version, deliberately run a fresh
candidate, review its exact source/hash/library provenance, then publish that run.
Verify public GitHub downloads and all three AUR packages, including a fresh yay
installation, user setup and a process-preserving upgrade. Monitor AUR comments
and rolling-library changes. New stable candidates select macOS checks by default.

An authenticated release listing can find GitHub drafts that the tag endpoint
returns as 404. The publisher checks all pages for the exact tag and rejects
ambiguous matches and API errors before creating or resuming a draft.
After creating a draft, it retries read-only discovery for up to sixty seconds of
backoff to handle delayed GitHub visibility. It creates no duplicate draft and
does not retry API permission errors. A persistent failure retains the same
candidate for a publisher-only retry.

Signing with an owner-controlled identity and appointing a backup maintainer are
optional follow-ups. Checksums and exact run/commit provenance are provided;
package/tag signing is not configured.

## Validation record

The earlier runs below record platform/package validation before the temporary
manual-only budget policy. That budget change was checked locally with actionlint,
source checks and all eight suite selections, including 104 success/failure/skip
gate scenarios. The automatic policy enabled on 2026-10-09 supersedes it; its first
public nightly validation is recorded at the end of this section.

On **2026-10-08**, the complete
[main CI run](https://github.com/ufna/zerus/actions/runs/37705887611) passed every
job at commit `548469a`, including both CLI platforms, both desktop platforms,
Rust 1.85 and the installed Arch bundle check. Linux CLI validation covered 135
Rust unit tests, 202 terminal smoke checks and 398 Python cases across 33 modules;
native authenticated tests remain opt-in. Desktop coverage includes 30 Linux
and 32 macOS CTest suites. The corrected account fixture also passed ten
consecutive runs on each platform.

The artifact-only
[release candidate run](https://github.com/ufna/zerus/actions/runs/37705888076)
also passed its full CI and rebuilt the stable source and binary packages with
makepkg. Downloaded results are available locally under the ignored
`artifacts/review-candidate/` directory and in the `zerus-0.36.2-candidate`
Actions artifact (fourteen-day retention):

- Both pacman packages are approximately 5.4 MiB. All four archive/package hashes
  pass `sha256sum -c SHA256SUMS`.
- All three `.SRCINFO` files are present and exactly match `makepkg --printsrcinfo`.
- The binary manifest pins `548469a20972ec24594130efdff2d4d35ba0c768`; versions are
  GUI/product 0.36.2 and CLI 1.46.1. The package contains 59 dependency notices.
- The source archive contains 433 entries and excludes Beads, agent instructions,
  workflow files and design prototypes while retaining runtime artwork/notices.
- The downloaded binary package was actually installed in a disposable Arch
  container. System binaries, repeated `zerus-setup`, compatibility links and an
  isolated terminal session in an ordinary non-Git folder passed.
- namcap reported no errors. Warnings about dynamic dependencies/interpreters
  remain in the separate `release-packaging-548469a20972ec24594130efdff2d4d35ba0c768`
  log artifact and local `artifacts/review-release-logs/`.

Production packaging uses `BUILD_TESTING=OFF`. Migration helper tests cover
idempotence, conflict preservation, dangling links, missing targets and root
refusal. Removing `qt6-svg` makes the actual resource-render test fail; restoring
it passes. The final handoff/tracker documentation may follow the pinned candidate
commit; regenerate the exact candidate after changing release source or URLs.
Those preparation checks did not establish public AUR installation. The first
public installation and deployment validation is recorded below.

On **2026-10-08**, the publication changes were validated locally with actionlint,
source checks, generated makepkg metadata and credential-free contracts covering
provenance, archive safety, library floors, immutable draft/public retries, version
downgrades and GitHub/AUR ordering. No hosted workflow was dispatched.

The **0.37.0 local review candidate** pins commit `11dff8f` and is available under
the ignored `artifacts/review-candidate-0.37.0-11dff8f/` directory. A clean isolated
checkout built the real `zerus-git` production package, then assembled the exact
source/binary archives and rebuilt `zerus` and `zerus-ade-bin` with makepkg. Rust's
135 release unit tests passed; all three package layouts/executable versions and
59 dependency notices passed validation. namcap reported no errors; dynamic-tool
warnings remain in the logs. Both stable packages are about 5.4 MiB. All six sealed
release assets passed SHA-256 verification and all three `.SRCINFO` files exactly
match makepkg output. Library floors were checked against the real `.BUILDINFO`.
This local packaging review does not replace the full exact-commit release checks.
Regenerate the hosted candidate from the selected final `main` commit before
public publication; later documentation/tracker changes have a different SHA.

### First public release: 0.37.0

On **2026-10-08**, the owner explicitly requested the latest build, required
checks and deployment. The [release candidate run](https://github.com/ufna/zerus/actions/runs/37830899221)
pins `998bc4e9565a645e59b26bec3e2ced187e3d1444` and passed quick/minimum-Rust,
full Linux and Arch checks. macOS hosted jobs were skipped. Linux covered 145
Rust unit tests, 212 terminal smoke checks and 427 Python cases in 35 modules;
24 authenticated/native opt-in cases were skipped. All 32 Arch CTest suites
passed. VCS and both stable packages passed layout/version/notice checks and
namcap with no errors; known dynamic-tool/plugin warnings remain in the logs.

Independent candidate review verified GitHub's artifact digest, all six sealed
asset hashes, 466 source-export entries, binary embedded source provenance and
all three canonical makepkg metadata pairs. Binary floors record Qt base/SVG
6.12.0, WebEngine 6.11.2 and KDE Frameworks 6.30.0, plus the builder's C/C++/glibc
versions. Both stable packages include 59 Cargo dependency notices and the
vendored libvterm/md4c notices.

The [successful publication run](https://github.com/ufna/zerus/actions/runs/37833745339)
resumed the same candidate after correcting authenticated draft discovery. It
published [v0.37.0](https://github.com/ufna/zerus/releases/tag/v0.37.0), verified
all seven download assets anonymously and synchronized the three AUR recipes.
The release tag and archive hashes remain unchanged. AUR HTML pages became
visible before RPC metadata refreshed; yay lookup succeeded after that refresh.

A disposable, fully updated Arch environment installed **all three flavors through
yay**: binary/stable source `0.37.0-1` and VCS `0.37.0.r60.ga2fa919-1`. Installed
binary checks, idempotent user setup and ordinary non-Git terminal sessions passed.
An actual X11 desktop GUI, its isolated tmux server and terminal process survived
binary package reinstallation and subsequent source/VCS flavor replacement.
No user services were enabled by package installation or setup.

Existing Linux and Mac source installations were also updated. Linux binaries
were built from the reviewed source against its installed Qt 6.11 libraries. Mac native validation covered 143 Rust tests and 34 Qt suites; the
terminal suite was rerun after correcting a test assumption about rounded native
font metrics. Runtime code was unchanged by that correction. The Mac bundle was
verified with its existing ad-hoc signing procedure. Both managed GUIs activated
Sessions after claiming their sockets; existing persistent process identities and
state access were preserved. These local Mac checks used no GitHub macOS runner.
Authenticated model-turn tests remain opt-in and were not run for this release.

### First public nightly

On **2026-10-09**, automatic [push CI](https://github.com/ufna/zerus/actions/runs/37848718266)
and the first [Nightly build](https://github.com/ufna/zerus/actions/runs/37848718703)
passed full source/Rust 1.85, Linux CLI, Arch desktop/package and both macOS suites
from exact commit `9eb3a0e3867b42ef82c758781f1b6a23ac848978`. Nightly assembly
reused the tested Arch binary, checked the generated package with namcap and
verified its installed binaries, user setup and isolated tmux session.

The [successful publisher](https://github.com/ufna/zerus/actions/runs/37850588572)
resumed that same candidate after the initial publisher encountered delayed draft
visibility. Bounded read-only draft discovery now handles that condition, covered
by regression tests for transient absence, timeout and immediate API rejection.
All 27 stable/nightly publication contracts pass locally.

[nightly-37848718703](https://github.com/ufna/zerus/releases/tag/nightly-37848718703)
contains five immutable assets and is a prerelease; the latest stable release
remains `v0.37.0`. A fresh, fully updated isolated Arch environment installed
`zerus-ade-nightly-bin 0.37.0.r77.g9eb3a0e.n1-1` through the real yay helper.
Public archive checksums, runtime dependencies, idempotent `zerus-setup` and an
ordinary non-Git terminal session passed. No host agent or tmux process was touched.

The next [Nightly run](https://github.com/ufna/zerus/actions/runs/37851516418) passed
all suites and installed-package checks at
`fef1a00f0e505dd02d9aa0ad94c655ef6d954a1a`. Its [publisher](https://github.com/ufna/zerus/actions/runs/37852775356)
was triggered automatically by successful completion, created and anonymously
verified [nightly-37851516418](https://github.com/ufna/zerus/releases/tag/nightly-37851516418),
and synchronized AUR `0.37.0.r78.gfef1a00.n2-1` without a manual publisher retry.
After AUR metadata refreshed, ordinary `yay -Syu` detected and installed the new
version. The upgrade retained an existing isolated tmux server, terminal process,
session identity and user links. Installed-package checks passed again; all files
expected under the container's pacman extraction policy were present, and the
downloaded archive's build manifest matched the exact verified source commit.

### Stable release 0.41.0 and Android 0.1.29

On **2026-10-11**, the owner requested a new release from main and updates to the
installed clients, including macOS. The [release candidate](https://github.com/ufna/zerus/actions/runs/38160427346)
pins `39018336bfed70d57af148ecb0310c08384ed385` and passed the complete
Linux/Arch/macOS matrix, including the pinned keyless native Codex contracts,
relay PostgreSQL/memory qualification and both stable package rebuilds. Review
verified the Actions artifact digest, source contents against `git archive`,
embedded binary provenance, library floors, dependency notices and sealed assets.
The [protected publisher](https://github.com/ufna/zerus/actions/runs/38162077178)
published [v0.41.0](https://github.com/ufna/zerus/releases/tag/v0.41.0) as the latest
stable release. All seven immutable asset digests and the three anonymous AUR Git
recipes match the candidate; the VCS recipe retains its seed for version-only changes.

Existing Linux and macOS source installations now run **Zerus 0.41.0 / hgs
1.50.0** from that exact revision. On both machines, focused composer,
question-form and update tests passed, installed/running artifact hashes match
the qualified builds, and the managed GUI claimed its socket before activation.
Updates preserved native process identities, existing settings and drafts, with
private rollback backups. The Mac bundle uses the existing ad-hoc source-install
signature; this release does not provide a notarized public macOS installer.
Background draft persistence is installed, but full input responsiveness under
general macOS computer load still requires workload verification.

[Android 0.1.29](https://github.com/ufna/zerus/releases/tag/android-dev-0.1.29)
(version code **30**) uses the same source, package and published signing
certificate. Firebase-enabled Debug and Release each passed 597 unit tests;
release lint passed. Its exact minified APK, source archive, metadata and checksums
were verified anonymously after publication, while APK-bound R8 diagnostics remain
private. Scheduled synchronization published the new desktop and Android catalogs;
their anonymous downloads match the sealed candidates. No physical Android device
was connected for installation. The existing Android signer and Firebase inputs
are now provisioned in the protected release environment. Continuous main-build
Dev activation still requires the Actions-aware mirror and its read credential;
the same-app channel selector alone does not activate that pipeline.

### Stable release 0.42.0 and Android 0.1.30

On **2026-10-11**, the owner requested releases for all platforms and updates to the
installed clients. The [release candidate](https://github.com/ufna/zerus/actions/runs/38166908970)
pins `dfc63abd15ae71d90378ec1d962ebd8dbd8dc2a9` and passed the complete
Linux/Arch/macOS matrix and both stable package rebuilds. Review verified the
artifact checksums, the manifest source commit and the source archive file by
file against `git archive` of that commit.

The first two [publisher](https://github.com/ufna/zerus/actions/runs/38168633520)
runs failed before any public write: GitHub answered `403 Resource not accessible
by integration` when the job token created the draft, although it had
`contents: write` (zerus-ktg3). Their AUR step was only the `--dry-run` preflight.
The owner token then created the `v0.42.0` tag at the candidate commit and a draft
holding exactly the candidate assets, with every size and digest verified. The
[third publisher run](https://github.com/ufna/zerus/actions/runs/38168967736)
verified that draft, published [v0.42.0](https://github.com/ufna/zerus/releases/tag/v0.42.0)
as the latest stable release, checked every asset anonymously and synchronized
`zerus` and `zerus-ade-bin`; their AUR Git recipes name `0.42.0-1` and the v0.42.0
assets.

[Android 0.1.30](https://github.com/ufna/zerus/releases/tag/android-dev-0.1.30)
(version code **31**) was built locally from the exact archived source with the
existing signer and Firebase inputs. Debug and Release unit suites each passed 598
tests and release lint passed. The signing certificate matches 0.1.29, the source
archive passed Gitleaks 8.30.1, and the published assets match the sealed candidate
anonymously; the R8 mapping stays private. The public v1 Android dev feed serves
the same APK digest, and the v1 desktop stable feed serves the v0.42.0 package and
archive digests. No Android device was connected.

Existing Linux and macOS source installations run **Zerus 0.42.0 / hgs 1.51.0**
from that commit. Native agent and tmux processes were unchanged; only the GUI and
hgs helper services restarted.

### Shared Dev channel activation

On **2026-10-11**, [main CI 38170405374](https://github.com/ufna/zerus/actions/runs/38170405374)
passed the full Linux/Arch/macOS matrix and sealed desktop and Android Dev builds
from `196d2817ebb9efbe4521d55d9a2e91702dbffa48`. The lightweight `dev` tag advanced
to that source. [Protected publisher 38172339849](https://github.com/ufna/zerus/actions/runs/38172339849)
published this same candidate through the existing mirror after the Actions archive
media-type fix in `d45e8d2`; no candidate was rebuilt or GitHub Release created.
Both anonymous [desktop Dev](https://zerus.dev/updates/v2/desktop/dev.json) and
[Android Dev](https://zerus.dev/updates/v2/android/dev.json) catalogs, every payload
size/digest and exact Git source archive were verified. Android **0.1.30-dev.201 /
code 10201** preserves `app.zerus.mobile` and the original signing certificate.

`DEV_UPDATES_ENABLED=true` now enables successful-main publication. The existing
protected Android inputs and restricted SSH transport are provisioned; each
publisher lends only its repository-scoped Actions/Contents-read job token to the
existing unprivileged mirror. There is no stored personal token or second public
host. The original three mirror timers and legacy v1 catalogs remain in use;
GitHub latest remains `v0.42.0`. A fresh isolated desktop controller discovered the
real public Dev feed, emitted one verified notice, persisted its channel choice
and suppressed a duplicate check. Retention keeps three Dev builds per platform,
a 72-hour retirement grace and the shared 20 GiB limit. Desktop installation stays
explicit. Mac Dev now uses the same candidate/mirror with signed ZIP and DMG
assets and `zerus@dev`; see [Mac publication](macos-distribution.md#publication).
