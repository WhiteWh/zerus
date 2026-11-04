# Native reservation data for the desktop actor

`zerus_desktop_ledger::native::inspect` is a dormant read-only prerequisite for
the Linux Post/Withdrawal worker. It reads an existing reservation; it never
reserves, dispatches, settles, retries or creates a lease. There is no production
caller, worker, SDK operation, migration or positive desktop command in this
slice. The existing [Linux native port](desktop-linux-notifications.md), GUI
poster and schema1 saved identities keep their current behavior.

The actor supplies the complete original `Reserve`. Under one deferred read
transaction, the unique request-ID lookup requires its exact reservation
fingerprint and stored intent/revision, writer epoch, service epoch, adapter,
lease and optional retry binding. The writer must still be unrevoked at those
epochs. The attempt must be reserved without a receipt, and the intent must be
current, unwithdrawn and not a historical/focus-suppressed baseline. Only session
Post and Withdrawal identities are supported; Badge and Presence are refused.

The identity is at most4096 UTF-8 bytes and must encode to the exact original
identity and intent digest. The saved JSON payload is at most64 KiB. SQL caps
both before materializing either string into Rust. Invalid JSON and trailing
bodies refuse. The snapshot retains the exact saved payload bytes and their
SHA256, including opaque payload fields. These are database intent bytes at that
revision, not a claim about original import or native wire bytes. The existing
`put_intent` contract forbids changing a revision's payload. No original source
file, import, token owner, category or native endpoint is inferred from it.

The returned `Snapshot` has private immutable fields, redacted Debug and no
Clone or serde-Serialize implementation. Its bounded accessors expose only the
saved request, sequence, identity, payload and digests. The read transaction ends
before returning. A later intent or writer change does not mutate an already
returned snapshot; it also does not keep that snapshot current.

Re-reading a reserved row is never permission to send. It may already have
reached the OS, or be awaiting an unknown outcome. Inspection cannot reproduce
the original `fresh=true` reservation response, renew a lease, grant profile or
frontend-drain proof, or resolve lost delivery. A future actor must separately
bind the original fresh reservation to its live configuration/profile, current
writer/service/adapter leases, expected agent owner, bounded payload, exact
daemon namespace and actual worker lifetime. It must recheck these at dispatch.
Native-ID mapping, activation tombstones, per-owner categories/actions and
legacy release remain separate qualified prerequisites. An imported marker,
serialized phase or this data snapshot supplies none of those permissions.

After revoke or withdrawal this lookup refuses a new snapshot; existing
`finish` still settles only the exact original already-reserved receipt under
its existing immutable fingerprint rules. This helper never changes reservation,
receipt, recovery or rollback behavior. Parent `.66.3.24` and authenticated
frontend drain `.66.3.23` remain incomplete and fenced.

Isolated SQLite tests cover the complete request and stored binding, epochs,
revoke/withdrawal/baseline refusal, terminal attempts, corrupt/oversized data,
exact-byte retention, no schema/data writes and unique indexed query plans.
They qualify this read helper only, not an actor/worker, native API or live lease.

## Original commit response

`native::inspect_original` consumes the existing non-Clone, non-serializing
`Reservation` returned by `reserve`. Only the original successful fresh commit
adds its private process-local origin, after the transaction commits. Exact
duplicate and coalesced responses carry no origin. Setting their public `fresh`
flag or copying an `Attempt` cannot create one; changing an original response's
public fields refuses. Existing reservation and receipt SQL, schema and response
data remain unchanged. The private field also prevents external struct-literal
construction; there are no such callers in this repository.

The helper checks the sealed complete original request and sequence against the
public response, then uses the same bounded indexed read-only inspection above.
Changed epochs, withdrawn or blocked intents, terminal receipts and changed
stored bindings refuse. An error consumes the response too. Losing it, dropping
it or restarting cannot reconstruct it by reading the row or reserving again.
Only the existing explicit retry after a confirmed pre-delivery refusal may
commit a different attempt with its own original response. Unknown is never a
retry reason.

`OriginalSnapshot` has private fields, redacted Debug and no Clone or serde
implementation. It exposes only a borrowed `Snapshot`. This proves origin from
a fresh commit response, not freshness at a later dispatch. It owns no store,
configuration identity, clock, current lease, role proof, native connection or
worker charge. The future actor must choose the correct store, bind the original
response to its current configuration/profile/leases and recheck all live guards
at native admission. Even a genuine original response does not prove that no
other caller used its public data to issue an effect. No production caller,
worker, controller or positive desktop API is added by this prerequisite.

Linux tests qualify exact commit origin, read-only inspection, public-field
tampering, duplicate/coalesced/lost responses, changed current bindings and
confirmed-refusal retry separation. The Mac is not qualified for this internal
helper under the coordinator's standing exception; selected ledger and root
checks at the exact merged head remain required once it is reachable. This is
not native execution or whole-profile handoff qualification.
