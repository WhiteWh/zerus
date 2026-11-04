# Incremental desktop catalog reads

The existing `desktop-v1` watch and request lane advertises
`catalog_reads.changes_abi = "desktop-catalog-changes-v1"` in status/hello.
The CLI help also carries `Desktop catalog changes ABI: desktop-catalog-changes-v1`.
These are display projections, with `durable:false`, `display_only:true` and
`authority:false`. They confer no posting, read-mark or approval authority.

`catalog_page` keeps its existing request shape and bounded insertion-watermark
walk. Its response additionally carries `epoch`, `computer`, `incarnation`,
`generation`, opaque `projection`, `start_revision`, `revision`, `online`,
`last_known` and `stale`. Revisions are canonical nonnegative decimal **strings**.
The first page captures `start_revision`; every continuation repeats that original
value. Ordinary changes do not restart or extend the baseline. They make its
later pages stale/incomplete. Updates and removals can affect baseline pages;
finishing that walk alone cannot establish current membership.

After the baseline, request the changes since its **first** start revision:

```json
{
  "op": "catalog_changes",
  "request_id": "00000000-0000-4000-8000-000000000001",
  "epoch": "00000000-0000-4000-8000-000000000002",
  "computer": "peer",
  "incarnation": 1,
  "generation": 2,
  "projection": "0123456789abcdef0123456789abcdef",
  "since_revision": "0",
  "cursor": null
}
```

Every continuation repeats these exact scope/start fields and passes the returned
`next_cursor`. The first change page fixes `through_revision`; subsequent pages
keep it even while new hints arrive. Items are ordered by revision/key and have
one final outcome per exact revision/key:

```json
{"op":"upsert","revision":"1","key":"run:example","row":{"run_id":"example","name":"codex/example/session"}}
{"op":"remove","revision":"2","key":"run:example","row":null}
```

Payloads are immutable history through that fixed high-water, including rows
which are later updated or deleted. `revision` reports the currently observed
head; `stale` means it exceeds this walk's high-water. `complete` on a change
page describes source coverage, **not** successful consumption of the whole walk.
Only the final page (`next_cursor:null`) establishes contiguous catch-up through
`through_revision`. Baseline plus fully consumed contiguous catch-up establishes
membership. Coalesce newer hints into one subsequent walk from the completed
high-water. Never adopt partial catch-up as a coherent complete catalog.

`catalog_changed` adds the same `changes_abi`, `projection`, high-water `revision`,
`online`, `last_known` and `reset_required` fields. Ordinary hints coalesce; they
must not restart an active baseline or delta walk. Metadata-only hints can repeat
a revision. Snapshot replacement (including same native source), alias removal,
reconnect and service epoch changes require a fresh baseline. An offline or
warming baseline can show retained rows but has a null `start_revision` and
cannot establish current membership or a complete zero.

A refusal has `accepted:false`, `reset_required:true` and no partial page.
`catalog_changes_gap` explicitly reports expired retained history;
`catalog_changes_reset` reports unavailable/replaced scope or an invalid change
read. Expired, refused, disconnected or replaced walks must be discarded/closed
and restarted from a baseline. `catalog_close` works for either cursor kind with
its original `{request_id,computer,cursor}` shape. Cursors are opaque, one-use,
scoped to the service/source/projection, and not durable across service epochs.

Both walks share the existing 64-cursor cap, 120-second idle and 600-second
absolute TTL, 128-item page cap, 1 MiB stored page budget and exact IPC-envelope
check. The actor retains only cursor metadata. The global change journal is
limited to 32,768 items and 16 MiB of charged key/payload bytes. Ingress eviction
and background collection each delete at most 256 items/4 MiB per call. If one
bounded eviction cannot admit a frame's history, its catalog mutation still
commits with an explicit retention gap. No slow subscriber can pin history or
allocate a per-session task/cache. Journal writes and catalog mutations commit
in the same SQLite transaction, with indexed revision/key reads and bounded GC.

Schema 13 follows the actual maintenance schema 12. Migration preserves retained
rows without inventing a watermark. Shared-read-setting attention counts retain
their existing scope and provenance and remain separate from device-local
review-later/ack overlays and final UI badges.

For a multi-page walk, `hgs desktop catalog-relay --json` amortizes CLI startup
over serial request/response lines. Require the help marker `Desktop catalog
relay ABI: desktop-catalog-relay-v1`. Its first line is
`{abi:"desktop-catalog-relay-v1",configuration}`; each subsequent reply is
`{abi,request_id,value:<original catalog reply>}`. Only `catalog_page`,
`catalog_changes` and `catalog_close` are admitted, through the existing local
authenticated IPC. Input retains the native control-line cap and each output
line retains the desktop frame cap. There is one outstanding request and no
relay cache. EOF closes the relay without starting or stopping the service.
The client retains one original child per walk and still checks paired-image
freshness and every original request, epoch, scope, revision and cursor fence
on every page. Unknown outcomes retire that walk; continuations are never resent.
