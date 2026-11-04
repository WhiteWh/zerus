# Retained producer identity session

The dormant internal `src/desktop/producer_session.rs` composes the existing
canonical [identity probe](desktop-producer-binding.md) and terminal
[owned channel](desktop-producer-io.md). No production caller, listener,
resolver, worker, command, help marker, store or frontend controller installs
it. Existing desktop/feed framing, cached instance replies and GUI behavior
remain unchanged. This is connection association, never current role or effect
authority.

The future sole configuration owner constructs one `Registry` with its fixed
configuration digest and nonnil canonical service epoch. Before handing work
to its blocking worker, `begin` reserves the registry's single slot, consumes
the original reader/stream, reuses same-user transport authentication and fixes
the expected independently observed live clean frontend. No FD is cloned and
there is no extract, reset or reconnect method. A second admission refuses;
poisoned accounting stays unavailable. Constructing another registry does not
establish another owner or permit. Production must retain the existing service
ownership lock and use exactly one registry for that configuration.

`Pending::bind` sends one closed version-1 challenge offer with ABI
`desktop-producer-v1`, operation `challenge`, sequence 1, configuration, exact
expected delegation hello and the server-generated challenge. It accepts only
one closed `ready` response with the same ABI/version/sequence/configuration,
frontend instance and complete challenge. Unknown or duplicate fields, invalid
UTF8, wrong phase/identity and malformed frames refuse. Strings cannot select
another operation. The existing 16 KiB cap and original ten-second monotonic
challenge deadline bound both directions; partial progress never renews it.

Readiness alone proves nothing. The same original one-use `Attempt` must then
match the exact challenge installed in the canonical responder through its
existing same-user probe. Its original deadline is checked before and after
that independently bounded one-second exchange. An absent, old, mismatched or
late responder refuses. The future host must independently resolve and pin the
canonical endpoint; a supplied path is not trusted by its spelling.

A successful `Session` privately retains both the original channel, including
its buffered tail, and the registry reservation. It exposes borrowed
point-in-time identity data only, with redacted Debug and no Clone or serde.
It has no heartbeat, currentness, role, profile, instrumentation, source,
capture, lease or native SDK permission API. A disconnected peer may cease to
be live immediately; retained identity never converts that into current proof.
Future semantic frames need their own exact sequence/phase validation; queued
bytes or duplicate readiness never repeat a freeze or effect.

Drop destroys the channel/reader/FD before releasing its slot. Pending and
Session stay with their actual worker across a dropped outer receiver or wait
timeout. `Registry::close` is a sticky new-admission fence; it never cancels a
worker, releases a held slot or proves drain. It is checked at bind boundaries;
its last check linearizes association only, never an executing permission.
Any error consumes Pending and closes that original connection without retry.
Releasing a slot never reopens a frontend writer.

Before any production caller, G1 still needs the durable prepared/frozen/abort
journal and replacement-frontend startup refusal, current profile/role and
known-zero Presence instrumentation, actual resource and store barriers,
continuous connection/epoch/proof invalidation, and whole-profile publication
and rollback. macOS signed host admission additionally requires its separately
qualified trusted installed requirement and kernel-bound live-code check.
Windows channel construction remains unsupported. None of the positive
G1/G2/G3/native paths is enabled by this helper.
