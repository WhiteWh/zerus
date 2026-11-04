# Notification preferences and foreground observations

The closed types live in `client/core/src/desktop/notification_control.rs`.
The Root route advertises `Notification controls ABI:
desktop-notification-controls-v1`. Consumers check that exact marker and the
existing frontend-request marker on the selected paired CLI before dispatch.
The native client companion supplies the actual window events and connection
lifetime; the generic policy sender refuses this operation.

Mutation uses `Request::NotificationControl { scope, control }` on the existing
original-child `desktop frontend-request --json` route. `scope` is the verified
current configuration/service epoch; `control` is the closed Configure or Focus
command below. Generic `desktop request` rejects `notification_control`.
Read-only requests on that generic lane are exactly
`{ op: "notification_preferences_status", request_id, scope }` and
`{ op: "notification_preferences_result", query: Query }`. Status and lookup
remain display/recovery data and cannot establish native provenance.

The canonical Responder's `notification_connect(Scope)` returns an owned native
`notification_control::native::Connection`. It generates the connection UUID,
starts Unknown/revision zero, exposes `snapshot`, `observe(FocusState)`,
`invalidate`, `id` and `scope`, and invalidates on Drop. Native toplevel events
use `observe`; Foreground renewals use `snapshot`. The service connection retains
this handle and invalidates it promptly on disconnect/loss. The original
`Registration::notification_evidence()` is the exact attached snapshot used for
reply comparison. Query-time invalidation retains every original child/pipe/Slot
resource charge until actual custody ends.

Store schema 16 atomically adds one preference singleton and at most 4096
indexed immutable request receipts. It adds no focus table, worker permission or
approval consent. Older CLIs fail closed on that newer store.

## Preference transfer

`Command::Configure` carries the logical request UUID, original preference
journal/revision, and exactly `Preferences { optional_questions }`. The default
is false. There is no notification-enable field, mode setting, approval opt-in
or foreground value. An authenticated service-mode frontend initializes the
unconfigured record from the existing optional-question preference; subsequent
edits use the last verified service journal/revision. Reconnect reads the
retained value and must not silently overwrite a configured record with a stale
UI setting. Missing or uncertain status is unconfigured/unknown, not permission
to replace retained settings.

The intended sender is the existing native frontend original-child runner,
using the paired service relay, retained Responder registration and resources.
Root adds a closed `NotificationControl { control: Command }` frontend request
arm, binds it to cached native Service mode, and rechecks its opaque Validated
permit after the transaction starts. Configure uses the serial operation key
`notification_preferences`. It remains safe-passive when the authenticated
route and store are writable; it grants neither ownership nor writers.

Status has an exact configuration/service-epoch Scope, independent preference
journal, revision, configured bit and preference. Saved receipts bind the
original UUID, configuration, fingerprint, journal, resulting revision and
exact value. A historical receipt acknowledges the edit, not currentness.
Journal/revision mismatch refuses before replacement. No source settings file,
readiness value, caption or watch payload supplies admission.

Unknown edits retain their logical UUID and original child/resource accounting.
Only read-only Query/Lookup of that original Configure recovers a result.
Missing receipts, replaced journals and an initially absent journal remain
unknown. A Root-proven not-applied result is possible only in the unchanged
original nonempty journal/revision; a new logical edit also requires confirmed
original-child exit. Unknown edits are never resent. Receipts and the singleton
must be bounded; schema allocation belongs to the implementation, not this wire
foundation.

## Native foreground snapshots

`Command::Focus` contains only a fresh request UUID and uses the independent
serial operation key `notification_focus`. Its native original-child evidence
contains `NativeFocus { connection_id, revision, state }`: the current retained
service connection incarnation and native window state Unknown, Background or
Foreground. The native Responder cache starts Unknown; Rust window focus events
update it. Disconnect/reconnect invalidates the cache/connection incarnation;
the initial state is never inferred from a UI file, a JSON focused flag,
ActivationWait, source activity or service readiness. Native mode cannot
register this service-mode request. Ordinary Hello/evidence stays compatible;
the additional native observation is bound only to this request arm.

The client sends one bounded off-UI snapshot at a time. Repeated Foreground
renewals run at most once every two seconds while connected. Background/Unknown
transitions bypass that renewal delay. It retains
the same paired relay connection selection and original child/resources through
each exchange. A native transition immediately increments the connection-scoped
cache revision and invalidates older Focus registrations, without releasing
their original child/resource charges. The existing native evidence query reads
the current cache and refuses an invalidated slot. Connection change or
disconnect sets Unknown and invalidates the old incarnation. A pending exchange
coalesces only the latest state; it sends the clear as soon as original custody
drains, without starting a second child. No unbounded worker or input queue is
created. Focus is suppression data, not a permission.

Foreground suppression is app-wide: an active Zerus window suppresses banners
for every session, matching the existing native AttentionTracker. It is not a
per-session focus selector.

Root retains at most one original canonical-frontend proof/connection lease.
A verified Background/Unknown update, old connection, failed proof, known UI
exit or verified disconnect clears suppression immediately when Root observes
it. If a snapshot was already admitted before the native event, propagation
through a blocked exchange has no zero-latency guarantee. An uncertain lost
connection or reply cannot renew it: the last Foreground snapshot expires on a
monotonic deadline within five seconds. The actor checks cheap held-process
liveness every turn, bounded full proof periodically, and full proof before
accepting a replacement lease. The original mutation deadline is not extended.
On Windows the periodic hint check uses the same retained frontend and ancestor
objects after the original relay exits; admission still checks that relay's
liveness and the original two-second deadline.
A stale native revision or a former connection cannot replace a newer lease.

The client compares the returned native connection ID/revision with the exact
NativeFocus attached to its original registered request. The echoed request UUID
alone cannot bind a returned observation. Receipt.service_epoch records the
historical commit; it never proves currentness.

FocusReply carries the exact UUID and Scope, native observation and remaining
TTL (zero except for Foreground). It has no durable receipt, lookup, journal,
replay, wall-clock expiry or persisted identity. A lost reply is followed only
by a fresh native observation with a new UUID after original custody drains.
Server restart starts with no focus lease; reconnect starts native Unknown and
requires a fresh observation. The attention runtime consumes this bounded lease
to suppress newly observed foreground edges without replaying them after blur.

Required notifications still depend on the independent durable ownership
grant and actual original host. Preferences and focus cannot rearm a revoked
grant, start a service, authorize approvals or establish closed-UI launch mode.

## Zerus Next consumer

In service mode the native client creates one control connection only after
the original verified watch hello. It probes both markers on the retained
paired helper and pins that image through each bounded exchange. One separate
admission slot accounts for the original child, registration and every pipe;
disconnect invalidates native evidence before joining that worker. Metadata
probes and transport run off the UI and watch reader.

Rust observes all native toplevel windows. A fresh focused visible window means
Foreground; failed observations mean Unknown unless another window proves
Foreground. Reconnect asks the native main thread for a new observation and
never replays the previous connection's focus. Background/Unknown invalidates
older registrations immediately even when an original helper still holds pipes.

Settings > Sessions sends optional-question changes through the separate typed
native command. An unconfigured service gets at most one initialization attempt
per connection from the existing local preference. A configured service's fresh
status is retained instead of that local value. Pending edits disable further
changes; Check result and reconnect query the original receipt only. A historical
save is followed by a fresh scoped status, and unavailable status offers no local
write fallback. Native mode retains the existing local setting path.
