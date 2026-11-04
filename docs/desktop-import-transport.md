# Desktop import transport v1

G1 owns the private immutable evidence publisher. G3 owns source decoding and
`client/core::migration::Bundle`. This transport connects them through a selected
local `hgs` child and inherited stdin/stdout. It opens no listener, registers no
service and reads no Qt or interim source store itself.

This initial transport accepts **Staged evidence only**. It cannot write a
handoff marker, claim drain, acquire service ownership, consent to approvals or
enable any effect writer. Fresh runtime drain and takeover remain separate G1
work. A staged record must never be relabelled Drained because it once existed.

## Capability and process admission

Before spawning the publisher, run the selected local CLI's ordinary `--help`
and require the exact marker `Desktop import ABI: desktop-import-v1`. Bound
that probe to five seconds and a small captured output. If unsupported, leave
source stores intact and report unsupported; never call an unknown command
speculatively or fall back to another source decoder.

Spawn exactly `hgs desktop import-evidence --json`, using the same selected
`HGS_CONFIG_DIR` / `HGS_STATE_DIR` environment as the desktop service. Retain the
canonical configuration from supported read-only `hgs desktop status --json`;
never approximate its fingerprint. Generate and retain one canonical import UUID.
There is no shell expansion or caller-supplied destination path.

The child allows ten seconds without the next input chunk and at most 120
seconds for input, including streaming and staging I/O. The parent must enforce
a 130-second deadline for the entire child, including final filesystem sync,
and bound stdout/stderr capture (16 KiB each is sufficient). Cancel only this
owned publisher child on timeout; never a service, process group or agent.
A blocked input reader belongs only to that child and ends with its process.
Close the child's stdin after the final record so it can verify EOF. Timeouts
are unknown publication outcomes and never authorize automatic UUID reuse.

## Framing

Every header is one complete UTF-8 JSON line including its LF, at most 4,096
bytes, with no unknown fields. The first header is:

```json
{"abi":"desktop-import-v1","configuration":"64-lowercase-hex","source":"com.hgdev.hgs-tray","replay":false,"import_id":"canonical-uuid","max_bytes":67108864}
```

The hash and UUID strings above are descriptive placeholders. `max_bytes` must
be 1 through 67,108,864 inclusive. It counts snapshot bodies, metadata page
bodies and the final manifest, matching Bundle's admitted byte budget. Framing
has a separate bound: at most 2,049 item headers after the initial header.
There are at most 1,024 distinct snapshots and 1,024 distinct pages. No
per-session map or directory discovery is involved.

Each item header is followed by exactly the declared number of raw bytes and
one additional LF delimiter. Raw bytes may themselves contain LF. Never encode
the body as an escaped JSON string or base64.

| Bundle item | Header | Body bound |
| --- | --- | --- |
| Snapshot | `{"t":"snapshot","reference":{"sha256":"digest","bytes":N}}` | 1 MiB |
| Page | `{"t":"page","reference":{"sha256":"digest","bytes":N}}` | 1 MiB |
| Final record | `{"t":"record","bytes":N}` | 256 KiB |

Digests are lowercase SHA-256 hex over the exact raw bytes. Every body is
nonempty. Snapshots must be valid JSON. A snapshot must precede each page that
references it; deduplicated snapshots may serve multiple pages. Each page is
shared `SnapshotPage` v1, whose exact schema/source hash must match its manifest
source. Page indexes start at zero and increase by one within each family.
The final `ImportRecord` must match the starting configuration/import UUID,
contain precisely the received page references in order, have
`evidence:"staged"`, and retain `automatic_approvals:false`. All supplied
snapshots must be referenced. Exactly one final record ends the stream;
missing records, duplicate bodies/pages, extra bytes and missing EOF refuse.

The publisher buffers one bounded body, small bounded metadata/ref collections,
and at most three 64 KiB input chunks. It stages private files without holding
`handoff.lock`, so slow input cannot block revocation. Source readers/rechecks
must already enforce G3's exact original identity and unchanged-source rules.
Transport parsing cannot substitute for them.

## Commit and result

All output is under the selected private `desktop/imports` directory. Body
names are fixed `snapshots/<digest>.json`, page names are fixed
`pages/<digest>.json`, and the manifest is `record-v1.json`. No input contains
a destination path. Existing private-directory and no-follow single-link file
checks apply; unsafe leaves are refused without repair.

After complete input, validation and file sync, the publisher takes the short
OS-backed `handoff.lock`. If it is busy, the same-user private service IPC can
ask the actor for its actual current notification-runtime owner. One bounded
finalizer on the existing client thread may use that original owner's lock;
there is no new listener, caller-supplied owner, effect or writer grant. A
rollback hold, stopping/recovering runtime, mismatched grant or unavailable
owner cannot lend it and publication remains deferred.
An exclusive UUID directory creation reserves that import ID permanently.
Private synced snapshot/page directories move into it, and the final synced
manifest is published last; parent directories are synced before success.
An existing UUID is never overwritten, including a partial directory left by
failure. A partial directory without a manifest remains ineligible. Source
stores and any existing handoff marker are untouched. No `owner.lock` or
desktop SQLite database is created by the publisher.

Exit zero prints one bounded JSON result: `abi`, `published:true`,
`evidence:"staged"`, exact `configuration` and `import_id`,
`import_record:{version:1,sha256}`, admitted body `bytes`, and snapshot/page
counts. Compare all identities before accepting the result. Reports contain no
snapshot bodies or source paths. Errors are nonzero and redacted.

There is one closed negative response: after all input and the final manifest
have been validated, contention from the actual `try_lock_exclusive` call
without an admitted current service owner may
return exit zero with `published:false`, `code:"handoff_busy"`,
`this_attempt_not_committed:true`, and the exact ABI, configuration, import UUID
and `import_record:{version:1,sha256}`. This invocation has not attempted the
UUID directory reservation. The owner-assisted path also returns this exact
attempt-only negative for a busy finalizer, full/closed actor queue, loan timeout,
failed owner query, stop or failed pre-claim filesystem check. The boundary is
before entering the custody rename syscall. Once that syscall is entered, any
rename/validation/commit/reply uncertainty remains unknown, including an error
before UUID reservation. Invalid/mismatched request framing is refused. Ordinary
local lock open/path/privacy errors do not become a typed OS-contention result.

Accept that negative only after the original child exits successfully and its
input writer and both output pipes finish completely, and only against the
exact expected manifest receipt retained before sending the final record. It
does not establish global absence or settle an earlier unknown attempt. A new
**explicit** attempt may use the same retained UUID and record after fresh
same-source checks. Lost, partial, malformed or mismatched negatives grant no
retry; neither does an ordinary exit 1 or an absent status result.

After a lost response, inspect the same ID using the supported read-only command:

```sh
hgs desktop import-status --json --import-id UUID
```

This checks one fixed manifest, never scans pages or directories and never
creates state. `record_present:false` also covers an incomplete UUID directory.
A present valid manifest reports its exact reference/evidence and page count;
it does not prove drain, previous sync durability, current delegation or effect
authority. An absent or unknown outcome must not silently generate another ID
or overwrite the old one. Preserve original sources and surface the result for
review. Native Windows filesystem/runtime qualification remains separate from
cross-target type checking.

## Native Qt/Settings report caller

The live switch identity's service connection captures managed sources off the
GUI thread after paired-CLI selection and configuration preflight, before the
startup Grant. Explicit Resume uses the same ordering. The report is best-effort:
busy, quarantined, unavailable and unconfirmed outcomes are recorded separately
and never prevent Grant, watch, catalog or normal preference edits. This first report slice
uses the existing Qt family decoders and the current local Settings bytes: 17
Qt families on Linux (16 on macOS and Windows), plus three paired Next policy/recovery
families, with at most 64 MiB, 1,024 snapshots and 1,024 pages. It is a private
Staged report, not materialization of these families into workers.

The Unix source adapter retains the original private, single-link Qt file and
Settings file identities and raw hashes. Mac defaults are exported read-only
through the fixed owned helper and re-exported before the final record; an
uncertain export never falls back to a plist. The local Settings writer pauses
new mutations during its original capture. The caller releases its capture after
the attempt; original child/pipe clones still retain it until actual completion.
This is source-specific exclusion,
not proof that Qt or the whole desktop drained. Every helper, writer and pipe
retains its original resources through actual completion. Shutdown reports
unconfirmed work rather than declaring an outstanding resource group drained.
On Windows source captures require local disk paths, pinned no-reparse
ancestors, private ACL and single-link checks. The parent chain includes the
local drive root, each intervening directory and the final source folder.
Each opens with `FILE_LIST_DIRECTORY`, attributes/security reads and read/write
sharing without delete sharing. Directory data access is needed for the
[Windows sharing check](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fsa/8c0e3f4f-0729-49f4-a14d-7f7add593819);
attributes-only handles do not prevent renames. No directory enumeration occurs.
An ancestor that does not permit this read access refuses capture; permissions
are never repaired. This also changes the read-marks checkpoint on hardened
profiles: list permission is required at every ancestor, including the drive
root. Retained parent pins on a removable source drive can prevent safe eject
until the original capture/checkpoint is released. Native tests rename only their owned scratch/outer/hgdev
folders, not the system/profile/temp ancestors outside those fixtures.

The foreign Qt INI is read-only
with read/write/delete sharing during each bounded read, then its leaf handle
closes. Only its parent pins, original volume serial/file index, size, last-write
time and hash remain during report publication and receipt waits. Recheck opens
the exact path no-reparse, repeats verification and stamp/hash comparison, and
closes before returning. No leaf handle remains during report/helper waits to
block Qt's classic `MoveFileExW(..., MOVEFILE_REPLACE_EXISTING)` save.
Replacement or in-place changes quarantine the report without adopting a new source.
Next's own Settings, journal and pending-read checkpoint keep exclusive
leaf handles. Settings mutations remain paused by its original writer lease until
all original capture references drop. A pre-open sharing violation can retry at
most twenty times at ten milliseconds within the original deadline; other
failures refuse without a source rewrite. UNC/redirected or junction-backed
stores stay unavailable and keep the report deferred. The pending-read state
machine and its unknown-publication rule are unchanged.

The Windows private report journal uses the same checked CREATE_NEW temporary,
original-handle rename, sync and bounded readback as the checkpoint. Its original
UUID and immutable expected record survive restart; a rename/sync uncertainty
is not converted into a replay. No adapter repairs source permissions, truncates
files, creates an absent source or adds a worker, schema or effect authority. Missing Qt is reported as not applicable without inventing an
empty source or a successful import.

The private managed `qt-retained-import-v1.json` retains the original UUID,
configuration, source hashes, attempt and expected manifest hash. Published
records are historical: later starts inspect the original ID instead of
re-seeding changed Qt state. Unknown attempts perform only original-ID lookup.
Only an exact known pre-reservation negative admits an explicit same-record
retry (at most 16 attempts). Invalid, changed or unsupported sources are kept
for review without a final manifest; no source is truncated, cleared or
rewritten. Service status carries body-free `retained_source_import` progress,
and the GUI shows deferred and review-needed reports independently of connection
readiness. Normal preference edits remain available after completed failures;
a later explicit known-negative retry must freshly capture the same source bytes.

For owner-assisted finalization the CLI relinquishes automatic stage deletion
before the request can become uncertain. The closed request carries only its
managed stage nonce, original import UUID, configuration, expected record hash,
directory identity and finite byte budget. The finalizer pins and renames the
input into a new private service-owned stage, revalidates the exact bounded
inventory, no-follow private single-link files, original identities and hashes,
and feeds the same Staged parser before reserving the UUID. The whole original
runtime owner and separate one-slot finalizer admission remain charged through
filesystem work and original stream cleanup. Revoke closes new admissions and
waits for this same resource; disconnect does not remove its input or release
its charge early. Lost replies and all post-reservation uncertainty remain
original-ID lookup only. Temporary-copy cleanup cannot change the certainty of
a complete exact reply; a cleanup refusal keeps the copy for review instead.

Native staging has private adjacent ownership and empty OS-lease sidecars; they
are outside the fixed three-entry input directory and never enter an immutable
import manifest. They bind configuration, import UUID, random native nonce,
directory and lease-file identities and creation time. The creator retains its
lease through the original request; a claimed container has its own worker
lease. These local cleanup records grant no effect/writer authority.

Before immutable commit, a best-effort sweep under the actual handoff lock
considers at most 256 directory entries and retires at most two verified stages or orphan sidecar sets
older than 24 hours. A pass admits another candidate only within 100 ms; each
candidate has finite file/metadata limits (1,024 snapshots, 1,024 pages, one
record, 64 MiB aggregate). No body scan or actor-thread I/O is added. The sweep
requires every retained creator/job lease to be free, private no-follow single-link files,
exact native ownership/directory/file identities and a closed data inventory.
An existing native claimed-container record still binds its exact input after
the creator's sidecars are gone; an unmarked standalone legacy stage does not.
Live, fresh, replaced, oversized, malformed and unverified legacy stages stay.
Published UUID directories and original source stores are never sweep targets.
Cleanup ends at any uncertain check and never authorizes publication replay or
settles an unknown original import. Unknown outcomes remain original-ID lookup
only even after a stale generated copy is removed.

This report does not replace the existing frozen pending-read capture or its
separate authenticated safe-passive page transfer. That transfer may wait for
the connected service and need not complete before Grant. An already-granted
service can publish under its actual in-process lock owner without stopping it.
If that original owner cannot admit work (including a rollback hold), publication
is durably deferred with `retained_import_handoff_busy` while the connection
continues. Only an explicit known-negative Retry may submit the same record
after fresh same-source checks; it never retries an unknown result. The caller never
silently revokes a runtime or treats status as permission. Rearm makes at most
one report attempt; its following reconnect is not another report retry.
Remaining interim operation,
draft/cache source adapters and actual migration/upgrade acceptance stay open
under G3.
