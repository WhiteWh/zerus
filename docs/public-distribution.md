# Public release catalogs and mirrors

GitHub Releases are the provenance origin for the legacy schema 1 catalogs.
The static website mirrors verified release bytes and serves anonymous catalogs at:

- `https://zerus.dev/updates/v1/android/dev.json`
- `https://zerus.dev/updates/v1/desktop/stable.json`
- `https://zerus.dev/updates/v1/desktop/nightly.json`

The schema is specified in [mobile architecture](mobile-architecture.md#public-distribution-and-update-channels).
These catalogs do not use relay credentials. The mirror does not build, publish
to GitHub/AUR, dispatch hosted CI, or install anything on a client.

## Stable Android candidate

The manual `android-release.yml` workflow creates a candidate only, using the
existing release-environment Android signer and private Firebase configuration.
It neither publishes a release nor updates a catalog. Select upstream `main` and
provide the exact selected SHA, committed stable version and `VERSION_CODE`, a
completed successful full Nightly/Release candidate (or historical full main CI)
run for that SHA, and the reviewed existing signing-certificate SHA-256. Ordinary
PR/manual CI cannot authorize signing. Every named platform suite and its
aggregate gate must succeed in the selected attempt; a reduced Dev main gate or
an absent earlier-attempt gate cannot qualify Stable.

Before dispatch, pause and drain Dev publication and check every public channel
and distributed private preview code. The workflow checks committed identity;
it cannot discover undisclosed private APKs or replace that distribution-floor
review. It has no arbitrary ref, command, signing fallback or publishing mode.

The candidate builds an exclusive clean source extraction with JDK 17, SDK 36
and at most two Gradle workers, without Dev/version overrides or shared caches.
Its original thirty-minute end includes admission, tooling, build, APK/R8
validation and cleanup, reserving one minute for cleanup. Kernel stalls and
unproven detached descendants do not establish successful cleanup. A same-user
process whose identity the job cannot read, such as a capability-holding user
manager, is accepted only when it started before admission. Unknown
process custody or cleanup refuses the artifact and retains the private root.

The exact-build Crashlytics mapping upload must execute successfully, rather
than be skipped, cached or up-to-date. Before private cleanup, a durable receipt
binds APK/source hashes, the R8 map identity/hash and successful task evidence.
The existing private Crashlytics route retains the mapping; no raw mapping,
signing input, Firebase configuration or build log is uploaded as an artifact.
After cleanup, the public candidate contains only a `release/` folder with the
existing four APK/source/`release-info.json`/`SHA256SUMS` files, and a nonsecret
`candidate-provenance.json` beside it. The Actions artifact digest binds both;
publish only the four `release/` files. Their existing v1 checksum/asset family is
unchanged, and the provenance receipt never becomes a catalog/release asset.
Publishing and final installed/upgrade checks remain separate reviewed actions.

## Verified main dev channel

### Version rule

Stable bug-fix releases increment the patch version (`0.42.1`, `0.42.2`);
feature releases increment the minor version. Dev names the **next planned
stable release** plus the growing upstream CI sequence: `0.42.1-dev.215`,
`0.42.1-dev.216`. `DEV_VERSION` commits the next desktop release base;
`mobile/android/DEV_VERSION` does the same for the independently versioned
Android app. Each must stay newer than its stable version. Advance the Dev base
when preparing a stable release, never bump the product version for each CI run.

The checked Arch package embeds its Dev identity during trusted main CI, before
qualification and sealing. About, Installed in Updates and update notices show
that identity. `hgs-tray --build-info` exposes the embedded product version,
full Dev version, build sequence, source commit and dirty state. Ordinary local
builds show their source revision (and local changes) without inventing a CI
sequence. Selecting a channel never changes the installed identity.

Android follows the same `next-version-dev.N` convention with its existing
package and signer. `versionCode` must increase across both Stable and Dev,
independently of the visible version; switching channels never permits a
downgrade. Existing published assets and seals retain their original versions.
Stable publication remains an explicit owner action; each verified Dev build
advances the existing tag/catalog without creating another GitHub Release.

### Publication and discovery

Dev is a channel in the existing application, with the same Android package ID
and signing certificate as stable. Settings selects Stable or Dev on Android,
and Stable, Nightly or Dev on desktop. Changing channels never installs another
application, clears pairing/drafts, or downgrades Android. Downloads and installation
remain explicit; desktop continues to provide notifications and instructions.

For dev, the provenance origin is successful upstream-main **CI artifacts** plus
the lightweight `dev` Git tag, rather than a new GitHub Release for every push.
Only an upstream main push builds public Dev candidates. `ci.yml` requires
successful distributable builds, package identity and installation checks rather
than the full regression matrix. Its Dev-only Arch and Mac modes skip regression
test execution. After this run's build gate succeeds,
the protected Android job assembles/seals one APK with the existing signer and
removes private inputs; it never waits on a polling runner. Android unit/lint
checks remain credential-free in the independent `CI regression` workflow.

The credential-free pinned-Qt Mac bundle starts with the Arch package and packages
its Release app. After the same run's build gate and unsigned bundle pass,
`macos-cask.yml` signs/notarizes/installs it and seals the ZIP/DMG. Dev assembly
still waits for the Arch, Android and enabled Mac candidates. No signing input
or publication credential reaches PR code or the parallel unsigned build.

Full regression tests start separately when the build run completes. Their
failures remain visible and are fixed in separate follow-up work, while the
successfully built Dev stays available for testing. Stable and Nightly retain
their full qualification policy. Internal same-repository PRs do not need an
additional manual trust step before ordinary Dev integration.

The final main-only release-environment job advances `refs/tags/dev` to that
commit using a fast-forward update. A late older build cannot move the tag back;
diverged history requires explicit review. Stable `v*` and immutable nightly tags
retain their existing publication contracts.

Candidate artifact names include the CI attempt. A failed-job retry can reuse
the earlier successful platform's artifact without overwriting it. Once a build
is public, retry its publisher with the original candidate; rebuilding that run
cannot replace the published seal or APK version code. A new source change gets
a new CI run and Android version code.

`publish-dev-feed.py` verifies the tag against a successful main CI run, the
workflow identity, GitHub artifact digest, exact source/run metadata and every
candidate file. It treats archives as bounded data and executes no candidate
code. Public catalogs are separate from the legacy release feeds:

- `https://zerus.dev/updates/v2/android/dev.json`
- `https://zerus.dev/updates/v2/desktop/dev.json`

Schema 2 records `channelTag: dev`, `buildId`, `buildNumber`, `buildUrl` (the exact
GitHub Actions run), `sourceCommit`, `publishedAt`, version and sealed artifacts.
Files have immutable `/downloads/dev-<platform>-<buildId>/<name>` URLs. An existing
build ID cannot acquire different bytes or metadata. Desktop compares exact source
identity rather than treating an unchanged semantic version as proof of freshness;
a local build of unknown provenance can be told a dev build is available without
claiming that it is older. Android still checks package, signer, SDK, advertised
APK identity, byte size/hash and a strictly newer `versionCode` before installation.

Mac Dev candidates additionally carry signed ZIP/DMG bytes and an optional
top-level `macos` object with signing provenance and sealed `downloads`. Existing
source/Arch artifact kinds stay unchanged so deployed clients accept the catalog.
`publish-dev.yml` updates `zerus@dev` only after independently checking the public
catalog and both file hashes against that CI candidate. Mac clients compare their
full packaged version and offer Homebrew commands or a DMG download; installation
and restart stay explicit. See [Mac distribution](macos-distribution.md).

The same signing configuration builds stable and dev APKs; no dev-specific
application ID or generated replacement signer is used. `mobile/android/VERSION_CODE`
commits the stable code, and `-PzerusVersionCode=N` can select a reviewed build code.
Main CI derives Dev codes from that exact source as
`max(10000, committed stable code) + CI run_number`; retries keep their original
code. The helper reads neither live feeds nor a newer main revision, and rejects
invalid inputs or overflow before Gradle. Dev must exceed the committed stable code.

Both mirror publishers observe the highest code across v1 Android Stable,
legacy Dev and Nightly and v2 Dev under the existing shared `.mirror.lock`.
A new candidate must exceed every published channel; malformed or symlinked
catalogs fail closed. The exact current seal remains valid after another channel
advances, provided its metadata and retained bytes still verify. Late obsolete
candidates cannot lower the sequence or replace an existing artifact.

Before stable promotion, pause automatic Dev publication and commit a stable code
above every distributed APK. Keep Dev paused until the stable catalog and its
assets are publicly verified, then restore it. The next main CI candidate uses
the new committed floor without resetting the workflow sequence.
`-PzerusDevBuildNumber=N` adds a dev display suffix only. Switching to a
channel with an older version leaves the current app/data installed until a newer
compatible APK is published. Do not reset the version-code sequence when changing
workflow names or channels.

Preview APKs share this same version-code sequence. Before distributing one,
compare its code with the live Dev catalog and the next main CI sequence; avoid
reserving codes ahead of normal publication. Returning from a preview to Dev
requires a verified main candidate whose code is strictly greater than every
distributed preview. An equal code is not an update, even when its display name
or source differs. Keep the preview immutable, publish a newer main candidate,
and verify an in-place upgrade with the original signer and retained local data.
Rerunning an existing CI run does not increase its sequence or version code.

Automatic dev discovery is throttled to fifteen minutes. Android WorkManager
checks remain subject to system scheduling and battery restrictions; checks do not
require a paired workspace. Its settings badge and one silent replaceable system
notice advertise verified updates, with persistent per-channel deduplication and
existing Android notification permission. Desktop keeps its explicit automatic
check opt-out and one fresh verified notice per build. Stable desktop retains its
24-hour throttle; stable Android retains its six-hour throttle.

The publisher retains three recent dev builds per platform. Older builds are
marked retired only after they leave the protected current catalog and receive
a further 72-hour download grace. Collection is restricted to validated dev-owned
directories, preserves stable/nightly content and fails closed on symlinks or
missing ownership evidence. The shared 20 GiB disk budget can stop new publication
while grace-period assets are still retained. Actions candidates expire after
three days. No existing GitHub Release or tag is deleted by this workflow.
The unchanged current catalog is revalidated against its retained seal/bytes
without downloading Actions again, so it remains served after its CI artifact
expires. A missing or changed retained seal stops synchronization.

### Rollout prerequisites

Source support alone does not enable the live channel. Before enabling the main
workflow, provision the **existing** signer in the main-only `release`
environment as `ANDROID_KEYSTORE_BASE64`, `ANDROID_STORE_PASSWORD`,
`ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD`, and the existing private
`ANDROID_GOOGLE_SERVICES_JSON`. The Firebase input is required so automated APKs
do not silently remove current FCM/crash reporting. Missing inputs fail the dev
candidate, retain the previous channel and never generate a new signing identity.
No private signing input or R8 mapping is included in public candidates.
The CI job is opt-in through repository variable `DEV_UPDATES_ENABLED=true`.
Leave it unset until the signer and publisher are provisioned, so ordinary full
CI keeps working during rollout. Once enabled, every successful main push is
eligible automatically; missing required inputs fail publication.
While Dev is enabled, the active upstream main CI run finishes instead of being
cancelled by newer main pushes. GitHub keeps at most one pending revision in the
concurrency group and replaces it with the newest push. This preserves publication
progress during frequent development without an unbounded build queue. PR/manual
checks and main checks before Dev activation retain their cancellation behavior.

Install `dev_updates.py`, `publish-dev-feed.py`, `receive-dev-token.py` and the
adjacent existing mirror modules on the public host. The protected `Publish Dev`
workflow follows successful upstream main CI and uses its automatically issued
`GITHUB_TOKEN` with only Actions/Contents read permission. Its restricted SSH key,
target and pinned host key belong in the main-only `release` environment as
`DEV_MIRROR_SSH_KEY`, `DEV_MIRROR_TARGET` and `DEV_MIRROR_KNOWN_HOSTS`.
The SSH forced command permits only the bounded, preinstalled receiver as the
existing unprivileged mirror user. It accepts a run ID and job token through
standard input, verifies the completed main run and selected tag, and calls the
same publisher. It never stores the token, executes candidate code or accepts a
caller-selected command. The token expires when its CI job ends; no personal PAT
or broad owner credential is required on the server.

Extend the site's existing fifteen-minute jobs with `--dev-platform`. These jobs
can verify the retained public catalog anonymously after CI publication, without
downloading expired Actions artifacts. A race with a newer tagged run may retain
the previous catalog until its protected publisher completes. Sites using a
separate persistent read credential can alternatively supply it through systemd
`LoadCredential`/`GITHUB_TOKEN_FILE`. Use the same mirror root/lock and serve `updates/v2`
with the same revalidation behavior as `updates/v1`:

```sh
python3 /opt/zerus-updates/publish-update-feed.py \
  --root /var/lib/zerus-updates --channel desktop/stable --dev-platform desktop --execute
python3 /opt/zerus-updates/publish-update-feed.py \
  --root /var/lib/zerus-updates --channel android/dev --dev-platform android --execute
```

Without `--execute`, validation downloads only into temporary staging and changes
no mirror content. Keep API credentials off artifact/CDN redirects. A bootstrap
client upgrade is required: older Android clients retain their legacy v1 dev
feed and cannot parse schema 2. Continue that legacy feed during the rollout;
never repoint it to incompatible schema 2 data.

Desktop Dev contains an Arch x86_64 package and source, plus signed Apple Silicon
ZIP/DMG assets when Mac candidates are enabled. The optional Mac metadata keeps
legacy desktop catalog parsing compatible. Dev has no Windows build:
the Windows job gates Dev like every other suite, but its portable zip stays a
GitHub asset of stable and nightly releases, and schema 2 has no artifact kind
for it. A portable Windows build on the Dev channel says so, never reports a dev
build as an update and offers no dev download. Stable Android may remain unpublished initially;
selecting it reports an unavailable catalog without changing the installed app.
Public deployment, key provisioning and bootstrap publication are separate from
local source validation.

## Seal an Android development release

Build and verify the APK using the persistent development signing identity. Keep
keystore paths and passwords in private local configuration, outside the source
archive. Development APKs must be universal, non-debuggable, at most 128 MiB,
and retain package ID `app.zerus.mobile` and the existing signing certificate.
Use a larger `versionCode` for every newly published APK, including rebuilds of
the same display version.

Freeze the exact source tree used for the build into a reviewed `.tar.gz` source
snapshot. Include new source files and the uncommitted changes, honor export-ignore for `.beads`, `.agents`, `.github`,
`design`, agent instruction files and other excluded metadata, exclude ignored
runtime/build/credential files, and audit the frozen archive for secrets before
publication. `git archive HEAD` alone does **not** describe an uncommitted build.
An uncommitted development release records the base commit plus
`source_dirty: true` and the exact checked source archive. Its release notes must
say **Development source snapshot; includes uncommitted changes**. The commit
alone is not the build provenance. Do not create a commit to conceal this fact.

Example with synthetic paths (use the SDK tools from the verified build):

```sh
python3 scripts/prepare-mobile-release.py \
  --apk artifacts/release/zerus-0.1.8.apk \
  --source-snapshot artifacts/release/zerus-0.1.8-source.tar.gz \
  --source-commit 0123456789abcdef0123456789abcdef01234567 \
  --source-dirty --channel dev \
  --aapt2 /opt/android-sdk/build-tools/36.0.0/aapt2 \
  --apksigner /opt/android-sdk/build-tools/36.0.0/apksigner \
  --output artifacts/release/android-dev-0.1.8
```

The helper extracts the real APK package/version/SDK metadata, verifies its
signature, checks frozen copies again, and creates `release-info.json` and
`SHA256SUMS`. It refuses an existing candidate directory. The source archive must
already have been audited; the helper validates bytes, not the correspondence
between an arbitrary archive and a prior build.

Publish the reviewed frozen candidate within the existing release authorization.
Use an authenticated paginated release lookup before creating or resuming a draft:

```sh
gh api --paginate --slurp 'repos/ufna/zerus/releases?per_page=100' \
  | jq '[.[][] | select(.tag_name=="android-dev-0.1.8") | {id, tag_name, draft, prerelease, target_commitish}]'
```

A draft may exist even when `/releases/tags/<tag>` returns 404. During the reviewed
0.1.8 publication, the tag lookup returned 404 while the authenticated list found
draft release `407980374` with its uploaded assets. **Do not blindly recreate a
release after a tag lookup returns 404.** If the authenticated paginated lookup
fails, stop and resolve access first. If it returns an existing matching draft,
resume that exact ID; if it returns an already published release, verify it rather
than publishing again. Multiple matches require investigation.

Create a draft only when the successful authenticated lookup confirms no matching
release. Keep the target equal to the candidate's disclosed source base:

```sh
gh release create android-dev-0.1.8 artifacts/release/android-dev-0.1.8/* \
  --repo ufna/zerus \
  --target 0123456789abcdef0123456789abcdef01234567 \
  --draft --prerelease --latest=false --title 'Android 0.1.8' \
  --notes-file artifacts/release/android-dev-0.1.8-notes.txt
```

Repeat the authenticated paginated lookup to obtain the created draft's exact
positive integer ID. The following uses the reviewed 0.1.8 ID as an example;
subsequent publications must use their own verified ID:

```sh
ZERUS_RELEASE_ID=407980374
gh api "repos/ufna/zerus/releases/$ZERUS_RELEASE_ID" \
  --jq '{id, tag_name, draft, prerelease, target_commitish, assets: [.assets[] | {name, size, digest, state}]}'
```

Verify that the response ID and tag match the selected candidate, `draft` and
`prerelease` are true, and `target_commitish` matches its disclosed base. Compare
the exact asset set, every uploaded size and `sha256:` digest with the frozen
candidate, including `release-info.json` and `SHA256SUMS`; all assets must be
`uploaded`. Do not rebuild or change a frozen candidate to repair an API lookup.
Only after these checks, publish that exact release ID without changing GitHub's
latest stable desktop release:

```sh
gh api --method PATCH "repos/ufna/zerus/releases/$ZERUS_RELEASE_ID" \
  -F draft=false -F prerelease=true -f make_latest=false \
  --jq '{id, tag_name, draft, prerelease, published_at}'
gh api repos/ufna/zerus/releases/latest --jq '{tag_name, id}'
```

Verify the published ID/tag again and that the latest stable desktop tag remains
unchanged. The reviewed 0.1.8 publication retained `v0.37.0` as latest.

The tag points to the disclosed base commit. The mirror requires Android dev
releases to be prereleases with an `android-` or `mobile-` tag and checked source
snapshot metadata. A published asset name is immutable: use a new release/tag and
larger Android version code for changed bytes. This command does not dispatch CI.
Stable Android releases require clean committed provenance and a stable channel.

## Desktop release compatibility

The pull publisher consumes current main's sealed `release-info.json`,
`SHA256SUMS`, `.SRCINFO` recipes and actual GitHub asset digests. Stable tags are
`v<version>`; nightly tags are `nightly-<workflow_run_id>` with exact source and
workflow version provenance. It resolves each tag to its real source commit and
never substitutes GitHub's generic `/latest` endpoint for independent channels.

Stable and nightly releases can also carry the Windows portable zip,
`zerus-<version>-windows-x86_64.zip`, named as `windows_portable` in
`release-info.json` and sealed in `SHA256SUMS`. It is a GitHub-only asset: the
mirror checks its exact name and GitHub digest but neither downloads nor serves
it, and the v1 desktop feed does not list it, because deployed GUIs reject unknown
artifact kinds. The feed of a release with the zip is therefore the same as
without it, and releases sealed before it (stable 0.42.2 and earlier, older
nightlies) stay valid. An undeclared or differently named zip fails closed.
Deploy the updated `update_feed.py` before the first release of a channel that
carries a zip: older mirror code accepts it only in nightlies and fails the whole
`desktop/stable` job on a stable release with one, keeping the previous stable
catalog and, in a job that also runs `--dev-platform desktop`, the previous
desktop Dev catalog.

New Stable and Nightly candidates also carry
`zerus-<version>-windows-x86_64-setup.exe`, declared by `windows_installer.asset`
and sealed in the same `SHA256SUMS`. CI compiles and tests this per-user installer
from the paired ZIP without changing its payload. Its bounded build metadata
binds installer bytes, ZIP bytes, source commit and installed release version;
assembly validates that metadata before copying either file. Nightly verification
passes its computed Nightly version into the installer build and survival test,
so renaming a Stable installer cannot create a Nightly candidate.

`windows_installer.portable_sha256` must match the declared ZIP's sealed digest.
An installer without that ZIP, an unexpected name or a changed pair refuses.
Older ZIP-only candidates remain retryable unchanged. When manually re-preparing
an older tag whose CI predates installer build metadata, use that tag's assembly
scripts and omit both `--windows-installer` and `--windows-installer-info` to keep
its ZIP-only asset family. Installer-aware re-preparation instead requires a new
Windows verification/build with matching installer metadata for the exact source
and release version; do not fabricate metadata for an old setup or rename it.
Retrying an already sealed candidate needs neither re-preparation nor a new build.

Like the ZIP, setup stays on GitHub and outside the v1 feed and mirror downloads.
After explicit owner approval, deploy the updated `update_feed.py` before the
first installer-bearing release; an older mirror
rejects this additional sealed asset and retains its previous catalog. Winget
continues to consume the portable ZIP and its existing layout.

<a id="winget"></a>
Stable releases also reach the Windows Package Manager community repository as
`ufna.Zerus`. After **Publish release** succeeds, its `winget` job runs
`scripts/publish-winget.py`, which reads the published release, requires the zip's
`SHA256SUMS` line to equal GitHub's asset digest, and proposes three manifests
(`scripts/winget_manifest.py`, schema 1.12.0) to `microsoft/winget-pkgs` from the
`winget-pkgs` fork of the token's account. The manifest installs the immutable
GitHub asset as a portable zip whose `zerus-windows-x86_64` folder winget puts on
`PATH` (`ArchiveBinariesDependOnPath`), keeping the adjacent CLI, connector and
app-local runtime together. Zerus Next manifests declare the separately installed
`Microsoft.EdgeWebView2Runtime` dependency. Keep the zip's top folder and executable names.
A winget installation has no Start menu entry; start it with `hgs-tray`. Nightly and
Dev builds are not published to winget. A version already in `winget-pkgs`, or
already proposed there by anyone, is never proposed again, so rerunning the failed
`winget` job is safe; a proposal closed without merging stops the job for a person
to inspect. Moderators review new packages, and each version passes the
repository's validation pipeline, so winget can list a release some time after
GitHub. Without the `release` environment secret `WINGET_TOKEN` (a classic token
with the `public_repo` scope of the account that owns the fork) the job only
warns. Run `scripts/publish-winget.py --version VERSION --output DIR` to write the
manifests of a published release locally without submitting them.

The owner has standing personal Microsoft CLA consent for all `ufna.Zerus`
proposals from `ufna`. The publisher checks the exact PR author, fork, version
branch and three manifest paths, then posts
`@microsoft-github-policy-service agree` once if `Needs-CLA` is present and the
author has not already replied. New proposals wait up to one minute for the
bot's label. Existing proposals are checked again on publication retries.
The trusted-main **Confirm authorized winget CLA** workflow runs hourly and
supports manual dispatch to reconcile later prompts with the same helper; it
never opens or changes a proposal or its package files. Both paths serialize
through `winget-cla-consent`, use only the existing `release/WINGET_TOKEN`, and
never execute external PR code. Other accounts and corporate consent are not
covered. A comment whose result is unknown requires inspection before a retry;
an existing author agreement is not posted again. Moderator review and listing
in winget remain independent of this confirmation.

AUR versions are read from the published recipe archive, including epoch and
package release. In particular `zerus-git` is a VCS recipe, not an invented copy
of the stable version. The feed describes the release recipes; live AUR
propagation can lag GitHub publication. Existing main's release/AUR publication
workflow remains responsible for those releases and credentials.

Desktop update checks are asynchronous and throttled to one automatic attempt per
channel per 24 hours. A periodic timer checks the throttle in a continuously
running GUI. Settings → Updates exposes a persisted automatic-check preference;
disabling it cancels automatic requests and pending automatic notifications,
while the manual check remains available. Selecting a channel checks it immediately
only when automatic checking is enabled and its 24-hour throttle permits it. Bounded verified public metadata is cached per channel;
failed checks preserve the known release and explicitly mark it stale. Manual
checks remain available in Settings → Updates. A confirmed newer release adds
an Update available entry in the tray menu and workspace status bar. A fresh
verified comparison posts one system notification per release; cached data alone
does not post a notification. Each update click opens Settings → Updates and
never enters the session-attention route.

Pacman ownership is resolved for the canonical running GUI executable, followed
by the actual installed package version. Arch package comparison uses `vercmp`.
Supported AUR families are `zerus`, `zerus-git`, `zerus-ade-bin` and
`zerus-ade-nightly-bin`. The UI only copies instructions, never invokes privilege
escalation or replaces owned files. Example full-system commands:

```sh
paru -Syu zerus-ade-bin
paru -Syu --devel
```

Replace `paru` with your configured AUR helper if needed. Review the full system
upgrade. VCS/source installs require deliberate source updates and rebuilds;
unknown owners receive download/build instructions. Strict plain stable product
versions can identify a newer release for local builds, but equality, patched or
nightly versions never imply that an unowned executable is up to date. Arch is
detected independently by bounded parsing of os-release, never by sourcing it.
Available yay/paru helpers supply safe copied commands. An unowned local build
on Arch gets an explicitly optional AUR installation command rather than a false
package-ownership claim; no command is executed by Zerus. A Homebrew cask
installation is recognized by its single `Caskroom/zerus/<version>` or
`Caskroom/zerus@nightly/<version>` record. The UI copies `brew upgrade --cask` for
the cask matching the selected channel, or the uninstall-and-install command that
switches casks; a `zerus@nightly` installation starts on the nightly channel.
Nightlies compare by Nightly run number. Zerus offers **Restart to finish update**
once the bundle's `ZerusReleaseVersion` differs from the one the GUI started with.
The Windows portable build has no installer or self-update: it starts on the
stable channel, because stable releases after 0.42.2 carry the same tested zip as
nightlies, and links a newer stable release or nightly to its GitHub release page.
Earlier portable builds saved Nightly as their default; that saved choice is kept.
Stable 0.42.2 and earlier have no Windows build: they are never reported as an
update, and the message points to the nightly channel. A zip that winget extracted
(a folder under `WinGet\Packages\ufna.Zerus_<source>`) offers
`winget upgrade --id ufna.Zerus --exact` on the stable channel instead of the
download link. On Dev it reports that the channel has no
Windows build and never reports an update. Updates never stop agents, tmux or
resident adapters.

## Desktop mobile-connection settings

Settings → Mobile connection edits the existing private connector configuration.
Credentials stay masked and saves are atomic. Saving is independent of applying:
configuration changes take effect only when the connector is explicitly restarted
using the displayed commands. Zerus does not restart the connector or native
agents. The panel preserves the managed relay's receipt identity; a different
relay or credential requires a new unused private receipt directory, so old
receipts cannot authorize replay against another workspace.

## Pull and mirror

Install the two adjacent files `scripts/publish-update-feed.py` and
`scripts/update_feed.py`. They require only Python 3.11+ standard-library modules.
Run each channel independently so an unavailable channel cannot starve another:

```sh
python3 /opt/zerus-updates/publish-update-feed.py \
  --root /var/lib/zerus-updates --channel desktop/stable
python3 /opt/zerus-updates/publish-update-feed.py \
  --root /var/lib/zerus-updates --channel desktop/stable --execute
```

Omitting `--execute` performs a read-only validation/download dry-run without
creating the mirror root or publishing files. Repeat independently for
`desktop/nightly` and `android/dev`. A dedicated unprivileged writer should own
the root, with nginx having read-only access. Deny dotfiles in the web server.
The job uses a nonblocking root lock, bounded assets/metadata/redirects and a
20 GiB retained disk budget (`--max-mirror-bytes` can override it). A service
runtime deadline should bound the entire job; each network socket has a 30-second
timeout. A fifteen-minute timer leaves additional anonymous API headroom. Failed jobs retain the old catalog;
there is no automatic deletion of immutable historical assets.

`publish-update-feed.py`, `publish-dev-feed.py` and `receive-dev-token.py`
refuse Python older than 3.11 and, once their arguments parse, report every
failure on standard error as one fixed line, never as exception text or a
traceback. Validation, archive and transport failures share each script's
`... failed; previous catalogs retained` line; an unexpected defect names only
its exception class. Failures exit 1, except that the receiver exits 75 for a
busy mirror, which `Publish Dev` retries.

Public releases can be read anonymously. For larger catalogs, optionally supply
a read-only GitHub credential using `GITHUB_TOKEN_FILE` (a systemd credential
path), `GH_TOKEN` or `GITHUB_TOKEN`. Only `api.github.com` requests receive it;
redirected asset/CDN requests never receive authorization. Credentials and signed
CDN URLs are not logged or placed in published files. No GitHub deploy secret is
needed on the website.

Stable releases and nightly prereleases from candidate schema 2 also carry the
notarized `zerus-<version>-macos-arm64.zip` of the `zerus` or `zerus@nightly`
Homebrew cask. New candidates also declare `macos.dmg` with the separately
notarized `zerus-<version>-macos-arm64.dmg` and its notarization ID; older ZIP-only
schema-2 releases remain valid. Like the Windows ZIP, both macOS formats stay on
GitHub: the mirror checks their sealed names and GitHub digests but neither
stores it (it never deletes and has a fixed disk budget) nor lists it in catalog
artifacts, which existing desktop clients would reject as an unknown kind.
**Deploy the current `update_feed.py` to the mirror before setting
`MACOS_CASKS_ENABLED`, and deploy the DMG-aware version before the first DMG
release**; an older mirror rejects such a release and keeps the
previous catalog.

Downloaded assets are checked against GitHub sizes/digests and sealed checksums.
Existing assets are streamed and rehashed for reuse, avoiding repeated binary
downloads. A differing immutable asset, tag mismatch, same-code Android
replacement, malformed/unsealed release, or rollback fails closed. Assets,
including `SHA256SUMS`, are durably linked before the small catalog pointer is
atomically replaced. Clients receive only `/downloads/<tag>/<name>` URLs.

## Local validation

```sh
python3 -m unittest discover -s tests -p test_update_feed.py -v
python3 -m unittest discover -s tests -p test_mobile_release.py -v
bash scripts/ci/gui.sh
```

The update controller, the Updates settings page and the update entries in the
sessions window and tray menu are part of the regular desktop build, so the
desktop script runs their checks with the rest of the Qt suite (see
[CONTRIBUTING.md](../CONTRIBUTING.md)): `test_updates` covers the controller
and `test_sessionswindow` the route from the sessions window to the Updates
page. Deploy only the reviewed current-main GUI within the existing update
authorization, with `tray/install.sh`, which replaces a running GUI through
`tray/update-installed.py`. An older worktree GUI must not replace a newer
desktop. Source commits, pushes and hosted CI dispatch remain separately
controlled.
