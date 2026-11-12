# Explicit Windows desktop service mode

The maintained user task is passive on a fresh installation. Ordinary
`hgs desktop registration install --json` refreshes an owned registration
without starting or stopping it and preserves its existing passive/background
selection. Read-only `registration --json --background` only renders a plan.
An explicit selection that already matches the verified running task is a
no-op, for both modes. In particular, selecting an already running passive
task preserves its unused Service grant and original process.

An explicit change uses the selected installed CLI:

```powershell
hgs desktop registration install --mode background --json
hgs desktop registration install --mode passive --json
hgs desktop registration status --json
```

The CLI advertises `Desktop registration mode ABI: desktop-registration-mode-v1`.
The JSON receipt separates `selected_mode` from `running_mode` and reports
`ok`, `outcome`, `request_id` and `pending_transition`. A pending or unknown
result never establishes that a manager operation did not happen: `changed`
and `manager_changed` are null when the original outcome is unknown. Inspect
the original UUID rather than issuing another logical action.

## Caller and ownership

The caller must be outside an agent tree and have either the actual selected
installed frontend as a retained ancestor, or an already attached, visible,
foreground classic console with a verified current console process list.
Windows Terminal/ConPTY cannot supply this classic-console proof; use the
Zerus Next Settings route. No console is attached, allocated or brought to
the foreground to manufacture admission. Settings callsite integration and
installed acceptance are separate from this native helper.

On Windows the installed frontend is one of three images:
- `%LOCALAPPDATA%\Programs\Zerus\hgs-tray.exe` from the per-user installer;
- an `hgs-tray.exe` beside the checking CLI;
- the portable zip's own `hgs-tray.exe`, bound to the stable CLI by that
  app's own staging.

The binding covers the documented portable route. Once local sessions are on,
the zip's app relays through the stable CLI staged from that zip.
- **Creation.** When the app starts `hgs session-host install` beside it, the
  install records `frontend.json` in the host images folder. It does so only
  when its direct parent is the app beside its own loaded image and no
  ancestor is an agent. One deadline covers the whole proof. Under the
  install lock, after the CLI, the image and the marker are complete, the
  install pins the app's loaded image and the staged CLI (deny-write and
  deny-delete, ancestors included) and reads the app's SHA-256 through that
  pin. Immediately before publishing, it checks the same ancestry, parent and
  pins again against that deadline. It then records both exact objects and
  the digest.
- **No other writer.** Every install removes the old binding first, so a
  failure leaves none. Any other install records nothing: from a terminal,
  from an agent, or with a copied CLI.
- **The record.** `frontend.json` and each folder from `.local` down to the
  images folder must be private to this user: owner, access, a single link,
  and no link or junction. They are held while the record is published or
  read. An unsafe record or folder is refused, never repaired.
- **The CLI.** A process that admits frontends establishes once, from its own
  loaded image, that it is the bound CLI. It holds no pin on it afterwards,
  so a newer zip can still restage the stable CLI while it runs.
- **A running app.** A frontend request or a Settings mode change from the
  app is proven for that exact process: its own handle and birth, and its
  loaded image pinned. The image must be the recorded object, and its digest
  is read through that pin within the request's original deadline, followed
  by the final process and ancestry checks. The proof travels with the
  request and is checked again where the request takes effect. A scope keeps
  at most four such proofs and drops each one as soon as its process exits.
  Requests still in flight keep their own pin until they finish.
- **A notification launch.** With the UI closed, the launch pins the
  recorded app, reads its digest through that pin, and keeps that pin
  through the launch and its checks until release. A proof from a running
  app never authorizes such a launch.
- **Refusals.** A malformed, oversized, linked, foreign or stale record
  admits nothing. Neither does a replaced app or CLI, or an edit that keeps
  the app's length and write time.
- **Rebinding.** The app stages again at start when the binding does not
  name it, no longer names the staged CLI, or names its path while that path
  now holds another file or other bytes. `session-host status` reports these
  facts as `cli_current` and `frontend_current`. So a moved, newly extracted
  or same-path replaced copy binds itself again. Same or newer version only,
  never a downgrade, and an unchanged CLI is not copied again.
- **Where the record is read.** A process that admits frontends finds the
  record beside the CLI it is running, never through `HOME`, `USERPROFILE`
  or another setting. Only when its own loaded image is exactly
  `<dir>\.local\bin\hgs.exe` does it read
  `<dir>\.local\share\hgs\host\frontend.json`, where the binding install
  writes it. A service started by Task Scheduler without `HOME` therefore
  reads the same record as the app's install. A CLI anywhere else, such as
  the installer folder or a custom path, admits no portable app.
- **Visibility.** A process that admits frontends records its own CLI when it
  starts. Every admitting request, with a GUI running or a notification
  launch, reads and checks the current private record first, with a read of
  its own; these reads run one at a time. A cached proof is reused only when
  that record names exactly its app. A failed or unsafe read refuses the
  request, even when a proof is cached, and never falls back to the record
  the process read earlier. A running service
  therefore admits a rebinding at once while the stable CLI is unchanged, for
  example a re-extracted or same-path replaced app; proofs made against the
  older record no longer admit anything once it has changed. A record that
  names another CLI, as after an upgrade, is refused until the service starts
  again on that CLI. No service or manager is restarted automatically.

As with the common frontend boundary, this prevents unintended agent descent.
Deliberate same-user parent-process spoofing is outside its scope. Caller
identities, images, ancestry and console facts are rechecked before effects.
Decoded status, SID, session number and a mode-check result confer no consent.

First install the passive registration and start its passive actor. The
authenticated frontend then submits its explicit Service Grant/Rearm against
that actor. Only a current stored Service grant for the actual selected
configuration and installed identity permits the background selection. Starting
with `--background`, refreshing a task or reading status cannot create or clear
that grant. A retained revoke always requires a fresh explicit frontend Rearm.

## Refresh source prerequisites

The Windows actor adds a bounded read-only retirement observation to its
`status` response under `executable_upgrade.refresh`, with ABI
`desktop-retirement-drain-v1`. It reports runtime, original serve-owner loan,
connection and durable operation drain independently. The status connection
itself accounts for one connection; another partial or active pipe prevents a
complete drain observation. This data neither releases the owner nor grants
permission to stop or start anything. An original-instance controller must also
retain and verify its native task/process/pipe, caller and handoff exclusion.
An older actor without this observation cannot establish these prerequisites.

The private registration Grant guard captures the checked database object and
its actual configuration, journal, owner epoch and frontend origin. It reads
again at effect boundaries and rejects a revoke, Rearm, replacement origin or
database, or a stale frontend image. A same-origin Grant reconnect that keeps
the ownership incarnation is allowed. This guard cannot be reconstructed from
status or a receipt, cannot Grant/Rearm, and is not runtime launch authority.

The retained native task now pins its loaded image with the shared executable
pinning code and the kernel's `ProcessImageFileMapping` check, using the SAME
held process handle. The selected replacement and the loaded old file are kept
separately, including their normalized ancestor pins. If a stable-CLI update
moved the old executable aside, discovery inspects only maintained
`hgs.exe.old-<seconds>-<random>` names in the pinned selected directory, with at
most 256 directory entries and the existing phase deadline. A pathname, an
alias or identical bytes do not prove the mapping. Unavailable evidence or an
exhausted directory/deadline bound refuses without selecting another process.
These pins last through the original native consumer's checks and release.

These are source prerequisites for an explicit refresh control, not that control
itself. Ordinary same-mode selection still does nothing. Native Windows PC
qualification remains outstanding; portable tests and Linux actor models do not
qualify the Task Scheduler or installed Settings path.

## Explicit original-instance refresh controller

`desktop-registration-refresh-v1` adds a distinct same-mode refresh for an
already retiring Windows service whose selected CLI was replaced. Ordinary
same-mode selection and `registration install --json` keep their existing
behavior. This operation never toggles mode, updates the unchanged definition,
reinstalls, revokes, Rearms or creates a Grant. Installed PC qualification
remains outstanding.

The Service consumer now offers **Settings > About > Service refresh required >
Refresh background service…** when a bounded native read proves the original
manual-refresh service and its current identity-current Service Grant. This
opens a separate native confirmation showing the original epoch and
configuration. The renderer sends only the displayed selection nonce; the
full target, including file and process birth identities, stays in native
memory. The selected helper, current target and immutable consumer binding
are checked before and after the dialog and immediately before spawn.

The operation uses one original request UUID. A lost reply, **Check result**,
or a reopened GUI with a durable pending refresh only observes that UUID;
none replays Stop or RunEx. **Check service status**, Retry, Resume and automatic
reconnect never invoke refresh. A possible start with missing or unavailable
original evidence remains unconfirmed and cannot offer a new operation.

After an exact settled receipt, this window keeps its immutable original
service binding and stays read-only. Close and reopen Zerus to authenticate the
refreshed endpoint through the ordinary startup path. The refresh receipt does
not grant business eligibility, clear review/default latches, Rearm, or schedule
imports. Revoked/blocked/changed-Origin refresh and default-mode native takeover
remain unsupported (zerus-5wmh.66.3.40.3.4). The availability guidance preserves
the selected mode and original request instead of offering a reinstall or mode
toggle. Native installed-Settings and Fast Startup/Restart qualification remain
outstanding; portable/Linux dialog and transport models are source evidence.

The closed command forms are:

```text
hgs desktop registration refresh-info --json
hgs desktop registration refresh --json --request-id NEW_UUID --scope TARGET_JSON
hgs desktop registration refresh-result --json --request-id SAME_UUID
hgs desktop registration cancel-refresh --json --request-id SAME_UUID
```

`TARGET_JSON` is the bounded `target` object from `refresh-info`, retained
without changing its original/configuration/selection/image/Grant fields. It
is selection data, not permission. The operation captures its own actual native
Caller and checked current Service Grant, and requires the same original COM
instance, process birth, pipe epoch/generation, images and owned definition.
A changed target refuses before a new reservation. The caller is the existing
genuine installed/portable frontend or the maintained foreground classic-console
route; JSON roles and Windows Terminal/ConPTY do not replace that proof.

The primary mode journal holds the distinct refresh scope and original UUID in
its single pending slot. Before any manager effect it reserves bytes for the
largest completion, review barrier and archive. A separate private anchor outside
the state loss domain keeps a missing/replaced journal from becoming a fresh
request. An unavailable anchor, an overlapping layout, an eight-archive bound
or the shared 8 KiB record limit refuses. Keep both records and the selected
mode; inspect the original result. There is no maintained history rollover or
loss-reset operation.

The controller keeps the original Caller, Grant/database/Origin, both image
pins and handoff exclusion. Runtime, owner loans, pipe connections and durable
operations must drain on the authenticated original endpoint before Stop.
The status observer reads through real EOF. The original selection is checked
before StopOffered, at the writer's entry/pre-persist/post-persist callbacks and
again before the original COM Stop; the existing Stop deadline is never renewed.
Only physical exit and original GUID disappearance permit one RunEx of the
unchanged selected definition. Its first native process/COM objects, exact
loaded replacement and endpoint stay retained through readiness and final save.

An existing UUID is result-only, including Prepared, Held and lost replies. No
result read adopts a runtime, repeats an effect, or converts readiness into old
completion. A pre-manager cancellation changes only that original's metadata,
with the existing native caller/configuration/passive-Grant checks. After an
effect was offered, uncertainty stays Unknown and uses the original Review/boot
barrier; no phase is downgraded. Refresh receipts neither rewrite historical
mode-choice receipts nor open the consumer's business or scheduling gates.

A failed final fence, directory sync or reply can occur after the settled
refresh receipt is published. Refresh error recovery reads that SAME UUID and
the independent loss evidence: it returns the exact read-only receipt or
Unknown, with uncertain effect fields. An empty pending slot never proves that
no Stop/RunEx occurred. A bounded timing trace records phase/operation wall and
monotonic timestamps, remaining budget and retained-process exit observations;
these diagnostics are never admission or cleanup evidence. Exit polling
validates the current owned definition once and uses that same checked task
for GUID disappearance, under the original Stop deadline.

Refresh allocates the ordinary two-second Stop allowance plus its unused
two-second Update allowance to ONE four-second Stop deadline, captured once
at Stop entry. It performs no Update or Rollback phase and cannot spend those
allowances again. Preflight 2 + Hold 3 + Stop 4 + Start 2 + Readiness 8 + Release 2
is 21 seconds, plus the existing three-second exchange/cleanup allowance, inside
the unchanged 30-second original-helper/runner bound. A declared overall
deadline begins at helper entry; every phase is clipped to it. Checks, debug
pause, physical-exit/GUID polls and Stopped publication all spend the same Stop
deadline. Expiry never renews it or authorizes another start. Ordinary mode,
first-choice and post-review choice budgets are unchanged.

This first controller supports a current, identity-current Service Grant only.
Revoked, blocked, changed-origin and unsupported actors are explicitly refused;
refresh cannot obtain a Grant through retirement. In particular, it does not
fix the revoked-service return-to-Next case in either GUI mode, including the
default native-takeover path (zerus-5wmh.66.3.40.3.4). An IPC-only blocked-state
transition needs its own native admission/startup proof and G1 review. Native
Task Scheduler/Caller tests on a disposable runner, portable transition models
and Linux actor controls remain separate from installed Settings and the
owner-gated Fast Startup/Restart qualification.

## Original task and rollback custody

Each transition checks the actual owned Task Scheduler definition, principal,
owner/DACL and current interactive session. A running instance is retained as
its original `IRunningTask`, immutable instance GUID and independently verified
desktop process handle/birth. The service process is authenticated through its
original named pipe, exact configuration/argv, executable file object and
lifecycle epoch. `EnginePID` alone is insufficient. A separate/shared engine
that cannot be bound to that exact service process refuses before stopping it.
The native Windows round-trip fixture must qualify the supported SDK topology.
Outgoing controller connections verify the kernel pipe server PID and its
actual user token, then join it to that same retained process. Incoming actor
connections keep their client impersonation and integrity checks. These are
separate endpoint roles; a failed check never falls back to the opposite role
or the checking process's own PID.

The maintained task is hidden. Instance discovery uses the same retained local
`ITaskService::GetRunningTasks(TASK_ENUM_HIDDEN)` connection, after validating
the registered definition, SID, ACL and exact root task path. It reads at most
256 entries under the original phase deadline and keeps only that exact path;
duplicates, unknown paths/counts, COM errors or expiry refuse. Other known task
paths are ignored and confer no authority. `GetInstances` continues to reserve
flags zero; no undocumented flags are used. Microsoft provides the
[hidden-aware enumeration example](https://learn.microsoft.com/en-us/windows/win32/taskschd/displaying-task-names-and-state--c---).

Discovery is security-context filtered. An empty scan never proves that the
original process exited or resources drained: the held-process physical exit,
task activity and endpoint-owner checks still apply. A running/queued task with
no visible original cannot authorize another start. Recovery still joins the
same recorded GUID to full native process/pipe readiness, then completes only
the original UUID. It does not repeat Stop, update, RunEx or revoke.

A registered CLI that refuses startup with exit code 78 retains its last bounded
error in the existing private desktop state's `startup-failure.json`, when that
state is safe and writable. The record includes configuration, PID/birth and the
original guard reason; it creates no state folders, changes no grant and never
retries startup. It is historical diagnostic data, never current readiness,
task-instance identity, authority or proof that resources drained. An unavailable
diagnostic leaves the original stderr and failure exit unchanged. Task Scheduler's
last-run result and this private record can explain an early exit after RunEx;
an absent running instance never permits issuing RunEx again.
The admitted mode helper reserves that diagnostic file before offering a start.
The scheduled process can write through the same checked user-owned object even
if its token chooses a different owner for newly created files. Failure data
includes the token's user/default-owner SIDs; the private fixture also reads
bounded owner/DACL metadata for its four known state entries. These observations
do not change token defaults, repair file ownership or relax a privacy check.
An untouched `reserved` record means no failure text was safely recorded.

The native task probe confirmed that an elevated scheduled process can have
the correct User SID while its default creation owner is
`BUILTIN\Administrators`. Before creating private state or starting workers,
the dedicated desktop service selects its actual primary token's User SID as
that token's default owner. The dedicated mode worker does the same only after
capturing and checking its native caller, and checks that caller again before
continuing. This covers SQLite's internal WAL, SHM and rollback-journal files
as well as locks and atomic replacement files. The operation has no PID or SID
argument, refuses impersonation, and rechecks the original primary token and
the selected owner. It changes only `TokenOwner`: user, privileges, groups,
integrity, session and the default DACL remain unchanged. Windows documents
this creation rule in [Owner of a New Object](https://learn.microsoft.com/en-us/windows/win32/secauthz/owner-of-a-new-object).

The existing exact-user, ACL, no-reparse and single-link checks still run.
Inherited ACLs must still satisfy the existing private-state guard; the creation
step does not repair a directory, rewrite any ACL, accept an Administrators-owned
file or replace one. A previously refused file remains refused. No task, grant,
revoke receipt or uncertain manager effect is replayed to repair that state.

Passive rollback saves one UUID before offering the original rollback revoke.
Only a known committed prepared command can reconcile its original drain with
that same UUID and original process. An absent or uncertain command is never
resent. After the settled receipt, the original selected `revoke-hold` child
physically excludes Rearm/native takeover throughout the task transition.
Its original stdin writer is not inherited by another child.

The current service epoch and an optional captured notification-writer epoch
are separate identities. If notification admission has not captured a writer,
the actor resolves only the canonical durable writer under its original
handoff exclusion, after checking the durable writer's service binding against
that actor. A captured writer must still match exactly. The service epoch is
never substituted for an absent writer; genuine no-writer cases retain the
existing null/marker protocol. Notification admission publishes its captured
epoch only after ledger authorization commits.

An existing command and receipt keep their original UUID and writer target,
including a receipt recorded incorrectly by an older build. They are never
rewritten to match a later writer. The hold continues to require exact receipt
equality. A refused readiness frame reports a closed decline code, or a static
receipt field name when a ready frame differs; it does not expose child text
or treat a diagnostic as authority.

Only the retained instance's `IRunningTask::Stop` is called. The helper requires
the same service process handle to signal actual exit and the original instance
GUID to disappear. It never stops all instances, selects a process by name or
uses a failed identity check as evidence of exit. Replacement is UPDATE-only,
preserves verified security, and ignores registration triggers. A single
`RunEx` explicitly selects the verified current session and marks its user
argument as the verified SID (`TASK_RUN_USE_SESSION_ID | TASK_RUN_USER_SID`). Its successful return
is insufficient: the same returned instance must reach exact service
process/pipe readiness.

The rollback hold remains alive through passive readiness. Completion requires
original stdin EOF, the matching Released frame, successful original child
exit, and both original pipe EOFs. Only then may the pending record be cleared.
Background enablement instead retains the actual handoff exclusion and checks
the stored grant plus operation-resource drain; the runtime rechecks authority
after that exclusion is released.

## Interrupted transitions

The private bounded record stores the original UUID, phase and observed instance
identities. It is metadata, never permission to stop or adopt a process.
Each Stop/Update/Start is recorded as offered before its native call. Unknown
manager effects stay pending; repeating the choice never replays them. A
before-manager interruption may resume only with the same original runtime,
UUID and native rollback exclusion. Background and initial passive starts
acquire their actual handoff exclusion and complete drain checks before saving
a new record, so transient refusal leaves no pending intent.

Repeating the same choice at `start_offered` or `ready` is observe-only. The
recorded original started GUID must bind to the exact owned task and a ready
service in the requested mode. Passive recovery also reacquires the same
UUID's read-only revoke hold, or proves an untouched, drained initial passive
bootstrap under the handoff exclusion. Any reacquired hold still requires its
matching release frame, original exit and both pipe EOFs. Completion changes
only installer metadata: no Stop, Update, RunEx or revoke is issued. A missing
GUID, changed definition, unready original instance or unverifiable exclusion
reports its exact unknown phase/reason; another instance is never adopted.

The phase budgets are independent: preflight 2 s, rollback 6 s, hold acquisition
3 s, Stop 2 s, update 2 s, start 2 s, readiness 8 s and release 2 s. Readiness
gets its full budget after earlier phases; it does not spend the drain's
remaining time. The original child exchange remains bounded by the shared
runner's existing 30 s maximum. Its timeout still leaves uncertainty pending
for observation, never permission to replay a manager call.

A refusal within Stop also includes `stop_timing` diagnostics
(`desktop-mode-stop-timing-v1`): the unchanged budget, elapsed milliseconds,
and fixed counters/times for native rechecks, metadata synchronization, the
instance Stop call and physical-exit confirmation. Nested rechecks are charged
to their own step, without double counting. Elapsed values cap at 60,000 ms;
unattributed time is explicit. These fields never select a deadline or change
the refusal wording, pending phase, authority or replay rules. The mode path
has no separate failure audit; the refusal detail carries this bounded snapshot.

If a fresh frontend Rearm won before hold acquisition, explicitly cancel only
the obsolete saved rollback metadata:

```powershell
hgs desktop registration cancel-rollback --json --request-id UUID
```

Before publishing `StopOffered`, the helper rechecks the same retained original
instance, task definition/owner/ACL and authenticated process/pipe under the
registration lock. A refusal proven before publication stays at `Held` and can
use the explicit cancellation above. The original Stop deadline includes these
reads and any debug hold; it is not renewed. A post-persist check failure,
directory-sync uncertainty or lost save reply remains `StopOffered`/Unknown.
The immediate selection and original-instance checks before COM Stop remain.
Historical offered records are never downgraded. Status, Retry and reconnect
do not cancel, and Settings currently has no Held-cancellation action.

Cancellation checks the original pending UUID and native caller twice. It can
discard any pre-manager record for either mode, including stale background
preparation after a restart. Passive rollback additionally requires a current
Service grant twice; background cancellation requires no grant. It changes no
task, grant or revoke receipt and does not contact Task Scheduler. It also covers
a saved rollback that never took effect.

For a `start_offered` or `ready` record, the same explicit cancellation instead
uses read-only Task Scheduler and native service observations. It requires the
recorded started GUID to bind to the exact owned definition, retained service
process/pipe and requested running mode, with fresh readiness and caller checks
through the metadata save. Other offered phases, a missing GUID, changed task or
unready original instance remain pending. This branch reports
`observed_start_cancelled`, preserves the previous completed UUID, and records
the cancelled UUID. It performs no Stop, Update, RunEx, revoke or hold acquisition.

This allows a completed passive start whose reply was lost to be abandoned after
a later Rearm or revoke prevents the old rollback hold from being reacquired.
The later authority is left untouched; this post-readiness metadata cancellation
requires no current Service grant and confers no authority. A later separately
confirmed choice receives a new UUID; there is no automatic UUID rotation or
unknown-effect replay.

## Review an interrupted mode change

Use a separate explicit review in Zerus Settings when available, or the updated
maintained CLI from an admitted foreground classic console outside Zerus:

```powershell
hgs desktop registration review-mode --json --request-id UUID
```

Use the original `pending_transition.request_id` from `registration status`.
Review cannot choose a mode. It performs no Stop, Update, RunEx, Grant, Rearm,
revoke or rollback-hold acquisition. It does not consume a later instance or
release an original operation, Owner, child or pipe charge. Retry, status and
automatic reconnect never perform it.

New attempts retain consumer PID/birth from the actual admitted native Caller,
plus native machine and kernel boot identity, before effects. Ambiguous records
from the current boot require an explicit reboot barrier. The first Review
binds that barrier to the original UUID and full immutable record binding,
machine and kernel boot; it returns `reboot_required` without acknowledgement.
**Restart the computer, then use Review interrupted mode change again.**
Use Windows **Restart**, not Shut down followed by power-on: Fast Startup can
keep the previous kernel session. If the kernel boot identity is unchanged,
Review still refuses acknowledgement. Confirming this distinction against
the actual BootId on a Fast Startup PC remains part of native qualification.
A genuinely different kernel boot on that same machine proves process death
only. Wall-clock age, file dates, a free lock and a missing direct child never
substitute for this proof. Native boot observation is read from the kernel's
loader boot sequence, not a registry counter or timestamp.

One narrow legacy `stop_offered` case can be reviewed without reboot: under the
exclusive registration lock and unchanged owned task definition, the recorded
old service PID/birth must be physically gone and its exact original GUID absent.
A live/reused/unreadable PID, queued/unknown activity or a filtered empty active
inventory supplies no such proof. An independently started later instance is
left untouched and lends no identity or authority to the old request.

Successful review archives the original phase, UUID and binding with a separate
`reviewed_unknown` disposition. The manager outcome remains **Unknown**; review
never claims that Stop succeeded. Status exposes the latest `reviewed_transition`
and keeps `activation_automatic_blocked` latched. A later mode choice is a second
explicit confirmation with a new request and complete fresh native caller,
current Grant, manager, task, endpoint and resource preflight. Unknown or queued
activity and unconfirmed cleanup must still refuse that choice.

The old switch route stays blocked after any review, even with a valid Grant.
For a separately explicit foreground choice, name the reviewed original and
generate a new request before invoking the maintained helper:

```powershell
$freshModeRequest = [guid]::NewGuid().ToString()
hgs desktop registration install --mode background --json --after-review UUID --request-id $freshModeRequest
```

Select `passive` instead when that is the separately chosen mode. These UUIDs
are routing fences, not consent or native authority. Settings must show its
second native confirmation first. Keep the new original UUID after a timeout;
only its read-only observation is permitted, never another generated UUID.

```powershell
hgs desktop registration mode-result --json --request-id $freshModeRequest
```

This lookup reads the bounded private receipt for that original UUID. It never
checks or adopts a current instance, retries an effect or infers readiness.
An absent receipt remains Unknown. After a new explicit choice was offered,
keep that UUID until its original physical cleanup is confirmed; a timeout or
missing receipt alone never permits another choice. Completed, cancelled and
same-mode no-op request IDs remain reserved after later mode changes. The
receipt ledger has a ceiling of 48 IDs and never evicts an old ID to permit
replay. Actual capacity is lower when archives or longer selected commands use
the shared 8 KiB record. Admission reserves the worst-case consumer, original
start identity, offered phase, reboot barrier and reviewed archive bytes before
any effect, so the last admitted transition can still record both reviews.

The original archive retains at most eight entries within the existing 8 KiB
mode-record bound, with no eviction or silent reset. Exhaustion refuses a new
intent before effects. Missing, inconsistent or changed original evidence fails
closed. Exhaustion leaves the current task and selected mode untouched, with
no new pending intent. Status and original `mode-result` lookups remain usable.
There is currently **no maintained history rollover or reset route**. Ordinary
registration install/remove does not clear these retained identities and is
not an exhaustion workaround. Keep the original record and current mode and
request a maintained migration before another mode change; do not delete the
record, discard an archive or rotate a pending UUID. The dedicated capability marker is
`Desktop registration mode review ABI: desktop-registration-mode-review-v1`.
Native PC and installed Settings qualification remain outstanding.

## First explicit background choice

A plain passive registration has one more explicit form, for an in-app
Settings choice (zerus-yfbd.1):

```powershell
$firstRequest = [guid]::NewGuid().ToString()
hgs desktop registration install --mode background --json --request-id $firstRequest
hgs desktop registration mode-result --json --request-id $firstRequest
```

The CLI advertises
`Desktop registration first mode choice ABI: desktop-registration-mode-first-choice-v1`.
The form admits only background, from a selected **and** actually running
passive task. It is refused while any original transition is pending,
interrupted, reviewed or archived: that history keeps its own Review route,
whatever the caller asks for. The caller, current Service Grant, original
instance, endpoint and selected image are checked as for any switch, again at
every effect boundary; no automatic, default or passive request uses this form.

The caller's UUID is reserved durably as the transition's original ID before
the first manager effect, with byte headroom for its completion scope, a later
review barrier and archive. Its receipt is distinct from a reviewed choice and
names no reviewed original. A repeated UUID only reads the original result; it
never stops, updates or starts again. A lost reply after a possible start stays
Unknown and is looked up by that same UUID. Only a closed not-started boundary
(an explicit pre-manager cancellation) records a refusal and allows a fresh
choice. An interrupted first choice records consumer and boot evidence like any
transition and takes the ordinary Review route above.

The latest reserved first choice is also kept in an independent anchor,
`<HGS_CONFIG_DIR>/windows-first-choice-<configuration>.json`, written durably
before the mode record that reserves it. The anchor lives in the config tree,
outside the desktop state folder that holds the mode record and the Service
Grant. Every mode status, operation and result cross-checks the two: when the
record no longer shows the anchored choice (missing, emptied or replaced
journal, or a wiped state folder), has first-choice history without its anchor,
or names another latest choice, the first choice is **lost**, never "never
written". Status reports `first_choice_lost` and latches automatic activation,
every mode operation refuses without a new ID, and `mode-result` stays Unknown.
There is no maintained clear or reset for a lost first choice; keep both files
and follow up manually. Registration install and remove keep the anchor exactly
as they keep the mode record. Only wiping both the config and the state trees
looks like a fresh profile.

The anchor proves a loss only from outside that loss domain. The CLI, before
it captures the caller or spawns the helper, and the mode helper again, resolve
the config folder, the state folder and its desktop folder natively (junctions,
symlinks, short names and drive aliases of existing folders) and compare them
by path components with Windows case folding, never by string prefix. When the
config folder equals or lies inside either one, status reports
`first_choice_unavailable: "anchor_layout"`, the client offers no first choice,
and the first-choice form is refused before any anchor, record reservation, UUID
or manager effect. Nothing repairs or moves such a layout; the ordinary and
reviewed routes are unchanged.

Status and `mode-result` read the record and its anchor together under the
registration lock, which every writer holds from anchor publication through its
record save, so a reader never pairs one commit's anchor with another commit's
record. While a writer holds the lock, both refuse after the lock's bounded wait
rather than report a loss. A writer stopped between publishing its anchor and
saving its record (a crash or power loss in that window) leaves the anchor ahead
of the record. That is reported as lost: the files alone cannot tell it from a
wiped journal, so it fails closed. A lost lookup keeps the original pending
(Unknown) and never becomes NotStarted or a Resume. The client latches on a loss
only while its latest read reports it; a later coherent healthy read lifts that
latch, while the durable review latch stays.

## Private qualification

The Windows unit suite runs the creation-owner regression in one bounded child,
never by changing a parallel test runner's token. On an eligible elevated token
it reproduces the original Administrators-owned lock refusal, selects the actual
User owner, opens the real desktop Store and checks the resulting locks, database,
WAL, SHM, rollback journal and replacement file. It retains the bad original file
and verifies its owner and identity remain refused. Token groups, privileges,
integrity, session, default DACL and parent token remain unchanged, and an
impersonating thread refuses. Hosted CI must execute this control without an
eligibility skip. The task round-trip independently checks the actual scheduled
service's file owners in each mode.
The native gate also runs a separate-process pipe-role control: the client end
refuses server-only client impersonation, verifies the actual server PID, and
the server end independently authenticates the original child PID.

`tests/windows_frontend_auth.py` includes the disposable hosted-Windows task
round-trip. The maintained registration installer stages the stable CLI under
the test's private HOME, with the native test frontend beside that selected
image. A portable `HGS_EXECUTABLE` override never bypasses the Windows stable
path rule. USERPROFILE is retained only for the canonical native instance
namespace shared with the Scheduler-launched service; native caller/process
checks remain mandatory. The runner's actual profile files are never staged or
changed. The fixture refuses a pre-existing per-user task and runs only on
disposable hosted Windows VMs.

It uses private configuration/state, a real authenticated frontend Grant,
passive/background/passive transitions, unchanged runtime on ordinary refresh,
observe-only completion and explicit cancellation without manual record resets,
including Rearm or a later revoke after a lost passive-start reply and a
separately explicit new choice after read-only cancellation,
and a retained unrelated child. Cleanup targets only the exact verified fixture
instance and checks its real process birth and exit; changed-birth cleanup
refuses. Only then are its private images/state removed. A failure preserves
uncertainty and private diagnostics instead of killing by task name or
executable pattern. This suite authorizes no owner's installed task, GUI,
agents or sessions.

Microsoft API contracts: [RunEx](https://learn.microsoft.com/en-us/windows/win32/api/taskschd/nf-taskschd-iregisteredtask-runex),
[instance Stop](https://learn.microsoft.com/en-us/windows/win32/api/taskschd/nf-taskschd-irunningtask-stop)
and [EnginePID](https://learn.microsoft.com/en-us/windows/win32/api/taskschd/nf-taskschd-irunningtask-get_enginepid).


### Service Settings review

The Service consumer exposes **Settings > About > Review interrupted mode
change** for the displayed original UUID and configuration. Its native dialog
confirms metadata review only. A reboot barrier requires a real **Restart** and
another explicit Review. Once reviewed, background or passive selection needs
a SECOND native confirmation and one new retained choice UUID linked to that
review. Lost replies remain original-ID lookup only. Retry and ordinary Resume
cannot offer a mode choice after review.

A background choice records its retained actual registration Grant incarnation
and authenticated ready service epoch/generation. Only a fresh matching latest
original result plus this frontend's current native Grant can make existing
business work eligible. The permanent manager/default latch remains; current
readiness alone never settles the old Unknown. Older choice receipts without
this scope remain read-only and require another separately confirmed choice
with the updated maintained CLI. None of these observations authorize refresh,
Rearm, cleanup, or adoption of another task/process. Native dialogs, installed
Settings and Fast Startup/Restart behavior still require PC qualification.


## Eligibility after a settled reviewed background choice

A later Service consumer start or reconnect can restore business eligibility
without another mode dialog. It reads the exact durable new choice UUID and
reviewed original UUID, verifies that this is still the latest applicable
settled background choice, and obtains a separate current native ownership
observation. The old completion's service epoch and generation remain historical;
a restarted service must authenticate its current endpoint independently while
retaining the same Grant journal and owner epoch.

The closed `desktop-ownership-observation-v1` exchange compares the requesting
frontend's authenticated native Origin with the existing actor's granted row in
one read transaction. It checks the actual database object and current service
epoch and generation. A display status, receipt or supplied identity cannot
construct the native observation. An old helper without this read capability,
a different frontend Origin, a later passive or interrupted choice, revoke,
Rearm, replaced registration/image/database, cancellation or unavailable proof
leaves the consumer read-only. No service is installed or started to obtain
that proof. Read workers keep their original child, image and pipe resources
through physical settlement, including an uncertain or timed-out exchange.

Restoration changes eligibility only. It creates no Grant, Rearm, import or
manager operation, including from the first watch frame, Retry, backoff and
scheduler wakes. A separately authorized normal action still needs its existing
native admission. The original unknown result and permanent automatic mode
activation latch remain unchanged. The interrupted-change section disappears
only while both the applicable settled choice and current native observation
validate; a newer interruption brings its guidance back.

Portable/Linux transport controls and hosted Windows tests are source evidence.
Installed Settings and native PC qualification remain separate.
