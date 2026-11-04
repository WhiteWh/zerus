# Durable quit store and custody proposal

Status: architecture accepted for the second G3/native/security agreement
(`zerus-5wmh.66.2.34.3.3`); the Git-lock amendment below is proposed for a fresh
G3 and native/security agreement (`zerus-5wmh.66.2.34.3.4.1`). The merged pure
model is documented in
[desktop-quit-journal.md](desktop-quit-journal.md). This proposal adds no runtime
implementation or positive delivery. Current real Quit still drops pending
requests. Windows delivery remains fenced pending its PC/kernel qualification.

## Native namespace and isolation

The production namespace belongs to the effective OS user, across all CLI,
configuration, account, installation and GUI generations. It uses the passwd
entry's `pw_dir`, obtained with bounded `getpwuid_r`, rather than `$HOME`:

- Linux: `<pw_dir>/.local/state/hgs/quit-v1/`.
- macOS: `<pw_dir>/Library/Application Support/hgs/quit-v1/`.

A small fixed bootstrap record beside that directory binds its UID, schema,
random native root identity and directory/database identities. The fixed bootstrap
lock/record distinguishes first use from missing initialized state; an incomplete
first-use cut requires operator recovery rather than another automatic attempt. The root is 0700 and files
are 0600, with pinned no-symlink walks and owner/type checks. Existing unsafe
permissions are refused, never repaired by normal startup or Quit. The parent
walk must refuse writable/foreign ancestors; system-owned ordinary ancestors
need not themselves have mode 0700. No selected state/config or temporary root
can substitute for this anchor. The store/custody filesystem must also have
qualified local locking and sync semantics; unknown/network filesystems refuse
rather than falling back to another folder.

**Isolation is checked before opening, creating or stat-probing any production
anchor, including its bootstrap and parent state directories.** Resolve the OS
profile as data, compare the environment lexically, then return Unavailable if:

- HOME is set to a different path from `pw_dir`;
- XDG_STATE_HOME, XDG_DATA_HOME or XDG_CONFIG_HOME is set away from the respective
  default under `pw_dir` (`.local/state`, `.local/share`, `.config`);
- HGS_STATE_DIR or HGS_CONFIG_DIR is explicitly set, including an empty,
  non-UTF8 or exact-default spelling;
- HGS_INSTALL_PREFIX is set (the maintained tray installer treats an explicit
  prefix as scratch);
- ZERUS_CLIENT_HGS or the original `--hgs` command-line option selects a CLI;
- HGS_SELF, HGS_PEERS or HGS_SSH explicitly changes configuration/fleet routing; or
- an explicit test/replay/headless fixture marker is present. The implementation
  inventory must include the private built-app and Phase4 smoke markers.

Absent overrides mean defaults. Empty or non-exact override spellings fail
closed; do not resolve `..`, symlinks or alternate spellings to turn an override
into a default. A lexical comparison cannot establish native ownership; its only use
here is refusal before I/O. Every store entry point shares this guard; an early
startup availability check cannot bypass it at a later Quit or recovery read.
An isolated launch retains drop-on-Quit and starts no preparer, helper or status
subprocess through this protocol. The future refusal presentation uses the
registration-time Undo toast note and Quit-time diagnostic rules below; today's
toast and bounded local stderr drop line remain unchanged until its caller lands.

### Original environment and entry-point provenance

Capture an immutable native `OriginalEnvironment` at the beginning of both GUI
and hgs entry points, before config loading, option-induced environment writes,
threads or any anchor I/O. Capture presence and raw OS strings, not `env::var`
success: empty and non-UTF8 values are explicit overrides too. Also retain the
original OS arguments, including `--hgs`. Missing or late origin evidence refuses.
This opaque type has no Deserialize, JSON constructor or environment allow flag.

Two existing mutations make a late snapshot unsafe: `src/config.rs::Config::load`
unconditionally sets HGS_STATE_DIR after resolving its default, and the client's
`lib.rs` sets ZERUS_CLIENT_HGS from `Options::hgs`. Record a known native default
derivation separately from the original observations. Config loading may derive
the default only from an originally default scope; it cannot overwrite the fact
that a user supplied an explicit selector, even if its bytes equal the default.
An original `--hgs` remains a refusal after its conversion to an environment key.

Every startup, recovery and real-Quit entry revalidates the original classification
against its actual current context and the exact recorded native derivations.
An unexpected subsequent selector/change refuses before any production probe.
Do not cache a permissive Boolean or recapture after stripping overrides. A later
entry cannot borrow an earlier permissive check after its context has changed.

Helper normalization is possible only from a genuinely admitted native parent
whose original scope passed. It may construct a minimal child environment using
that parent's verified default configuration derivation; it may never erase an
original explicit override to obtain admission. The child captures its received
environment before its own Config::load and checks consistency with the parent's
sealed native derivation. This is not independent evidence of the GUI's original
environment. That classification is necessary but insufficient: original-stdin
nonce, actual parent-observed child
birth, sealed registration and inherited exact custody must establish the parent/
helper origin graph described below. A caller JSON field, environment flag or
decoded classification cannot mint ProductionAnchor or live-worker authority.

The initial slices deliberately use the strict selector refusal above. This also
fences the **maintained macOS LaunchAgent**, which always passes `--hgs`, and any
Linux launcher passing it. Do not describe those launches as production-admitted.
Coordinator-approved child `zerus-5wmh.66.2.34.3.5` must qualify exact selected CLI
and configuration origin before durable quit is enabled **anywhere**. It must
bind the verified executable identity, configuration and fleet, rather than flag
presence, and qualify both the Mac LaunchAgent and arch `tray/install.sh` launch
shapes with private real launches. Fake, old, changed or mismatched executables/
configuration refuse. This proposal supplies no exception or current caller for
those shapes; that child requires its own concrete G3/native/security agreement.

The separate [selected-CLI origin proposal](desktop-quit-cli-origin.md) uses one
trust root per platform shared with G1: the existing own sealed signed Mac pair,
gated on paired installation and genuine qualification, and a proposed Linux
build-paired digest plus descriptor/actual-executed-image binding. It preserves
the strict fence until qualification, binds immutable original configuration and
fleet derivation, and makes a stale GUI/CLI pair unavailable for durable quit.
It adds no current admission, caller or replacement pairing mechanism on Mac.

### Selector and marker inventory

The implementation review must export a complete source-derived inventory of
every HGS_* and ZERUS_CLIENT_* variable actually read by its GUI, loader, helper,
transport and lifecycle call graph, with source location and classification.
Audit both direct reads and wrapper/prefix reads, original arguments and loader
writes. The current source inventory establishes these required classes:

| Class | Current names / source boundary | Initial production rule |
| --- | --- | --- |
| State/config/install | HGS_STATE_DIR, HGS_CONFIG_DIR, HGS_INSTALL_PREFIX; config.rs, machines.rs, settings/skins.rs | Any original explicit presence refuses, including default-looking bytes |
| CLI/fleet | ZERUS_CLIENT_HGS, `--hgs`, HGS_SELF, HGS_PEERS, HGS_SSH; client cli.rs/lib.rs/core lib.rs, config.rs, platform/mod.rs | Any original explicit presence refuses; no PATH/legacy substitution for a refused selection |
| Test/native-host replacement | All HGS_TEST_*; HGS_HOST_ALLOW_UNSTAGED, HGS_HOST_IMAGE, HGS_EXECUTABLE, HGS_LINK_MARKER; host, swarm and platform modules | Any presence refuses; test hooks cannot select production authority |
| Replay/smoke/fixture/performance | ZERUS_CLIENT_REPLAY, ZERUS_CLIENT_REPLAY_INTERVAL_MS, ZERUS_CLIENT_HEADLESS, ZERUS_CLIENT_SMOKE_FILE, ZERUS_CLIENT_SMOKE_NOW, ZERUS_CLIENT_UPDATES_FIXTURE, ZERUS_CLIENT_VOICE_CAPTURE, ZERUS_CLIENT_PERF and all ZERUS_CLIENT_PERF_*; client commands/display/lib/updates/voice modules | Any presence refuses, even an empty or false-looking value |
| Process/session/lifecycle inputs | HGS_ACCOUNT_ID, HGS_AGENT, HGS_ARCHIVE_ID, HGS_CLIENT, HGS_EVENTS, HGS_EXPECTED_ID, HGS_FALLBACK_SECS, HGS_FORK_AGENT_HINT_REVISION, HGS_FORK_PARENT_ID, HGS_FORK_SOURCE_NAME, HGS_FRESH, HGS_FRESH_REPLACES, HGS_HOST_PANE, HGS_HOST_SESSION, HGS_LAUNCH_ID, HGS_REQUESTED_ID, HGS_RUN_ID, HGS_SESSION, HGS_TRACKING; cli.rs, host and state modules | Original presence refuses in these initial slices; none supplies native quit authority. Later native child inputs require explicit reviewed provenance |
| Other behavioral controls | HGS_AGENT_HINT, HGS_AGENT_HINT_OFF, HGS_AGENT_HINT_OTHER, HGS_ANSWER_CHANNEL_SECONDS, HGS_CLAUDE_PROJECTS, HGS_MOBILE_ALLOW_INSECURE_LOCALHOST, HGS_MOBILE_CONNECTOR; config/accounts/phones/state modules | Original presence refuses pending specific call-graph qualification; no mobile startup is part of this protocol |
| Presentation-only inputs | HGS_LANG, HGS_TAB, HGS_TAB_COLORS; ZERUS_CLIENT_NOTIFICATIONS, ZERUS_CLIENT_QUERY, ZERUS_CLIENT_RENDERER, ZERUS_CLIENT_SINGLE_INSTANCE, ZERUS_CLIENT_WATCHDOG | No selector exception or admission authority; retain exact original/current observations and prove no effect on the admitted CLI/configuration/resources. A test mode still refuses through its marker |
| Unknown future prefixed key | Any unclassified HGS_* or ZERUS_CLIENT_* | Refuse before production I/O until explicitly inventoried/reviewed |

HOME/XDG comparisons remain the lexical OS-profile rules above. The inventory is
not permission to read an alternate folder. Qualification includes defaults both
before and after Config::load, HGS_CONFIG_DIR alone, each explicit normal-looking/
empty/non-UTF8 override, changed later context, `--hgs` conversion, helper handoff
and refused-marker combinations. All refused cases count **zero attempted**
production parent/bootstrap/anchor probes, not merely zero successful opens.

A test-only typed `FixtureAnchor` exercises storage/kernel mechanisms in private
folders and cannot construct `ProductionAnchor` or positive production admission.
There is no normal path/environment setting that promotes it. Prefer cfg(test)
or an explicit non-default fixture feature. Private application proofs use a
synthetic OS-profile/decoy view, never inspect the owner's real anchor, and record
before/after inode/mtime plus zero anchor-probe calls. A fixture I/O observer must
refuse every attempted production probe while counting it, so a failed guard
cannot touch the real anchor and the test still fails on the attempted access. Linux can additionally use
a private mount/user namespace so even a failed guard can reach only the decoy.
The same real Quit/exit hook must freeze pending actions and report isolation;
checking an unused standalone policy function is insufficient.

## Bootstrap, existing state and private I/O

Fresh initialization has one fixed bootstrap lock/record, one root directory and
fixed database names. Create with no-replace/private flags, fsync files and parent
entries, and publish Ready only after the database schema is durable. A cut in
initialization leaves a fixed, bounded incomplete state. It never creates another
staging name, root identity or generation on a later attempt. Existing bootstrap
with a missing/changed root, database or header is Unavailable. Startup reads do
not initialize anything. A Ready store with a hot rollback journal is also unavailable to normal opens;
only the explicit operator recovery path may let SQLite roll it back. Before
opening SQLite, a bounded raw header read verifies its application/schema/page
identity on the pinned existing file. Operator recovery uses the same bounded
VFS and does not clear logical reservations. It must preserve original
request tombstones and all unresolved resource charges; there is no automatic
reset, delete-and-recreate or fresh-generation repair.

Proposed shared implementation: a separate quit-store module/crate used by native
Tauri and hgs, isolated from TaskStart/G1 ledgers. The public boundary accepts a
native anchor capability, not a deserialized folder or G1 ticket. No production
caller is added until its own qualification/review.

A pathname preflight followed by SQLite `open(path)` is insufficient: it can
follow a changed ancestor, and a size check after a write is not a hard bound.
Proposed implementation is a small, separately reviewed fd-relative SQLite VFS
for this private store:

- All file opens/deletes/truncates resolve a small closed leaf allowlist against
  the pinned directory descriptor, with no-follow, nonblocking, no-ctty and
  close-on-exec flags. Recheck exact private file identities. Refuse directories,
  links, FIFOs, terminals, sockets, devices and unknown files.
- Enforce per-file byte/count caps **before** each write/truncate, using checked
  offset arithmetic. Database cap 128 MiB, rollback journal cap 128 MiB; no WAL,
  shared-memory, arbitrary temp, ATTACH, extension loading or mmap files.
- Use DELETE journaling, synchronous FULL, 4096-byte pages and max_page_count
  32768, with each setting read back. The VFS cap remains the hard journal bound;
  SQLite's journal_size_limit only bounds leftover files after a transaction or
  checkpoint and is not an active-write bound.
- Serialize raw header/hot-journal inspection and complete connection lifetimes
  (open, every transaction, close and callback/context destruction) with one
  exact private, non-inherited StorageMutex. Pin its cross-process file identity;
  the first implementation review must specify native callback/context lifetimes.
  All production connections go through that same guard. If VFS
  lock callbacks rely on this coarse lock, that assumption and exclusion of
  unguarded connections require explicit review and cross-process proof.
- Use a uniquely named VFS registration private to this connection/context; do not change the
  process default or affect another SQLite consumer. Callbacks cannot unwind over
  C, grant native authority or accept filenames from UI/JSON. Unsupported I/O or
  lock behavior refuses; there is no silent standard-VFS fallback.

The VFS callbacks are that slice's only new unsafe code. Write an explicit safety
invariant for each callback, keep its context alive through connection close and
unregistration, and prevent unwinding across C. A cut/fault harness must exercise
every I/O path, including failed/partial reads, writes, truncate, sync, delete,
access, lock and close; ordinary SQL success is not callback-lifetime evidence.

This is a proposed mechanism, not qualified code. A simpler standard-VFS solution
may replace it only after proving the same descriptor identity, active file-size
and file-count guarantees and receiving a refreshed pin review. The
[SQLite VFS](https://www.sqlite.org/vfs.html) and
[file-method interface](https://sqlite.org/c3ref/io_methods.html) define the I/O
boundary. [SQLite's pragma documentation](https://www.sqlite.org/pragma.html)
distinguishes database page caps from leftover-journal limits.

Physical proposed store budget is 288 MiB: database + rollback journal (256 MiB),
one immutable capture spool (16 MiB), bounded headers/lock files and margin. The
logical 64 MiB row/payload charge from the pure model remains separate. A cap or
fsync failure is a failed/unknown transaction with zero unjournaled delivery;
recovery must retain ambiguous reservations. No VACUUM, unbounded integrity scan,
background compactor or unqualified pruning is part of admission.

## Exact schema and indexed operations

All tables use a fixed application/schema identity and strict closed columns.
Unknown versions refuse; neither imports nor old GUI state migrates into current
admission. Proposed tables/indexes:

| Table | Key and bounded fields | Required index/use |
| --- | --- | --- |
| global | singleton revision, root identity, logical usage, active generation | one row; compare full before-image |
| registrations | original quit UUID, immutable context/capture blob + digest, sequence | UUID unique; sequence for stable pages |
| groups | registration + original group ordinal/UUID, full Action, totals/counters | unique group UUID; registration/ordinal |
| members | original request UUID, group/member ordinal, descriptor + planning digest, phase | UUID globally unique; group/ordinal |
| lanes | exact physical machine/run/conversation identity and unresolved request | unique exact-session key; no alias/name inference |
| resources | generation + bounded resource ordinal, purpose, phase, native birth/boot, I/O header/file identity, drain facts | generation/phase; at most four exchanges |
| notices | group UUID, bounded summary, rendered/ack digest | immutable sequence/cursor |
| tombstones | spent request UUID, exact identity digest and settled outcome digest | UUID unique; hard retained count/byte caps |

Proposed storage DDL below fixes the important widths/keys. SQLite application_id
and user_version must match before accepting the database. UUIDs use exact
16-byte canonical values; digests use 32 bytes. No SQL is accepted from callers.
`phase`/`purpose` integers have closed Rust enums as well as CHECK constraints.

```sql
CREATE TABLE global (
  singleton INTEGER PRIMARY KEY CHECK(singleton=1),
  revision INTEGER NOT NULL CHECK(revision BETWEEN 0 AND 9007199254740991),
  root_id BLOB NOT NULL CHECK(length(root_id)=16),
  active_id BLOB CHECK(active_id IS NULL OR length(active_id)=16),
  retained_rows INTEGER NOT NULL CHECK(retained_rows BETWEEN 0 AND 500000),
  charged_bytes INTEGER NOT NULL CHECK(charged_bytes BETWEEN 0 AND 67108864),
  summaries INTEGER NOT NULL CHECK(summaries BETWEEN 0 AND 16384),
  exchanges INTEGER NOT NULL CHECK(exchanges BETWEEN 0 AND 4)
) STRICT;
CREATE TABLE registrations (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  id BLOB NOT NULL UNIQUE CHECK(length(id)=16),
  context BLOB NOT NULL CHECK(length(context)<=262144),
  capture_sha BLOB NOT NULL CHECK(length(capture_sha)=32),
  capture_bytes INTEGER NOT NULL CHECK(capture_bytes BETWEEN 1 AND 16777216),
  slot_phase INTEGER NOT NULL CHECK(slot_phase BETWEEN 0 AND 5),
  spawn_seal_sha BLOB CHECK(spawn_seal_sha IS NULL OR length(spawn_seal_sha)=32),
  startup_nonce_sha BLOB CHECK(startup_nonce_sha IS NULL OR length(startup_nonce_sha)=32),
  expected_helper_birth BLOB CHECK(expected_helper_birth IS NULL OR length(expected_helper_birth)<=512),
  worker_ready_for_exit INTEGER NOT NULL CHECK(worker_ready_for_exit IN (0,1))
) STRICT;
CREATE TABLE groups (
  registration BLOB NOT NULL REFERENCES registrations(id),
  ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 0 AND 4),
  id BLOB NOT NULL UNIQUE CHECK(length(id)=16),
  action BLOB NOT NULL CHECK(length(action)<=8192),
  total INTEGER NOT NULL CHECK(total BETWEEN 1 AND 50000),
  issued INTEGER NOT NULL CHECK(issued BETWEEN 0 AND total),
  completed INTEGER NOT NULL CHECK(completed BETWEEN 0 AND issued),
  unknown INTEGER NOT NULL CHECK(unknown BETWEEN 0 AND issued),
  PRIMARY KEY(registration,ordinal)
) STRICT, WITHOUT ROWID;
CREATE TABLE members (
  request_id BLOB PRIMARY KEY CHECK(length(request_id)=16),
  registration BLOB NOT NULL,
  group_ordinal INTEGER NOT NULL,
  ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 0 AND 49999),
  target_key BLOB NOT NULL CHECK(length(target_key)=32),
  payload BLOB NOT NULL CHECK(length(payload)<=262144),
  planning_sha BLOB NOT NULL CHECK(length(planning_sha)=32),
  phase INTEGER NOT NULL CHECK(phase BETWEEN 0 AND 4),
  resource_claim BLOB CHECK(resource_claim IS NULL OR length(resource_claim)=16),
  canonical_request_sha BLOB CHECK(canonical_request_sha IS NULL OR length(canonical_request_sha)=32),
  observation BLOB CHECK(observation IS NULL OR length(observation)<=4096),
  FOREIGN KEY(registration,group_ordinal) REFERENCES groups(registration,ordinal),
  UNIQUE(registration,group_ordinal,ordinal)
) STRICT, WITHOUT ROWID;
CREATE INDEX members_page ON members(registration,group_ordinal,ordinal);
CREATE TABLE lanes (
  target_key BLOB PRIMARY KEY CHECK(length(target_key)=32),
  request_id BLOB NOT NULL UNIQUE REFERENCES members(request_id)
) STRICT, WITHOUT ROWID;
CREATE TABLE resources (
  registration BLOB NOT NULL REFERENCES registrations(id),
  slot INTEGER NOT NULL CHECK(slot BETWEEN 0 AND 4),
  claim BLOB NOT NULL CHECK(length(claim)=16),
  purpose INTEGER NOT NULL CHECK(purpose BETWEEN 0 AND 3),
  phase INTEGER NOT NULL CHECK(phase BETWEEN 0 AND 6),
  binding BLOB NOT NULL CHECK(length(binding)<=4096),
  native_birth BLOB CHECK(native_birth IS NULL OR length(native_birth)<=512),
  retained_output_bytes INTEGER NOT NULL CHECK(retained_output_bytes BETWEEN 0 AND 131072),
  outcome_reserved_bytes INTEGER NOT NULL CHECK(outcome_reserved_bytes BETWEEN 0 AND 4096),
  PRIMARY KEY(registration,slot)
) STRICT, WITHOUT ROWID;
CREATE TABLE notices (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  group_id BLOB NOT NULL UNIQUE CHECK(length(group_id)=16),
  body BLOB NOT NULL CHECK(length(body)<=8192),
  ack_sha BLOB CHECK(ack_sha IS NULL OR length(ack_sha)=32)
) STRICT;
CREATE TABLE tombstones (
  request_id BLOB PRIMARY KEY CHECK(length(request_id)=16),
  target_sha BLOB NOT NULL CHECK(length(target_sha)=32),
  outcome_sha BLOB NOT NULL CHECK(length(outcome_sha)=32),
  outcome INTEGER NOT NULL CHECK(outcome BETWEEN 0 AND 2)
) STRICT, WITHOUT ROWID;
```

Registration phases are the pure model's six phases (0–5); ReadyForExit is a
separate negative completion flag, not new admission. Resource phases are closed:
Reserved (0), IoBound (1), SpawnAttempted (2), ChildBound (3), Draining (4),
Drained (5), Unknown (6). Purposes are helper, capability probe, delivery and
receipt observation. Slot 0 is the helper; slots 1–4 are exchanges. Reusing a drained exchange slot
changes its exact claim; an old drain acknowledgement cannot release its successor.
Unknown resources are never overwritten. The known machine/run/conversation tuple
is checked alongside target_key, so the digest is an index, not equality authority.
Request uniqueness spans members **and** tombstones in the same transaction.
Counters are read/updated transactionally; SQL CHECKs are defense in depth, not
proof that a supplied Usage is the real count.

Store payloads are the per-member descriptors; the database does not duplicate a
second whole-capture blob. The one private capture spool is a bounded copy of
that same approved registration for helper startup, prepared after reservation.
Native context/group/index overhead and small outcome reservations are added to
the model's minimum row charge before admission; the actual full native charge
must still fit 64 MiB. Before each effect reserve up to 4 KiB for its minimal
validated outcome. Transfer only actual bytes to settled metadata after drain;
reserve fresh room before another issue. Keep original paths/choice in the
original payload rather than duplicating them in each outcome. Disk-full or an
ambiguous outcome commit retains the issued/unknown state, never restores Pending.

The initial capture budget includes the entire outer registration, native CLI/GUI
binding, expected-machine map, descriptors, planning digests and cleanup evidence,
not just the inner pure Record. Recheck the same 16 MiB aggregate cap. A common
50,000-local-target registration must still qualify; unusually large captures
refuse as a whole. Full root request bodies need not be duplicated per member:
a shared versioned canonical Request builder/validator deterministically derives
them from the immutable descriptor. Compute the real request hash through that
same validator at issue and receipt read. Never invent a second root hash schema.
Execution/receipt metadata also charges the global row/payload budget.

Local and remote effects retain original machine/run/conversation/archive/request
semantics. Remote capability and exact machine checks precede forwarding. A bare
alias or old command cannot fall back to a launch. Clear context needs its own
read-only receipt prerequisite; session-action-status does not accept it today.

Reserve reads actual usage/slot and indexed UUID/lane conflicts inside an
IMMEDIATE transaction, applies the pure plan, and atomically commits all immutable
groups, member rows, summaries and the worker charge. Any conflict/cap/error
refuses the whole registration. Pure Usage, callbacks and decoded phases are data,
not proof of actual namespace or currentness.

Each later CAS binds expected global revision, immutable registration/group/member
identity and digest, and the exact prior slot/member/resource phase. Index lookups
never load the retained journal. Pending -> NativeSpawnAttempted consumes the
member before any fork. Lost/unknown reserve, spawn or acknowledgement never
restores Pending or refunds a charge by itself.

## One original five-second window

The native first real-Quit event freezes Delays atomically, before draft flush or
worker startup. Freeze and ordinary grace expiry compete for the same one-use
member; an action already issued by the ordinary native dispatcher is excluded.
Capture native removal choice/evidence at registration, not in a frontend closure
or setting. Retain original deadline/group/member order and original request UUID.
Coalesced menu, OS and updater requests share this same freeze and cutoff.

Use Linux CLOCK_BOOTTIME and macOS mach_continuous_time converted with checked
native timebase arithmetic, with the exact boot identity and nanosecond units.
Both choices include sleep. Apple's
[continuous-clock contract](https://developer.apple.com/documentation/kernel/1646199-mach_continuous_time)
requires a conversion from ticks; no serialized std::Instant, wall-clock timestamp
or new helper-start deadline is accepted. Unavailable clocks refuse admission.

```mermaid
flowchart LR
  F[Real Quit freeze] --> R[Durable summary and worker reservation]
  R --> P[Prepare bounded private custody and I/O]
  P --> B[Commit exact header and file identity]
  B --> S[Seal one-use helper spawn attempt]
  S --> H[Spawn reviewed hgs helper]
  H --> I[Reserve each native attempt before fork]
  I --> D[Observe actual exit and all I/O drain]
```

Check the same original boot/cutoff before and after every slow preparation,
transaction/fsync and immediately before each spawn. At now >= cutoff start
nothing. Late preparation can preserve a no-new-start outcome only. Failed initial
reservation starts no helper or best-effort action. A sealed but uncertain spawn
counts conservatively as issued/unknown, not untouched.

### One-use helper startup, not a decoded producer claim

Sealing binds a hash of an ephemeral 32-byte native startup nonce; plaintext is
never put in the database, capture, logs or UI. The real-Quit native actor alone
creates it. The helper cannot become current by reading a valid journal/header
or acquiring a newly opened lock.

After the sealed fork, the parent must bind the **actual spawned helper's** PID,
boot and birth under the original reservation. Only after that durable bind and
a fresh cutoff check does it write a fixed frame (at most 256 bytes, below POSIX
PIPE_BUF) to that child's original stdin pipe, then close it. This frame carries
the one-use nonce and registration binding. The writer retains custody until its
actual completion. The helper has null stdout/stderr, so GUI exit leaves no
unowned helper-output reader; all progress goes to the journal.

The helper verifies its native birth against that parent-observed binding,
unchanged inherited custody/header, compiled helper capability, nonce hash and
original boot/cutoff. Its CAS consumes the startup seal exactly once before
constructing any private live-worker authority. No JSON/record/fixture token can
construct that authority. No nonce replay, new helper after uncertain spawn or
self-attested birth substitution is allowed. If the parent dies before the birth
bind/frame, EOF, incomplete input or cutoff permits no action; the ambiguous
resource reservation remains charged. Qualification must exercise every cut.

All CLI capability probes begin only after grace expiry/real-Quit freeze and
after durable resource reservation. A --help probe must itself be bounded,
accounted and drained; an unsupported helper/peer refuses before dispatch. No hgs
preparer runs during Undo grace, no GUI self-helper or daemon is introduced, and
no timeout kills a detached call.

The GUI's draft flush remains a separate 150 ms operation. Its exit gate waits
only within the one original five-second handoff bound; it cannot give preparation
a new five seconds after draft completion. An admitted helper/child can outlive
GUI exit and cutoff. A no-time shutdown/crash drops unissued actions and emits
bounded diagnostics; it does not start a new preparer while exiting. Updater
restart events currently bypass the normal flush authorization and require an
explicit native integration test/review of the new shared freeze gate.

### Installer and updater survival

The existing prohibition on killing native agents extends to every admitted
quit helper and its children. `tray/update-installed.py`, `install.sh`,
`tray/install.sh`, `mac-install.sh` and the cask/AUR flows must never signal them,
including through a process group, executable-name sweep or service teardown.
They must also finish or report a bounded refusal rather than wait indefinitely
for a detached helper. Existing cask rules prohibiting GUI quits and LaunchAgent
unloads remain in force. No packaging or install operation repairs, resets or
reinitializes an unknown generation.

An executable replacement or GUI relaunch while an old helper drains observes
the same charged global generation and refuses a second helper. Binary/config
changes cannot make a fresh admission namespace. Qualification must exercise
the real-exit route **through `tray/update-installed.py` itself**, with pending
actions and a draining admitted fixture helper, then start the replacement GUI
and verify preserved charges, no second helper and actual eventual drain. An
in-app restart alone does not cover the updater's exit bypass. Exercise maintained
install/packaging launch shapes privately; never update the owner's installation
to qualify this behavior.

## Native resource custody

One charged generation spans old/new GUI and configuration identities. It owns
one helper plus at most four exchanges, each accounting for Child, stdin writer,
stdout reader, stderr reader and retained output. Each output stream retains at
most 64 KiB and then continues draining/discarding; saturation never permits a
replacement, unbounded buffer or timeout kill.

A single private, exact-header generation custody file uses the reviewed generic
close-only InheritedLock mechanism, with host proof added here. Acquire/bind it
before spawn and install its inherit hook last. Retain its Arc with every helper,
child and I/O resource until actual verified exit/drain. It is not a G1 ticket,
TaskStart ledger, ordinary FileLock or imported ownership token. Storage-mutex
unlock/drop and GUI exit cannot release custody.

A member's effect state and its resource state are separate. A malformed receipt
can leave effect Unknown even when an owned Child and every I/O holder are
verified drained. That member is spent and its exact-session lane remains held;
a different pending target may use a freshly proven available exchange before
the original cutoff. Unknown **custody** cannot free an exchange or spawn a
replacement. Native facts are private in-memory witnesses, never decoded tokens.

For remote exchanges, persistent SSH masters or unqualified proxy descendants
must not become an untracked background cohort. Propose disabling ControlMaster/
ControlPersist for these admitted calls, without touching existing user transports,
and fence any route whose process/I/O custody cannot be qualified. A transport
loss does not prove the remote action or its resources ended. The exact route,
capability and remote status behavior needs the same final native review.

Normal worker exchange release requires its owned Child wait and all writer/read
joins, with the custody handle held through the durable drain commit. Worker
release requires AdmissionClosed, every exchange drained, exact unchanged file/
header, last-close acquisition and fresh exact process-birth checks. A stale PID,
age, cutoff, GUI exit, effect receipt or unknown record alone proves none of this.
Missing/tampered custody and boot mismatch remain unknown/charged.

If a parent crashes before recording physical drain, a free flock or a vanished
PID alone is insufficient, particularly for remote/SSH descendants. The initial
implementation provides no automatic unknown-resource reclamation or force-reset
API. A normal ReadyForExit record can be reclaimed only by a new native observer
that proves unchanged private header/file, original boot, actual last-close
acquisition, no live exact helper birth, all current slots drained and no
unsettled resource history. No record alone can manufacture that witness. Operator recovery is an explicit audited repair/audit workflow that restores
verified evidence or supplies separately reviewed native exit/drain proof; it
cannot erase unresolved charges or replay a request. An outcome whose physical
custody cannot be established remains held and new durable handoffs refuse.
This availability cost must remain visible in diagnostics and documentation.

## Durable cleanup denial, including old resident binaries

Root authoritative lifecycle remains session_action attempt/perform. Add the
closed optional KeepForReview constraint to that original Request validator and
canonical hash, with a new exact remote capability before any action. Old peers
refuse without dispatch. No recovery read calls attempt/perform to settle a claim.

A per-Scope flag is insufficient: supervisor.rs independently calls after_archive.
A marker understood only by a new executable is also insufficient: old resident
supervisors and maintenance binaries cannot be restarted for this feature.
Proposed compatibility barrier (`zerus-5wmh.66.2.34.3.4`), amended after
private legacy interoperability controls. Publication remains unavailable until
the pre-hold quiescence and exclusion requirement below is actually qualified:

1. Freshly verify the exact run/conversation, recorded launch owner nonce and both
   original ownership receipts, under the existing repository-then-state lock
   order. A missing/replaced/unknown linked-checkout owner refuses before stop.
   A positively verified main/non-Git folder needs no linked-checkout hold;
   inspection failure is not that proof. These locks live under the selected
   state root; they do **not** exclude an old caller using another configuration.
2. **Prove quiescence before installing any hold.** Use kernel-observed process
   use and the existing termination-custody evidence principles: exact birth
   identities, current directories, open files and executable identities, with
   bounded complete coverage of every relevant caller/configuration. Cover both
   the checkout and its exact common/per-checkout Git metadata. Hidden or
   unreadable processes, omitted file observations, changed identities, incomplete
   rosters and uncertain prior operations are UNVERIFIED, never absence of use.
   Manual unreadable-process consent cannot establish this proof. Reuse neither
   termination's signalling/freezing path nor its authority to stop an owned
   agent: this check sends no signals and restarts nothing. The check must also
   exclude an old mutator entering its decisive window between the observation
   and durable publication; a point-in-time snapshot is not that exclusion.
   If this cannot be proven before publication, publish **no hold** for that
   worktree, reserve no durable delivery for it, and retain today's drop-on-Quit
   behavior with the registration-time Undo toast note and Quit-time diagnostic
   rules below.
   Do not silently omit a member or repack an original group to permit partial
   delivery; original group validity, ordering and refusal rules still apply.
   Do not claim that its requested action will survive Quit. This is a proposed
   denial/fallback boundary, not implemented behavior or a quiescence API.
3. **Install the native Git worktree lock as the first durable hold record**,
   after qualified quiescence/exclusion and before either version-2 write or any
   stop. Its existing `locked` leaf belongs to the exact pinned Git
   per-checkout metadata directory. Create without replacement or following links,
   verify its private native identity, and durably sync both file and directory.
   Never overwrite, adopt or borrow an existing foreign/unknown lock. Use the
   bounded ASCII reason `Zerus quit hold <hold-uuid>` with a fresh canonical
   public hold UUID; this is not the secret helper-start nonce. Bind its exact
   bytes, inode/native file identity and metadata directory to the original
   owner, request and registration. This is negative evidence only.
4. Preserve the original owner nonce, path/common/metadata identity and branch/
   archive restore information. Write and fsync the private negative-only
   ownership version 2 **before** writing and fsyncing the matching Git owner
   marker version 2. Include bounded KeepForReview and exact Git-barrier bindings.
   Repeated sources for one owner use indexed audit rows, not an unbounded list
   in the 16 KiB marker. Stop only after verifying all three durable records.
5. Every partial cut remains denied/manual-reviewable, including a lock with
   version-1 records. Never roll back, downgrade, expire or repair away a hold.
   An unexpected replacement or missing barrier refuses stop and positive
   delivery; a successfully created but incomplete barrier remains held for
   explicit recovery. The no-hold fallback applies only before the first hold
   record: uncertainty after publication never authorizes deleting the barrier
   or rolling records back to pretend no hold was installed. Root/bootstrap,
   selected-CLI and resource gates still apply.
6. New readers centrally refuse automatic cleanup for restricted ownership.
   Legacy `owned()` rejects version 2; the Git lock must additionally deny old
   manual Git removal and pruning. An existing lock alone is **not** proof that
   an already-issued old removal has drained. Before enabling delivery, qualify
   old callers paused before/after their final Git checks, across distinct
   state-root lock domains. If the actual old operation cannot be excluded or
   proven drained, refuse before stop. Do not signal old agents/callers or infer
   safety from process names, idle state or a filesystem lock's presence.

### Why the dual version-2 records are insufficient

Two actual private pre-hold CLI controls used source `ed80595e`, a separately
built and hashed executable, and Git 2.56.0. With matching synthetic version-2
receipt/marker records, fresh old manual review plus the existing exact
unreadable-process consent removed the checkout and its Git marker. The private
receipt and archive stayed byte/inode-identical and the branch stayed retained.
This violates the required negative-evidence preservation; version refusal in
`owned()` protects automatic cleanup but not every manual path.

A second control installed a native Git worktree lock before the same version-2
pair. Old manual removal refused as locked, offered no process exception, and
preserved checkout, branch and every receipt/marker/archive/lock byte and inode.
Two separate native Git restore-step controls also refused a missing but still
registered checkout. These are **two actual legacy CLI cases plus two Git-only
controls**, not old Qt, supervisor, maintenance, partial-cut, race or production
qualification. The required matrix below remains open. No mutable hold, store
or delivery caller is enabled by these controls or this proposal.

### An already-issued native Git removal defeats a newly installed lock

A further private control ran the actual source-pinned V1 `hgs` manual-removal
caller (`ed80595e`) with unchanged installed Git 2.56.0, built from
`a018953688f1b10bddf91bff8747068f5f4746a4`. The hgs SHA-256 was
`aa3cdac5973566cb6e3d57798d7fbb82956c4bc6dec706f43e8b2e6abb974b7f`;
the Git SHA-256 was
`2912a2a32f408c9b8122e476251b32f4b35f4c3f0c15961686ae87ca456d384c`.
The fixture used the existing exact unreadable-process consent, without force
or unlock. It paused only its own native Git child with ptrace at syscall exit:
`newfstatat(AT_FDCWD, worktrees/checkout/locked, ..., AT_SYMLINK_NOFOLLOW)` had
already returned ENOENT, before Git userspace resumed. The child's cwd was the
repository's common Git directory, not the checkout. No executable, register or
syscall result was patched.

While that child remained stopped, a separate real `git worktree lock` succeeded.
The fixture synced the lock file and parent directory, then the private V2
receipt and parent, then the matching Git V2 marker and parent. After resuming,
native Git exited 0 and the actual old hgs returned `status: removed`, exit 0.
The checkout, V2 Git marker and newly installed Git lock were deleted. The
private V2 receipt, archive and maintenance SQLite retained exact bytes and
inodes, and the branch remained. This is an R3 negative-evidence counterexample,
not a production incident or permission to weaken R3.

Raw packet `legacy-inflight-git-counterexample.zip` SHA-256
`74553b771226abb2da584d645efb3b30a4c516ae3a56d6875ac83b8addd53308`
contains 21 members: source exports, runner, 261 syscall pairs, file snapshots,
build provenance and command outputs. Exactly **one actual negative race case**
executed; an earlier tracer relative-cwd setup attempt is retained and excluded.
Separately reviewed Qt packet `legacy-qt-lock-proof.zip` SHA-256
`4959c70fad9bcb865d39c9eaa698e993a474bd3871cd1da1e3dba00d25f9e416`
qualified only 11 fresh locked reviews and five stale Qt confirmations, with
zero successful removals. Those 16 passing cases do not qualify this native race.

The coordinator's proposed quiescence direction is a requirement to evaluate,
not a proven mechanism. Existing process-use helpers are not an admission API:
the Linux `worktree_processes` roster covers cwd/executable observations, while
`open_files` currently filters individual fd-entry/read-link failures. An empty
result from that helper cannot certify complete open-file coverage. Termination
custody pins an owned ancestry and uses signals; its successful completion does
not exclude independent legacy callers, especially another configuration.

These legacy windows remain **unprotectable by the qualified mechanisms**:

- a native Git child already past its absent-lock check, as demonstrated;
- a paused old reader/remover that later spawns or resumes native Git, including
  one with no checkout cwd or open file at the observation instant;
- an independent old configuration/caller entering between a process sample and
  lock publication. The selected state-root repository/writer locks do not cover
  that domain, and repeated empty samples supply no atomic exclusion.

No available proof closes those windows. Until a concrete no-signal mechanism
excludes them through publication and receives fresh G3/native/security review
and actual qualification, the affected worktree is ineligible for hold publication
and durable delivery. Neither an idle/process-name heuristic nor a Git lock,
post-publication recheck, manual consent, old approval or eventual process exit
repairs already erased evidence. Keep no-hold/drop-on-Quit fallback before the
first write; after any partial hold retain denial/manual recovery. The release
gate must account for these unresolved windows, the complete legacy matrix and
the separately required shipped release surface. Mutable hold/store/caller and
release implementation remain unapproved.

### Release belongs to a new reviewed native operation

A completed action receipt alone must not unlock the worktree: the private
version-2 record by itself did not stop the demonstrated old manual removal.
Normal release therefore requires both verified effect completion and actual
helper/exchange/pipe/writer/reader drain, **and** a fresh explicit removal/release
review through the new qualified native implementation. Bind that review to the
original owner and unchanged hold UUID, exact barrier file, metadata, checkout,
branch/archive, current source and fresh file/process fingerprints. It consumes
no old approval. The reviewed native actor alone may remove its exact barrier
as part of that operation; legacy Qt/CLI calls cannot release it. Version-2
ownership is never rewritten to version 1 and no branch is deleted implicitly.

Unknown effects or custody, incomplete writes, lost acknowledgement, crash,
restart and changed/missing/replaced lock identity retain the hold. There is no
startup/timeout/age-based automatic unlock or replay. A cut before release leaves
the barrier held; a cut after an attempted unlock leaves an explicitly unknown
release, never permission to repeat it. Any new observation must distinguish
those cuts with actual file/operation evidence. An explicit operator recovery
may release only after its own separately reviewed native custody/source proof
and fresh exact owner/barrier confirmation. It cannot erase unresolved charges,
replay delivery, downgrade ownership, borrow a foreign lock or convert uncertainty
into permission. The exact release transaction and its legacy-race exclusion
must be agreed and qualified before implementation; this document supplies no
release API or new UI action.

### Worktrees presentation and interoperability matrix

While held, Worktrees uses its existing Blocked state, unavailable Remove choice
and Details for the native Git reason `Locked by Git: Zerus quit hold <hold-uuid>`.
A person running Git also sees why the worktree is locked. Existing Quit/cleanup
Review navigation can open that row. The new exact review/release surface needs
webui's explicit ruling before UI implementation; no Unlock button, new area or
old consent is introduced here. **No hold-installing or delivery runtime may
reach users before the reviewed release surface is agreed and shipped in the
same or an earlier build.** This is an enablement gate, not later UI debt. An
ordinary Git unlock outside this protocol is
not repair: lost/unknown restriction remains for manual review under a fresh
owner-directed boundary.

Qualify every current-hgs and actual old-hgs/Qt path that can remove, prune,
restore or restamp a checkout: manual removal, automatic archive cleanup,
Terminate cleanup, supervisor completion, periodic/missing maintenance, explicit
archive Restore and new registration. Include:

- matching V2/V2, private-V2/marker-V1, private-V1/marker-V2 and missing/replaced
  cuts, both the same and older configurations and stale archive/approval data;
- existing and missing checkouts with retained Git metadata, foreign locks,
  ordinary removal, single and double `--force`, every unlock/prune variant,
  and original branch/archive
  identity throughout;
- paused readers/removers resumed at every decisive boundary, before/after first
  lock publication, each record fsync and verified/unknown release;
- crash/restart before and after release, original helper/resource drain and
  explicit recovery, without any automatic unlock, downgrade or replay.

For each case record exact executable/source/Git versions and before/after
bytes, hashes, inodes and native registration identities. The old caller itself
must execute: a new reader unit test or direct Git command is not its substitute.
All negative records stay byte-identical across old paths and partial outcomes;
refusal to delete the checkout alone is insufficient. A caller reaching
`created()` must neither carry a restricted original owner nor erase/restamp its
negative evidence. Any actual violation stops delivery and requires a concrete
amendment with fresh G3 and webui/native/security review; never narrow the matrix
or restart old agents to make it pass.

## Read-only recovery, notices and retention

Next start uses bounded stable pages (at most 50 items and 512 KiB per page;
individual oversized historical fields refuse rather than truncate into a trusted
request). Cursor binds an insertion high-water mark and original ordinal/sequence;
new registrations cannot move prior pages. Read only the unresolved rows being
observed, never the complete retained journal or an unbounded per-session cache.

Read genuine exact session-action receipts, and the separately qualified Clear
context API when available, using original request UUID/body/hash/target/machine.
A claimed receipt projects uncertain without rewriting it. Absent, unsafe or
unavailable receipts never establish no-effect, retry or not-issued authority.
Fresh genuine completion may update this quit journal's historical projection;
it cannot replay an effect, create a live worker or free physical resources.
Remote observations also obey exchange accounting; no capacity means deferred/
unavailable observation, never an unaccounted helper.

Use existing sticky action/cleanup notices with bounded ordered counts and at most
five names per group. Keep native removal choice and negative evidence across all
partial outcomes. UI acknowledgement is exact notice identity only; it is neither
receipt acceptance nor resource release. Unknown effects and unissued pending
members remain explicit. Today's Tauri client has no visible Quit notice:
drop-on-Quit is only `action_delay::cancel_all`'s local stderr line, which may
include up to five Debug-quoted session names. A hidden window need not appear
before exit-flush acknowledgement/timeout. Current toasts and that pre-existing
bounded local line remain unchanged until the qualified delivery caller lands.

For that caller, a fresh native availability evaluation at each original group
registration chooses one fixed catalog sentence as core Text in the group's
existing Undo-window toast note, with Russian translation: for example,
"Quitting Zerus before this runs cancels it." It describes the whole original
group, never some members, and never comes from a cached availability Boolean.
This includes isolation, refused selectors, an inherited agent HGS_STATE_DIR,
running-GUI pair skew and an already known storage/cap refusal.

A refusal first appearing at Quit is diagnostic only, using bounded closed
codes/counts/digests alongside the existing drop line, never success. No pre-exit
dialog/confirmation, exit delay or next-start notice is added for a refusal
before any durable record. Such a confirmation would require owner direction.
New origin/store reasons expose no session names, paths, raw environment values,
PIDs, process names or provider errors. A partial hold/reservation still retains
the existing denial/manual-recovery and custody requirements.

Retention is indexed and bounded. Only genuinely settled payloads/notices may be
compacted into spent UUID tombstones; never prune unresolved evidence to admit
another batch. Retained rows <= 500,000, summaries <= 16,384 and total charged
bytes <= 64 MiB are checked from the real transaction. Physical VFS caps remain
independent. No timer, expiry or age creates execution/cleanup permission.

### Stored-payload privacy

Registrations, captures, notices and tombstones contain session names, paths and
targets. The quit store, bootstrap/custody files and capture spool are **never**
included in any export or diagnostics bundle, including Private analytics'
aggregate export, and are never uploaded. New origin/store diagnostic logging
is restricted to bounded counts, closed reason codes and digests; it must not include payloads,
session names, paths, targets, raw environment values or startup nonces. Operator
recovery output obeys the same rule. Its audited repair evidence remains private
under the same storage bounds, rather than becoming a public transcript or dump.
Qualification must attempt every existing export/bundle path with recognizable
synthetic payloads and prove their absence, including on error and recovery.

## Required qualification before implementation/enabling

Design agreement is not kernel or end-to-end evidence. Each mutable slice still
needs exact pinned review and raw hashed logs with executed counts. Required
private controls include:

- namespace/isolation predicates individually, complete selector/marker inventory,
  immutable original environment before/after loader mutations, helper origin,
  default-profile resolution and actual private-app real Quit with pending actions
  against a decoy anchor, unchanged inode/mtime and zero attempted production probes;
- no-follow ancestor/leaf races, unsafe types/owners/modes, fixed bootstrap cuts,
  missing/tampered state, symlink/TTY refusal and no automatic reinitialization;
- actual DB/journal/file/count caps, every transaction/fsync failure cut, disk full
  and ambiguous commit, callback lifetime/locking and cross-process serialization;
- 50k registration, 16 MiB including the full native envelope, all hard quotas,
  original order, same-session conflicts and blocked old generations/configs;
- unknown spawn, parent/worker crash, slow startup/preparation, pipe saturation,
  child exit before drain, old worker alive, cutoff equality and boot mismatch;
- old resident supervisor/maintenance and Qt cleanup reading negative ownership
  version 2, byte-identical files across every partial marker cut, deletion races,
  Qt manual-review routing, followed by unchanged native Stop/
  Pause/Terminate/restore contracts for latest supported agents;
- complete native pre-hold quiescence and exclusion across legacy configurations,
  already-issued Git checks and paused readers, including hidden/unreadable
  process and fd observations; absent proof gives no hold/no durable delivery
  and existing drop-on-Quit diagnostics, never a positive snapshot inference;
- genuine bounded receipt/status recovery with no dispatch, stable concurrent
  paging and existing notices; fresh GUI restart, actual update-installed.py
  real-exit/replacement while draining, bounded installer completion without signals;
- every export/diagnostics/Private analytics and operator recovery privacy path;
- exact CLI/configuration-origin child .34.3.5 qualifying BOTH maintained Mac
  LaunchAgent and arch tray/install.sh private launch shapes before any enabling;
- Linux and macOS kernel/private built-app proofs. Windows remains fenced.

Open review decisions are the exact fd-relative VFS/locking mechanism, first-use
bootstrap versus explicit repair cuts, the narrowly accepted normal-drain witness,
and the operator audit/repair surface that never forgives unknown custody. No
mutable delivery code is authorized by this proposed document before those
concrete G3/native/security decisions are resolved.
