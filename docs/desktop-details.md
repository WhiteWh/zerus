# Desktop service detail subscriptions

The background desktop runtime uses its existing raw watch handle for each
configured computer. Detail demand does not start another fleet or a worker per
session. The adapter retains bounded subscription selectors and capability
summaries; Activity bodies, process inventories, tasks and organization rows
remain in bounded native frames and subscriber queues.

A desktop-v1 `watch` hello advertises `value.details` only while the background
runtime is active. It contains `abi: desktop-details-v1`, the service `epoch`, a
random `lease`, and fixed limits. The lease belongs to that original watch queue.
It expires on EOF, queue overflow, revoke or service shutdown. There are at most
eight subscribers, 64 demands per subscriber and 512 distinct target groups.
Revoke and shutdown close the entire original background watch stream, including
watchers with no detail demand. Clients treat that EOF as the expected end of the
old epoch and reconnect through the normal lifecycle, not as detail-only failure.
Existing native per-computer topic limits apply, capped at 16 Activity targets,
16 process targets, 16 projects and one organization.

## Controls on the existing request lane

Every request includes a canonical UUID `request_id`, the watch hello's `epoch`
and `lease`. These are ephemeral read subscriptions, not durable delivery
receipts or writer permission.

- `detail_follow` adds `id` (a consumer UUID), `computer` (empty for local), and
  `subscription`. The latter contains a watch-v1 `topic` plus its typed selector:
  Activity/processes use `session: {name, run_id?, conversation_id?}`; tasks use
  `project`; organization has no key. Activity supports nonnegative `after` or
  a tail of at most 100. Processes supports `interval_ms` from 1,000 to 600,000
  and an explicit `refresh`. Other topics/fields refuse.
- `detail_unfollow` removes that lease's `id`. Unknown IDs are harmless. The
  response means local demand was removed, not that native work has drained.
- `detail_source` takes `computer`, the expected source `generation`, and
  `part: hello | meta`. This is one indexed, bounded lookup of existing source
  metadata. The response includes `native`, `online`, `complete` and the selected
  `value`, preserving offline last-known provenance. A legacy listing is never
  presented as a native Hello. Missing, changed or oversize input refuses; the
  complete desktop response envelope must fit its byte limit.

Follow responses echo the consumer ID, generation, revision, state and code.
Replaying the same ID and exact selector does not create another subscription.
A changed view uses a fresh consumer ID; changing its existing selector refuses.
Processes refresh is scheduling on the same target, preserving its native minimum
read interval. Matching target demand is shared across subscribers. Activity
sharing uses the earliest requested cursor, or the largest bounded tail; process
sharing uses the shortest requested interval. Conflicting session identities
never share one native target.

## Frames and recovery

Targeted `detail` values contain `lease`, `id`, `computer`, `generation`,
`revision`, `detail_seq`, and the original decoded watch-v1 `frame`. Native
bodies, keys, cursors, run/conversation identities and native `frame.seq` are
preserved. `detail_seq` is contiguous only within that exact lease/ID/source
generation/revision. The outer desktop sequence is monotonic and may skip frames
targeted to other subscribers. Native frame sequences also skip catalog and
unselected topics; they are not a filtered-stream continuity counter.
Detail/source and catalog publication have no cross-stream event-order contract:
within one actor poll, a detail frame may precede a catalog change from an earlier
native event. Consumers must not correlate their delivery order or outer sequence
to infer a common snapshot or causal order across these streams.

`detail_status` identifies the same route and reports starting, ready,
incomplete, unsupported or ended state. Ready means the subscription control
was admitted after the fence; `complete` remains false there. Native snapshot
pages and their final `last` flag establish topic replacement. Consumers reject
old epochs, leases, consumer IDs, generations and revisions. An older response
must not replace a newer route status. Source disconnect/gap or a missing detail
sequence invalidates the view. There is no GUI-fleet fallback.

Original accepted Hello and catalog.meta frames are additionally forwarded as
`source` values with lease/computer/source generation. A later subscriber obtains
its bootstrap through `detail_source`; there is no second metadata cache.
Computer aliases always come from the configured route, not metadata fields.
Metadata and detail frames never grant read-root, writer or approval authority.

A changed shared target first queues unsubscribe and a unique Ping atomically.
Only the exact original-generation Pong permits a separately queued subscribe.
The two phases matter: native state coalescing can replace an earlier queued
state in place, across a Pong that has not yet been consumed. Frames for the old
target are discarded while the fence is pending. Native process/task/organization
callback tokens reject unsubscribed reads even when they finish later.

Control batches use nonblocking admission, at most three lines/8 KiB, with
128 batches/1 MiB shared across the original source group. Full/closed admission
is incomplete, never a successful rearm. Post-Pong admission retries are bounded
by the original 15-second fence deadline. Initial queue refusal remains explicit;
it does not silently claim subscription or launch another source. Subscriber
queue limits charge complete encoded bodies, including the envelope.

Closed leases release all their demand; only the last interested subscriber
unsubscribes a native target. A leave whose merged selector is unchanged preserves
the remaining routes' revision, detail sequence and current snapshot; a join still
rearms because the adapter does not cache topic contents. The actor retains each
original fleet ingress permit through successful broker application and queue
admission. Failed broker apply
invalidates details and asks the original fleet to resnapshot. Revoke closes
leases before the existing fleet shutdown; original worker/reader/connection and
control admission accounting remains the drain boundary.

Native detail pools also have finite pending read queues: 16 process jobs,
eight task jobs and one organization job, using nonblocking admission. A full queue
keeps the current subscription pending for its next one-second read window, without
emitting a read error or waiting for the safety interval. Superseded jobs remain
cancelled; a disconnected worker reports its original token-checked error. This
prevents repeated subscription replacement from accumulating cancelled reads
behind a slow worker.
EOF, a queue close, Pong or a local status is not native cancellation or drain
proof. Existing one-shot archived/user reads remain separate from subscriptions.
