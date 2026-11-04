# Durable desktop read queue

The service's schema 4 includes indexed read intent and exact dispatch receipts.
The queue lives in `zerus_desktop_ledger::reads`; the service delivery component
is `src/desktop/delivery.rs`. Activation, writer handoff and frontend routing
belong to the service lifecycle. Merely opening the database or submitting an
intent never grants writer ownership. Existing `hgs read` remains unchanged.

`zerus-desktop-read-policy` contains the existing client `ReadMark`, field merge,
snapshot settlement, receipt interpretation and retry delays. The client reexports
its previous names and uses the shared policy. The service does not instantiate
the client's in-memory map of all pending reads.

## Intent and dispatch

`enqueue` requires a canonical UUID request and a nonempty, bounded mark naming
computer, session name, run and conversation. Computer/name/run/conversation form
the queue identity. Marks merge only within that identity. Each changed mark gets
a new revision; an unchanged request returns its original receipt even after the
mark is retired. Reusing a request ID for changed input refuses. The actor must
commit with its existing FULL SQLite durability before acknowledging acceptance.
Passive imports can record intent but cannot authorize delivery.

`reserve` checks the current non-revoked writer and service epochs in the same
IMMEDIATE transaction as its reservation. There are four global slots. Each slot
goes to a due computer with the fewest calls in flight, then its oldest due mark.
Indexed host summaries and per-computer due indexes avoid scanning pending marks.
Each returned ticket pins the attempt UUID, both epochs, queue sequence, revision
and exact mark. An attempt UUID is never reused, including after a crash.

The service worker checks the existing writer/service epoch and admits only
computers observed by its own fleet. It passes the exact mark to the selected
host's existing `hgs read` and honors cancellation and retained native resources.
A policy row or this reservation is not runtime handoff proof. No subprocess is launched by the queue itself.

`finish` requires the exact ticket. A result for an older revision releases only
its owned slot and cannot delay the changed mark. Exact repeat completion is
read-only; a changed result refuses. Revocation permits the original worker to
settle, but prohibits new dispatch. A new service epoch cannot settle an old
reserved attempt. `NotSent` releases a proven undelivered call without counting
a failure. The worker must not release its slot until its owned call has ended.

Failure delays remain 10, 20, 40, 80, 160, then 300 seconds. Confirmed delivery
waits 120 seconds. Partial application waits 120, 240, 480, then 960 seconds.
Changed intent resets the counters and becomes immediately eligible after any
older owned call ends. Clock addition saturates rather than overflowing.

## Frozen pending-read transfer

In service mode the native Next startup path first disarms legacy read delivery.
One owned background job freezes the managed read-mark store and checkpoints it,
then fully consumes `next_pending_reads` before sending any mutation. The whole
source is limited to 16 MiB and 50,000 entries; an invalid final entry refuses
the capture, including its valid prefix. The original file is retained. Read-only overlay borrows are retried within the
same 30-second capture deadline; a timed-out capture retains its original freeze
handle for Retry without reopening mark writers.

The job waits for the canonical responder, selected configuration and CLI ABI.
It submits the closed native `import_pending_reads` request through the existing
original-child frontend authentication. JS cannot choose its paths or rows, and
generic desktop requests refuse this operation. Normalized transport pages have
at most 64 entries and 48 KiB, inside the unchanged 64 KiB control-line limit;
the underlying capture adapter's 1 MiB pages are never truncated.

The actor commits each page into the existing read ledger, even while passive.
Mark request UUIDs depend on configuration and the exact normalized target,
including run, conversation, reply and attention identity, never retry, service
epoch or page cursor. The original receipts survive retirement. An identical settled row is not revived. A different frozen part after settlement
becomes new intent at the next revision. A conflicting unsettled/in-flight mark
stays current; the frozen mark receives a durable superseded receipt so it cannot
block the page or overwrite the current intent. Replays return that original
receipt, including after the current row retires.

`pending_reads_import_result` checks at most one page of those indexed receipts
without mutating anything. A lost reply is looked up before the producer advances;
partial or unknown transfer retains its original source and resource bindings.
The client exposes bounded progress as `pending_read_import`, independently of
native service readiness. The service status UI shows a paused transfer and its
existing Retry action. Transfer completion means durable queued intent, not
host acknowledgement, worker drain, approval consent or a writer handoff.

On Windows the checkpoint retains its original exclusive read/write file handle
and pinned parent chain through capture and original helper resources. It rejects
reparse points (including junction ancestors), multiple links, changed sources
and ACLs outside the existing user/SYSTEM/Administrators/OWNER RIGHTS policy.
New managed folders have protected inheritable private ACLs; prepared files
inherit that policy and explicitly name the creating user as owner. Existing
ACLs are verified, never repaired.

Dirty/new state is bounded and flushed before renaming the original owned
prepared-file handle, then flushed and read back through that same handle. An
error after the rename attempt remains unknown: the source and prepared file
are never deleted as recovery. An unchanged checkpoint does not rewrite bytes
or unknown fields. Before rename, a failed preparation requests deletion of only
its newly created temporary file through that original handle; a cleanup failure
retains both diagnostics and never falls back to deleting by path. File sharing
violations get at most 20 brief 10 ms retries within the original capture deadline,
without holding the marks mutex during sleeps. Other failures, exhausted retries
and unknown publication retain the same freeze for the explicit Retry action.
There is no directory-flush or whole-volume durability claim on Windows.
`zerus-5wmh.66.3.31.6.1` tracks the native Windows validation; implementing this
adapter does not itself enable the service-mode/default-on switch.

Windows checkpoints support local drive paths only. A redirected `%APPDATA%`
on a UNC/network share remains unavailable, including when represented by a
reparse/junction ancestor. The paused-transfer notice stays visible and service
read submissions remain blocked; it is never reported as an empty successful
transfer. Retry does not change that path restriction or relocate user data.

## Observation and restart

The canonical client slots are `lifecycle.background.reads` and `approvals`,
each with `ready`, `code` and conservative original-runtime `drained` values.
Approvals also exposes nullable `enabled`, independently of readiness; disabled
policy can be ready for an authenticated enable request. `automatic_approvals`
reports that durable opt-in, not an assumption that workers are healthy.
Passive status preserves the stored opt-in with both readiness slots false;
absence of the runtime does not imply that consent was revoked or an older
owner drained.
Component-local failures remain `unavailable`; stopping uses `draining` or
`drained`, and a missing writer uses `awaiting_handoff`. Readiness grants no
writer authority. The actor rechecks current writer identity and the opaque
frontend proof at mutation time.

A successful delivery receipt never retires a mark. `observe` accepts a fresh
authoritative catalog row in source order and the exact pending revision. It
uses the shared client settlement policy: changed run/conversation, a superseded
part, or confirmation of every still-current part retires the intent. A stale
pending revision cannot clear a newer mark. If a worker is still running, its
slot remains occupied until completion even when the snapshot has settled it.

Startup stays revoked and recovers at most four owned dispatches. Each old
attempt becomes permanently `unknown`; its pending acknowledgement survives
with failure backoff. Only read acknowledgements may receive a new attempt after
unknown delivery, because the host's exact run/conversation read operation is
idempotent. This grants no replay permission for posting, approvals or other
actions. A recovery error pauses this component and is visible in `read_queue`
status while IPC remains available.

## Bounds and proof

Each encoded mark is at most 32 KiB; its identity and individual fields have
smaller limits. Internal pages contain at most 128 rows, bounded by a captured
sequence watermark. They are not desktop-v1 frames; future transport must also
respect that protocol's byte limits. Maintained pending/in-flight totals make
status a constant-size indexed read. Receipt history stays on disk under the
service's database admission limit. Safe retention and allocated database/WAL
pressure qualification remain separate work; unresolved marks and unknown
attempt identities must never be evicted to admit an effect.

Focused tests cover durable reopen, default revocation, exact request/attempt
replay, stale completion and snapshot refusal, changed intent during an owned
call, partial application, fair four-slot admission and SQLite-full rollback.
A 50,000-row fixture verifies indexed host, due and keyset query plans without
table scans or temporary sorting and checks bounded dispatch and pagination.

## Service delivery interface

Schema 11 adds the explicit default-off automatic-approval policy, immutable
policy request receipts and an index for bounded catalog lookup by session name.
The actor calls `Runtime::start(config, epoch)` only under its existing lifecycle
ownership. `poll(&mut Store)` never waits for a running executor. Four read slots
and two approval slots include resources retained by the native runner, even
when its main worker has returned. `request_stop()` cancels admission and owned
calls; `drained()` remains false while an original worker, persistence completion
or runner resource remains held.

After successfully applying an internal `FleetEvent`, the actor calls
`observe(&Store, &event)` while retaining its ingress permit. Failed apply,
incomplete/offline sources and passive UI feed frames cannot establish delivery
evidence. Discovery visits one catalog page per second, with at most 128 rows;
read retirement keyset-pages at most 16 pending intents per tick, with a captured
sequence watermark. It looks up each session through the catalog index and uses
the existing exact settlement policy, including superseded run identities. An
unacknowledged early intent cannot starve later intents.

The component uses these closed request shapes:

- `read_intent`: canonical UUID `request_id` and the exact `mark` fields
  `computer`, `name`, `run_id`, `conversation_id`, `reply_id`, and
  `attention_signature`. Acceptance follows the queue commit; it means queued,
  never delivered or read.
- `automatic_approvals`: canonical UUID `request_id` and Boolean `enabled`,
  accepted only through the authenticated native frontend route after its
  confirmation UI. Generic desktop requests refuse it. This explicit service
  policy does not import the old Qt or UI opt-in. The
  policy update and immutable request receipt commit together. Replaying an old
  request returns that receipt without undoing a newer setting.
  A new Enable requires the actual ready delivery runtime and a freshly checked
  current writer. An authenticated Disable can commit with degraded, stopping
  or absent background workers while the route and store remain writable;
  generic requests still refuse it. A failed commit never reports success.
- `delivery_result`: a new canonical UUID `request_id` and the canonical
  `original_request_id`. This indexed lookup returns the original acceptance
  receipt after a transport timeout; it neither replays nor proves delivery.

All store and approval-journal mutations stay on the actor. Bounded executors
perform native I/O and return one bounded completion. A failed worker start
retains its original reservation as not sent. A failed completion commit retains
the exact completion for persistence retry, without invoking the native action
again. Status distinguishes the queue, policy, held slots and component errors.

## Automatic approvals

A current service catalog row is only a discovery candidate. An executor must
inspect the actual live native session and match its run, conversation and
reviewed one-time question/choice through the existing automatic-approval
selector. The actor rechecks its current source, writer and explicit policy,
then commits the existing private approval journal reservation before scheduling
an answer. The answer carries that reservation's original native request UUID
and the exact question hash/run/conversation selectors. Broader grants, ordinary
questions and startup trust remain manual.

Unknown, prepared or previously settled journal reservations never authorize
another answer, including after a restart. Disabling the policy cancels pending
inspections and answers; cancellation after a possible send remains unknown.
Read receipt retries remain distinct from this no-replay approval rule. Neither
status nor a command receipt establishes worker drain, currentness or release.


## Read-only approval audit

The selected desktop service accepts the closed `approval_audit_list` and
`approval_audit_detail` requests from `desktop::approval_audit::Request` in the
shared core. Probe the selected CLI for `Desktop approval audit ABI:
desktop-approval-audit-v1`, then use its existing `desktop request --json`
transport. Each query and reply binds the request ID, configuration and service
epoch. These reads need no background writer or automatic-approval opt-in.

The first List omits `journal` and `through`; its result contains at most fifty
entries, `next`/`previous` keyset cursors and a fixed `through` high-water ID.
Subsequent pages retain the returned journal identity and high-water, using
`before` for older or `after` for newer entries. Exact `host` and `session`
filters use their existing indexes. Detail requires the same journal identity
and a positive entry ID. `Request::matches` validates the reply scope, shape and
256 KiB bound before the client unwraps `result` for its existing journal UI.

The service holds one original read-only connection and file identity. Its actor
only validates and admits queries; all journal file and SQLite I/O runs on the
original bounded client I/O thread. At most one audit query is admitted through
response and transport cleanup. Concurrent requests receive
`approval_audit_unavailable` instead of queuing more readers. The reader retains
its journal token between requests, so paging and Detail keep the same scope.
The original service owner lock stays held through actual reader and transport
cleanup even if logical shutdown reaches its deadline; a timeout never proves
drain. These reads add no listener, worker pool or per-session cache.

A changed journal, protection, schema or missing filter index refuses instead of adopting
old cursors or repairing the journal. A fresh first List can select the current
journal. An absent journal is an empty read and creates no directory or database
file. Opening an existing WAL journal read-only can create SQLite's `-wal` and
`-shm` sidecars; the original database file remains unchanged. Unreadable and
oversized records remain unavailable, never truncated. This API
has no Begin/Finish, reservation, worker, policy or consent operation. Ordinary
`hgs approval-log` compatibility remains unchanged.
