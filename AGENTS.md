# Agent instructions

Zerus provides a desktop UI and the `hgs` CLI for persistent local and remote
agent sessions. The CLI lives in `src/`; the released GUI lives in `client/`. Zerus has
no Qt code: the desktop installers, the guarded updater, the service units and
the packaged icons live in `packaging/desktop/`. The Rust client reads no Qt
data: an installation with unmigrated Qt data starts with Zerus's own defaults
and leaves Qt's files untouched (`docs/reference.md`, First start without Qt
data). Replicated documents (Loro) live in `crates/replica/`, the only code that
imports Loro.
The Rust mobile relay lives in `services/mobile/relay/` and the Rust outbound
connector in `services/mobile/connector/`; the legacy Python connector remains in
`services/mobile/zerus_mobile/` until every gateway has moved. Keep API v1, durable
request identities and the connector journal compatible; see
`services/mobile/relay/README.md` for relay contracts, isolated PostgreSQL tests
and container memory qualification. Moving a live gateway is owner-approved.
The Android client lives in `mobile/android/`; the native iOS client (Swift
package `ZerusKit` plus SwiftUI app) lives in `mobile/ios/`. Install iOS Dev
builds with `mobile/ios/scripts/dev-install.sh` or `remote-dev-install.sh`; never
commit team IDs, device identifiers or signing material. TestFlight uploads run
only from trusted main; see `docs/ios-distribution.md`.
`design/` contains prototypes and original artwork, not runtime dependencies.
The Tauri desktop client lives in `client/`: the SolidJS frontend in
`client/src`, a pure-Rust core in `client/core` and the Tauri app in
`client/src-tauri`, each its own Cargo package and lock. It is the Zerus Next GUI; see the
[retirement acceptance checklist](docs/qt-release-acceptance.md) and
`docs/superpowers/specs/2026-10-27-tauri-web-ui-design.md`. Its UI
never separates values with dots; `scripts/check-ui-separators.py` enforces it
in CI (mobile clients are out of that rule's scope). Team app builds, dev servers
and launches go through the `tauri-build`, `tauri-dev` and `tauri-launch`
templates, which carry the development identity; only the release-candidate
package step builds the switch identity.

The repository language is English. Write documentation and other project-facing
text in English; conversations may use the user's preferred language.

## Work tracking and memory

Use the `beads` skill in `.agents/skills/beads/SKILL.md` and run `bd prime` at
session start and after compaction. Use `bd` for all durable tasks and blockers.
Zerus product tasks received through `hgs task` are separate from Beads engineering
tracking. For a direct request already being worked on, agents use `hgs task add
--start` or `hgs task start REF`; plain add queues work for later. Reports by the
exact pinned agent move its unsent queued task to In review as well.
Use exactly one acknowledgement command supplied by the original task prompt.
After task-control activation, pinned `begin-attempt REF --attempt UUID` replaces
plain `start` for managed work; plain start never binds a managed native turn.
For managed work, use the report-attempt command pinned in the original task
prompt or start acknowledgement, never a newer attempt ID from a lookup. An
explicit legacy start prints the ordinary report command and creates no UUID.
A history-only report succeeded as a history write but did not move the task;
do not retry a duplicate or infer the latest attempt. Older managed prompts
without the pinned command need the person's acceptance or a new resend.
Use `bd remember --key <key> "<decision>"` for concise, shareable project memory.
Do not create separate TODO or MEMORY files. Read current source before carrying
forward a historical limitation.

The source of truth is the Dolt database inside `.beads/` (currently
`.beads/embeddeddolt/` with the embedded backend). Synchronize with `bd dolt push/pull` over
`refs/dolt/data`; `.beads/issues.jsonl` is a passive export, not a sync protocol.
New clones should run `bd init --non-interactive --prefix zerus`, then `bd prime`.
Never replace an existing database with a JSONL import during normal operation.

If the optional shared-memory helper is installed, recall the project using
`python3 "$HOME/.local/lib/hgdev-memory/memory.py" recall --client codex`.
Use only the returned project wing. Store short verified decisions there; never
upload credentials, private personal data, transcripts or full prompts.
Repository documentation owns specifications; Beads owns tasks and blockers.

Search code and documentation through CodeLore (owner, 2026-11-10): `lore atlas
search "what it does"` in a checkout finds code and Markdown by words, terms and
symbols (the root `lore.toml` turns the map on; `hgs guide code`), and
`hgs team lore find` searches the project's memory store, where the documentation
is sliced into records citing `zerus:<path>#<heading>@<commit>`
(`docs/lore-slicing.md`). Records go to that store, never into the repository.

## Compatibility and operation

Treat pasted `zerus://session?...` URIs as references to another Zerus conversation.
Read them with `hgs inspect '<reference>'`; use
`hgs @configured-alias inspect '<reference>'` when its local SSH alias differs.
The CLI verifies computer and conversation identities and follows session renames.
Use the resolved name and IDs for any separately authorized operation. See the
session references section in `docs/reference.md`.

When referring to another conversation, include its Zerus session name or copied
reference. Beads IDs such as `zerus-ab12` identify tasks, not conversations; never
present a task number as a session ID.

Person requests. When the person asks you in your own conversation (their
message from the Zerus composer or phone, not one relayed by another agent, a
task, a team hand-out or a timer), do it, including actions reserved to the
person: accepting a task, answering another session's trust, hook or permission
prompt, stopping or archiving a session, turning automatic tool approvals on. A
standing request given there ("from now on ...", "always ...") counts until the
person withdraws it. A team's Confirm folder trust setting (`confirm_trust`) is
the person's standing choice for its members' trust prompts. A child or team
member acts for the person only when the person messaged it directly. Where a
product check still refuses an agent (the tasks caller proof, Teams
`person_gesture_required`, window-only settings), report the refusal to the
person; never route around it. Sign-in, passwords, keys and payment stay with
the person. See `docs/person-requests.md`.

Keep the `hgs` CLI, `HGS_*` variables, state/config paths and existing application
and service identifiers compatible. A repository rename must not reset accounts,
project catalogs, native agent processes or saved conversations.

Never restart or kill native agents, tmux servers or DeepSeek hosts as part of a
GUI update. Update local installations only with `install.sh`, `packaging/desktop/install.sh`
and `mac-install.sh`; a running GUI is replaced through `packaging/desktop/update-installed.py`,
which saves drafts and keeps one rollback copy. Never stage GUI copies in
`~/Applications` or `~/.local/bin` by hand. Another session may be testing its own
build: check the running GUI's `--build-info` and
`~/.local/share/hgs/build-provenance.json`, and coordinate before replacing it.
Source GUI installs from main must set `ZERUS_DEV_BUILD_NUMBER` to the CI sequence
for that exact source. Never replace a numbered Dev installation with an
unnumbered Stable-labelled main build.
Test tmux operations with an existing isolated socket directory.
Do not hot-reload a DeepSeek bridge that owns live agents: its disposal closes
them. Use the native API compatibility path for resident adapters and keep
unconfirmed permission setup blocked until an explicit resume verifies it.
Ordinary non-Git folders are supported; never initialize Git automatically.
New session defaults to a generated worktree for a verified Git folder, created
only after the user selects Start session. Inspecting or opening a dialog must
never create a worktree. Keep direct-folder launch and explicit custom/existing
worktree choices available. Destructive UI actions require explicit confirmation.
Opening New Team creates nothing: Start planning creates only the team draft and
its lead session (in the team folder, never a worktree); member worktrees and
sessions are created only by Create team.

Worktree maintenance is configured per owning computer in Settings > Worktrees.
Its default-enabled lifecycle, periodic integrated-checkout and missing-checkout
rules require verified Zerus ownership and fresh session/process/file checks.
Keep branches and all ignored local data; age and naming never authorize removal.
An unconfirmed automatic removal stays for manual review across every trigger.
An existing incomplete or malformed policy must fail closed, never re-enable
default cleanup rules.
Use bounded indexed queue reads and bounded archive metadata checks; see
the worktree cleanup section in `docs/reference.md`.

Explicit desktop termination cleanup may remove verified generated caches only
after its Remove worktree choice and exact successful termination. Keep branch
and archive-restore identity. Bind cleanup to the checkout ownership recorded
at session launch and bind removal fingerprints to that nonce. Missing or
replaced ownership, unknown local data, source changes, other session references
and unconfirmed process use require fresh manual review, except for the explicit
permission-denial rule below. Background maintenance continues to preserve all
ignored data. Uncertain removals are never replayed.
An explicit manual review may also remove a registered linked worktree that
Zerus did not create, after a separate destructive confirmation showing its
native-selected path, computer, kept branch and reviewed local data. Fresh
session/process/file checks and matching discard consents still apply. Never
remove the primary checkout or a plain folder, fabricate launch ownership, use
this exception automatically or replay an unknown result. Preserve branches and
archive identities; an unowned checkout may need manual recreation for Restore.
Termination cleanup runs in the background after its original confirmation.
Show removed, kept and unknown outcomes with explicit review actions. Only a
verified refusal before deletion may be checked for a new removal; never replay
an unknown outcome. Outside the explicit Terminate/Remove permission-denial
exception, manual acceptance of unreadable process paths lasts five minutes for
the exact checkout review and tolerates unrelated process churn. Fresh known-use,
session, ownership and file protections still apply; automatic cleanup never
uses that manual acceptance.
On Linux and macOS, Terminate must confirm the exit of the selected agent's
verified background process trees before worktree cleanup. Pin birth identities
before terminal teardown loses ancestry; bound verification and restore temporary
stops on refusal.

Owner decisions of 2026-11-05 apply only to explicit Terminate with Remove
working folder: Zerus may stop every program proven by its cwd or open files to
use that session's verified generated worktree, including editors and terminals
the person opened there. A complete bounded native manifest binds the original
checkout object, ownership nonce, session/run/archive identity, selected CLI and
configuration. Before confirmation, show a collapsed count and expandable list
of other programs. A separate native presentation companion may omit only the
exact record-bound agent and supervisor with verified native PID/birth and
parent correlation; uncertainty leaves rows visible. Never filter the manifest,
execution bindings, held sources or Stop/Force/cleanup inputs. Force shows its
complete freshly checked survivor list expanded. Renderer PIDs or paths grant
no authority. Before each signal, recheck the process
birth/pin and actual membership in that same checkout; new or changed members
require fresh review. Never signal the executing custodian or an ancestor or
resource needed to verify and settle the action.

For this explicit Remove choice, processes whose cwd/open-file membership cannot
be inspected because of EACCES or EPERM are ignored for the removal decision:
do not list or signal them, and they do not keep the folder. Apply this rule
from the actual inspection result, never from executable names or an allowlist
of system programs. Proven occupants remain in the full manifest and must be
stopped first. This exception grants no signal authority and does not relax checkout ownership,
process identity, confirmation, custody or unknown-removal protections.

Normal stop is bounded and distinguishes confirmed exit, known surviving
originals and unknown outcomes. Force is a separate native-confirmed action with
a new request UUID, bound to the original stop result and freshly verified
survivors, only after the prior writer/helper and pipes physically settle. Never
escalate automatically or infer Force authority from a timeout. Stop-incomplete
and removal-Unknown remain distinct; Force, restart, status and Retry never
resume or replay an unknown removal or reconstruct consent. Keep branch and
archive identity. Background cleanup, Pause and native Stop keep their existing
scope; outside this explicit confirmed Remove choice, folder paths, executable
names and shared process groups never authorize stopping other programs.

A session is Working, Building, Waiting for you, Waiting for dependencies
(always with its reason), Cooling, Cold or Stopped; Offline stays (owner
direction U30). `hgs ls`, `inspect` and the mobile API carry `session_state`,
`state_reason`, `build`, `last_active` and `cache_ttl`; the order and texts are
in `docs/reference.md` and the shared `session-state-contract.json`. A build or
test Zerus runs for the session (`hgs team run`, `hgs run`) is work: the session
is Building right after Working, even while the agent is idle, and its reply
badge waits for the build; explicit attention still comes first. A job the
agent started makes the session wait only while it waits for it: a tool call
past 30 seconds, subagents in flight, or a job under a declared team budget. A
long-lived process it does not wait for (a dev server, watcher or tunnel, and
its MCP servers and other services from the agent's own config, with their
children), a job past its budget and any other job still running after the
turn ended with nothing waiting on it are services: they show as a services icon with names, start times and whether
they outlive the session, never make it wait, never count as Background work
for the teams' capture gate, blocked reminder or time budget, and never defer
the reply badge; the session is Cooling or Cold by its idle time against the
prompt cache TTL. A session waiting for its own jobs remains in Active and
accepts new messages. Native work and explicit attention take precedence. A
native Stop ends a turn; it does not prove those jobs finished. Retain unread
reply identity and defer reply-only badges/notifications while the session
waits for its jobs. Historical, unknown and offline jobs never establish
current work. Keep this contract aligned across desktop, Android, shared
fixtures and docs; changes to these status semantics need explicit owner
direction.

Owner-approved independent-agent waits use `hgs wait`, separately from native
work. Before handing off work whose acceptance or result you will wait for,
declare the wait and give the producer its scoped reply ticket. Acknowledge
acceptance explicitly; deliver the result through the normal guarded send before
finishing the wait with that exact submission receipt. Cancel abandoned waits.
Never infer acknowledgement or completion from prose, Beads status, a native Stop
or peer idleness. Preserve exact waiter identity, native work/attention priority,
bounded previews and paged journals. Check the selected installed CLI's help
marker before using `wait` on an older peer. See `docs/agent-coordination.md`.

Retire idle sub-sessions you started and the owner does not use when their result is accepted or they have been idle 24 hours, provided they have no planned task. See `hgs guide stop`.

Use non-interactive filesystem commands and `ssh/scp -o BatchMode=yes` in automation.
On Windows, reach `ssh.exe` only through `hgs.exe`: Windows OpenSSH hangs after
using a synchronous pipe, so scripts and tests must not pipe it directly.
Use synthetic fixture paths, reserved example domains and local/private configuration
for deployment values. Never commit login material, runtime data or personal audits.
All Beads data on the repository remote may eventually be public, including history.
Git and Beads attribution may use the team's work addresses; the source privacy
guard still rejects addresses elsewhere, including Beads free text. Use a GitHub
no-reply address instead if email privacy matters.

## Validation and handoff

The owner evaluates features through local main builds. After a short review and
ordinary merge of a trusted internal PR, promptly build main's CLI and GUI on the
owner's Mac and update through the maintained installers. Do not wait for hosted
CI, regression completion or public Dev publication. Use a clean, persistent Git
checkout and incremental build caches, disable test targets for this build, and
use the exact main push's CI sequence as the Dev number; the run need only exist.
Retain local signing, source identity and the updater's draft/native-process
checks. Monitor CI separately and track failures as follow-up work. This local
evaluation loop does not authorize unrelated production relay changes.

Session counts can reach tens of thousands or more. Histories, audit journals,
logs and administrative views must use bounded pages, indexed queries and
background I/O; do not load complete journals or grow per-session GUI caches
without a bound. Keep pagination stable while new records arrive.

Automatic tool approvals are a separate, dangerous desktop opt-in, off by
default. Grant only a verified one-time native permission, reserve its private
audit record durably before delivery, and retain exact run/conversation/question
identity. Unknown delivery never authorizes replay. Ordinary questions, startup
trust and broader persistent permission changes remain manual: the person
answers them, or an agent does at the person's request, a standing one included.
An agent may turn this opt-in on at the person's request.

A team's standing confirmations are a separate bounded exception with their
own audit categories: the startup folder trust prompt of a member whose
worktree Zerus created for that team, and the resume of a member whose agent
exited by itself, at most three times an hour. Trust covers the team repository's
main checkout and its subfolders on the member's account; its audit row names
that scope. These confirmations never widen automatic tool approvals. Session
and task command effects use this computer's own `hgs` as one original child,
admitted at its spawn; a merge is admitted at its ref update. Closed-task key
maintenance uses bounded replica writes without adding history. An unknown
delivery is never replayed. Quitting
a member's agent by hand stops neither the member nor the team. Zerus never
stops a team's agents by itself.

Dev publication is build-gated: successful distributables, package/source identity,
signatures and installation checks admit Dev without waiting for the full regression
matrix. Main regression tests run separately and keep failures visible; file and fix
them as follow-up work without blocking an available Dev. Internal same-repository
PRs need no additional manual trust approval for ordinary Dev integration.
Main regressions cover shared Rust/protocol contracts, Linux CLI, Arch, macOS and
Android. PRs add Windows/iOS for affected desktop, native or shared paths;
unknown change inventories retain both. Nightly and Stable retain the full matrix.
Shared candidate tests may run once in a same-SHA qualification job when final
sealing requires its successful result. Do not start a second full matrix for a
SHA already under full verification; check the resolved source and wait, or select
missing platforms. A PR run does not replace the release's own required gate.
Keep the iOS marketing version for ordinary Dev uploads; change it only when a new
App Store/marketing version is actually needed, never just to publish another build.
iOS TestFlight Dev uploads follow successful Nightly verification, using the exact
source's main CI sequence. Manual checks remain available. Public
standard GitHub runners are free; do not introduce paid larger runners or raise
the 10 GiB cache limit without owner authorization. Authenticated model tests stay
opt-in. Validate workflow edits locally before pushing. See `docs/ci-and-aur.md`.
Native terminal/lifecycle changes need isolated real-agent contract coverage for
the latest available release of each supported agent. Ordinary CI resolves one
official version and package integrity per agent before native execution; it
never adds historical versions or falls back to them on discovery failure.
Keep terminal sizes, animation modes and native method coverage. The separate
upstream report captures current releases and capabilities without updating live agents. Review
capabilities before adopting them; a skipped native suite is not compatibility
evidence. See the native compatibility section in `docs/ci-and-aur.md`.
External fork workflows require maintainer approval for every external contributor.
PR/manual checks inherit read-only cache tokens; only exact upstream main pushes
and scheduled checks may write caches. Publish jobs have no cache access. Keep
action SHA pins enforced and never execute PR code with publication credentials
or on development-machine runners. Review workflow changes before approving runs.
Dependency proposals are monthly and grouped, with one ordinary PR per ecosystem.
Cargo proposals update only the lockfile within reviewed manifest constraints;
review manifest migrations explicitly. Zerus tracks the newest stable Rust: raise
`rust-version` and the pinned CI toolchain to each stable release.
Keep security alerts and security updates enabled separately. Never auto-merge.
AUR packages are `zerus` (stable source), `zerus-git` (upstream main),
`zerus-ade-bin` (stable binary) and `zerus-ade-nightly-bin` (verified nightly binary),
with x86_64 support. Stable fixes increment patch; Dev builds use the next planned
stable base from `DEV_VERSION` (Android: `mobile/android/DEV_VERSION`) plus the
growing verified CI sequence, `next-version-dev.N`. Keep installed build identity
independent of selected channels and retain all published asset versions. See the
version rule in `docs/public-distribution.md`.
Stable publication remains owner-triggered. Daily nightlies
publish automatically only after full CI and package checks succeed. Publication
credentials remain confined to trusted main in the release environment, never PR
code. Nightly tags/assets are immutable and never become the latest stable release.
Retry publishers with the same verified candidate; never rebuild or overwrite its
assets. Pure VCS version bumps do not create AUR commits. Stable releases after
0.42.2 and nightly prereleases also carry the tested Windows x86_64 portable zip
unchanged; it stays GitHub-only (not mirrored or in the v1 feed). Dev builds have
no Windows asset. `publish.yml` proposes each stable zip to winget as `ufna.Zerus`;
its manifest relies on the zip's `zerus-windows-x86_64/hgs-tray.exe` layout.
The owner has already personally agreed to the Microsoft CLA for every
`ufna.Zerus` proposal opened by `ufna` in `microsoft/winget-pkgs`. When its bot
requests consent, post `@microsoft-github-policy-service agree` once after
checking the author's existing replies; do not ask the owner again. This is
personal consent, with no company or employer representation. The winget
publisher handles this automatically; `winget-cla.yml` reconciles delayed
prompts from trusted main. Ordinary author confirmations on external PRs opened
from the owner's account are already authorized; substantive package questions
still go to the owner. Moderator review and actual winget availability are
separate from CLA consent.
The Homebrew casks `zerus` (stable), `zerus@nightly` and `zerus@dev` (tap
`ufna/homebrew-zerus`) ship signed, notarized Apple Silicon `Zerus.app` once
`MACOS_CASKS_ENABLED` is set. Dev also requires `DEV_UPDATES_ENABLED`, follows
successful main CI and uses the existing bounded mirror; it creates no GitHub
Release per push. Publish its cask only after verifying the exact public catalog
and signed ZIP/DMG hashes against that CI candidate. Apple credentials stay in the main-only
`macos-signing` environment and the tap deploy key in `release`. Casks never quit
the GUI, unload LaunchAgents or write to user homes; `zerus-setup` stays explicit.
See `docs/macos-distribution.md`.

Run checks appropriate to the change; see `CONTRIBUTING.md`. Update Beads status
and export with `bd export -o .beads/issues.jsonl` before staging tracker changes.
Inspect `git status` and the staged diff. Commit, push and synchronize only when
explicitly authorized; the default workflow is conservative.
Mirror substantive instruction changes in `AGENTS.md` and `CLAUDE.md`.
