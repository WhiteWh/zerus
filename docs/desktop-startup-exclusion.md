# Preparation and replacement startup exclusion

The internal `startup_exclusion::Startup` and `producer_prepared::Context` own
one existing handoff boundary. The [startup consumer](desktop-startup-consumer.md) now holds Startup in one
local helper; producer Context still has no production caller. They are not
producer leases, current role proofs,
permission consumers or positive startup receipts.

For existing desktop state, `Startup::hold` reads the bounded existing writer
policy and prepared record, takes the actual handoff file lock, then repeats
those reads while retaining it. Active policy, either retained preparation
phase, malformed/unknown/incomplete state and an unsettled handoff marker
refuse. Only absent or Revoked handoff markers can pass this legacy-startup
check. No table migration, service start, journal mutation or writer revocation
runs here. Genuine directory absence creates no directory, database or lock;
that result owns no exclusion and never proves readiness or current ownership.

The future consumer must claim its canonical instance before this check and
hold the existing boundary through actual writer admission. A snapshot followed
by a later start is insufficient. An absent-state result also needs that actual
canonical startup ordering; it must not become a general permit that survives
concurrent service creation. With a supporting CLI, the consumer retains the actual helper/pipe lifetime
and refuses before GUI writers when this check fails. Its old-CLI compatibility
path retains the previous GUI startup and grants no held/current producer proof. Its canonical ordering is part of the consumer
contract, not an independent property of this Startup value.

`producer_prepared::Context` consumes the original authenticated `Session`,
takes the existing presence fence and handoff lock, and probes the service's
canonical endpoint again off-actor. Production resolution reuses the existing
`TMPDIR` policy, never a client-supplied path. The fresh ordinary hello must
match the original instance, clean build, configuration and live non-replay
identity; only its fresh query nonce differs. Changed or missing endpoints
refuse and destroy that original connection. Same-user socket authentication
and a point-in-time hello do not prove signed native-host trust, continuous
liveness, current profile instrumentation or completed drain.

A retained Context keeps both locks and the same Session through preparation;
no field or lock can be extracted or cloned. Field declaration order destroys
the Session/FD and its registry reservation before releasing the locks on error,
panic or ordinary drop. A replacement holding the boundary excludes preparation
before its fresh socket query. A preparer holding it excludes replacement even
before a row is committed; after commit, the durable row continues to refuse
replacement after all locks are released. The service epoch/configuration reads
for prepare and abort intent now occur inside their IMMEDIATE transactions.

The private socket tests exercise both lock orders and changed instance, build,
configuration, replay, writer identity and missing endpoint cases. They verify
actual peer EOF and reuse of the original registry slot after context refusal.
Prepared and abort-requested rows both refuse replacement after lock release.
No-state, malformed marker, active policy, future schema and linked-root cases
stay distinct. These checks do not qualify actual frontend startup, worker
cancellation, native posting, current source admission or a whole profile move.

Before any production prepare caller, reconciliation must also recover the
persisted original without an in-memory Ticket after crash or sync failure.
AbortRequested never clears that fence. Existing positive G1 APIs remain gated
on current role/profile proofs, continuous negative consumption, actual resource
and store barriers, G3 capture/publication and whole-profile rollback.
