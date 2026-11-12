# Paged encrypted phone state

Proposed phase 1 design for `zerus-a4mub.47`; no implementation or migration is
enabled by this document. API v1, durable request IDs, original send bytes,
receipt authority and attachment identities stay unchanged.

## Sources and preserved contracts

`origin/main` was fetched at `854477cf137271feef01036be9a9a5618624e1f6`
(batch 5).
Batch 7 is not in that snapshot. References marked **M** use that commit;
**C** uses #2118's `73c2e221c8472eaf3d5d835b8b3238c508f7dcff`; **P** uses
#2123's `ed2bff5551c530cc0313a5f5c3cedc54fd72ca6e`.

| Reference | Source evidence |
| --- | --- |
| A, M | Android [PrivateStore.kt:18](https://github.com/ufna/zerus/blob/854477cf137271feef01036be9a9a5618624e1f6/mobile/android/app/src/main/java/app/zerus/mobile/PrivateStore.kt#L18) selects `zerus_private`; `:21` opens the non-exportable Android Keystore AES key `zerus_private_v1`; `:29` decrypts; `:36` encrypts and commits; `:62` reads `message_state` before the legacy `drafts` fallback. |
| B, M | iOS [Keychain.swift:8](https://github.com/ufna/zerus/blob/854477cf137271feef01036be9a9a5618624e1f6/mobile/ios/ZerusKit/Sources/ZerusKit/Keychain.swift#L8) selects service `app.zerus.mobile`; `:61` selects account `sealed-state-v1`; `:66` reads its 32-byte state key; `:74` uses AfterFirstUnlockThisDeviceOnly. [SealedFileStore.swift:32](https://github.com/ufna/zerus/blob/854477cf137271feef01036be9a9a5618624e1f6/mobile/ios/ZerusKit/Sources/ZerusKit/SealedFileStore.swift#L32) opens AES-GCM with filename AAD; `:39` writes atomically; `:76` maps a name to `<name>.sealed`. |
| C, M | Android [MessageState.kt:22](https://github.com/ufna/zerus/blob/854477cf137271feef01036be9a9a5618624e1f6/mobile/android/app/src/main/java/app/zerus/mobile/MessageState.kt#L22) and iOS [State.swift:415](https://github.com/ufna/zerus/blob/854477cf137271feef01036be9a9a5618624e1f6/mobile/ios/ZerusKit/Sources/ZerusKit/State.swift#L415) contain drafts, outgoing, picker state, context operations, session actions, conversation index, terminal buffers, machine aliases/colors and speech-applied IDs. Android `MessageState.kt:109` defaults missing arrays to empty. |
| D, M/C | Android [MessagePersistence.kt:56](https://github.com/ufna/zerus/blob/854477cf137271feef01036be9a9a5618624e1f6/mobile/android/app/src/main/java/app/zerus/mobile/MessagePersistence.kt#L56) preserves typing/file ownership across enqueue acknowledgement; [OrderedStatePersistence.kt:57](https://github.com/ufna/zerus/blob/854477cf137271feef01036be9a9a5618624e1f6/mobile/android/app/src/main/java/app/zerus/mobile/OrderedStatePersistence.kt#L57) writes before acknowledgement. iOS [AppModel.swift:173](https://github.com/ufna/zerus/blob/854477cf137271feef01036be9a9a5618624e1f6/mobile/ios/Zerus/App/AppModel.swift#L173) (M) loads before creating its writer. Android [MessagePersistence.kt:19](https://github.com/ufna/zerus/blob/73c2e221c8472eaf3d5d835b8b3238c508f7dcff/mobile/android/app/src/main/java/app/zerus/mobile/MessagePersistence.kt#L19) and iOS [AppModel.swift:174](https://github.com/ufna/zerus/blob/73c2e221c8472eaf3d5d835b8b3238c508f7dcff/mobile/ios/Zerus/App/AppModel.swift#L174) (C) warm retention, then still persist the whole state. |
| E, C/P | [services/mobile/vectors/send_receipt/README.md:19](https://github.com/ufna/zerus/blob/73c2e221c8472eaf3d5d835b8b3238c508f7dcff/services/mobile/vectors/send_receipt/README.md#L19) (C) defines exact coverage and the 100-row/1-MiB settled cache; `:39` states the protected-backlog limitation. Android [OutboxRetention.kt:119](https://github.com/ufna/zerus/blob/73c2e221c8472eaf3d5d835b8b3238c508f7dcff/mobile/android/app/src/main/java/app/zerus/mobile/OutboxRetention.kt#L119) (C) preserves the typing fast path. [OutboxRetentionTests.swift:263](https://github.com/ufna/zerus/blob/ed2bff5551c530cc0313a5f5c3cedc54fd72ca6e/mobile/ios/ZerusKit/Tests/ZerusKitTests/OutboxRetentionTests.swift#L263) (P) pins invalidation when only pins change. |
| F, M | Android [WorkspaceJournal.kt:75](https://github.com/ufna/zerus/blob/854477cf137271feef01036be9a9a5618624e1f6/mobile/android/app/src/main/java/app/zerus/mobile/WorkspaceJournal.kt#L75) already uses private SQLite, FULL synchronization and small indexed blob cells (`:90`); iOS [WorkspaceJournal.swift:79](https://github.com/ufna/zerus/blob/854477cf137271feef01036be9a9a5618624e1f6/mobile/ios/ZerusKit/Sources/ZerusKit/WorkspaceJournal.swift#L79) applies file protection and `:121` uses transactions. Their per-connection transport keys are different (`Android :116`, iOS `:130`); do not use those keys or that journal for local compositions. |
| G, M | Android [AttachmentStore.kt:50](https://github.com/ufna/zerus/blob/854477cf137271feef01036be9a9a5618624e1f6/mobile/android/app/src/main/java/app/zerus/mobile/AttachmentStore.kt#L50) assigns reference ownership to private state. [ZerusViewModel.kt:3437](https://github.com/ufna/zerus/blob/854477cf137271feef01036be9a9a5618624e1f6/mobile/android/app/src/main/java/app/zerus/mobile/ZerusViewModel.kt#L3437) currently scans current/durable drafts and outgoing to protect files. A serializer-only change would leave that scan and the global in-memory arrays intact. |

Before an implementation PR, refresh these locations against batch 7's main;
retain these immutable commits as the design's evidence.

## Layout and bounds

Use a **separate native SQLite message store**, in Android's no-backup directory
and iOS's private application-support directory, with mode/protection and backup
exclusion matching F. Reuse the **existing private-state AES keys** in A/B for
every new encrypted cell; expose seal/open operations, not exported Android key
bytes. Opening existing data must look up the key without creating a replacement
when the key is unavailable. Pairing and transport keys remain independent.

Each cell has a fresh GCM nonce and versioned, unambiguous AAD binding store UUID,
generation, record kind, internal ID, payload version and part number. No target,
text, intent, proof or recovery content is stored in plaintext. SQL indexes use
opaque IDs/order and keyed scope/request tokens; their random token secret is
stored only inside an encrypted store-metadata cell, under the same state key.
Tokens use the exact frozen target and original request identity, never current
navigation or Unicode-normalized strings. Index metadata exposes equality/order,
not human-readable scope or content.

| Object | Representation and hard limit |
| --- | --- |
| `message_state` | Fixed root descriptor: format, store UUID, generation, fixed database-root key and minimum reader. Raw descriptor ≤4 KiB; existing encryption/base64 wrappers keep the stored value ≤8 KiB. No per-message IDs or changing counters here. |
| Record | Indexed by original scope/request identity. Encrypted descriptor ≤16 KiB references immutable payload versions. Internal storage IDs preserve duplicate/malformed legacy rows without replacing their durable request IDs or granting authority. |
| Payload | Text, canonical intent, large metadata and opaque legacy fields use ordered 32-KiB plaintext chunks; every persisted ciphertext cell ≤64 KiB. A bounded descriptor carries part count, byte length and digest, not an ever-growing array of chunk references. |
| Index/root metadata | Fixed-size encrypted summaries and indexed rows: scope/frontier, enqueue order, eligible count/bytes, attachment ownership, migration state and last local operation ID. No whole-backlog JSON cell. |
| Reads/cache | Keyset pages ≤64 descriptors and ≤256 KiB; payload reads have a separate byte budget. Global decoded cache ≤128 rows/1 MiB, plus the active composition or explicitly opened payload. Never populate all protected rows into `MessageState.outgoing`. |
| Writes | Stage ≤16 chunks/512 KiB per transaction. Final metadata transactions cap changed rows at 256 and encoded input at 2 MiB; larger maintenance/import work splits before publication. Indexed work is O(log N), plus changed payload bytes and at most the existing 100-row settled pool, rather than O(N protected payloads). |

An atomic user command must fit its final metadata transaction; only invisible
staging and independent maintenance may split. Disk-full or admission failure
preserves the last acknowledged state and composition and releases no send.

Move **all variable collections in C**, including recovery drafts and opaque
unknown fields, into this same store. Otherwise an 8-KiB outbox index would not
bound the complete `message_state` value. Other private keys, such as account or
voice stores, are outside this bound. The database's **total disk usage remains
unbounded with protected user data**; this design bounds each value, batch and
steady-state mutation, not the amount of data a person may retain. Oversized
legacy data is chunked, never clipped to a new admission limit. A logical edit
still costs O(its own changed bytes); a large edit can require several batches.

After cutover, the descriptor stays unchanged. Steady-state writes touch only
SQLite, avoiding both the old message JSON rewrite and Android's whole-preferences
file serialization. Use FULL-synchronized transactions, rollback journaling and
short bounded reads; no foreground VACUUM, checkpoint of an unbounded WAL,
whole-table scan or whole-store re-encryption.

## One commit domain after cutover

Replace whole-state reducers with ordered, revision-checked record commands and
paged views. Enqueue must atomically insert the original outbox identity/payload,
clear the captured draft generation and transfer attachment references. Failed
message-to-draft review has the same atomic requirement. Preserve D's late-typing,
acknowledgement and file-selection semantics; never reconstruct a global snapshot
to reuse an old reducer. Sender blocking uses a fresh indexed scope summary, not
the visible page. Cursors bind store/generation, scope, upper enqueue sequence and
last key; stale revisions refresh the view without changing delivery authority.

Stage immutable payload versions first, then publish their descriptors, indexes,
summaries and ownership in **one SQLite transaction**. A local operation UUID and
revision resolve an unknown commit before another command is accepted. No network
operation is released until durable acknowledgement. Restarted sending/uncertain
lanes retain their original identities and existing recovery rules; migration,
status, paging and missing data never authorize a send or replay.

Keep E's coverage, protected classifications, pin invalidation and global settled
limits. Index pin ownership with record commands; never rebuild an all-row pin
set. Unpinning queues bounded eligibility work, while eviction rechecks current
pins transactionally. Large retirement batches keep reference tombstones until bounded GC
settles them. Attachment files keep their existing encryption and locations.
Replace G's all-row scan with transactional ownership lookups, including picker,
recovery and in-flight owners. Unknown ownership disables deletion. Migration
never deletes attachment bytes.

## Atomic migration and older readers

Run only at cold private-store admission, before any writer, transport or importer
is admitted. One process/file lock covers all message-store entry points. Do not
dual-write a live old and new state. The legacy ciphertext is authoritative until
the root publication below; a `ready` database alone is not authority.

The new root plaintext is an array shaped as
`["zerus-message-store", 2, {"store":"S","generation":"G","root":"R"}]`.
This is deliberate: Android's old `getJSONObject(0)` in A throws, while iOS's
keyed `MessageState` decoder rejects the top-level array. An object containing
only a new version/reference would become an empty Android state (C).
Android also replaces the legacy `drafts` fallback with encrypted `[{}]`, which
fails its required identity decoding, **in the same SharedPreferences editor
commit** as the root. Preserve the old values in the staging backup first.
iOS replaces `message_state.sealed` using the same name and filename AAD, never
unlinking it first. Unknown versions and missing referenced stores fail closed.

| Step | Write order and injected crash/failure recovery |
| --- | --- |
| 0. Admit | Lock, read existing key and authenticated legacy snapshot; keep storage/Send unavailable during upgrade. Failure creates no writer and leaves legacy bytes untouched. |
| 1. Prepare | Create a staging generation tied to a digest of the exact old storage values and their presence bits. Save those ciphertext bytes in chunked encrypted backup rows. Interrupted preparation leaves the old state readable. |
| 2. Copy | Import every collection in bounded transactions, preserving UUIDs, generations, revisions, exact UTF-8, full/compact intent, proofs and attachments. Resume by source digest and ordinal; changed source means a new import, never merging snapshots. Duplicate/unknown rows stay protected. |
| 3. Seal | Verify required chunks, digests, complete inventory and ownership. Retain an exact source backup; only E's already-authorized settled retention may retire redundant rows. Commit authenticated `ready` metadata and synchronize database and directory before publishing a reference. Failure still selects the old state. |
| 4. Publish | Atomically replace the legacy slot(s) with the bounded descriptor/old-reader guards. Android needs one editor commit, not two calls to A's one-key writer, followed by verified preference-file/directory synchronization, serialized with all private-preference writers. iOS needs a durable atomic replacement with file/directory synchronization. Neither Android's commit return nor B's `.atomic` call alone is the durability proof. This is the cross-store linearization point. |
| 5. Acknowledge | Reopen/authenticate descriptor and matching ready generation before opening the new writer. A failed/unknown publication acknowledgement blocks writes and transport; retain both copies. Retry only the identical local publication, or resolve on fresh process startup. Never resume the old writer after an unknown publication. |
| 6. Reclaim | Only after confirmed publication and successful new-store admission, reclaim backup/staging/obsolete versions in bounded, reference-checked transactions. Crash during GC leaves the current published state readable. No uncertain or referenced data is reclaimed. |

On restart: **legacy slot → legacy state only; v2 descriptor → matching complete
generation only**. A v2 descriptor with missing/corrupt data is a storage error,
never an empty state or an automatic fallback to an older snapshot that could
hide acknowledged writes. SQLite owns atomicity of subsequent record changes.
The crash contract assumes qualified atomic-replace/FULL-sync filesystem
primitives; corruption/key loss is preserved and reported, not repaired as empty.

The initial legacy authentication/decode can still require O(legacy value size)
memory with A/B's current APIs. It runs off the UI thread, once, with bounded
output batches. Allocation, authentication or disk-space failure must leave the
old ciphertext intact; no hard legacy row/byte cutoff is allowed. This is not a
claim of bounded-memory legacy decoding.

## Shared fixtures and qualification

Add `services/mobile/vectors/message_store_v2/`, mirrored into Android test
resources and consumed by ZerusKit as the send-receipt vectors are. Freeze the
common record/descriptor grammar, AAD bytes, synthetic key/nonce crypto vectors,
platform legacy wrappers, ordering, limits and recovery decisions. Cover legacy
pre-part-C/full/compact/unknown rows, Unicode byte distinctions, duplicate IDs,
oversized payloads and attachment ownership. Production nonces remain random.

For **each numbered migration step**, inject failure before and after its durable
effect and restart a fresh reader; for step 2, fail both mid-chunk and between
record/index batches; for step 4, fail before replace, after replace and after
durability but before acknowledgement. Assert exactly the old or complete new
logical state, unchanged protected bytes/IDs, and zero transport calls. Also kill
publish-before-ready, partial-index, empty-fallback and skipped-fsync mutants.
The fixture crash model retains only synchronized writes, so a skipped sync
cannot pass merely because the test host happened to flush its cache.
Freeze all still-distributable older readers, including Android's legacy-drafts
path: after publication every one must report incompatible storage, not empty.
Any reader that swallows the marker blocks platform enablement.

Test normal enqueue, failed-to-draft transfer and GC at each transaction boundary;
missing/foreign/reordered chunks, wrong AAD/key/generation and unknown schemas
must not grant authority. Reuse E's exact receipt, no-replay, typing and pin-only
tests. At 100, 10,000 and 100,000 protected rows, instrument maximum cell/batch
size, loaded rows and touched objects: draft edits must not walk the outbox.
JVM models alone do not qualify SharedPreferences crash behavior: use disposable
Android CI emulators (supported minimum API and current API), native temporary
SQLite/atomic-file tests on Mac, and the unsigned iOS simulator build. No owner
phone, signing, display or installation is part of this design phase.

## Proposed PR slices

| Order | Change | What it proves before proceeding |
| --- | --- | --- |
| 1 | Common grammar, fixtures, legacy-reader/marker characterization and key-provider admission interfaces; disabled. | Existing keys/names stay usable; every supported old reader refuses the new marker; no empty fallback. |
| 2 | Android encrypted row/chunk backend, indexes, staged payloads and root admission; disabled. | Native transaction durability, limits, missing-key refusal and every backend fault point. |
| 3 | Matching ZerusKit backend; disabled. | Shared vectors and crash decisions agree; native Mac tests and iOS CI compile it. |
| 4 | Android record commands, all MessageState collections, ownership ledger and bounded UI queries; disabled. | No global protected-row hydration; existing races, send identities, protected data and typing budget hold. |
| 5 | Matching Swift commands/paged consumers; disabled. | Same behavioural contracts and bounded work on the publishing actor. |
| 6 | Cross-store migration/admission plus bounded GC on both platforms; disabled. | Every step's RED/GREEN crashes, older-reader refusal and old/new snapshot equivalence; reference loss never deletes data. |
| 7 | Enable Android cold-open migration. | Real platform restart/failure matrix and large synthetic protected backlogs pass; no owner-device operation. |
| 8 | Enable iOS cold-open migration. | iOS CI/native filesystem matrix passes against the same fixtures; no device or signing operation. |

Do not enable either writer until its complete path and counterpart's format
recognition are qualified. No live migration, implementation commit or rollout
is authorized by this phase-1 draft.
