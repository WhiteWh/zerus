# Service attention discovery

`desktop::attention::Runtime` consumes only the service's own `FleetEvent`, after
successful `Broker::apply` and while the original ingress permit is held. The
service lifecycle owns activation, writer admission, native host and shutdown.
The discovery component creates none of them.

The actor calls `new(epoch)`, `observe(store, event)`, then
`poll(store, Option<&mut Outbox>)` while enabled or retiring. Initially disabled
discovery skips both observation and polling, retaining no reply backlog or new
attention storage. Disabling an enabled policy schedules an indexed, fixed-highwater
retirement walk of at most eight existing live edges per turn. The original outbox
revokes their callbacks/queued posts and reserves exact native withdrawals after
the original post receipt and resources settle. Retirement needs that outbox;
without it, status remains `retiring` and no native effect is inferred. Idle means
the original operations and callbacks settled, with the same host kept connected;
it is separate from host stop/join. Unknown outcomes are retained without replay.
The lifecycle calls `request_retirement()` when resuming its durable policy-off
state after restart. Re-enabling waits for retirement before restarting discovery.
Enabling resets the baseline; a fresh owned observation
consumes current history before later edges may post. An enabled baseline can
progress without a native host. `reject(computer)` invalidates a rejected source;
`request_stop`, `drained` and `status` fit the service lifecycle. The original
service supplies `Policy { enabled, optional_questions }` (both default false)
and refreshes foreground suppression, which expires after five seconds. These
Rust calls are not generic IPC settings or writer permissions.

The initial catalog snapshot and retention-gap replacements consume old edges.
Subsequent work follows indexed catalog deltas. A changed baseline key is
skipped for the delta walk only if its conversation has saved Tracker state;
a key-first journal index bounds the lookup. An empty saved Tracker counts as
known state. Without a prior record or a before-image, its first observed value
is consumed as baseline. A new edge first encountered on an unread startup page
can therefore be consumed during this bounded first-baseline window; the service
prefers that omission to posting an old unread edge as if it were new. Later
observations of that conversation catch up normally. Journal retention loss
restarts a baseline before any unproven row can be consumed. Foreground refresh
only suppresses observations while active and never restarts a baseline. One
actor turn processes at most four rows and eight dirty effects. The temporary
page has at most 128 rows and 1 MiB; only the 64 computer cursors remain resident. Each row passes through the
shared attention Tracker, preserving Mute, suppressed baselines, optional
questions and reply deferral for background jobs, subagents and independent
agent waits. Unknown/offline observations cannot grant a current source.
An unsent retained reply is checked against the current shared policy again
when a host arrives. A historical duplicate cannot overwrite a current row.

Schema 14 stores bounded Tracker state in SQLite (65,536 conversations / 64 MiB),
not a resident session cache. The permanent logical-edge index is limited to
262,144 entries / 128 MiB of charged identity/payload data. Per-row child and
question lists are limited to 64 each, Tracker state to 64 KiB. Quota or SQL
failure refuses new posting; old delivered/unknown identities are never evicted
to make an edge fresh again. These are encoded data/work limits, not physical
memory or kernel timing guarantees.

Intent IDs hash only the original target (computer, genuine run, conversation,
reply ID or component attention signature). The logical index also binds the
agent and keeps the first identity/token/payload across rename, resume, reconnect
and restart. Component signatures include the actual request identity, so an
unrelated optional question does not recreate a main approval banner. Mapping
is committed before ledger creation; partial SQL work cannot rebind a resumed
edge to a different run. Refused and unknown attempts are never retried.

`Outbox::offer_intent` builds its reservation from the original private Session.
`retire_intent` revokes callbacks and cancels retained queued work without
claiming a native close. Withdrawal waits for the original post receipt, uses
its exact native mapping and obtains its own reservation. Live restoration uses
`restore_live_head/page`: an immutable highwater of attention edge IDs, at most
16 indexed candidates per page, with the original callback guard still applied.
It does not scan retired attempt history or resend notifications.

A non-retryable offer or SQL failure stops discovery and is explicit in status
(`stopped_on_error`, `error_retryable: false`). Repairing a writer underneath the
runtime does not replay its failed offer; lifecycle recovery remains explicit.

## Platform attachment

`notifications::attachment::Runtime` owns the original outbox and selected
platform runtime. Its constructor takes the original service owner resource;
that resource also remains in the original endpoint and command callbacks.
Mac selection is supplied as the opaque native selection made off actor, and
Linux takes an explicit Unix bus address. This component does not change the
background registration, policy admission or client switch.

Windows takes the selected service configuration and passes the same original
owner resource to its in-process worker and native delegates. Restore retries
retain the current page offset through its bounded FIFO. Finish waits for the
worker's restored/ready acknowledgement, and stop waits for `try_join` plus
native drain. The actor must continue polling original Closed callbacks through
that join; a joined worker alone cannot settle the attachment. Native setup
faults remain unavailable, and no registry repair occurs here.

Before allowing discovery effects, attachment restores at most sixteen indexed
candidates per turn, then waits for the platform's original restoration
acknowledgement or ordered FIFO completion. A second fixed-highwater walk over
the dirty index recovers retired delivered mappings that still need their first
withdrawal after a crash. These mappings authorize cleanup only: the normal
callback guard still refuses withdrawn intents. A consumed withdrawal is never
restored for replay, and ephemeral Wayland tokens are never recovered from disk.
Policy retirement then uses the same retained host. Stop requires both original
operation resources to settle and the actual platform worker to join; an
abandoned undrained attachment keeps its original owner held rather than permit
replacement. The returned activations still need the service-owned authenticated
frontend handoff; this component adds no generic click request or UI launcher.

Revoke, rollback, native takeover and runtime replacement first fence new posts
and clicks, then finish restoration and the bounded retirement walk while the
original host stays connected. A begun post keeps its original terminal receipt
and copied resources; only a confirmed delivered mapping can produce its exact
withdrawal. An unknown post or withdrawal is never replayed. The native stop is
sent after this phase and the original operation resources settle, followed by
the actual host join. Policy disable uses the same retirement walk and leaves
the host connected and idle instead of stopping it.

Shutdown retirement has one overall ten-second deadline from the first stop
request. Repeated stop requests never extend it. A cleanup error or expiry stops
discovery and sends native stop, exposing the failure rather than leaving a
takeover waiting indefinitely. Unwithdrawn banners remain uncertain; neither a
cleanup failure nor a timeout authorizes replay of a post or withdrawal. Original
command/callback resources and the actual native worker still have to drain.

On Linux a namespace fault remains visible even after the original socket and
executor have actually closed and the same worker has joined. This is physical
resource drain, not a clean callback fence. A failed teardown or a panicked
worker supplies no such proof and still prevents settlement/replacement.

The ledger remains revoked throughout cleanup. Its separate fresh withdrawal
reservation validates the original delivered post's identity, token, native ID
and receipt binding under the same writer/service incarnation. It cannot reserve
a post or another device effect, retry a withdrawal, or adopt a stale writer.
Only its private original reservation permits the native snapshot check after
revocation; a read-only inspection cannot reconstruct that authority. Restoration
can load these mappings for cleanup after revoke, while click checks still
refuse them immediately.

An attachment failure is visible in notification service status, separately
from its readiness and physical drain. The service keeps polling the same
original host, operation resources and selectors until their actual teardown
and join, then selects a fresh attachment under the still-current grant with
backoff of 1, 2, 5, 10 and at most 30 seconds. Revoke/native takeover cancels
retry; a timer cannot rearm ownership. Recovery does not replay prior posts or
withdrawals, and an outage does not turn the user's policy off and retire all
shown mappings. Successful attachment clears the error and resets backoff.

On Linux an absent notification daemon can be activated only through the bus's
fixed `StartServiceByName("org.freedesktop.Notifications", 0)` after an
authenticated `NameHasNoOwner` reply. Setup stays bounded by the original
deadline. An owner/namespace loss faults the original worker and starts recovery
only after its socket/executor teardown and actual thread join; its status still
reports an unclean callback fence even when these resources have drained. A
fresh attachment ignores valid mappings from a foreign bus/owner namespace and
never redirects their numeric IDs. Incomplete teardown or a panicked worker
retains the original owner and prevents replacement.

## Authenticated activation handoff

The native frontend consumer checks both `Desktop frontend request ABI:
desktop-ui-request-v1` and `Desktop activation ABI: desktop-activation-v1`, then
uses the existing registered original-child `desktop frontend-request --json`
route with `Request::ActivationWait { request_id }`. It registers the fresh
Envelope and binds the original Child before stdin, retaining the registration,
child and pipe resources through completion. This is a separate ephemeral
consumer, not the durable policy-request state machine. One wait lasts at most
15 seconds; the selected CLI reads for at most 25 seconds and the owned runner
must retain its ordinary 30-second maximum. One canonical frontend may wait at
a time. Generic requests, agent ancestry and unregistered children cannot take
activation payloads; ordinary watch frames contain no click target.

The response value is the closed `desktop::activation::Reply`, bound to the
original wait UUID, selected configuration and current service epoch. Its
optional `Open` contains a distinct click UUID, original post UUID, exact
computer/run/conversation target, original token and optional ephemeral Wayland
context. `None` means idle, not readiness or an ownership grant. The service
rechecks the original Delivered mapping at handoff, so intervening withdrawal or
revocation refuses it. Each actor turn checks held-process and canonical-peer
liveness. Full retained ancestry validation is limited to once per 500 ms while
waiting, and is always repeated immediately before handing a click to a waiting frontend. The queue has at most 32 clicks, expires them after 30
seconds, and never persists context. A proof refusal before any frame is offered keeps the known-unsent click under
its original 30-second age for a newly authenticated receiver. Once a handoff is
attempted, a disconnected/uncertain outcome is not replayed automatically; the
next wait is a new read and another native click is a distinct event.

The service's `Router::closed_frontend` checks a fresh canonical instance
observation made off actor. Absence alone cannot authorize a launch: the Linux
mechanics force service mode and would also freeze/import the native read store.
Until the authenticated notification ownership handoff exists, this decision
refuses without consuming the queued click or extending its original age.
The handoff must durably record a SERVICE-MODE frontend's grant for the exact
configuration and selected frontend build/identity; revoke, rollback and native
notification takeover end that grant. Missing, unknown or mismatched grants
refuse. This is ownership state, not a separate mode preference. Unknown, live
and legacy observations also do not launch. A future admitted decision supplies
the opaque one-use `Launch` to the selected installed frontend mechanics.
Linux owns the exact-target GUI consumer and launch implementation: the Wayland
token goes only into that child environment as `XDG_ACTIVATION_TOKEN`, never
process-global state, argv or disk. The parent lifecycle still owns callback
attachment, selected image launch, policy and final cutover; the transport alone
does not activate any of them.

On Linux, `ui_authority::Scope::select_frontend(image)` binds the root's explicit
maintained-image choice under the same installed-image catalog used for inbound
authentication. It performs no PATH lookup or alternate-install fallback. The
actual running CLI and selected `bin/hgs-tray` may have different installation
prefixes; a system CLI with a user GUI remains supported. It retains the original files and stable
directory identities. If the maintained local-WebKit setting exists, its exact
setting and `libexec/hgs-tray-launch` are retained too. File/entry replacement or
selection changes refuse; unrelated directory writes do not. The opaque handle
exposes `image()`, `entry()`, `revalidate()` and original-child observation.
The selected catalog path is canonicalized before pinning, including a symlinked
installation prefix. Group-writable files/directories are accepted only for the
effective user-private group: bounded NSS reads require its name to equal the
user's and its listed membership to be empty. Other group write and all world
write still refuse. Exact file ctime remains part of custody, so relabelling or
link changes require a fresh selection shortly before launch.
The Linux helper keeps its original `Child`, waits through wrapper exec, and
uses `observe_original_child` / `revalidate_child` against the retained image and
birth identity. `revalidate_for_launch` also binds the opaque Launch to the same
service configuration. A cached exited child or another process is never adopted.
This is image custody, not a navigation capability or launch permit: the helper
still requires the selected frontend's actual target-argument ABI, and only the
service-owned `Launch` supplies an exact target. No installed program is queried
or started by selection itself.
