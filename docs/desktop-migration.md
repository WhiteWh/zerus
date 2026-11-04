# Desktop import evidence and writer handoff

G1 (`hgs desktop`) and G3 (read-only migration) agreed this v1 contract on
2026-10-30. The shared Rust crate is `crates/desktop-migration`, package
`zerus-desktop-migration`. It contains structural types and pure validation,
not an importer, effect worker or ownership proof. Existing Qt/interim readers
remain the import stack. See the [migration inventory](superpowers/specs/2026-10-29-qt-retirement-inventory.md#migration-contract)
and [service design](superpowers/specs/2026-10-29-hgs-desktop-service-design.md#handoff-coexistence-and-upgrade).

## Marker

`desktop/handoff-v1.json` below the existing `HGS_STATE_DIR` is at most 64 KiB:

```json
{
  "version": 1,
  "configuration": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "source": "com.hgdev.hgs-tray",
  "import_id": "11111111-1111-4111-8111-111111111111",
  "writer_epoch": "22222222-2222-4222-8222-222222222222",
  "phase": "imported",
  "import_record": {"version": 1, "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},
  "automatic_approvals": false
}
```

`Handoff::parse` rejects missing, unknown and malformed fields, unsupported
versions, wrong configuration/source, noncanonical UUIDs, non-lowercase hashes,
and migrated approval consent. Development/local identities and replay never
participate. `writer_epoch` is the durable cutover generation, distinct from a
service process/watch epoch, and authorizes no delivery replay.

The closed v1 phase enum is `imported`, `quiesced`, `complete`, `revoked`.
`revoked` durably withdraws the service's migrated writer authority before a
rollback or Qt start. G1 alone commits it. Reason/history stay in G1's private
journal, without extending the marker payload. Older consumers fail closed on
the unknown phase; none may interpret it as a new import or reseed.

The import record always resolves to
`desktop/imports/<import_id>/record-v1.json`; JSON cannot supply another path.
`Handoff::verify_import_record` checks the exact byte digest, version and matching
configuration/import identity. Missing or mismatched evidence leaves G1 passive
and is never repaired by rewriting or replacing it with defaults.

## Immutable import evidence

`ImportRecord` is a metadata-only manifest, at most 256 KiB. Its exact fields are
`version`, `configuration`, `source`, `import_id`, `evidence` (`staged` or
`drained`), `created_at` (Unix seconds), `automatic_approvals` (false), `sources`
and `pages`. Each source records a family, reader schema version, format, exact
source-byte SHA-256 and optional verified build identity. There are at most 256
source records with distinct family names and 1024 distinct page references.

Each page reference has only `sha256` and `bytes` (1 to 1 MiB) and resolves to
`desktop/imports/<import_id>/pages/<sha256>.json`. `verify_page` checks exact
length/digest. Private pages carry bounded family metadata, schema
and snapshot/quarantine references. The manifest and handoff marker contain no
message, question, token or session arrays. No credentials are copied from their
privileged stores. Snapshots and quarantine details are never repository data.

G3 assembles read-only snapshots through the existing importers. Its evidence is
always explicitly `staged` and ineligible for takeover. G1 owns the executable
publication boundary and immutable `drained` publication after a fresh drain.
The producer must run after that drain and recheck source identity/hash; a stale
pre-drain bundle cannot simply be relabelled. Neither owner changes a committed
import. These fields describe a past import, not current process ownership.
Automatic-approval reset evidence must inspect both Qt's former choice and
Next's interim policy. Neither carries consent to the service.

Previously unhandled families may import once under the versioned first-G3 rule
in the inventory. Next's existing values win. A family record includes schema,
source path, time, key names and the exact source-byte hash; a changed Qt source
since the original switch is recorded and remains eligible for a new family.
Failed/partial families remain unhandled. Already handled settings/layout/drafts
never re-seed after rollback.

## Locks and live proof

G1 retains `desktop/owner.lock` for its process lifetime. G3 never takes it.
`desktop/handoff.lock` is a separate OS-backed exclusive lock. G1's publication
helper holds it briefly to commit immutable evidence. If the active notification
runtime holds it, G1's actor can lend its actual current Owner to one bounded
Staged-only finalizer on the existing private IPC client thread. That whole
Owner stays charged through filesystem work and original connection cleanup;
revoke refuses new jobs and waits for admitted work. Other contention stays
deferred, with only an exact known-negative explicit retry. G3 has no filesystem committer. G1 rereads
the exact import UUID and digest under that lock, owns all marker transitions
(`imported`, `quiesced`, `complete`, `revoked`), and retains the lock while any migrated
writer runs. Commits use atomic replacement with durable file/directory flushes.

Readers/committers require private directories and private regular single-link
files, no-follow opens and byte limits. Pure validation does not perform these
filesystem checks. A complete marker never replaces fresh runtime proof:
versioned build/configuration/writer-epoch hello, stopped old admissions, durable
drain, exact shared effect identities and both service locks. The revised
coordinator ruling of 2026-10-30 uses explicit durable revocation before rollback
or Qt start on the normal path, with live legacy-GUI detection revoking the
abnormal path. It does not require the service to own Qt's Unix socket; the
temporary instance start lock alone cannot prove long-lived exclusion. Takeover
remains fenced until revocation and exact effect guards exist. Windows permanent
lock exclusion is separately unqualified. Arch remains passive until the explicit
Linux handoff even if a marker is copied there.

`configuration(state, config)` extracts the service's existing algorithm:
canonicalize existing path ancestors, then SHA-256 the state and config paths in
that order, each native encoded byte string prefixed by its little-endian u64
length. Missing descendants are resolved without creating directories. G1 and
G3 must call this same helper; lexical paths or independently approximated hashes
cannot establish configuration identity.

## Producer API

`zerus_client_core::migration::Bundle` is a Rust-side bounded iterator over
existing source decoders. It accepts validated `SourceStream` metadata and lazy
chunks, the production identity, configuration fingerprint, canonical import
UUID, time and an explicit total byte budget. Development identities, replay and
duplicate source families are refused before a decoder is read.

Each decoder checks its supported source schema and exact identity/hash and
yields bounded JSON values. It must refuse an oversized individual source item
before allocating it; it must not load a complete unbounded journal. Unknown,
partial, changed or unreadable data ends the bundle without a final record.
The source remains unchanged and its family stays unhandled for retry.

`BundleItem` yields a private `Snapshot { reference, reader }`, then a body-free
`Page { reference, reader }`, followed by exactly one final `Record` after all
sources succeed. Consume each `Read` before requesting another item. At most
one current body and one small pending metadata page are retained by assembly;
each serialized body is at most 1 MiB. The total budget includes unique bodies,
pages and the final manifest. The manifest retains at most 1024 page references,
not source values. Repeated snapshot bodies are emitted once by digest.

The shared `SnapshotRef` contains only `sha256` and `bytes` (1 to 1 MiB) and
resolves only to `desktop/imports/<import_id>/snapshots/<sha256>.json`.
`SnapshotPage` v1 contains `family`, `schema`, `source_sha256`, `index` and that
reference. `ImportRecord::snapshot_page` verifies the page's membership, exact
bytes and matching source family/schema/hash. `SnapshotRef::verify` checks the
private body's exact bytes. No caller supplies a final filesystem path.

This iterator cannot itself cross the core-to-hgs process boundary. G1 owns the
bounded authenticated transport and secure publisher, reusing its existing
platform file/Location/lock code: no-follow private single-link regular files,
create-new UUID directory, fixed digest names, hash/length rechecks, durable
file/directory flushes and atomic final record publication. Existing UUIDs,
held locks and partial/newer/mismatched bundles refuse publication. G3 writes no
service files and has no marker, drain or effect authority.

`migration::sources` supplies the first existing-decoder adapters:

- `creations(Origin::Qt | Origin::Next, bytes)` reuses
  `creation::decode_journal` (128 operations, 8 MiB). It preserves original
  dispatched/unknown states, request IDs and unknown header/operation fields;
  it does not call the live creation state machine. The same decoder now
  validates the entire journal before the client loads any operation.
- `qt_pending_reads`, `qt_shared_read_migrated` and `next_pending_reads` retain
  exact host/run/conversation/reply/attention identities. Qt's compound key must
  match its value. Unknown private fields are retained alongside the existing
  `ReadMark` decoder's normalized shape. Queues are bounded at 16 MiB and 50,000
  entries, in private pages of at most 64 entries/1 MiB. Nothing queues a read,
  retries delivery or changes a host's read state.

Attach `SourceStream::recheck` using the same existing source reader after
decoding. It runs once on exhaustion and refuses changed hashes or read errors
before a final manifest. G1 still checks source identity and its current drain;
this callback creates no runtime authority. Unsupported/oversized sources stay
unchanged and unhandled. These APIs do not imply a complete production import.
Source metadata is available through `SourceStream::identity()`; chunks and
recheck state cannot be replaced after construction.

`migration::transport::Publisher::probe(selected_hgs, environment)` connects
this producer to G1's [desktop-import-v1 transport](desktop-import-transport.md)
using the existing core child-process runner. It bounds the exact help-marker
probe to five seconds/256 KiB, then obtains the configuration from supported
read-only desktop status. It keeps the selected executable/environment and
rechecks capability/configuration before publication or fixed-ID inspection.
Call every probe, source decoder, stream and wait off the GUI thread.

Build the bundle with `publisher.configuration()` and a durably retained import
UUID. `publisher.publish(bundle)` accepts only an untouched bundle with a fresh
recheck attached to every source, within the 64 MiB wire budget. It writes one
bounded body at a time and closes stdin after the final Staged record. Both
publisher output pipes have a hard 16 KiB capture limit; overflow or the
130-second child deadline stops only that owned child. A `StagedReceipt` is
accepted only when its configuration, UUID, manifest digest, evidence, byte and
snapshot/page counts match exactly what was sent. No report contains bodies or
source paths. This API has no startup hook and does not select source families.

Failures retain `PublishFailure.import_id`. They never generate another UUID,
retry publication or overwrite evidence. Explicit `publisher.status(id)` checks
only that retained manifest and reports absent or its body-free digest/evidence.
Absent also covers incomplete publication and never authorizes reuse. Present
does not establish current drain, sync durability, delegation or effect authority.
The opt-in `migration_publisher_native` integration test exercises the real
selected CLI with synthetic sources and isolated HGS directories; run it with
`HGS_IMPORT_TEST_BIN` and `--ignored`. It starts no service or native agent.

`qt_task_forms(&QtSnapshot)` preserves `tasks/newCard/*/title|request`,
`titleDraft`, `noteDraft` and `messageRequest` in private entries
`{key, kind, project, task?, value}`. Reserved-card request scope/project,
request UUID, target and stage must match the existing Qt schema. Incomplete
text drafts and unknown request fields are preserved; neither request nor task
IDs are regenerated. Existing DraftStore import still owns task description,
comment and attachment drafts. Form input is bounded at 16 MiB/50,000 entries,
individual items at 1 MiB, and pages at 64 entries/1 MiB.

`task_delivery(Origin, project, task, marker_id, bytes)` reuses the existing
`tasks_write::outbox::decode`. Its source must be the existing
`outbox::path(root, project, task)`; the caller verifies/rechecks that exact file
and marker under the current drain. The original marker must equal the record's
request UUID. Private evidence preserves the exact intent and unknown fields,
with distinct project/task/request identities. A delivery is one indivisible
item: over 1 MiB is refused before JSON allocation, never truncated. Missing or
mismatched markers, unknown versions and orphan/unidentified files stay for
review; this adapter does not discover or authorize them. It never reserves,
releases, submits or reconciles a message. Source discovery and remaining
families are separate slices.

`task_start_delivery(project, task, marker_id, bytes)` covers Next's separate
existing-executor Start checkpoint at
`outbox::path(draft_root.join("task-start"), project, task)`. It reuses the
bounded outbox decoder and the live task-start writer's pure structural check.
The exact canonical reservation UUID, plan UUID, original request UUID and
project/task scope must agree. All four saved phases (`delivering`, `review`,
`started`, `released`), the complete original guarded CLI plan and opaque
unknown fields remain private evidence without mapping or settlement. The
distinct `next-task-start/<scope hash>` family cannot replace discussion
delivery evidence. The indivisible source and evidence envelope must each fit
1 MiB; future, partial or mismatched records refuse. The caller verifies and
freshly rechecks both the source file and its exact marker under G1's drain.
This adapter never calls task open/start/recover/release, reserves an identity
or writes a source. Startup publication and G1 consumption remain separate;
neither an imported phase nor an original plan authorizes execution.

`qt_pending_launches(&QtSnapshot)` and `qt_executor_launches(&QtSnapshot)`
preserve the separate legacy Qt maps `workspace/pendingLaunches` and
`tasks/pendingExecutorLaunches`. They use the existing QtStore/Qt JSON reader,
not the live creation or task state machines. Their distinct families are
`qt-pending-launches` and `qt-executor-launches`, schema 1, with format
`qt-ini`, `qt-plist` or `qt-defaults` and the raw Qt snapshot hash.
Private chunks start with `{kind:"pending-header",key,present}` and continue
with `{entries:[{launch_id,record}]}` pages. An absent key is distinct from an
explicitly empty map. Original keys, local empty host, task/project/parent,
saved `waiting`/`reserved`/`review` phase, session, timestamp, hidden status and
unknown fields remain unchanged. No age-based expiry, state conversion,
name-based matching or ID generation occurs. Creation-schema records in the
legacy map refuse; Qt already saves those only in its separate creation journal.

Executor input keeps Qt's 64 KiB/16-record caps, checked before JSON allocation;
legacy placeholders use the bounded 16 MiB/50,000-entry reader. Private pages
keep the existing 64-entry/1 MiB limit; a single oversized record refuses, never
truncates. Malformed identities, unsupported phases/shapes and partial sources
cannot complete a family manifest. The caller still discovers only the existing
source and freshly rechecks it under G1's current drain. These are Staged
evidence adapters with no startup publication, status service, placement,
launch, task delivery, release, settlement or replay authority. Deferred,
draft operation and cleanup sources remain uncovered; download-index metadata
has the separate adapter below.

`qt_repository_clones(&QtSnapshot)` preserves the separate Qt
`workspace/repositoryClone/<scope>/pending|previous|parent` namespace as
`qt-repository-clones`, schema 1, using the existing Qt source format/hash.
Qt's scope is lower-case hex of UTF-8 compact JSON `[hgs executable,host]`;
the adapter decodes that original identity without choosing an executable or
host, or claiming it belongs to the current service configuration. Private
chunks have `{kind:"clone-header",present}` followed by
`{entries:[{key,executable,host,kind,record}]}`. Pending/previous records reuse
`repository::Pending` serde and the existing pure UUID, name and path checks;
the original object and unknown fields survive. The remembered parent remains
text. A previous record proves neither settlement nor that a clone stopped.

The existing 16 MiB/50,000-entry source budget and 64-entry/1 MiB pages apply;
each source item is bounded at 1 MiB before JSON allocation, and scope JSON at
16 KiB. Unsupported scopes/suffixes, invalid/mismatched original identities or
destinations, unsafe credential-bearing repository URLs and oversized records
remain in the unchanged source without a final family record. Attach the fresh
source recheck before publication. No live Clone, status, retirement, clone,
checkout or launch call runs. These adapters add no discovery, startup
publication, consumer or execution authority.

`next_repository_clones(settings_bytes)` additionally covers Projects Clone's
later native-only `repository_clones` Settings extension as
`next-repository-clones`, schema 1, format `settings-json-v1`. It reuses the
bounded Settings and version-1 checkpoint decoders (64 exact opaque scopes),
without opening Settings or starting a writer. An absent extension is explicit;
null, future, malformed, duplicate or oversized records refuse the family.
Pending/previous credential-bearing URLs, including userinfo, queries or
fragments, remain only in the original source without a final family record.
The source hash covers the full original Settings bytes. Private snapshots
include a `clone-header` with presence and extension byte length, followed by
`clone-checkpoint-bytes` chunks with exact byte offsets and base64 bodies of at
most 512 KiB before encoding. Concatenating them retains the extension JSON's
original bytes, order and whitespace, including pending/previous request IDs;
unrelated settings are excluded. The original scope hashes are never resolved
to a current CLI/computer/project. Attach the same fresh source recheck before
publication. No status, settlement, retirement, clone or replay is authorized.

`download_index(folder, bytes)` reuses the pure bounded decoder extracted from
the existing `download_cache::Ledger::open`. The index `.zerus-files.json` is
already shared by Qt and Next. Its one family is
`download-index/<sha256(canonical UTF-8 folder)>`, schema 1, format
`download-cache-json-v1`, with the hash of the original index bytes. The caller
uses the existing `download_cache::folder` resolver and read-only source reader,
deduplicates canonical shared/aliased roots, and supplies the freshly verified
absolute UTF-8 folder. Non-UTF-8 or unsupported folder identities stay unhandled;
never bind a lossy path conversion. The family hash selects no filesystem path.

Private snapshots start with
`{kind:"download-index-header",folder,content:<original root without files>}`
and continue with `{entries:[<original file records>]}`. Header extensions,
names, saved times, sizes, content hashes and original host/path/source digests
survive unchanged. The existing decoder owns schema/name/hash/source validation;
this adapter does not build another cache reader. The 32 MiB source cap,
50,000-entry cap and existing 64-entry/1 MiB pages bound assembly. A newer,
malformed, oversized or partial index cannot complete a manifest. Attach the
same source reader's fresh file/folder identity and hash recheck before staging.

Only index metadata is preserved here. Downloaded content, edited files,
unknown local files and in-progress files stay in their existing folder; their
bytes are neither opened nor copied by the adapter. It never acquires the
live cache's Qt lock, creates a folder, calls the live Ledger, updates a saved
time, reserves a download, trims or removes anything. Index evidence proves
no current content identity, cleanup permission or reuse authority. G1's
current drain/publication and G2's fresh content/ownership guards remain
separate. This is Staged evidence only, with no discovery or startup hook.

`qt_attention_observed(&QtSnapshot)` and `next_attention_observed(bytes)` use
the existing `attention::seed` object decoder, extracted verbatim as the pure
`decode_observed` helper. The families are `qt-attention-observed` and
`next-attention-observed`, schema 1. Qt uses its existing source format/raw-store
hash; Next uses `attention-observed-json-v1` and the original file hash. The
interim file is `attention-observed.json` in the client's own data root.

Private snapshots begin with
`{kind:"attention-observed-header",present,hosts:{host:"object"|"value"}}`.
An object host has paged `{host,key,value}` entries; an unknown non-object host
has one `{host,value}` entry. Empty host objects, absent Qt keys and explicit
empty roots remain distinct. Original phase/request/reply/question identities,
tokens, retry times, seen-question arrays and unknown or malformed child values
survive unchanged. The existing seed parser accepts those children; retaining
them privately is not semantic validation. The report/manifest stays body-free.
The 16 MiB input, 50,000-entry, 64-entry page and 1 MiB body/header limits apply;
an oversized individual entry refuses rather than truncates. The existing
source reader must freshly recheck identity/hash before the final Staged record.

Neither adapter constructs a Tracker/Delivery, seeds live state, normalizes
historical entries, posts/withdraws a notification, routes a click, schedules a
retry or settles a read. The existing initial seed still prefers Next's own
file, even if malformed. Native Linux owner/token maps, current producer drain,
source discovery and production handoff remain separate.

`qt_local_read_marks(&QtSnapshot)` preserves the separate
`attention/sessionMarks` and `attention/readReplies` maps together as
`qt-local-read-marks`, schema 1, using the original Qt format/raw-store hash.
Each private `{kind:"local-read-header",scope,present}` precedes pages of
`{scope,key,value}` entries. Absent maps differ from explicit empty maps.
Known mark fields use the existing `marks::Mark` serde decoder; unknown mark
fields and explicit false/empty acknowledgements/replies remain original.

`next_local_read_marks(bytes)` preserves `attention-marks.json` as
`next-local-read-marks`, schema 1, format `attention-marks-json-v1`, with its
raw-file hash. The existing ordered file decoder is reused without constructing
the live store. The private header is
`{kind:"local-read-file-header",fields:{marks:bool,replies:bool},content}`;
`content` retains original other root fields. Pages contain
`{scope:"marks"|"replies",index,key,value}` in original pair order, including
duplicates. Missing optional mark fields stay missing. No live cap, deduplication
or normalized reserialization replaces this evidence.

Keys keep their original JSON text, including whitespace and the distinct Qt
raw command versus Next frontend-agent spelling. Only their existing 3-string
conversation tuple or 4-part fallback tuple (marks only, integer creation time)
is checked. No key conversion, alias inference or current identity proof occurs.
Both adapters are bounded at 16 MiB; Qt has a 50,000-entry cap per map and Next
uses the current writer's 20,000-pair cap per map. The existing 64-entry/1 MiB
pages, total bundle bounds and fresh source rechecks still apply. Unsupported,
future, oversized or changed sources cannot complete the family. Neither
adapter sets/clears a mark, migrates a fallback, marks a reply read, settles a
pending read, drops an orphan, seeds a live store or grants replay authority.

`qt_linux_notifications(&QtSnapshot)` and `next_linux_notifications(bytes)`
preserve `qt-linux-notification-identities` and
`next-linux-notification-identities`, schema 1. Qt's `attention/linuxOwner`
and `attention/linuxTokens` keep the existing Qt source format/raw-store hash;
Next's `linux-notifications.json` uses `linux-notifications-json-v1` and its
original file hash. Validation reuses G4's pure `linux_notify::Saved::valid`
and `decode_saved`; the typed projection never replaces original evidence.

Private `{kind:"linux-notification-header",fields,content}` retains the
original owner, optional bus ID and unknown Next header fields. Qt's fields
record owner presence and token state `absent`, `empty-text` or `object`;
Next's fields record optional bus-ID and uncertain-array presence. Pages have
`{kind:"token",id:<original string key>,token}` and
`{kind:"uncertain",index,token}`. Original unknown-delivery array order and
duplicates survive; typed map/set coalescing never reduces the admitted raw
entry count. Missing legacy lineage is never filled with the current daemon.

The existing 16 MiB file and combined 8,192-entry policy caps apply to the
original data too, along with 64-entry/1 MiB pages and fresh source rechecks.
No bus, daemon, endpoint or producer lease is authenticated by these values.
The adapters call no namespace adoption, post, close, activation or retry,
create no live notifier/lock/source reader and seed or rewrite no file. G4
retains its secure reads and native effects; G1 retains the runtime admission
and drain. This evidence remains Staged and cannot authorize historical effects.

`next_linux_owned_notifications(bytes)` separately retains the optional
AgentAttention sibling `linux-notifications-owned.json` as
`next-linux-owned-notification-identities`, schema 2, format
`linux-notifications-owned-json-v2`. It reuses G4's explicit
`decode_owned_for(..., AgentAttention)`, including raw entry/file budgets and
each row's owner/identity and empty-legacy-home checks. SoftwareUpdate rows or invalidations in
this file refuse; no Update sibling family is invented here. Schema-one
sources and their original suppression home remain unchanged.

Private `linux-owned-notification-header` records byte length, raw entry count,
unknown-field presence and byte encoding. Subsequent
`linux-owned-notification-bytes` chunks contain exact offsets and base64 of at
most 512 KiB of original bytes. Order, whitespace, original IDs, pending/unknown
states and private unknown fields survive without calling policy `encode` or
constructing a Model. Attach the same fresh source recheck before publication.
This single-file adapter proves no combined owner-pair inventory, valid restart,
commit, source discovery, current profile, permission consumer, lease or drain.
It opens no file and has no production caller or historical effect path.

`recovery_policy_caches(&QtSnapshot, next_settings_bytes)` decodes both former
`recovery/sharedPolicy` values before returning either source stream. Families
`qt-recovery-policy-cache` and `next-recovery-policy-cache` use schema 1 and
the original Qt raw-store/Next raw-settings hash; Next's format is
`settings-json-v1`. It reuses the original `RecoverySync::new` cached-envelope
parser, extracted verbatim as `recovery_settings::decode_cached`.

Each private `{kind:"recovery-policy-cache",key,present,cleared,record}` keeps
the original policy/order and unknown envelope or policy fields. Absent keys,
explicit empty text and an empty cached object remain distinct. Unsupported
envelopes and oversized bodies refuse both streams, with a 1 MiB input/body
limit per cache and the existing 4 MiB Next Settings decoder limit. Both source
rechecks must succeed before a final Staged record.

The existing parser validates envelope shape, not a current revision or consent.
Unknown policy versions remain private historical evidence, never execution
authority. The adapter constructs no RecoverySync, compares or reconciles no
policy, starts no timer/worker and invokes no get/set/sync command. Authoritative
policy and current causal proof stay with `hgs`; no cached enable flag changes
them. Existing seed/parser behavior is unchanged.

`qt_organization_checkpoints(&QtSnapshot)` preserves these separate objects
through the existing QtStore/Qt JSON decoder:

- `workspace/organization` (the desired local state);
- `workspace/organizationBeforeProjects` and `workspace/organizationBeforeSwarm`;
- `swarm/observedOrganization` and `swarm/observedVersions`.

Its family is `qt-organization-checkpoints`, schema 1, with the existing Qt
format and raw source hash. Missing keys differ from empty objects. Legacy
groups, local Windows folders, unknown fields and causal heads remain original
evidence; they are not converted, merged or treated as fresh observations.
The schema describes this evidence envelope, not a validated apply request.
Non-object checkpoints and unknown declared organization versions refuse;
the causal map has no inferred organization version. The total Qt input is
bounded at 16 MiB.

`next_organization_draft(bytes)` validates the existing `organization-draft.json`
through `swarm::draft::Draft::from_json`, without its filesystem loader or live
controller. Its family is `next-organization-draft`, schema 1, format
`organization-draft-json-v1`, with the original file hash and existing 24 MiB
input cap. A private `{kind:"draft-header",content}` preserves version and
unknown root extensions. `base`, `desired` and `versions` remain separate
original objects; pending edits are not acknowledged or applied.

Both adapters page an object with
`{kind:"organization-header",scope,present,fields:{<field>:"array"|"value"}}`
followed by `{entries:[{scope,field,index?,value}]}`. `scope` is the exact Qt
key or Next `base`, `desired`, `versions`. Array entries carry zero-based
contiguous indexes; empty arrays have their descriptor and no entries. Other
values, including empty/nested objects and null, keep their original value
without an index. Each object is bounded at 50,000 fields/items, private pages
at 64 entries/1 MiB; an oversized individual project/value/header refuses
without truncation or a final manifest. Attach fresh source identity/hash
rechecks to both streams before publication.

These checkpoints never enter `Draft::load/set_aside/store`,
`Controller::restore/due`, the missing-field initializer or the one-time local
presentation seed. They add no apply request, source write, currentness,
settlement, replay or service authority. Production discovery and G1's fresh
drain remain separate.

`provider_operations(&QtSnapshot, next_settings_bytes)` reads the existing
`speech/providerOperation` checkpoints together through QtStore, the bounded
Settings decoder and the pure pending decoder extracted from `Providers::new`.
The two families are `qt-provider-operation` and `next-provider-operation`,
schema 1, with each settings source's exact byte hash and existing Qt or
`settings-json-v1` format. Both must decode before either stream is returned.

Each private body is
`{kind:"provider-operation",key:"speech/providerOperation",present,cleared,record}`.
Absent differs from explicit empty text (`cleared:true`); neither infers a
current settled operation. An active record retains original machine/request
IDs, host/computer/action and unknown private fields. Existing UUID checks own
identity validation; known actions are `profile_upsert`, `profile_remove` and
`profile_test`. Unsupported shapes/actions/identities refuse the pair. Source
operation text and the complete encoded body must fit the existing 1 MiB bound;
the Next settings input keeps its 4 MiB cap. Attach both source readers' fresh
identity/hash rechecks before a final Staged record.

This does not re-seed an already handled speech setting or create a live
provider page. No status poll, receipt settlement, profile mutation, recording,
credential read or operation replay runs. Credentials remain in their existing
processing-computer stores. Current drain, source discovery, G2 receipt checks
and consumer integration remain separate.

`qt_worktree_snapshots(&QtSnapshot)` preserves Qt's
`workspace/worktreeSnapshots` as `qt-worktree-snapshots`, schema 1, with the
existing Qt source format/hash. `next_worktree_snapshots(bytes)` preserves
`worktree-snapshots.json` as `next-worktree-snapshots`, schema 1, format
`worktree-snapshots-json-v1`, with its exact source hash. It shares the pure
bounded saved-cache decoder with the existing Tauri loader and the existing
catalog shape check with `Catalogs::accept`.

A private `{kind:"worktree-cache-header",present}` precedes
`{entries:[{cache_key,record}]}` pages. Missing Qt keys differ from empty
caches. The original 64-lowercase-hex cache key hashes the original executable,
host and path; it remains opaque without that exact tuple. Never invert it or
infer ownership from the currently selected CLI, alias or lexical path.
Original timestamps, stale/partial flags, checkout fields and unknown data stay
unchanged. They establish no present freshness, process exclusion or cleanup
permission.

The writer's 64-snapshot/8 MiB serialized-cache caps apply; Next's input keeps
the existing 16 MiB read cap, private pages the 64-entry/1 MiB body cap. An
oversized individual catalog, invalid key, malformed shape or source recheck
failure prevents the final record. No oldest-entry pruning, truncation,
redating, live `Catalogs` construction, catalog/status read, checkout access,
creation or removal runs in these adapters. Source discovery and G1's fresh
drain/publication remain separate; evidence stays Staged and grants no action.

`draft_checkpoint(origin, key, manifest_bytes)` preserves original per-draft
metadata through the pure header decoder shared with the existing DraftStore
loader. Families are `qt-draft-checkpoint/<key-sha256>` and
`next-draft-checkpoint/<key-sha256>`, schema 1, format `qt-draft-manifest-v1`,
with the hash of the exact manifest bytes. The existing readers discover and
verify the key/folder binding, process bounded source batches within shared
bundle limits, and attach a fresh file/folder identity/hash recheck.

One private body is
`{kind:"draft-checkpoint",key,record,attachment_bytes_included:false}`.
Original text, cursor/anchor, label, form state, request/voice/transcription
fields, sending/uncertain flags and unknown fields remain unchanged. Optional
older v1 fields stay absent; no defaults or uncertainty normalization is
manufactured. Malformed present fields, wrong key/version and oversized input
or body refuse. The existing 1 MiB manifest/body, eight-attachment, 10 MiB
individual and 20 MiB total attachment-reference limits apply; references
carry fixed lower-case digests and original lengths, never a supplied blob path.

This is metadata/reference evidence only: it neither opens, copies nor verifies
attachment content, including audio. Bytes stay in the existing stores and
read-only draft import path. No live DraftStore/composer, `loaded()` flag
normalization, recording, status request, send, recovery or reseeding runs.
Successful earlier draft seeds remain handled. A consumer needs the existing
blob readers and fresh exact content/receipt checks before any effect; a saved
submitted/sending flag grants no replay authority. Source discovery and G1's
current drain remain separate; evidence stays Staged.

`qt_retained_view_preference(&QtSnapshot, key)` retains exactly these three
fields under the design owner's 2026-10-30 ruling:

| Qt key | Schema 1 family | Target handling |
| --- | --- | --- |
| `workspace/projectFolderSplit` | `qt-retained-project-folder-split` | Private evidence only. Never decode/seed QSplitter bytes, including when Next's Projects folder/worktree splitter lands; Next keeps its default. |
| `tasks/inspectorSplit` | `qt-retained-task-inspector-split` | Private evidence only. The mockup has no board/inspector splitter; never map this onto `workspace/inspectorWidth`. |
| `workspace/removeWorktreeAfterTermination` | `qt-retained-termination-remove-default` | The `.10.17` Terminate checkbox imports once through the existing `termination-default` G3 family only when Next has no value; an explicit current false wins. Its remembered state only prefills the dialog. Every Terminate/removal still needs explicit confirmation; never automatic-maintenance consent. |

The mockup targets are `design/webui/src/screens/Projects.tsx` and
`design/webui/src/dialogs/SessionDialogs.tsx`. The private body is
`{kind:"retained-view-preference",key,present,value}`. `value` retains the
existing Qt decoder's type (`bool`, `int`, finite `double`, `text`, `bytes`,
or a scalar `list`): scalar/list data is in `value`, bytes in `base64`.
An absent key differs from false or an empty text/byte/list value. The source
format/hash stays the existing Qt snapshot's. No splitter or layout decoding,
default conversion, seed/handled record, UI, consent or removal runs.

Each family is separate, so an unsupported field does not mark another as
handled. Raw value and complete body are bounded at 1 MiB, lists at 64 scalars;
unsupported variants, nesting, non-finite numbers and oversized data remain in
the original source without a final family record. Attach the existing source
reader's fresh identity/hash recheck. This is retained Staged evidence only.

`tray_config(bytes)` retains the actual configured `tray.conf` through the
existing `menubar::tray_conf_value` parser. One `tray-config` family, schema 1,
format `tray-conf-v1`, records the exact original byte hash; the shared file
does not become separate Qt/Next copies. The caller pins its actual configured
path (including an override) with the existing reader, and freshly rechecks
file identity/hash after G1's current drain.

The private body is `{kind:"tray-config",text,parsed}`. `text` preserves the
exact UTF-8 comments, line endings, duplicate assignments, unknown keys and
malformed lines. `parsed` contains only present `hgs`, `local-poll`,
`peer-poll`, `terminal`, `icon-color` and `open-mode` strings, with the existing
last-assignment-wins behavior. Explicit empty values differ from absent keys;
no defaults or semantic validity are inferred. Missing files are not invented
as empty input. Invalid UTF-8 or input beyond 64 KiB refuses; the private body
also keeps the 1 MiB cap. Source changes prevent a final manifest.

This is Staged source evidence only: it does not execute text, replace the
selected CLI/configuration, alter polling/terminal/service definitions, seed
preferences or start anything. Existing runtime fallback behavior is unchanged;
discovery and G1's authenticated drain/producer boundary remain separate.

`approval_policies(&QtSnapshot, next_settings_bytes)` inspects both policy
stores before returning either `qt-approval-policy` or `next-approval-policy`
stream. It reuses the existing Qt flag conversion and bounded Next settings
decoder without starting either settings writer. Private snapshots distinguish
an absent key, the former enabled choice and Next's old Qt-at-switch notice;
unknown Next policy fields survive privately. The raw source hashes remain in
the body-free manifest. Malformed flags, unsupported schemas or oversized
policy bodies refuse the pair and retain both originals. Attach each source's
fresh identity/hash recheck before publication. Every snapshot/manifest keeps
`automatic_approvals:false`; evidence grants no opt-in, writes no setting or
approval reservation and never delivers a request. Source discovery and G1's
policy/UI integration remain separate.

## Retained Qt shared organization

On the production switch only, the existing local organization topic queues a
one-time recovery on its organization worker. No GUI/pump/writer lock is held
across Qt, settings or CLI I/O. `swarm::qt_import` uses `QtStore::snapshot` to
read the current `workspace/organization` only. Backups, `swarm/observed*`,
pending edits, local layout/default/collapse/order and run memory are not
initialization sources. Supported project fields are validated within
the existing project/folder/member limits, then the existing shared projection
filters implicit membership and this computer's Windows folders. Source
extensions and local fields remain untouched in Qt for their separate families.
Malformed, ambiguous, future and over-limit input refuses without marking the
family handled. An existing Qt store missing the current organization is not
treated as empty.

The shared pure decoder also follows `SessionOrganization`'s constructor for
the legacy `groups` spelling and obsolete `pinned` bucket. A present `projects`
key takes precedence over `groups`; a malformed `projects` never falls back.
Known missing legacy arrays/color use Qt's constructor defaults. The fixed
General project is appended under Qt's original default-project rule. Pins
return to their exact `before_pin` project when it survives, otherwise to the
validated default, appended in their original order. Qt's move makes those
restored placements explicit. Both shared recovery and local presentation use
this same decoded copy; original bytes, unknown fields, backups, run memory and
evidence hashes stay unchanged. Duplicate identities, a pin bucket with folders,
missing folder IDs and a pin-only cache with no surviving default refuse rather
than discard data or generate an ownership ID. Existing handled records are
never cleared or replayed; a previously refused, unhandled family can retry.

Recovery uses the existing `hgs swarm initialize` / `import_missing` writer:

- `initialize --g3-status --json` takes `{version:1,expected_node_id}` and
  returns `{version:1,node_id,initialized,gui_import_pending}` without saving
  or supervising a service. `initialized:true` alone is insufficient: the
  earlier CLI bootstrap may still await its GUI import. A missing catalog
  establishes its local node through the existing metadata-only `swarm
  inventory`, which does not initialize projects or read Tasks.
- `initialize --g3 --json` takes
  `{version:1,expected_node_id,source_sha256,organization}`. It verifies the
  exact node under the existing catalog lock and calls the original
  missing-field initializer. Existing values, concurrent heads and tombstones
  win. The normal snapshot carries `g3_import:{version:1,source_sha256}`.
  Legacy `initialize` input/output and the closed catalog file schema remain
  unchanged. Old CLIs cannot mistake the wrapper for a project organization;
  a missing protocol response never completes a G3 record.

Before initialization, the existing Settings writer durably saves metadata at
`g3_shared_organization_pending`: version, exact node and the normal G3 family
record (family/schema/source path/time/key names/raw source-byte hash and the
post-switch mtime observation). It contains no operation or source body. Qt's
path/source/hash is freshly rechecked before the command. An exact successful
receipt completes `g3.families.shared-organization` in that same writer. If a
reply is lost, a subsequent same-node status with no pending GUI import can
complete the original metadata without reading Qt again. Unknown/foreign
metadata and failed barriers remain pending; no completed family re-seeds after
rollback. A catalog already initialized by an earlier Qt import is kept as-is.
This is shared project metadata recovery, not G1 evidence publication, a drain
claim, a marker phase or effect authority. Local presentation families remain
separate.

## Retained Qt local project presentation

The first-G3 `organization-presentation` family imports only local `collapsed`,
`order` and `default_project` from the current Qt `workspace/organization`.
The coordinator's explicit 2026-10-30 exception permits these three fields to
fill absent or empty/null Next defaults (`[]`, `{}`, `null`): old Next writers
always serialized those defaults. A nonempty current Next field wins,
independently of the other two, including changes made during the Qt read.
Every other family retains strict key-presence precedence. This exception may
cause one cosmetic change; it never repeats after the family is handled.

The existing production-switch recovery hook queues the read on the existing
organization worker, even without an organization-topic update. No GUI, hub,
pump or writer lock spans Qt I/O. The existing QtStore and G3 family metadata
builder validate the bounded current source and freshly recheck its canonical
path/raw hash. The same bounded legacy groups/pin decoder used by shared
recovery supplies presentation identities and order. Overlapping source/target,
malformed/future shapes and duplicate/oversized identities refuse. Unsupported
local layout files still pause all layout seeds and writes for that startup.

Values and `g3_organization_presentation` commit together in the existing local
`organization-layout.json` writer. The record uses the same family/schema/
source path/time/key names/raw hash/mtime-after-switch shape as Settings G3
records. Unknown layout extensions survive. A failed read, oversized target or
failed file commit cannot leave a completed family on disk; the existing writer
retries pending bytes, and an unhandled family retries at the next startup.
A handled family skips Qt before reading it, across rollback and a second
switch. This never seeds `run_groups`, run tokens, shared membership, project
vivid state or local Windows folders. The design owner's 2026-10-30 ruling
keeps their original bytes in the private organization checkpoint. With the
approved Project color Soft/Bright fill consumer merged (`.66.2.30`), the
Settings first-G3 `project-vivid` family (`.66.5.3.9`, schema 1) imports only
explicit boolean `vivid` fields into `projects/vivid/<exact-project-id>` once.
Every existing Next value, including false, wins. Absent flags remain absent;
names never substitute for IDs. The existing organization decoder normalizes
legacy groups and removes the obsolete pinned bucket. Duplicate, oversized or
malformed identities and malformed candidate flags refuse the whole family,
without partial values; it retries at the next startup. The usual Settings
writer commits values with the handled record, naming the original
`workspace/organization` key and the exact Qt store hash, path, time and
mtime-after-switch condition. A handled family stays handled across rollback;
the Qt store is never written. Vivid never belongs in organization-layout/shared
state. Local Windows drive/UNC folders still wait on the Windows Next Projects local
folder model (`.66.2.31`, G3 dependent `.66.5.3.10`); no independent G3 folder
stack is created. No G1 evidence/marker or effect authority is added.

## Delivery boundary

The shared crate validates no running process and offers no `ReadyForWrites`
boolean. It writes no marker, lock, source store or registration. G1 composes
structural results with fresh runtime evidence before taking writer authority;
the G3 importer and G1 integration are separate implementation slices. Client
settings, local layout and drafts retain their existing owners. G1 takes no
connector, draft or run-layout writer and never restarts agents or other services.

The existing organization-layout owner uses `Layout::try_from_json` before
enabling saves. It accepts the legacy unversioned/v1 shape within its existing
byte/member/run limits and preserves unknown extension fields on later saves.
Malformed/newer/over-limit layouts and repeated member/run identities refuse;
an unreadable original also pauses layout saves and the Qt run seed for that
startup. In-memory presentation changes never authorize replacing the retained
file with defaults. A fresh start must validate it again before writes resume.
`Layout::from_json` remains a compatibility projection for read-only callers;
file owners must not use its fallback to authorize a replacement.
