# Dormant Linux Poster Class M source audit and measurement contract

Status: source audit and measurement design, 2026-11-01. This is an
allocation-family inventory, not an exhaustive source proof. SDK executions and
allocator measurements for this audit are zero. No numeric SDK heap bound or
whole-profile qualification is established.
The production allocator/metadata/whole-profile gate remains open.

The baseline is the dormant startup from PR #822: one original worker, one
concrete async-io Unix connection, one socket-reader task, two logical receivers
over one raw16 queue, sequential Hello/AddMatch/RemoveMatch calls, and no body
consumer or production constructor caller. F=65,536 bounds the serialized frame,
H=4,096 bounds declared fields length and A=16,384 bounds cumulative auth input.
The E2 reservation B=2,097,152 covers its enumerated wire/text/private envelope
only. None of the following changes that reservation, role admission or release.

The audit is pinned to zbus 5.19.0, zvariant 5.15.1, zvariant_utils 4.2.0,
zbus_names 4.3.5, async-broadcast 0.7.2, async-executor 1.14.0, async-task 4.7.1,
async-io 2.6.0, blocking 1.7.0, event-listener 5.4.2 and the actual locked Rust
dependencies/toolchain. A future measurement must record the exact lock,
compiler, target, feature closure and source/binary hashes, including auxiliary
allocator/queue sources; a version label alone is insufficient.

## Decoder input frontier

`zbus/src/message/header.rs` defines PRIMARY_HEADER_SIZE=12 and minimum wire
header size 16. `Message::from_raw_parts` in `message/mod.rs` passes the remaining
`bytes.slice(PRIMARY_HEADER_SIZE..)` to Fields deserialization. In
`zvariant/src/dbus/de.rs`, `ArrayDeserializer::next` runs `seed.deserialize`
before testing whether the position exceeded the array end. A nested
`ValueDeserializer` receives the remaining byte slice too.

Thus H constrains a declaration, not every byte reachable during malformed nested
decoding. A child can inspect remaining frame/body bytes up to F before the
outer-array rejection. Its temporary allocations and error construction must be
audited against that frontier. This is source-derived reachability, with zero
runtime reproduction, heap measurement or exploit claim. PR #826 separately
corrected the earlier overbroad H wording.

## Reachable allocation families

Paths in this table are package-relative pinned source locations. The design
packet SHA-256 is
`08af4cba775212d10598aca23e65a769bfa6d692d57abaa979d4398e6cab01cd`.
It contains 64 complete locked source exports and 16 logged source observations;
its original proposal is superseded by the measurement requirements below. G1
and G3 independently verified its source hashes. Numbers in the table denote
topology or protocol limits, never allocation byte estimates.

| Family and source | Reachable storage and lifetime | Required later observation |
| --- | --- | --- |
| Fields visitor: `zbus/src/message/fields.rs`, `FieldsVisitor::visit_seq` | Each tuple decodes a FieldCode and generic Value before converting to its required field type. Duplicate known fields replace previous values. Strings normally borrow message data; a wrong-type nested container can allocate before rejection. | Late error after an expensive child; duplicate/replaced fields; type conversion and name-validation errors; do not assume H bounds child work. |
| Generic Values: `zvariant/src/value.rs`, `SignatureSeed`, `ValueSeed` | Arrays own a Vec of Values, dictionaries own BTreeMap nodes, structures own a Vec of Values; variants box nested Values. Container constructors clone signatures. Invalid fields can create these temporary trees even though no successful body consumer exists. | Simultaneous partial tree, signature copies and error allocations through the scalar-normalization boundary. Cardinality, depth and capacity high-water cases. |
| Container storage: `zvariant/src/array.rs`, `dict.rs`, `structure.rs` | Vec growth and BTreeMap node allocation are distinct from wire bytes. Replacement of duplicate dictionary keys can release earlier values; that does not erase their transient peak or cumulative cost. | Requested capacity, replacement peaks, completion and destruction, including errors after many entries. |
| Signature parsing: `zvariant_utils/src/signature/{mod,child,fields}.rs` | Dynamic Child owns Box<Signature>; dynamic structure Fields owns a boxed slice built from a Vec. Clone is a deep clone for dynamic children/fields, while static children can share references. A variant is parsed by the Value visitor and again by its value deserializer. | Complex signatures, repeated parse/clone overlap and failure during parsing; do not count one wire signature as one allocation. |
| Cached header: `zbus/src/message/{mod,fields}.rs`, QuickFields | Message owns an Arc<Inner> containing Data and OnceLock<QuickFields>. FieldPos caches byte ranges; QuickFields clones the body signature. The original decoded header and cache overlap during construction. Message clones share Inner; they do not create a second complete cached tree. | Header/cache overlap, retained metadata for distinct queued messages, and final Arc destruction after both readers/current message disappear. |
| Decoder limits: `zvariant/src/container_depths.rs`, signature parser | Data decoding checks structure/array depths and a total container depth. Signature parsing separately checks its structure/array depth. These checks restrict recursion but are not heap byte budgets. | Limit-edge and over-limit errors with already allocated child state; nested variants/arrays/dictionaries/structures. |
| Decoder/name errors: `zvariant/src/error.rs`, `dbus/de.rs`, `zbus_names` validators | serde custom errors allocate display Strings; signature mismatch errors can own a cloned Signature plus expected text. Array overrun constructs formatting while the child result still exists. Name error variants own diagnostic strings. | Actual pre-normalization peak, error-string/signature lifetime and every conversion layer; post-normalization scalar errors alone cannot establish this peak. |
| MethodError/control/auth: `zbus/src/error.rs`, handshake modules | MethodError conversion owns name/detail and shares its Message. Error clones can copy text/signature metadata. REJECTED/DATA/error formatting has separate transient allocations. E2 already counts specified text/wire lanes; Class M still includes their allocator/Arc/Vec bookkeeping and diagnostic construction. | Maximum auth boundary and token counts, malformed control replies, error/detail conversion, sequential call teardown; do not double-count or silently subtract E2 credits. |
| Raw broadcast: `zbus/src/connection/mod.rs`, `async-broadcast/lib.rs` | Connection initially creates broadcast(DEFAULT_MAX_QUEUED=64), then Builder sets the logical capacity to 16 before starting the socket reader. async-broadcast initializes VecDeque storage at its original capacity; reducing logical capacity does not shrink that storage. Inner Arc/Mutex, receiver state and Event listeners also allocate. | Initial construction, raw16 saturation plus blocked current broadcast, both receiver destructions and queue allocation destruction. An empty queue can retain capacity. |
| Pending calls: `connection/pending_method_calls.rs` | Each sequential call creates broadcast(1), enters a HashMap and owns a receiver. Removing a call does not imply map capacity was released. fail_all collects pending senders in a Vec; the closed error and clones are scalar after the ingress normalization. | Setup/removal call phases, pending receiver disposal, fail_all, retained map capacity until connection destruction; capacity-one is separate from raw16. |
| Connection/async locks: `connection/mod.rs`, async-lock/event-listener | Arc<ConnectionInner>, shared sender map/status, empty/lazy maps/OnceLocks, mutex/wait listener state. No object server, filtered subscription or requested name may be added. Credential cache allocation is conditional; this worker does not request a connection getter. | Constructor high water, lazy first poll/wait allocations, exact handle destruction; enumerate current initialized fields rather than assuming every optional cache exists. |
| Futures/tasks/wakers: async-trait expansion, zbus executor wrapper, async-executor, async-task | Boxed trait futures retain their captures across await. Executor state has a runnable queue, active-task slab and sleeper vectors/wakers. async-task allocates a task block containing header/future/output/scheduler storage. The pinned tick and each readiness registration retain real owners. | First spawn, repeated Pending/Ready cycles, raw progress while tick stays pending, cancellation and last capture destruction. An empty executor is not an allocation deallocation hook. |
| Shared async-io reactor | Once-only global reactor/driver, poll registration/slab/timer/waker storage and its capacity. Per-startup registrations/captures must still disappear. Global infrastructure can outlive a joined startup. | Separate cold and warmed-process results, allocation origins and capacity reuse. No subtraction of an unexplained baseline or claim that the global driver joined. |
| Blocking close pool: zbus Task::spawn_blocking, blocking/lib.rs | The underlying Unix close uses a blocking job. Job/task/closure storage and the captured socket are per-operation; global pool queues/threads and their capacity may persist. Thread/native stack allocations are outside a Rust GlobalAlloc counter. | Actual close job return/capture destruction before clean completion; timeout/abandonment stays unknown and pinned. A cancelled outer future is not proof that a blocking job ended. |

Unknown/unused paths remain explicit. The audit does not adopt GVariant, a Tokio
executor, FD delivery, a body deserializer or optional server/stream APIs. An
unknown feature closure, newly initialized cache/task or competing origin stops
qualification until its source and lifetime are included.

## Measurement surface and approval boundaries

Instrumentation is a later, separately scheduled prerequisite. This design adds
no runtime code, manifest, feature, export or origin. Before any instrumentation,
a fresh G1/G3 agreement must pin all of the following in one exact code plan:

- Test entry point, crate/target and allocator symbol, `cfg(test)` isolation,
  compiler, target ABI, libc, concrete System implementation, complete feature
  and dependency closure, and source/binary identities.
- Nonallocating callback algorithm, static layout and actual compiled size,
  synchronization, in-flight states, identity/generation handling, recursion,
  report protocol, and every unsafe/layout invariant.
- Fixture generators and their source hashes, fixed scalar phase hooks, exact
  cold and warmed run topology, finite case/campaign deadlines, address-space
  and other resource limits, output/log ceilings and owned-child cleanup.

The subsequent implementation requires fresh pinned reviews before separately
scheduled private execution, followed by raw proof review. This document grants
no instrumentation approval, execution window, or numeric memory admission.

The candidate measured child uses the pinned startup and dependencies without
a production constructor/export or SDK fork. A cold child executes exactly one
original startup. A warmed child records its exact preparatory startup and
finite repetition count separately; it is a different experiment. Each attempt
keeps its original worker/connection, clean-join evidence and retained records.
Unknown attempts stay spent and pinned; never rerun one for a cleaner success.

The private bus and traffic producer run in separate owned processes outside
the measured address space. In-child generators, hooks and observers remain
included and identified. The environment is built from scratch with private
HOME/HGS/XDG/TMPDIR/TMUX_TMPDIR and private D-Bus, plus private Xvfb if needed.
No owner display, bus, account data, GUI, native agent or service is involved.

## Probe coverage and ordinary allocator behavior

The candidate `cfg(test)` GlobalAlloc wrapper delegates alloc, alloc_zeroed,
realloc and dealloc to unchanged System behavior. It preserves layouts,
alignment, return values, null/failure and realloc semantics. There is no
allocation header, copying realloc, callback allocation, formatting, logging,
OS I/O, unwinding or SDK patch. Its symbol must be absent from production
binaries and normal parallel test campaigns; the exact measured child runs only
its separately approved test. A disabled pass-through control is a separate,
entirely unmeasured, never-armed process.

A measured child tracks identities from its first applicable allocator callback
through final reporting, including pre-epoch allocations, all participating
threads and cross-thread frees/reallocations. State must be statically
initialized and nonallocating. It cannot pass through early calls and later arm
an incomplete table, forgive unmatched preexisting events, or claim an absolute
all-process live baseline reconstructed after the fact. If early tracking cannot
be established, the baseline/history is UNAVAILABLE; a narrower coverage claim
requires a separately reviewed explicit restriction. No lazy TLS, lock or
initialization allocation, or recursive event, may silently disappear.

Candidate ceilings are 262,144 table entries, at most 16 MiB TOTAL static probe
metadata including padding, counters and generations, at most 64 KiB report and
4 MiB raw log per child. These are probe limits, not product B or an SDK budget.
The actual compiled footprint is reported and included in the future
address-space/resource setup. Pointer identities stay private and are never
printed. There is no unbounded per-message event history.

Any exhaustion, counter/generation wrap, duplicate or unmatched identity,
layout mismatch, recursion or ambiguous concurrent event makes the entire
observation sticky UNAVAILABLE. Never evict/fabricate a record, ignore an event
or claim a low-memory success. Even after such a fault, preserve ordinary System
behavior: do not suppress or defer a real deallocation, change a layout/result,
free the old allocation after a failed realloc, unwind or deadlock. A probe
fault grants no product refund or startup retry.

Tracking safety is not established here. The exact later algorithm must prove
linearization and in-flight representation for concurrent allocation, free and
realloc, including cross-thread frees, pointer reuse while a System callback is
returning, ABA, recursion and report boundaries. It must prevent duplicate
ownership, lost reuse and temporary undercount, or mark the observation
UNAVAILABLE while maintaining allocator behavior.

## Requested-byte metrics and epochs

Define these metrics precisely at intercepted API boundaries in the code plan:

- Attempted versus successful cumulative requested bytes and call counts,
  separately identifying alloc, alloc_zeroed and realloc. A failed allocation
  creates no live identity. Counter overflow makes the observation unavailable.
- Current live requested bytes and the peak at the defined callback accounting
  boundaries. These are logical requested-byte interface metrics. They do not
  observe System realloc's internal old/new physical overlap, allocator usable
  bytes or internal transient/physical peaks; those remain unmeasured.
- Retained allocation identities associated with each finite epoch after each
  explicit boundary, including preexisting buffers resized in that epoch.

A failed realloc preserves the original record, size and ownership. Successful
moved and in-place realloc preserve the original allocation identity/generation
and record the resize epoch. Address reuse after deallocation gets a new bounded
generation. The exact accounting/linearization of a successful resize is part
of the separately reviewed algorithm, not an inference from returned pointers.

Epoch transitions label existing records; they never clear the pointer table,
reset away live generations, stop cross-thread tracking or discard warm-up
retention. The association rule must be bounded and non-forgetting across the
finite campaign: birth plus latest resize alone cannot erase earlier retention
attribution when a buffer grows, shrinks or changes again in later repetitions.
Warm-up remains tracked through its actual clean join into subsequent epochs.
Report baseline, growth and retention separately without net subtraction.

The Rust requested-byte total includes wire/text storage and metadata. Do not
subtract B, 24F, snapshots or unknown allocations to manufacture a Class M heap
number. Keep composite totals unless a source-backed, independently reviewed
classification proves a disjoint split. Unclassified Rust allocations remain
visible, including those made by shared threads. A counter/capacity value is
never completion evidence.

## Observation phases and retention controls

1. Track from the first applicable callback. Before original group admission,
   address preparation or worker handoff, identify source/toolchain/features,
   binary, private custody and the pre-epoch all-process live baseline. Probe,
   harness, global infrastructure and preallocated reporting storage are
   included or separately identified; none is silently subtracted from total
   physical/whole-profile use.
2. In the cold one-startup child, retain shared reactor/pool initialization in
   the result even if a future envelope explicitly excludes identified
   persistent infrastructure. A warmed experiment cannot erase the cold result.
3. In a warmed child, record the exact preparation and finite repeated-start
   count, each original clean join, source-backed global capacity reuse and
   every retained per-startup registration/capture. Do not delete records or
   reset away unknown histories between attempts.
4. Observe auth, Hello, each match call, raw activation, maximum queue/current
   message overlap, both-reader destruction, each removal, original close job,
   actual socket-reader/executor completion, connection/executor drop, wake
   capture disappearance and exact original join. Hooks record fixed scalar
   phase IDs without allocating; they must not move the phase, introduce a
   retained Message or add a control call.
5. Freeze a coherent observation at an explicit boundary, not the allocator or
   outstanding owners. Tracking continues while shared reactor/pool threads
   can allocate/free after join and during reporting. Identify post-boundary
   and reporting epochs. Preallocate output storage and include/identify its
   cost; never discard unknowns to make reporting possible. Observer costs
   stay included and identified or require a separately reviewed explicit
   coverage restriction.
6. Pair every successful retention observation with the existing exact graph
   destruction and original join. A retained allocation needs a source-backed
   owner/exclusion or stays unresolved. Timeout, lost handle, panic, poison,
   cancellation, OOM or missing final report is UNAVAILABLE and unknown/pinned
   as applicable, never a successful upper bound. Do not omit its corpus case.

The future runner has separately agreed finite per-case/campaign deadlines,
address-space/resource limits, report/log ceilings and exact owned-child cleanup.
Raw hashed logs include executed, refused and unknown case counts. It never
changes owner limits, signals unrelated processes or treats a killed fixture as
a clean join. Stable/zero bytes, public-handle loss or process exit grants no
Gate Idle, B refund, native-effect settlement, replay, role handoff or production
permission. Only the original clean graph plus exact CleanExit and join releases
B under the existing lifecycle contract.

## Required later corpus

The later code plan must bind each case to a generator/source hash and distinguish
source controls, probe controls and actual SDK executions. Proposed groups:

- Legal minimal/maximal F and H inputs in both endians, fragmentation at each
  primary/header/body phase, full raw16 plus current broadcast, sequential
  control success/MethodError/invalid replies and maximum A authentication.
- Complex signatures, duplicate known fields and last-value replacement;
  wrong-type header Values containing arrays, nested variants, dictionaries
  and structures; limit-depth and over-depth inputs; invalid UTF-8/name/type
  conversions and late truncation after a large partial tree.
- Malformed declared outer fields lengths with a nested value extending into
  remaining frame/body bytes, all within F. Compare early and late rejection;
  do not filter the case solely by H or reconstruct an unsafe Message.
- Scalar error normalization versus peak before normalization, partial frame
  cancellation, early/unarmed EOF, the exact approved empty local-close EOF,
  FDs, failed setup/removal/close, and original deadline expiry.
- Cold/warm starts, repeated maximum and mixed valid/error traffic, exact
  successful join, lost public handles, delayed resource destruction and
  unknown outcomes. Unknown attempts are spent; never replay for a cleaner
  measurement or classify their pinned fixed group as a leak-free success.
- Probe correctness: cross-thread allocation/free, pointer reuse, moved/in-place
  and failed realloc, zeroed/aligned allocations, table/counter/generation
  exhaustion, identity/layout mismatch, recursion/unmatched events, concurrent
  pointer reuse/realloc, report-boundary races and early process death. A probe
  failure cannot become a low-memory success.

## Claim boundaries and remaining gates

A Rust GlobalAlloc probe observes requested Rust allocation sizes. It does not
measure allocator size classes/usable bytes, fragmentation, freed-but-retained
pages, C/GTK/native allocations, stacks/TLS mappings, FD/ancillary/kernel buffers,
external tracing sinks or RSS. Shared reactor/pool infrastructure and its
capacity must be reported separately, with source evidence, rather than assumed
gone. Any unclassified retained allocation stays visible.

Finite corpus observations are measurements, not an exhaustive upper bound. A
later numeric requested-allocation envelope needs a complete source/lifetime
argument plus those observations, exact exclusions and fresh G1/G3 acceptance.
A physical allocator/whole-profile memory claim requires additional separate
qualification over the actual complete G1 host/profile. Neither this audit nor
a future counter pass closes those gates or changes startup's budget/release.

No current producer, prepared-startup proof, negative consumer, Original factory,
store/writer, native ID, Notice/PostBatch, Notify/CloseNotification, app sink or
production role permission is introduced. G1 owns any future handoff and whole
profile qualification. Dormant owned-store persistence, if selected next, keeps
its own separate pre-code contract and private-path qualification.
