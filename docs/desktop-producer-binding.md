# One-use producer identity probe

The internal `src/desktop/producer_binding.rs` helper retains one expected
frontend and one fixed ten-second challenge deadline. It has no actor,
listener, controller, thread or production caller; it adds no command, help
marker, status, store or positive API.

The future host selects its configuration, current service epoch and expected
clean, live frontend identity independently. The helper rejects an already
writer-owned expectation and nil identity UUIDs, generates fresh random challenge and producer UUIDs,
and exposes only the challenge data for the existing cached frontend slot.
Nothing installs it automatically. `probe_at` consumes the attempt on every
outcome and reuses the existing same-user canonical socket exchange, one-second
socket deadline and 4096-byte reply cap. The host must choose and pin the
canonical path through the existing resolver; a supplied path is not a trust
claim. Expiry is checked before probing and again after the result. Waiting,
changing callers or receiving a reply cannot replace the original deadline.

Matching requires the exact retained service/producer/challenge IDs and the
original configuration, application, build, frontend instance and mode. A
connected older or malformed responder remains live legacy/unknown, never
absent. Unsafe paths stay unknown and are neither removed nor repaired. A
new attempt has fresh random IDs, so an old cached offer or stored reply cannot
bind it. There is no retry loop or pending-connection registry in this slice.
Windows refuses the socket operation as unsupported.

`Matched` is private, non-clone, non-serializing point-in-time identity data
with redacted Debug. It owns no producer connection and grants no current
delegation, lease, installed-code trust, profile capability, known-zero proof,
role drain, source coverage, native job or capture permission. The future
controller must retain and account the actual authenticated connection and
worker, enforce one producer, qualify current epochs/profile/role proofs,
durably prepare/fence replacement frontends and recheck live binding before
publication/effects. A timeout or returned probe value never proves those
resources drained. Mac native host signing requires its separately qualified
live-code verification; same-user identity alone is insufficient for that lane.

Private Unix-socket tests exercise the existing cached responder, a fresh
second attempt, legacy/unsafe/absent paths and refusal before exchange after
expiry. They qualify this helper only. No owner endpoint, display, notification
center, native agent or current GUI behavior is touched.
