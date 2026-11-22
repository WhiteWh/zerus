# Desktop operation ownership and recovery evidence

The test-only root `desktop::operations::tests::integration` harness connects
G2's unchanged conformance oracle to the merged frozen-original bridge and
existing receipt finish with real SQLite fixtures. Its 27 vector executions are
MODEL CONFORMANCE, counted separately from Rust test entries and eligible
G2/SQLite proposal/receipt segments. Fixture admission, reservation and Unknown
seeding happen before the bounded fake driver starts; that driver has no
admit/reserve/worker/current-guard or resource-release capability.

The legacy oracle's child/stdout/stderr/stop/drain/release flags omit stdin and
never free the actual SQL undrained slot. A held-input observation here is
symbolic, not physical worker evidence. Follow-up examples use explicitly
independent fixture databases, without claiming continuous release or retry of
an uncertain attempt. Exact old-epoch still-reserved results may settle after
revoke, while terminal Unknown, changed originals and forged tickets refuse.
Those conformance harnesses alone qualify no production actor, task call,
callback, timer, custody or current Operations admission. The separate guarded
[Task Start actor](desktop-task-start-reconcile.md) consumes native admission;
later kinds and the remaining G2 integrations stay separate.

The additional test-only `operations::tests::owned` slice sends the unchanged
proposed argv and input to fixed private synthetic executables through the
existing `run_owned`. File-backed fixture admission and exact custody reservation
precede the fake port; these fixtures establish no current producer authority.
The spawn hook registers the actual child birth before input starts. A failed
post-spawn registration still returns that actual Child to its owner and pins
the private fixture; it never reports StartFailed or accepts its reply.

Real gated child and stdin/stdout/stderr lifetimes, exact descendant exits and
an isolated host exit/reopen exercise the existing custody probe and recovery
commit. Pipe controls strip the descendant's inherited lock, so a blocked
writer/reader itself must retain custody beyond the bounded runner return.
Only the crash survivor retains inherited custody. No direct Boolean drain
proof is supplied. Unknown outcomes remain terminal after genuine resource
release, and repeated calls never create another child. This qualifies synthetic
process and storage mechanisms on Linux/macOS, not actual task delivery,
production admission, native agents, Windows custody or whole-profile handoff.

G2 (`zerus-5wmh.66.4`) moves window-independent operation lifecycles into
`hgs`, preserving original identities and reviewable uncertain outcomes.
The ownership split agreed with G1, G3 and the task executor owner on
2026-10-30 is:

| Owner | Responsibility |
| --- | --- |
| G1 | One service actor and database connection; registration and locks; writer/service epochs; shared effect fences; admission, revoke and drain |
| G2 | Typed operation policy, identity and consent checks, bounded steps and reconciliation through that actor; operation-specific tables once admitted |
| G3 | Existing source decoders, versioned read-only adapters and immutable Staged evidence |
| Existing recovery and voice services | Their current authoritative work and receipts; desktop retirement creates no replacement worker |

G1's native `desktop-ledger::Kind` stays limited to native desktop effects.
There is currently no admitted positive job/effect IPC or generic scheduler.
Those requests still refuse `handoff_required`. Display coverage, a parsed
Complete marker, Staged or Drained evidence and old successful records grant
no current execution, release or takeover authority. G2 adds no second ledger,
daemon, fleet reader or source writer. Core sharing with the root executable
and any executing actor integration require their own packaging/API agreement.

## Passive recovery projection

`client/core/src/operation_recovery` is a pure consumer of one exact immutable
import. This first slice has no command, frontend, startup hook, source discovery
or executable filesystem reader. It opens no stores, makes no task/native call,
reserves no receipt and never invokes live creation state transitions. Its
reader trait is a boundary for the existing G1 secure reader, not a second file
or security implementation.

`Projection::new` accepts the selected import's trusted `ImportRecordRef`, exact
manifest bytes, configuration fingerprint and import UUID. It uses the shared
`ImportRecord` validation, then checks the exact manifest digest and identities.
The caller obtains that reference from G1's selected import, not an untrusted
snapshot. No JSON body chooses a path or another import.

`Projection::page` takes an optional opaque cursor, explicit limits, a caller
reader and cancellation check. For each selected metadata page it uses shared
`ImportRecord::snapshot_page` membership, length, digest and source binding.
Recognized operation bodies also pass `SnapshotRef::verify` before decoding.
Missing or changed bytes fail with a body-free unavailable/invalid error;
they never become an empty successful operation set.

The currently decoded families are:

| Families | Schema / format | Existing validation |
| --- | --- | --- |
| `qt-creations`, `next-creations` | 1 / `creation-json-v1` | `creation::decode_journal` for the header and `valid_operation` for each original operation |
| `qt-task-delivery/<scope hash>`, `next-task-delivery/<scope hash>` | 1 / `task-outbox-json-v1` | The pure G3 task-delivery adapter, reusing the existing outbox decoder and exact project/task/marker scope |
| `next-task-start/<scope hash>` | 1 / `task-start-outbox-json-v1` | The pure G3 Start adapter, reusing the existing outbox and original guarded plan checks |

The scope hash remains SHA-256 of project, LF, task UUID. The snapshot reference
retains the complete original private record, including unknown fields; the
projection never rewrites it. Qt and Next observations retain separate family
provenance even when their operation IDs agree. They are evidence observations,
not duplicate live operation admissions.

Summaries contain only bounded original operation/request IDs, source/page
identity, scope and recorded phase. Creation summaries omit checkout paths,
commands and diagnostics. Task summaries omit prompts, plans, attachments and
native output. A summary cannot reconstruct a live request: a future receipt
lookup must reverify the complete private original plan and current owner.

`RecordedState::Terminal` means only that the original writer recorded bound,
failed, stopped, started or released. It is not an exact live receipt or proof
that an effect is settled now. Every pending/interrupted/unknown creation and
task Start is `NeedsReview`. A task-message outbox is always `NeedsReview`;
opaque future fields claiming success cannot settle it. Nothing in this API
can retry, resume, release, clear or grant consent.

## Bounds, coverage and cancellation

- A manifest has at most 256 sources, 1024 page references and 256 KiB, using
  the shared import contract. Each metadata page and snapshot is at most 1 MiB.
  The existing publisher's total selected import budget stays at 64 MiB.
- A projection call scans at most 64 metadata pages and reads at most 8 MiB.
  Defaults are 16 pages and 4 MiB. The minimum 2 MiB allowance permits progress
  through one maximum-sized metadata page and body. An unfinished page may
  reread its metadata on the next call; each call charges those bytes.
- The caller's existing private no-follow, regular-file, single-link and
  identity checks remain mandatory. A reader must enforce the reference size
  before allocation, use only G1's fixed digest paths and perform reads off the
  GUI thread under its own deadline. The pure consumer checks returned bytes
  again. It cannot make a blocking external reader cancellable.
- Cancellation is checked before metadata/body reads and after a body read.
  An unfinished page stays at the same cursor position. There is no operation
  cache: only bounded manifest metadata, one current private body and at most
  one call's bounded summaries are retained.
- The cursor binds configuration, import UUID, exact manifest digest and page
  position. A different manifest refuses it, including a replacement with the
  same import UUID. A new import starts a new traversal.

`coverage()` distinguishes decodable source metadata, unsupported families and
evidence belonging to another responsibility (reads/approval policies). It also
lists domains without a decodable source. Decodable metadata is not a claim of
verified bodies, current discovery or completed migration. Absent and future
families remain explicitly unavailable. Unsupported pages carry their exact
manifest positions and their private bodies remain untouched.

The existing speech service and provider-operation store already retain their
own original receipts after the client exits. The client checkpoints the
provider operation's original profile ID and `expected_revision` or
`expected_profile_revision`, without its credential or editable profile body.
Refreshing a provider list never replaces these saved expectations; legacy
checkpoints keep their original status-only lookup.

Draft transcription checkpoints retain the capture/request identity, selected
machine/profile/revision and intended `draft_revision`. A restored revision is
historical evidence, including when a new editor reuses the same counter value.
Checking a restored capture reads its original job and saves the transcript for
review. It never resubmits audio or grants insertion permission. Explicit
**Add to draft** checkpoints the current insertion revision before emitting an
insertion carrying that revision and the original insertion UUID. Missing legacy
revisions remain unbound until that explicit action. Neither path sends a
message. The current session voice frontend continues to refuse dictation
insertions; this metadata contract does not enable a new draft-only UI.

Separate pending launch/executor maps, deferred input, draft transcription,
provider operations, cache maintenance and termination cleanup are unavailable
in this consumer until their individual schemas and policy slices are admitted.
This remains true if G3 has added an adapter that this consumer does not yet
understand. `ManifestEnd` means only the end of this immutable manifest; it
never claims complete source discovery, drain or G2 coverage.

Each deferred input reserves its client request UUID and a consent checkpoint
before its dispatch, and saves them with its draft in the existing durable save
(`operation_recovery::deferred_input`, draft manifest field `deferred_input`).
The deferred inputs are a send, an answer, the compaction of a compact-to-send
and the start of a resume-then-send. The dispatch uses that request id. The
checkpoint records the original intent and identity only: the kind, the draft
save revision and the exact message digest, the run and conversation, the
question for an answer, a resume's start choice, and the CLI receipt family
(`input_receipts`, `question_receipts`, `compact_receipts` or `resume_input`;
`action_receipts` for a version 3 resume-then-send the service resumes).
Restart, import or reconnect never rebuild executable consent from it, and
nothing sends from it. Imported and recovered drafts never carry one. A
present but invalid field leaves the draft not restored, never read as absent.

The executing kinds that reconcile these receipts come one at a time (slice 3).
The first, `deferred_send` (`desktop::deferred_send`, ABI
`desktop-deferred-send-v1`), defines its closed wire:
- **Intent.** The local session, the reserved request ID, the checkpoint's
  exact JSON and the exact `hgs send <session> --json` stdin.
- **Verification.** Strict typed decoding, so unknown or duplicate keys
  refuse, as the CLI's own decoder does. The intent must be a `send`
  checkpoint whose request ID, run and conversation the stdin repeats, and
  whose message digest the stdin's text and decoded attachments reproduce.
  A plain send only: no subagent target or compaction. v1 refuses any `@`
  or other-machine session, and any session but a terminal agent's (Codex,
  Claude, Kimi): an OpenCode or DeepSeek send goes to its own native backend
  and receipts, so it stays on the direct path, decided before any offer
  (`deferred_send_agent_refused`). The CLI's own send rules are mirrored, so an
  original the CLI would refuse never spawns: a run, a non-empty message
  within 64 KiB without control characters, no `/` or `!` command, at most
  eight attachments with valid names, types and labels, each 10 MiB and
  20 MiB in total. An intent whose service line does not fit the control
  frame stays on the direct path, decided before any offer.
- **Plan and binding.** The fixed `send <session> --json` call. The plan's
  input digest is the SHA-256 of the exact stdin, which is the digest the
  CLI's receipt records.
- **Receipt.** Recorded only from the CLI's own matching `submitted`
  receipt: a submission to the terminal, never proof the agent read it.
  The wire owns the matching rule (`Outcome::from_receipt`). The CLI keys
  `input_receipts/<request_id>.json` by request ID alone and keeps an
  earlier `submitted` record when a later submit of that ID with other
  content or another session is refused. So a record matches only when its
  session (`name`), run, conversation, stdin digest and terminal transport
  are all the plan's and its status is `submitted`. The outcome carries
  that digest. `in_progress`, a duplicate or missing field, or more than
  256 KiB is never an outcome.
  Failed only at the no-spawn boundary (`deferred_send_not_started`), and
  only when the drain finds no record of the request: an exact `submitted`
  record still records it, and any other record leaves it Unknown.
  Everything else after a possibly started child is Unknown and never
  resent: `in_progress`, a missing, corrupt, mismatched, inaccessible or
  replaced receipt, a timeout or a lost worker.
- **Lookup.** A scoped read-only lookup by session, request ID and plan
  fingerprint.

The service actor executes it on Linux. The native frontend lane carries the
request on its own ordered lane. The actor consumes the permit after BEGIN
IMMEDIATE, under its current lease and scope, and keeps live consent for
that admission alone: restart, import or reconnect retires a queued original
(`deferred_send_interrupted`) and never rebuilds consent. Before any child,
preparation opens the original receipt store: the CLI's owner-only state
root, and its receipt folder when it exists, as no-follow, non-blocking
directory descriptors. The reservation commits in the same actor turn that
checks the exact lease and row; the child worker then checks the selected
session itself from that root, its own record (name, terminal agent, run and
conversation) and the absence of an OpenCode or DeepSeek binding, since the
name's prefix never authorizes a send; the consent veto runs last, in the
actual spawn turn. A refusal there never started anything. Each original
runs once, through the shared original-child runner: this service's own
pinned image with the fixed call, the exact stdin and the effect's
close-only custody lock. The custody, consent, lease and serve-owner loan
stay held until every original pipe really ends, a descendant's included.
The whole send, the CLI's lock wait included, has 30 seconds. The child's
exit settles nothing. After the physical drain, the actor's own drain
worker, holding the serve-owner and consent loans until it physically ends,
reads the CLI's receipt from those held objects within one never-renewed
5-second deadline: only an owner-only, single-link file within 256 KiB, its
version unchanged across the read, and its name, folder and root still the
original's afterwards. The actor then settles the receipt in the drain's
transaction, keeping the objects the read used until its commit: Recorded
from the exact `submitted` record, Failed as above, and Unknown otherwise,
with the receipt's or the child's reason. A moved, replaced or changing
store, a late read, and an original of an earlier run, whose store
observation went with it, are Unknown and never a safe absence. The same
original admitted again gets only its own receipt. macOS refuses every
original before any row (`deferred_send_native_image_unavailable`), Windows
refuses it too (`deferred_send_unsupported`), and only Linux advertises the
help marker. An unauthenticated hello naming it gets
`frontend_authentication_required`, like its siblings.

**Not admitted** (zerus-tlb9q). A refusal is `deferred_send_unavailable`,
which can hide anything up to an admitted original, so the client looks it
up and never sends it again. One refusal is closed instead:
`deferred_send_not_admitted`, for an original refused before its ledger
admission whose ledger provably holds no row for the request. In the writer
transaction the actor reads the row first, and only that successful empty
read, followed by a refusal before `ledger::admit`, proves it. A refusal
before that transaction (the owner stopping, no lease) is proven by a fresh
BEGIN IMMEDIATE read on the same held store. Only a request of this epoch's
own scope is ever answered not admitted or sealed. A busy or unreadable
store, an existing row, another connection's request (an expired one
included) and any failure from the admission on stay unavailable. Before
the reply the actor seals the request ID for the service epoch, and a sealed
ID never admits: a copy of the same request held behind the refusal gets the
same answer, while another connection's copy stays unavailable. The seals belong to the epoch, outside the
background runtime, so a runtime the service stops and starts again under
the same epoch keeps them; a new epoch refuses the old scope anyway. Each
kind seals at most 1024 IDs per epoch and never evicts one; a full fence
answers unavailable. The read-only lookup never answers not admitted. The
client takes it only from a completed exchange whose reply is exactly that
refusal: this kind's ABI, the original's request and scope, not accepted,
not durable, no receipt and the closed code. The send then settles as never
started ("the desktop service could not start it"): the draft stays and its
identity is released, with no lookup, direct path or resend. The deferred
answer (`deferred_answer_not_admitted`, the card keeping its choice), the
compact-to-send (`compact_to_send_not_admitted`) and the session resume
(`session_resume_not_admitted`) follow the same rule, each with its own seals.

The second kind, `deferred_answer` (`desktop::deferred_answer`, ABI
`desktop-deferred-answer-v1`), defines its closed wire for one `hgs answer
<session> --json` to a question card:
- **Intent.** The local session, the reserved request ID, the checkpoint's
  exact JSON, the question's consent identity as the person saw it at the
  click (`pinned`, exactly the closed `CONSENT_FIELDS` projection) and the
  exact stdin.
- **Verification.** Strict typed decoding of the checkpoint and the stdin,
  as the CLI decodes it. The checkpoint must be a valid `answer` checkpoint
  of this request whose identity hash is that of `pinned`; `pinned`, the
  checkpoint and the stdin name the same question, question hash, run and
  conversation. The stdin's answers are exactly the client's answer items,
  and the checkpoint's message digest is theirs. An empty conversation is
  accepted only for the CLI's own startup question identities. v1 refuses
  any `@` or other-machine session and any session but a terminal agent's,
  and offers only the sources whose `rejected` receipt proves that nothing
  was delivered: Claude permission mode, resume, usage limit and folder
  trust, Codex hooks trust, update, rate limit and folder trust, and Kimi
  folder trust, cache hint, plan review, tool approval and wire questions.
  Codex asynchronous questions and goal resume, Claude hook questions and
  Claude tool approvals stay on the direct path
  (`deferred_answer_source_refused`), as do OpenCode and DeepSeek
  (`deferred_answer_agent_refused`), decided before any offer.
- **Plan and binding.** The fixed `answer <session> --json` call. The plan's
  input digest is the SHA-256 of the raw stdin, which is the digest the
  CLI's receipt records. Before its child the actor compares `pinned` again
  with the native current question read from the original store
  (`consent_current`); that fresh observation, never `pinned` alone, decides
  source and transport. The observation is `hgs answer <session> --observe
  --json` (stdin: the question ID and the expected run and conversation).
  It gives `found` and the question's consent identity as `hgs inspect`
  shows it, rebuilt only from these sources' pure reads: it never fits a
  clipped panel, takes a lock, lists Claude's approval queue or writes a
  receipt, cache or any other file. A question it cannot see without an
  effect, or of another run, is not found, and the answer does not start.
- **Receipt.** Recorded only from the CLI's own exact
  `question_receipts/<request_id>.json` record of this session, run,
  conversation, question, question hash and stdin digest with status
  `answered` or `submitted` (Codex's asynchronous submission, not the
  provider's completion). Its `open_terminal` flag is only data for an
  explicit Open Terminal action. For Codex's update question (a
  `codex-update:` ID, which only the `codex_update` source may have), the
  record's `codex_update` phase travels in the outcome, strictly paired: it
  is present if and only if the answer is Update now (`answered` with
  `open_terminal`). Absence is the only "none"; `installing` or `exited` is
  the receipt's own observation, and `exited` does not prove the update
  succeeded. Any other value or shape, a phase on another status or answer,
  or Update now without one is Unknown (`deferred_answer_receipt_phase`),
  and the serialized outcome, receipt and lookup keep the same pairing.
  Failed only at the no-spawn boundary
  (`deferred_answer_not_started`) or from the exact `rejected` record
  (`deferred_answer_rejected`). Everything else after a possibly started
  child is Unknown and never resent.
- **Lookup.** A scoped read-only lookup by session, request ID and plan
  fingerprint.

The service actor executes it on Linux, as it executes a deferred send:
the same native lane, admission after BEGIN IMMEDIATE under the current
lease and scope, live consent for that admission alone (restart, import or
reconnect retires a queued original as `deferred_answer_interrupted`), the
original receipt store opened before any child (here the `question_receipts`
folder), the reservation, the selected session's own record checked from
that root, and the consent veto last in the actual spawn turn. Between the
session check and the original child, the worker observes the native
current question without any effect: `hgs answer <session> --observe
--json` runs from this service's pinned image in its own process group,
with its frozen stdin, no custody and its own 10-second deadline inside the
answer's 30 seconds. The held state root and the selected session must be
unchanged after its reply too. Only a reply that finds the question with
exactly the pinned consent identity of a v1 source starts the original
child; no reply, a failed or late one, a malformed or larger one, another
identity or another source starts nothing, and the answer fails as never
started. The original `hgs answer <session> --json` then runs once through
the shared original-child runner with the exact stdin and the effect's
close-only custody lock. After the physical drain, the drain worker reads
the CLI's own `question_receipts/<request_id>.json` from the held objects,
by the deferred send's rules, within the same never-renewed 5-second
deadline. It settles Recorded from the exact `answered` or `submitted`
record, Failed from the exact `rejected` record (`deferred_answer_rejected`)
or at the no-spawn boundary when no record exists
(`deferred_answer_not_started`), and Unknown otherwise; an original of an
earlier run is Unknown however exact its record. macOS refuses every
original before any row (`deferred_answer_native_image_unavailable`),
Windows refuses it too (`deferred_answer_unsupported`), and only Linux
advertises the help marker. Linux also advertises
`desktop-deferred-answer-update-phase-v1`. The client offers a Codex update
answer only to a service with that line, checked on the same route before
any offer, and keeps it direct otherwise. A Recorded update answer shows its
phase notice. Only the click that asked may open Terminal: a later check or
a reload's lookup only observes, and never opens Terminal, answers or offers
again. An unauthenticated hello naming the kind gets
`frontend_authentication_required`, like its siblings.

The observation fails closed by design. A Kimi panel that the person's
inspection fitted but that is clipped again when the observation reads it
gives no card, or one without a transport, so the answer does not start.
Kimi's cache hint includes its idle time in the question's hash, so a
redraw between the click and the observation changes the identity and
refuses. Claude's resume prompt keeps `can_answer` outside the hash, but
that is not a consent field and does not matter.
The third kind, `compact_to_send` (`desktop::compact_to_send`, ABI
`desktop-deferred-compact-to-send-v1`), defines its closed wire for one cold
Compact and send: one `hgs compact-context <session> --json` and then, only
in the same live admitted operation, one `hgs send <session> --json` whose
frozen stdin names that compaction:
- **Checkpoint.** A version 2 `compact_to_send` checkpoint reserves both
  request IDs, the compaction's (`request_id`) and the send's
  (`send_request_id`), distinct canonical UUIDs saved together with the draft
  before the offer. Version 2 is exactly that pair, and a reload never derives
  another send ID. A version 1 checkpoint keeps today's direct semantics.
- **Intent.** The local session, both request IDs, the checkpoint's exact
  JSON and the exact stdin of each child.
- **Verification.** Strict typed decoding of the checkpoint and both stdins.
  The compaction's stdin is the CLI's own three fields. The send's stdin is
  held to the deferred send's rules (`deferred_send::frozen`), and its
  `expected_compaction_id` must be this compaction: the CLI checks it again
  under its input lock before the first input byte. One session, run and
  non-empty conversation run across the checkpoint and both stdins, and the
  checkpoint's message digest is the send's. Only a local terminal agent's
  session is taken: DeepSeek and OpenCode (`compact_to_send_agent_refused`),
  another computer and an original larger than the control frame stay direct,
  decided before any offer. A plain deferred send still refuses
  `expected_compaction_id`.
- **Plan and binding.** The two fixed calls and both raw stdin digests. The
  operation's key is its compaction request; another send under it conflicts.
- **Receipts.** Each child has its own step: Queued, Running (durably
  reserved, so it may have started), Recorded, NotStarted or Unknown. The
  compaction is Recorded only from the exact `compact_receipts` record of its
  request, session, run and conversation with status `submitted`; the
  `in_progress` placeholder, `uncertain` and anything else are not. The send
  is Recorded only by the deferred send's own `input_receipts` rule.
  NotStarted (`compact_to_send_not_started`) is only for a child the
  operation's own ledger proves never crossed its reservation. Exit codes,
  stderr and a refusal printed after a possible start never decide. The send
  never runs after a compaction that never started, and never before the
  compaction settled. Evidence only moves forward (`Receipt::follows`): a
  settled child never changes, so nothing can overwrite a recorded compaction
  or send.
- **Settlement and lookup.** Only the drained receipt settles. The send's
  exact receipt means sent. Neither child started means nothing ran. A send
  that never started after a compaction that ran or may have leaves the
  compaction's identity. A send that may have started is unknown. The lookup
  is read-only and keyed by the compaction request and the plan fingerprint.

The request has its own ordered lane. On Linux the service actor runs the
compaction the way it runs a deferred send:
- Admission happens after BEGIN IMMEDIATE, under the current lease and scope,
  with live consent for that admission alone.
- The `compact_receipts` folder and the state root are opened before the
  child.
- The compaction's custody is reserved durably before it can spawn.
- In the spawn turn, after its wait, the selected session's own record is
  read again from the held state root, then the consent veto runs. The
  runner's deadline check comes last.
- The original `hgs compact-context <session> --json` runs once through the
  shared original-child runner.
- After the physical drain, the receipt is read from the held objects within
  one never-renewed 5-second deadline.

A reserved child counts as never started only from the runner's no-spawn
result together with a read of the held store that finds no record of it.
An original no runtime holds is never read, so it stays Unknown. Restart,
revoke or a lost preparation retires the operation
(`compact_to_send_interrupted`). A child never reserved is then recorded as
never started from the ledger. A reserved one keeps its custody for the
read-only drain. Store recovery drains an earlier run's Unknown without
reading any receipt, and records a send never reserved as never started
(`compact_to_send_interrupted`): no runtime is left to continue it.

A compaction that never started also ends the send. After one that ran or
may have, the same live operation waits, and only it: a restart never resumes
a wait. The wait has one budget of 600 seconds from the compaction's drain,
never renewed, inside the operation's own deadline of 690 seconds from its
first turn. The service runs one operation at a time, so an operation admitted
while another waits stays queued; its deadline starts only when it gets that
turn, and the time spent queued never expires it (zerus-qw7wp).

Off the actor, with the operation's loans, each read checks the selected
session's own record from the held original root, with the root checked
before and after. It continues only while that record's
`compact_context_request` names this request, run and conversation:
- `completed`, not superseded, on a session whose activity and phase are both
  idle: the send may start;
- `submitted`, `compacting`, or completed while the session is busy: read
  again;
- anything else ends the wait without the send (`compact_to_send_slot_ended`).

The slot is a continuation condition only. Hooks carry no request ID, so a
`/compact` typed in Terminal meanwhile completes it too, and the compaction
keeps what its own receipt proved.

These end the wait without the send:
- a stop, a revoke or a lost source (`compact_to_send_vetoed`);
- another session record (`compact_to_send_session_changed`);
- a replaced root (`compact_to_send_root_changed`);
- the budget (`compact_to_send_wait_expired`);
- the deadline (`compact_to_send_chain_expired`).

The send was then never reserved, so the ledger proves it never started.

Once the slot completes:
- The send's `input_receipts` folder is opened under the same held root,
  never resolved again.
- Its custody is reserved.
- In its spawn turn, after its wait, the session's own record and the slot
  are read again from the held root, then the consent veto runs. The
  runner's deadline, bounded by the operation's own, comes last.
- The original `hgs send <session> --json` runs once with its frozen stdin,
  whose `expected_compaction_id` the CLI checks again under its input lock.
- The send is Recorded only from its exact `submitted` input receipt; a
  refusal printed after a possible start stays Unknown.

Linux advertises `desktop-deferred-compact-to-send-v1`. macOS refuses every
original before any row (`compact_to_send_native_image_unavailable`) and
Windows refuses it too (`compact_to_send_unsupported`).

The fourth kind, `session_resume` (`desktop::session_resume`, ABI
`desktop-deferred-session-resume-v1`), defines its closed wire for one
resume-then-send of a stopped session: one `hgs session-action <session>
--json` that resumes it and then, only in the same live admitted operation,
one `hgs send <session> --json` bound to the run that resume returned:
- **Checkpoint.** A version 3 resume-then-send checkpoint (a resume, never a
  fresh start or a resume with compaction) reserves both request IDs, the
  resume's (`request_id`) and the send's (`send_request_id`), saved together
  with the draft before the offer. Its receipt family is `action_receipts`.
  A version 1 checkpoint keeps today's direct `hgs resume` path.
- **Intent.** The local session, both request IDs, the checkpoint's exact
  JSON, the exact resume stdin and the send's frozen template.
- **Verification.** Strict typed decoding. The resume stdin is the CLI's own
  `{request_id, action: "resume", expected_run_id, expected_conversation_id}`
  with no other key. The template is held to the deferred send's rules
  (`deferred_send::frozen`), names the original run, its conversation and its
  own request, and no compaction. One session, run and non-empty conversation
  run across the checkpoint and both stdins, and the checkpoint's message
  digest is the template's. Only a local Claude, Kimi or Codex session is
  taken. DeepSeek, OpenCode (`session_resume_agent_refused`), another
  computer and an original larger than the control frame stay direct,
  decided before any offer.
- **Binding the send.** `hgs session-action` mints the resumed run itself, so
  the send's actual stdin exists only after the resume: `session_resume::bind`
  replaces only the template's `expected_run_id` with the `result_target` run
  of the resume's own exact receipt, and the CLI's input receipt records that
  stdin's SHA-256. The digest is fixed in the same write that records the
  resume and never changes.
- **Receipts.** Each child has its own step. The resume is Recorded only from
  the exact `completed` action receipt of its request, session, run and
  conversation, with no archive, whose `result_target` names the same session
  and conversation and a new run. The receipt's bytes are read from the held
  store and projected by the CLI's own observation rule
  (`session_action::status`, as `observe_original` applies it). Its exact
  `failed` receipt is Failed (`session_resume_refused`): the CLI writes it
  before its launch began, or once the server that created the resumed
  terminal says, twice in a row, that it has no such pane, before any record
  of the new run while the session's record still names the original run
  (zerus-5sl7r), so nothing was resumed and nothing remains. `claimed`,
  `uncertain` and anything else are not. The send is Recorded only by the
  deferred send's own `input_receipts` rule for the bound stdin. NotStarted
  (`session_resume_not_started`) is only for a child the operation's own
  ledger proves never crossed its reservation. The send never runs without a
  recorded resume, since there is no resumed run to bind.
- **Settlement and lookup.** Only the drained receipt settles. The send's
  exact receipt means sent. Neither child started means nothing ran. A send
  that never started leaves the resume Recorded, Failed or Unknown. A send that
  may have started is unknown. The lookup is read-only and keyed by the resume
  request and the plan fingerprint (`session_resume_result`).

On Linux the service actor runs the resume the way it runs a compaction:
admission after BEGIN IMMEDIATE under the current lease and scope, with live
consent for that admission alone, which covers both children; the
`action_receipts` folder and the state root opened before the child; the
resume's custody reserved before it can spawn; in its spawn turn the selected
session's own record of the original run read again from the held root, then
the consent veto; the original child once through the shared runner, which
marks its custody close-on-exec before anything else runs, so the terminal
server and the agent it starts never hold it; and, after the physical drain,
the receipt read from the held objects within one never-renewed 5-second
deadline. `completed` means the resumed run's supervisor wrote the session's
record of that run, or, for a start slower than `hgs session-action`'s own
10-second wait (a login shell, its hooks or the state lock before that first
record), that its terminal was positively alive then; the agent confirms its
conversation later (zerus-5sl7r). A start seen alive whose terminal cannot be
observed at the end of those 10 seconds is followed for at most 8 more, until
an exact observation decides. A dead pane kept, a pane running another
process than the launched one (tmux `respawn-pane`), or a terminal never
observed exactly, is `uncertain`.

Each observation is of the exact terminal the resume created, as its server
answered the launch: on tmux the server's pid and start time, the pane's
`%N` and the pane's process (`new-session -P`), with that process's kernel
birth read right after the launch; on the Windows session host the host's pid
and creation time, the pane and its root process. Only that server says the
pane is gone, and the pane is alive only while it runs the launched process
(on tmux its pid and kernel birth). A missing or refused socket, no host, a
server or host other than the one that created the pane (one started at the
same path while the original runs on), or a failed or late answer is never
read as an absent terminal. Each observation has a 2-second deadline; tmux's
runs in the C locale. On Windows that deadline is absolute over the pipe, the
hello and the request: the connection is shut down at it, however the host
spreads its answer, and a hello answer or reply whose decoding ends at or after
it is the connection's loss, whatever it says, even when its bytes were already
read (zerus-xngdd). A resume that is not recorded
ends the send.

After a recorded resume the same live operation waits, and only it. The wait
has one budget of 180 seconds from the resume's drain, as long as the direct
path waits, never renewed, inside the operation's own deadline of 270 seconds
from its first turn, when it gets the work slot (the clock compact-to-send
uses, `operation_clock`). Off the actor, with the operation's loans, each read
takes the resumed run's own record from the held original root, with the root
checked before and after:
- the record shows a confirmed (no `expected_id`), idle (activity and phase),
  live agent with no unconfirmed input or settings and no pause: the send may
  start (`state::resumed_readiness`, the CLI's own record gates, read without
  any effect);
- not written yet, unreadable, or starting or busy: read again;
- failed, ending or out of its conversation: the wait ends without the send
  (`session_resume_ready_ended`);
- its recorded supervisor exactly exited without recording an end: the wait
  ends without the send (`session_resume_ready_gone`, zerus-5sl7r). Exact
  exit is no process at its pid (or only its zombie), or another kernel birth
  there (start ticks on Linux, the stored start on macOS) than the one the
  wait pinned when the process showed the recorded start. The recorded start
  is wall-clock text, which Linux derives from a boot wall time that moves
  when the clock is set: a start that differs while a process lives, or one
  that cannot be read, proves nothing.

A record that becomes unreadable after a readable one (an atomic update can
fail one held read) ends the wait only once it stayed unreadable for 10
seconds from its first unreadable read and the supervisor the last readable
record named exactly exited (`session_resume_ready_gone`). A run never
recorded keeps the wait to its budget.

A resumed Codex stays pending until its first message, and only its terminal
shows when it takes one (zerus-5wmh.66.4.10). For a Codex session the record
only says whether the session ended. While it has not, each read runs the
send's own effect-free observation, `hgs send <session> --observe --json`
with stdin `{expected_run_id, expected_conversation_id}`, from the service's
pinned image in its own process group, with no custody and a 5-second
deadline that never passes the wait's. The CLI runs the send's own gates
before its lock: the record and its exact live process, a pending resume's
own verification (the supervised Codex holding the expected rollout open)
and the empty native composer. It takes no lock and types nothing; it writes
no receipt, attachment, diagnostic capture, record, attention state or read
mark, and prints `{ready, reason}`. One gate, the CLI's first step
(`cli::observe_gate`), decides every argv with `send` and `--observe`. It runs
before the helper rendezvous, any machine guard, `@host` or `--client`
forwarding, a dry run, a session reference or `--session-id` selection, label
repair, provider routing and stdin. Only exactly `hgs send <session> --observe
--json`, naming a local Codex session by its plain name, goes on. Every other
form refuses with no effect: nothing forwarded, no provider contact, no stdin
read, no lock, ID repair or receipt. OpenCode and DeepSeek sends would ignore
the flag, and an older remote CLI would not know it (zerus-5wmh.66.4.10.1).
`state::dispatch` refuses any `--observe` that reaches it by another route. Only a clean, timely reply of at most
8 KiB with `ready` lets the send start, and the record is read again after
it. Anything else reads again until the budget ends the wait. The
observation runs once more right before the send's spawn turn, and that
turn checks that the record has not ended.

A stop, a revoke or a lost source (`session_resume_vetoed`), a replaced root
(`session_resume_root_changed`), the budget (`session_resume_wait_expired`)
and the deadline (`session_resume_chain_expired`) end it too; the send was
then never reserved. Panels that only the terminal shows, such as a Claude
resume or usage-limit choice, are checked by `hgs send` under its input lock,
and a refusal there after a possible start stays Unknown. Once ready, the
send's `input_receipts` folder is opened under the same held root, its custody
is reserved, and in its spawn turn the resumed run's record and its readiness
are read again from the held root before the consent veto and the runner's
deadline, bounded by the operation's own. A restart never resumes a wait or
starts the send. Linux advertises `desktop-deferred-session-resume-v1` and,
for the Codex observation,
`desktop-deferred-session-resume-codex-v1`; macOS refuses every original
before any row (`session_resume_native_image_unavailable`) and Windows refuses
it too (`session_resume_unsupported`).

The composer offers it only from a "Resume and send" of a stopped local
Claude, Kimi or Codex session, while the desktop service route is set (Linux
and macOS in desktop-service mode). Only then does the waiting save reserve a
version 3 checkpoint; a fresh start, a resume with compaction, another
computer's session and a subagent keep version 1 and the direct `hgs resume`
path. When the start call would run, the whole resume-then-send goes to the
service if a live service advertises the kind and the original verifies, and,
for a Codex session, only if the service also advertises the Codex
observation; otherwise the direct path starts it, decided before any offer.
An older service never sees a Codex original. From
the hand-over the draft is the delivery the service holds, like a handed
"Compact and continue": its row shows the message sending, the status shows
the service's line, and Cancel send stays shown but locked. Once offered,
nothing falls back: a lost reply goes to the read-only lookup, and Unknown is
never sent again. Only the exact `session_resume_not_admitted` refusal settles
without a lookup, as never started (see the deferred send's not admitted
rule). A recorded send clears the draft; its row, recorded under
the stopped run, then belongs to the run the resume's receipt returned as well,
so it stays shown until the agent's history shows the message
(zerus-5wmh.66.4.9.3). A send that never started
keeps the draft and releases its identity: after a refused resume with "The
session was not resumed", after a resume whose agent never became ready with
the direct path's own "The agent did not become ready" line. A send that may
have started stays uncertain until Allow retry. A reload only looks it up.

A draft holds one such identity until it is resolved. A definite outcome
resolves it, as does a cancellation before its call was dispatched or, for a
start or compaction, the session seen ready for the send. An uncertain outcome
keeps it. A compaction, like an interrupt, a clear or an answer, types into the
agent, so its outcome is definite only when `hgs` never started or refused
before any input (exit 1 without "delivery uncertain"). A timeout, a broken
transfer, any other exit, a mismatched acknowledgement and a refusal naming an
earlier attempt of the same request are uncertain. `hgs` reports every failure
after the typed command, Enter or Escape as "delivery uncertain". Once its call is dispatched, a Cancel send, an interruption or a time
limit keeps it too, and the call's own late answer then decides: a definite one
resolves it, an uncertain one keeps it. Only calls still in flight are tracked
for that late answer, at most 32; past that the oldest goes, and its draft
keeps the identity until Allow retry, which also retires its draft's tracking.
While it is held, the composer stays uncertain, with a notice that delivery is
not confirmed (or, after a reload of a draft saved while sending, that it was
interrupted), and refuses a new Send, a start or compaction, and a Retry of a
failed message. A reload of a draft that holds one, including a draft saved
while a start or compaction waited, loads it in that state. History carries
only text, never a request id, so it never settles a held identity, not even
the row of its own send; only evidence carrying that request id can, which a
later slice reconciles. A draft holding none keeps the legacy text match. Apart
from the definite outcomes above, only the user's explicit Allow retry ends a
held identity; the next attempt then reserves its own before its call. Rename,
recover and import treat a draft that holds one as occupied, even without text
or files, and never overwrite it.

This fail-closed rule changes behaviour in rare cases. After a quit or crash
while a start or compaction ran, and after a Cancel send or time limit while
that call was still running and then answered uncertainly, the user clicks
Allow retry before sending again. A Retry of a failed message waits until the
composer's own unresolved delivery is resolved, and a Retry that ends
uncertainly leaves the composer uncertain even though its draft is another
message. An uncertain send whose text later shows in history no longer clears
its draft by itself: its failed row goes, and the draft stays uncertain until
Allow retry.

## Executing kinds and the exit boundary

The termination-outcome prerequisite shares the maintained CLI's exact
`session-action-status` owner as `state::observe_lifecycle_receipt`. It takes
the original request bytes (at most 8 KiB) and uses the same request hash,
64 KiB private receipt reader and target/archive checks. It never calls
`attempt`, acquires a writer or creates missing state. Returned observations
are data, including a later recorded completion; they prove neither current
checkout ownership nor resource drain and cannot authorize cleanup or retry.
A future actor must check its typed original/current scope after BEGIN, retain
it durably and perform receipt I/O off the control thread. That integration is
not implemented by this prerequisite. Existing Terminate calls keep their
45-second local and 60-second peer budgets; the 30-second owned-runner cap is
unchanged, and no new Terminate dispatcher is introduced.

The separate dormant `creation_clone` policy covers only an explicit STATUS
lookup for an original creation clone in `cloning` or `unknown`. Its expected
binding keeps the configuration digest, distinct creation/clone UUIDs, physical
machine, exact original host, target and parent, integer source revision and
raw source digest. It has
no creation UI-snapshot fingerprint or G1 ledger kind. Original bytes (up to
1 MiB) stay unchanged; duplicate known raw keys refuse before Value projection.
Completed-worktree, bound-session or acquisition-confirmed contradictions stay
review-only, and a prospective launch UUID is retained without being used.

One retained instance yields at most one private proposal using the existing
`repository status` invocation, without stdin or a new clone identity. A lost
proposal, cancellation, pressure or uncertain result cannot regenerate it.
Replies are at most 64 KiB and must pass the existing repository decoder plus
both original parent and target comparisons. Only `cloned` or `failed` produces
a closed receipt of at most 16 KiB, retaining observed path text and discarding
private diagnostics/extras. An original unknown state may be explicitly checked;
an unknown result remains spent review. The returned binding and receipt are
data, never placement, project/worktree mutation, launch, release or settlement.
This pure module has no source factory, current routing proof, actor, I/O or
production caller; G2 `.4.3`, `.4.2` and G1 current Operations admission stay open.

The separate pure `creation_worktree` policy proposes one original 15-second
`worktrees status` lookup. Its binding keeps the original source target distinct
from the expected prepared destination, common directory and branch. Pending
worktree or original unknown records require receipt support and settled clone
evidence; ambiguous ownership and later-stage contradictions remain review-only.
The same private byte, duplicate-key and one-shot rules apply. Created evidence
requires exact observed destination/common/branch and `effects_started=true`;
its closed receipt has nine fields. Failed evidence requires an explicit Boolean
`effects_started` and has only six fields, with the complete expected binding
returned separately. Optional failed path/branch identities must still match;
they are discarded rather than manufactured when absent. A nonempty or malformed
`resource_error` refuses. Neither variant calls the reducer or grants ID rotation,
retry, launch, settlement or release, including a failed no-effect observation.
The encoded receipt cap remains 16 KiB; original/reply caps remain 1 MiB/64 KiB.
There is no G1 operation kind, actor, caller, I/O or current production admission.

The separate [original launch-status contract](desktop-creation-launch-status.md)
defines a proposed bounded target-side observation. It accounts for bootstrap
identity reads as well as selected sources, keeps missing evidence unknown and
leaves the current whole-session listing path unchanged. The command, capability
marker and caller are not implemented or admitted by that contract.

The pure `task_message` policy names `TaskMessageRecovery`: the existing
`--recover-only` command can reserve an uncertain canonical comment or reconcile
task metadata, so a future caller needs executing-kind admission. This slice
only proposes the original queue/note call once, with the existing 90-second
timeout, exact original marker and frozen stdin. It never reconstructs missing
input. Send/reopen and deferred native input remain review-only.

A new task message freezes its one actual `hgs task message` stdin before its
outbox checkpoint, and the editor sends that same call.
`outbox::reserve_message` saves the stdin in the same write: a canonical stdin
(the content's serialization) as its digest `input_sha256`, so the record holds
one copy of the content; any other as its exact `input` bytes. The complete
record must fit the 32 MiB record limit, or nothing is written or sent. Reading
a record (`outbox::decode`) refuses a present but malformed or mismatched
`input` or `input_sha256`, or both at once, so such a record is never treated
as legacy and no call is made. `message_call` sends the frozen bytes, recovery
included, and the policy requires them to equal the retained input. A legacy
record without either never qualifies for an admitted actor; only the direct
path still serializes its content. A reservation, reopened or new, is durable
data about the original, never consent to execute it.
The service-owned leaf for these queue and note messages is specified in
[the task message design](desktop-task-message.md).

Configuration/project/task are independently retained expected data, not fields
observed in an outbox record. Raw record and input each stay at most 1 MiB;
known duplicate fields are refused at every inspected level before projection.
Queue/note evidence must carry the exact original task/request/mode/dispatch and
expected state, without any message executor or nonempty native delivery fields.
The closed eight-field receipt explicitly carries `executor: null`, with the
complete binding returned separately; it is capped at 16 KiB and contains no
body, files, prompt or errors. A missing reservation is never no-effect proof.
No receipt delivers, consumes, settles, releases or rotates anything, and this
module supplies no runner, I/O, store, current G1 kind/profile or caller.

The first pure policy is `operation_recovery::task_start::Original`. G1 and G2
agreed its contract on 2026-10-30. It has no actor integration, process runner,
SQL, positive job IPC or executing permission. The dedicated operation
table/API remains G1's lane, separate from native `desktop-ledger::Kind`.

The [closed native request boundary](desktop-task-start-reconcile.md) carries
bounded exact producer checkpoints in a distinct TaskStartReconcile lane. Its
Linux/macOS actor admits only the fixed, explicitly requested recover-only
worker under its current native guard. Windows and unsupported originals stay
refused. This adds no general Operations permission and leaves the background
profile unchanged.

`Key` separates kind (`task_start_reconcile`), schema 1, configuration,
project/task and the original request UUID from every reconciliation-attempt
UUID. `Binding` adds the exact marker, original executor's stable machine,
session, run and conversation, and three distinct fingerprints:

| Field | Exact meaning |
| --- | --- |
| `source_record_sha256` | SHA-256 of complete original source record bytes |
| `ui_snapshot_sha256` | Existing `outbox::snapshot` fingerprint of the serialized record Value, as used by the UI |
| `task_start_reconcile_input_v1_sha256` | SHA-256 of the exact retained `TaskCall.input` bytes; this versioned field is its digest domain, without a hash prefix |

G1 supplies the immutable expected binding and retained input. Verification
checks the existing outbox/Start identity validators, all original native scope
bindings and parsed equality between the input and the complete saved
`intent.plan`. Opaque fields stay intact. Missing retained input stays reviewable;
this policy never regenerates request bytes from a plan or another build graph.
An actor must not replace bytes, consent or fingerprints under the same key.

The ordinary Next Start producer constructs its one actual delivery `TaskCall`
before the durable checkpoint, then executes that same captured call. When the
complete encoded checkpoint fits 1 MiB, `intent.retained_input` preserves the
exact input string, including its original serialization. The size check counts
the duplicated plan, JSON escaping and unknown record fields, reserving 12 KiB
for the worst-case 2048-character settlement error. If either input
or checkpoint exceeds that bound, the producer omits this optional field and
continues through the existing native Start path. Native 31 MiB plans and 32 MiB
outboxes keep their limits; the 64 KiB frontend control frame stays separate.
The future actor must independently recheck its record/input bounds and refuse
missing retained input. Existing pending records are never backfilled from a
plan, and a new UUID reusing a terminal record cannot adopt its old input.
Reservation or checkpoint failure offers no delivery. Settlement and the G3
adapter preserve the retained field as private original data; generic outbox
equality/snapshot checks and explicit native release semantics are unchanged.
This producer checkpoint supplies evidence, without current actor admission or
permission to execute a recovery call.

Each explicit `step` returns NeedsReview, one private Proposed call, or
SettledSubmitted evidence. Default review, cancellation and unknown outcomes
produce no proposal. Historical started/released phases never prove current
settlement. An explicit request can propose only:

```text
hgs task start-deliver TASK --project PROJECT --json --recover-only
```

The unchanged retained input accompanies it. This existing command cannot
submit native input, but can settle original canonical task metadata from an
existing native receipt and exchange task metadata remotely. It is therefore
a fenced reconciliation effect, not a generic read-only command. It never
uses automatic `task_start::release`, a new Start/plan or a new delivery UUID.

Only a reply matching schema 1, original request/project/task, nested task
membership and `submitted` can propose settlement. The returned receipt is a
closed allowlist of schema/id/project/uuid/state, below 16 KiB; host task views
and errors are discarded. A missing, malformed, refused, uncertain, oversized
or timed-out reply remains reviewable with the original identity. Debug output
cannot expose private proposed input. This evidence still grants no authority:
G1 must match its exact reserved original operation, attempt and epochs.

The pure component accepts at most 1 MiB each for the record, retained input
and host reply, 512 bytes per executor identity and existing project bounds.
Larger legacy outboxes (up to 32 MiB) remain in their existing stores for review;
they are never truncated. There is at most one proposal per step. G1's agreed
execution admission is 32 unsettled operations, **one executing reconciliation
globally per configuration**, and one coalesced current lookup per original
scope, with a 30-second owned-child deadline, 1 MiB stdout and 16 KiB stderr.
All owned pipes/readers must actually drain before execution admission releases;
returning from a bounded stop does not establish that fact. These are actor
requirements, not guarantees implemented by this pure component.

G1 owns durable pre-execution reservation, current writer/service epoch and
lease/policy checks, fresh live writer drain, revoke and drain accounting.
Source/import/body provenance stays separate from that current ownership
proof. A remapped/missing computer identity refuses; a current display alias
never replaces the original executor. Immutable unknown attempts stay unknown.
A later separately admitted reconciliation may use only the unchanged original
call under its command's receipt semantics; notification retry rules do not
apply. No unknown result permits native input, a new delivery identity, changed
plan, automatic release or broader action. Pending/unknown work is never evicted
under pressure; admission refuses instead, and status uses bounded keyset pages.

The existing merged task Start/message outboxes remain their owners' records.
Only admitted producer schemas are recognized; a provisional new-session task
executor journal is not consumed by this slice. Creation migration preserves original durable effect
barriers. Further slices must coordinate their writer drain and exact receipts
before moving any execution into the actor, with kill/revoke/restart proof for
each kind. Pure tests cover frozen-input reconstruction, unknown/cancelled
decisions, identity and consent changes, exact receipts and historical records.
They do not claim a real process-kill, pipe-drain, late-epoch or worker restart
proof: those remain required at G1's executing reserve/finish boundary.

The shared test-only [operation-port vectors](../client/fixtures/desktop-operation-port-v1.json)
pin UTF-8 source and retained input bytes, the distinct UI/source/input hashes,
the versioned consent domain and the length-prefixed operation-key domain. A
Unicode project name distinguishes UTF-8 byte lengths from character counts.
There are 27 valid/invalid model traces: pressure, fresh=false, revoke before
reserve, exact ticket/receipt identities, unknown terminality, restart, separate
explicit attempts and child/stdout/stderr drain before global release.

`client/core/tests/support/operation_port.rs` is an observation checker that G1
can reuse against its fake-port traces. It calls no port and implements no
ledger, actor or runner. `operation_port_contract` tests that checker and the
actual merged pure Original/Proposal policy. These are model conformance and
fixed-byte interoperability evidence, not executing-backend proof. G1
`zerus-5wmh.66.3.22` still owns the real schema, guards, reserve/finish, worker,
startup recovery and actual drain qualification; G2 `.66.4.2` depends on it.

All slices preserve `.9.30`: Dock Quit, logout/shutdown, other Cocoa
`terminate:`, Windows `WM_ENDSESSION`, SIGKILL and Tauri restart retain the
Exit/`exit_work` fallback without requiring a page flush. The updater restart
explicitly calls `quit_with_flush`. This status slice changes neither path.

See [import evidence and handoff](desktop-migration.md),
[immutable publication](desktop-import-transport.md) and the
[G2 retirement inventory](superpowers/specs/2026-10-29-qt-retirement-inventory.md#g2-durable-operation-and-recovery-owners).
