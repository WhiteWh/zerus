# Desktop operation storage contract

G1 owns `zerus-desktop-ledger::operations` and the service's schema transaction,
separately from native notification `Kind` and attempt policy. G2 owns the pure
`operation_recovery::task_start` verifier and proposals. The storage APIs are
internal actor calls. There is no positive frontend operation IPC, executing
operation worker or execution admission in this slice. The service stays passive.
Imported state and a Complete marker cannot grant execution authority.

The actor must first verify G2's complete original record, exact frozen input,
executor identity, current producer drain, policy and current writer/service
lease. A future executing actor must repeat these checks before reservation and
spawn. Database epoch checks are an additional fence, not a substitute for this
runtime proof. Missing old input remains review-only: this API never recreates
wire bytes from `intent.plan`.

## Frozen original and attempt identities

Schema 7 adds two private data tables and maintained totals:

| Table | Durable data |
| --- | --- |
| `desktop_operations` | Insertion sequence; unique operation and admission IDs; kind/schema/configuration/project/task/original request; closed Key and Binding; exact source bytes and retained input; distinct source, UI, input, plan and consent hashes; stable executor machine/session/run/conversation; provenance; policy state/revision and bounded outcome |
| `desktop_operation_attempts` | Unique attempt UUID; original operation/revision; complete Ticket and its fingerprint, admission/binding IDs, writer/service epochs and lease; reserved/submitted/review/unknown outcome; independent drained flag |
| `desktop_operation_totals` | Transactionally maintained unsettled count and globally undrained count |

The only kind is `task_start_reconcile`, schema 1. Key and Binding mirror G2's
closed data schema without a dependency from the ledger back into the client.
The root actor revalidates G2 policy before translating them. Native notification
kinds and retry rules remain separate.

The operation ID is SHA-256 of `desktop-operation-key-v1` followed by NUL and
these UTF-8 fields in order: `task_start_reconcile`, `1`, configuration, project,
task, original request UUID. Each field has its u32 big-endian byte length
before its exact bytes. The input/plan hash is SHA-256 of the retained bytes.
Consent conservatively binds the complete preserved plan: SHA-256 of
`task-start-reconcile-consent-v1`, NUL, then those same input bytes. Raw source
and existing UI snapshot hashes remain distinct. Shared UTF-8 vectors live in
`client/fixtures/desktop-operation-port-v1.json`.

Frozen columns and tickets cannot be updated through SQL triggers. Terminal
attempt outcomes cannot change; resource drain cannot be undone. There is no
retention/deletion API, and deletion is refused by triggers. Pressure never
removes unknown or pending work. The existing private desktop database page
limit still applies; this is not a hard WAL-size or physical-disk qualification.

## Internal calls and bounds

`admit(Admission)` freezes one original and returns its Admitted identity.
An exact duplicate returns the original metadata; changing an admission UUID's
identity, bodies, Binding or provenance refuses. It grants no execution.
`load(operation_id)` returns one bounded private Frozen original for policy
revalidation. Neither Admission nor Frozen implements Debug or Serialize.

`reserve(Reserve)` runs in an IMMEDIATE transaction and checks the exact current
nonrevoked writer/service epoch and original revision. The actor may call it
only after a new explicit lookup decision for the unchanged original. A new
reservation commits before execution. An exact duplicate or coalesced original
returns `fresh:false` with the complete existing Ticket and must never spawn.
An unknown result permits no timer retry. A later independently admitted
explicit lookup can reserve a new attempt for the same original only after the
previous resources actually drain and all current guards pass again.

`finish(Ticket, Outcome)` verifies the exact original attempt, lease, epochs and
revision. Submitted receipts use a closed allowlist and must match schema,
original request, project and task. They may settle only the original still
reserved attempt after revocation. Unknown is immutable; a late submitted reply
cannot replace it. Finish does not release execution admission.

`drained(Ticket, VerifiedDrain)` is separate. Only the actor's actual owned-child,
input writer, stdout and stderr lifetime accounting may construct that proof. No frontend,
adapter or import deserializes it. A bounded stop returning is insufficient. An inherited stdin reader can also
keep an input writer alive after the owned child exits; that lifetime stays charged.
One partial unique index holds the globally executing-or-undrained slot until
this exact terminal attempt is drained. Revoke refuses marker/checkpoint success
while any operation holds that slot. Startup recovers old reserved attempts to
Unknown in bounded batches and preserves the undrained flag.

At most 32 unsettled originals and one undrained attempt exist per configuration.
Each raw record and input is at most 1 MiB; Binding, provenance and allowlisted
outcome are each at most 16 KiB. Oversized originals remain in their existing
stores for review. Indexed status uses a fixed insertion watermark, at most
128 rows and 512 KiB per call, without loading private bodies or retaining a
per-operation cache. Service status reports admissions paused. This slice's
SQL/reopen and simulated-drain tests do not claim real process/pipe-drain proof;
that belongs to the executing actor/worker integration in G1 .66.3.22.

See [G2 policy](desktop-operations.md) and
[read-only delegation identity](desktop-delegation-hello.md).

## Dormant root policy bridge

The internal `desktop::operations` module connects the existing G2 verifier to
ledger data without adding an executing actor. `VerifiedOriginal` owns bounded
exact raw record and retained-input bytes, with no Debug, Serialize or Clone.
Closed Key, Binding and Executor translations retain every field; preparation
produces only the existing `Admission` data. It does not call `admit` or
`reserve`, manufacture a current guard, or infer Operations admission from a
Complete marker or the background profile.

Reload reads exactly one existing `Frozen`, checks the expected complete
Admitted identity, binding digest and revision, then repeats G2 validation before
returning a pure original recover-only proposal. Input/plan hashes use the exact
retained bytes and consent uses the existing ledger derivation. `Frozen` does
not expose the stored plan/consent columns: real SQLite tests assert those
columns separately; reload does not claim to have independently read them.
The proposal supplies no current state, reservation or dispatch authority.

Result settlement binds that reloaded original to the complete existing Ticket
and uses G2's bounded receipt allowlist before the existing ledger `finish`.
An unchanged old-epoch ticket may finish its own still-reserved attempt after
revoke. Forged tickets and terminal Unknown attempts refuse; no fresh-epoch check
discards an otherwise exact late receipt. Finish does not release the undrained
slot, reserve another attempt or invoke resource-drain APIs. Errors contain no
source, reply, provenance or SQL diagnostic bodies. There is no production
caller, CLI/IPC command, worker or current producer/drain export. Tests combine
G2 and real SQLite fixtures only, without claiming native process/pipe/custody
or production admission qualification. G1 .22 and .23 remain unfinished.

## Prepared custody and resource recovery

Schema 8 adds `desktop_operation_custody`, keyed by the exact attempt UUID and
ticket fingerprint. Its immutable, bounded Prepared record contains version,
configuration, complete Ticket, kernel boot identity, service PID/birth and
private file device/inode/change time/length/SHA-256. The helper PID/birth may be
registered once. These identities are diagnostics and bindings; process names,
elapsed time, environment scans and imported evidence never release a charge.

Any future real helper must use `operations::custody::reserve`, which commits
reservation and custody together. The root first creates and syncs the private,
create-new `desktop/operation-custody/<attempt UUID>.lock`, its closed header and
directory entries, while retaining the [inherited flock](desktop-owned-runner.md).
It must register the helper durably before returning Child to the input writer.
An exact coalesced reservation remains status-only. Failure before that atomic
commit cannot leave a half-reservation; failure afterwards keeps custody charged.
An uncommitted file is never reused. The older storage-only `reserve` primitive
cannot admit execution without this custody path and the current runtime guards.

File preparation has no production caller yet. Recovery opens only the exact
existing file, checks the private directory, no-follow single-link leaf, bounded
header and stored file identity, then tries the nonblocking lock. Only successful
acquisition of that same file may release the exact terminal attempt. The proof
guard stays held through the actor's drain commit. Missing, replaced, altered,
linked, nonprivate, held or unsupported state remains charged and review-only.
No custody file is removed while charged. Linux currently qualifies ext-family,
Btrfs, XFS, tmpfs and overlay filesystem types; macOS requires a local mount.
This restriction affects operation custody, not existing CLI path selection.

One background inspection can be outstanding. The actor does bounded indexed
zero-or-one candidate reads and polls the result without file I/O. A stuck read
keeps its one slot; it never launches another inspection or clears a charge.
Retries after held/unavailable results are bounded to once per ten seconds.
Restart still turns old reservations into immutable Unknown before recovery;
acquiring custody never rewrites that outcome or starts another lookup.
Unregistered prepared custody follows the same lock proof, covering a host
crash before helper registration without requiring an environment scan.

An offline revoke first excludes a live service with owner.lock. With existing
state, it may wait up to two seconds for this same background proof; timeout is
not proof. A never-used service still returns the no-handoff no-op before any
store or directory creation. Status and pending revoke replies name the attempt,
recorded helper state and original revoke UUID. The updater keeps Qt stopped and
gives manual steps: wait or stop only that helper, restart only hgs-desktop through
its manager and retry the original revoke, with reboot as the last resort.
No process, service or machine is stopped automatically by recovery.

SQL tests cover atomic reservation/custody failure, exact replay, immutable
registration and missing-custody refusal. Isolated host/helper tests cover a
real host exit and file-backed journal reopen while its helper still holds
custody, followed by verified release without changing Unknown. No owner reboot
is performed; boot changes alone do not bypass file verification. These tests
qualify custody mechanisms, not the still-fenced G2 execution/drain integration
or native Windows custody. Current producer drain in G1 .23 remains mandatory.
