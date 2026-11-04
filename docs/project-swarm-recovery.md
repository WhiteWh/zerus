# Project swarm recovery

The Next recovery UI reuses `swarm::sync::Controller`, the native organization
worker/writer and the existing `hgs swarm preview/join/resolve/sync` contracts.
The UI follows `design/webui/src/dialogs/setup/Swarm.tsx`; Qt's `SwarmDialog.cpp`
and `SwarmConflictReview.cpp` define behavior and exact conflict choices.
Opening or inspecting never joins, binds or resolves a catalog.

## Bounded view

`swarm::dialog::view(snapshot, revision)` projects the existing observed catalog
into the Computers tab's identities, local access and sync states. It copies at
most 512 computers and 1 MiB of text (plus fixed row overhead); individual labels
are at most 512 bytes and peer errors 4,000 bytes. Malformed, duplicate or
oversized data refuses the whole view, never a prefix. Conflict count is metadata;
conflict payloads remain in the existing native snapshot for explicit review.
The projection performs no source read or polling.

## Catalog-operation reservation

`Controller::begin_external(revision)` accepts only the exact current revision
of an initialized, settled catalog, without applying/pending edits or a held
observation. The opaque `ExternalTicket` is bound to that controller by private
in-process identity, carries its immutable observed snapshot and has redacted
Debug output. It is never serialized or accepted from the webview.

While reserved, ordinary edits, another reservation and scheduled applies are
refused; source observations are held without adopting them. Dropping a ticket,
a source failure, timeout or source termination does not release the reservation.
Nothing retries a Join or Resolve.

`finish_external(ticket, observation, now)` accepts one bounded, validated,
initialized snapshot for the same local node. The native caller must
serialize the external operation with this worker, invalidate pre-command source
reads and obtain this snapshot **after** that operation finishes, even when its
outcome is unknown. Neither a command error nor a cached topic snapshot proves
that completion. A failed fresh read keeps the reservation closed for review.
If an unknown delivery could still complete later, a single snapshot is also
insufficient: operation-specific settlement is required before release. The
native integration must qualify that behavior rather than infer it from
the decoder, an exited wrapper or a timeout.

Completion discards observations held during the operation. The ordinary
10-second causal-head guard protects the result's versions, and the source must
also echo its completed catalog identity before normal observation admission
resumes. An old catalog identity is discarded, never queued for later admission;
elapsed time cannot qualify it. This helper does not establish source freshness
or perform that native source barrier itself.

These are GUI catalog coordination primitives, separate from G1 production migration: no service lease,
role drain, source export, marker phase or effect permission is granted.

## Native Preview, Join and Sync

The main webview's `project_swarm_*` commands keep one opaque context and job
on the existing organization worker. Opening reads saved computer choices only.
Preview binds a saved alias and the configuration revision to the observed local
node, swarm and digest, and the peer's returned node, swarm and exact project IDs.
Join repeats the saved-configuration and local-catalog reads before issuing the
existing guarded CLI invocation once. Sync uses the same reservation and worker.
The root CLI remains responsible for its peer node/swarm and project-map guards.

The worker uses the maintained accounted runner. A returned result does not
release its reservation while the actual child, input or output readers remain.
After they finish, a fresh local read must agree with the existing topic source's
node, swarm, digest, organization and causal versions before the controller can
adopt it. A generation check discards callbacks already in flight. Settlement
uses existing source notifications, not another poller. Watch `machine_id` and
the local swarm `node_id` are the same persisted identity; both source headers
still have to match.

A failed or lost Join reply can follow enrollment. Its error remains visible,
its preview is consumed and nothing resends it. A failed post-call read holds
the reservation; a later explicit Preview or Sync first performs read-only
reconciliation. It cannot join again from unresolved evidence. Closing a read
cancels queued work; an admitted changing call cannot be closed until the runner
returns, and late actual resources keep their charge. Reopening never replays.
Replay without a selected fake CLI dispatches nothing.

The lazy `ProjectSwarmDialog` follows the mockup's Computers and Connect computer
tabs. It keeps one native view, coalesces fixed-size change events and rejects
status replies that predate an explicit action. It adds no polling timer.
Computer filtering/sorting uses stable IDs, and project mappings are exact IDs
with identical IDs preselected (the CLI merges those keys) and distinct projects
separate by default. A failed/uninitialized preview cannot
join; changing the selected computer discards the displayed preview. Close and
Escape restore focus, while an admitted changing call keeps the dialog open.
The unconnected-catalog notice preselects its first saved unconnected computer.
Manage connections uses the existing Machines page. The conflict count is shown;
its review tab remains unavailable until the separate conflict adapter lands.

No G1 producer lease, migration capture, role drain or service effect authority
follows from this GUI catalog coordination.

## Bounded conflict review data

`swarm::conflicts::Catalog` borrows the existing native snapshot. It qualifies
the whole view before returning pages: at most 16 MiB of encoded JSON, 50,000
conflicts, 1,024 raw variants per conflict, 500 projects with 200 folders each,
and 512 computers. It rejects malformed, duplicate or unsupported fields and
excess data instead of showing a prefix. Summary pages contain 64 rows; choice
pages contain 32 unique JSON values. Each copied page is capped at 1 MiB.
Only descriptive page data is serializable. Existing bounded Fleet/Pump titles
can supply the session labels; missing titles keep Qt's fallback.

One nonserializable `Review` retains one original conflict, its node/swarm,
complete raw variants and grouped value indexes. Equal JSON values share their
origin list and temporary “Shown now” marker. Opening, paging and choosing a row
construct no command. The selection describes Qt's consequences and identifies
project removal, folder-link removal and computer-identity merge as needing
the existing explicit confirmation. Files, sessions and messages are preserved
by these catalog choices.

`Review::resolve` only constructs the existing `swarm resolve` invocation. It
requires the same node/swarm and exact original key, field and variants in the
supplied current catalog, plus confirmation for the choices above. It keeps the
original JSON value and **all** reviewed variant IDs as causal parents; grouped
display rows never remove parents. Unrelated catalog edits can coexist with the
review. The CLI is not an exact-head compare-and-swap: unseen concurrent heads
remain conflicts. Native integration must reserve the existing writer, obtain a
fresh read, recheck the review, account actual child resources and settle the
source after one call. An unknown result must never replay that call.

The main-window-only `project_swarm_conflicts` and
`project_swarm_conflict_review` commands qualify a borrowed snapshot on the
blocking pool, outside the organization lock, with one such review read at a
time. A changed native revision or replaced snapshot refuses the response.
The one native-held selection uses an opaque review ID. Choosing an off-page
value is refused; changing the value page clears the choice. Paging the summary
list leaves the review intact. Changed variants refuse old reviews and clear
their choice; reopening the conflict gives a new identity and Qt's changed hint.
Labels use existing bounded Pump rows, without reading history.

`project_swarm_resolve` consumes that review on admission, reserves the existing
controller/writer and repeats the review comparison against a fresh `swarm get`
before issuing exactly one Resolve. It uses the same accounted runner, actual
child/pipe lifetime and post-call source barrier as Join and Sync. Neither a
malformed/unchanged success reply nor an unknown result confirms the choice.
The review is never restored for retry, including after close/reopen. A fresh
read and matching source are required before ordinary catalog edits resume.

The Conflicts tab loads its panel and styles on demand. It follows the mockup's
list, review table, consequences and action row, with the 480 px **Confirm
catalog change** dialog for removal and identity choices. Cancel has initial
focus; Escape closes only that confirmation and returns focus to its action.
Changed native review identity or choice closes an old confirmation.

Only overflow adds navigation: 64 conflict summaries under the 210 px list
column, and 32 unique values below the choices table. Summary paging keeps the
review, value paging clears the choice, and resolving selects the first item of
the current page (clamped backwards when it empties). At a pager bound, focus
moves to the other button of that pager, never into a list row. A whole-view
refusal uses the existing footer and shows no partial list. The client retains
one summary page and one review page and coalesces source changes without a
poller; no event or lost response can repeat Resolve.

No filesystem, second writer, CRDT, poller or migration authority is added.
