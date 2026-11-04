# Desktop instance identity challenge

The existing Next single-instance listener remains the owner of the canonical
`hgs-tray` socket. The service never binds, removes or brokers that socket.
`show` and `sessions`, the startup lock, updater discovery and rollback keep
their existing behavior. This additive protocol only identifies a compatible
live frontend. It grants no writer permit, delegation, drain proof or takeover.

Callers first require the exact trimmed help line
`Desktop owner ABI: desktop-delegation-v1` from their selected local CLI before
invoking `hgs desktop owner-status --json`. Older CLIs may interpret an unknown
command as a session launch. The new command makes one read-only socket probe;
it does not start the service, write state, invoke children or retry.

The query is the 59-byte ASCII line `desktop-delegation-v1 NONCE\n`, where
NONCE is a fresh canonical UUID minted for each probe. It fits the listener's
existing 64-byte line budget. Next replies with one JSON line, at most 4096
bytes including LF. Its closed schema carries `abi`, `version:1`, the exact
`nonce`, `application`, `gui`, `build` (version, source_commit, source_dirty),
`instance_epoch`, `writer_epoch`, `configuration`, `live` and `replay`.
`writer_epoch` is currently null; the hello cannot assert effect authority.
The app chooses an instance UUID at startup. This is not a writer epoch.

The accepted identity is the production `com.hgdev.hgs-tray` Tauri app, a known
clean source build, live and not replaying, with an exact configuration hash.
Configuration comes once from a background probe of the selected local hgs:
ordinary `--help` must advertise the exact `Desktop service ABI: desktop-v1`
line before `desktop status --json` is called. Each child has a five-second
owned-child deadline and bounded output; failed/absent capability stays unknown
for the instance. No per-socket-probe child, GUI-thread I/O or automatic retry
is introduced. A second launch that forwards activation never starts this
configuration probe. Replay and nonproduction builds do not probe a CLI.
The listener only reads cached identity. Changing configuration requires a new
instance; later handoff must independently revalidate the selected configuration.

Unix peers authenticate with SO_PEERCRED or getpeereid. The probe uses
nonblocking connect and one total one-second socket deadline, not a timeout
renewed by each byte. A full backlog or a connect that cannot complete within the deadline stays unknown. The
existing Qt path selection is shared with live Next, preserving lexical cleanup
and macOS's per-user temporary directory without expanding `/tmp` symlinks.
Only a missing endpoint is `absent`. A stale, unsafe or unreachable existing
entry is `unknown`; a connected peer ignoring/rejecting the query or returning
an unsolicited, stale, mismatched, newer, dev, replay, malformed, late or
oversized reply is `live_legacy_or_unknown`. These are passive observations,
not service error states. They never establish that no frontend exists.
A valid reply is `live_next`, still with authority, takeover and writers false.
Future effect batches must apply their own bounded scheduling and fresh checks;
this command has no polling loop or retained cache.

Windows currently reports unsupported/unknown. Its permanent instance lock and
native ownership protocol need their separate runtime qualification. No Unix
probe result substitutes for that proof.

## Native frontend policy requests

### Ownership companion source (stage 1)

The closed source contract is `desktop::ownership` in the shared core. Its ABI
is `desktop-ownership-v1` and its help marker is exactly `Desktop ownership ABI:
desktop-ownership-v1`. The selected CLI advertises it only with the root
status/result/actor routes. The client companion must bind its native startup
mode and original-child registration before using these routes.

Before canonical instance claim and writer creation, native startup calls
`Responder::configure_ownership_mode_once(Mode::Service | Mode::Native)`.
Configuration still comes from the selected CLI and is cached separately with
`configure_once`. Generic `Hello` stays unchanged. Only ownership requests carry
the optional `Evidence::ownership_mode`, copied from the immutable native cache;
ordinary evidence omits it. Missing or wrong mode refuses registration and the
root independently joins it to the canonical peer/original-child proof. The
intent has no mode, role, build, PID or confirmation field.

The root entries are `desktop ownership-status --json` (bounded
read-only `Status` without a subscription), the existing authenticated
`desktop frontend-request --json`, and `desktop ownership-result --json` with
the closed `Query` on stdin. All require the ownership and frontend help markers
on the selected paired relay; none starts a service. This permits native startup
to select configuration, configure its responder and request takeover AFTER
canonical claim but BEFORE Bootstrap hold or native notification creation,
without waiting for the later service-source connection.

The authenticated payload is `Request::DesktopOwnership { request_id, action,
journal, revision }`. All three actions share one native ordering key.
`Grant` is the ordinary service-mode startup/connection intent and cannot
supersede a retained revoke/native takeover. `Rearm` is a new explicit user
intent after that state. Both require cached `Mode::Service`. `NativeTakeover`
requires cached `Mode::Native` and closes service ownership before GUI writers
can resume. A pure maintained frontend update only invalidates the closed-UI
launch grant: it does not revoke background work or set `rearm_required`. The
new authenticated canonical frontend's ordinary Grant binds its new identity,
without explicit Rearm. A real retained revoke, rollback, native takeover or
pending drain still requires the existing rearm path. The journal/revision come from the bounded status read; no journal
is permitted only at initial revision zero. The root derives selected frontend
build/file identity from native proof, not the payload. No action inherits an
automatic-approval opt-in.

`Receipt::matches` joins the original request ID/fingerprint, configuration,
journal and revision. `Pending` and `Granted` never release native posting.
`NativeReady` additionally requires revoked, checkpointed and actual drained
completion. `Receipt::native_ready()` also joins the current actor observation
to that exact native command/journal/revision, so a historical settled takeover
cannot release a new GUI after ownership changes. The client consumes this only
from its original retained CLI response, never a webview-supplied receipt.

For a never-service configuration with no desktop database, handoff, sidecars or
retained notification-host state, an authenticated NativeTakeover instead returns
`Outcome::NativeAbsent`: accepted, non-durable, null journal and revision zero.
It holds the existing owner exclusion if present and rechecks the same original
native proof; it creates no directory, database, revoke or ownership row and does
not force a later service-mode frontend to rearm. `native_ready()` recognizes
this explicit observed no-op only after `matches()` on the original CLI reply.
An unknown retained state or live but unreachable owner refuses it. A read-only
lookup never manufactures NativeAbsent and a lost no-op response stays unknown.


For an unknown mutation, retain its original logical UUID and child/pipe/cache
resources, then issue only `Query::for_request` read lookups. A missing record
alone is not proof of not-applied. Only an explicit root `definitive` negative
AND confirmed original-child exit may retire that logical request and allow a
new intent; the old mutating request is never replayed automatically. Refusal,
pending/unknown and stale scope leave the relevant writer admissions fenced.

Schema 15 stores one ownership row with a stable journal ID and increasing
revision, plus the newest 4096 settled command receipts and at most one pending
takeover. Revoke/rollback invalidates the grant in the same transaction as the
writer fence; a pending takeover superseded by revoke closes as refused, never
as NativeReady. Rearm cannot replace an undrained original runtime. Initial
Grant and explicit Rearm do not import approval consent. An already admitted
background service attaches its original platform host only under the retained
grant and actual serve/handoff exclusions; passive serve never starts it. The
ownership epoch is stable across same-image/same-build reconnect Grants, and
rotates on a new-image authenticated Grant or explicit Rearm. The service process
epoch stays separate. Losing only image identity still permits background work
but refuses closed-UI launch until the new canonical frontend grants.

A replacement drain that exceeds the request's bounded wait keeps the SAME
stop-requested runtime. Normal actor turns continue its original joins without
a client retry. Only after actual drain can an already admitted background
service replace it under the still-current grant; revoke/native takeover still
block restart. No old request or native effect is replayed to replace a runtime.

The Linux closed-instance path probes canonical absence off actor, selects the
same maintained frontend bound to the grant, then rechecks the original live
delivered mapping and grant on actor before consuming a Launch. Its click keeps
the original thirty-second deadline through help/selection. The help child and
uncertain pipes retain the real owner; after spawn, the GUI lifetime is separate
from the backend drain and is never killed on service stop. A pure observer retains
the SAME original GUI Child through eventual exit/reap across backend revoke,
without retaining any Root exclusion or native effect resource. Its navigation and
Wayland token stay in the original child's environment. A failed or uncertain
launch does not authorize replay. A running frontend receives only the existing
authenticated ActivationWait route.

Settled takeover/revoke requires original platform/operation/callback joins. A
Mac host's existing FD3 exclusion can outlive Root; it also blocks settlement
until actually released. Read-only revoke lookup cannot advertise a settled
receipt while the current runtime or operation/native host resources remain
undrained. Status is display only, never an independently supplied drain proof.

Independent status/result reads do not create or migrate an absent database.
NativeTakeover can authenticate its original CLI child and acquire the original
owner lock while a service with existing state is absent; it does not start a service. Existing
operation custody remains a drain prerequisite. Only a missing request under
the same nonempty journal and unchanged original revision is a definitive
negative. Missing/replaced state or an initial unjournaled request stays unknown.
For an unknown outcome, the client must first confirm its original child exited
and then obtain a fresh definitive lookup before retiring that logical operation;
a negative observed while that child was still live is not terminal. The service rechecks the original two-second native permit
after acquiring the SQLite write transaction and before committing a new intent.

The selected CLI advertises `Desktop frontend request ABI: desktop-ui-request-v1`.
After its confirmation UI, Next prepares a closed `AutomaticApprovals` request
with a stable request UUID and a fresh attempt UUID. Its native responder reserves
one of eight short-lived slots. The maintained runner binds the original `Child`
before sending the envelope to `hgs desktop frontend-request --json`, and retains
the registration with the child and all pipes. An expired registration supplies
no evidence and cannot release a still-retained resource slot.

The instance listener answers the fixed `desktop-ui-request-v1 ATTEMPT` query
from that cache. The service matches the socket peer's PID, birth, selected CLI
image and direct frontend parent to the actual canonical instance peer and its
installed frontend image. Configuration, request fingerprint and original child
binding must all match. Bounded ancestry inspection refuses agent markers,
agent executables and unavailable ancestry. Native serials prevent a delayed
older setting from overtaking a newer one. A wire role, confirmation flag or
nonce alone supplies no authority. Generic desktop requests cannot change this
policy.

Ordering lanes are mapped exhaustively from the closed native request type,
including the read-only `ActivationWait` request. Activation reads have their own
serial lane and pass the same original two-second admission check before a
bounded receiver can be installed; ownership and policy requests cannot obsolete
an unrelated read. Unknown wire operations are refused before native admission.

On Windows this request uses the existing per-user activation pipe, preserving
its name/home hash, lifetime lock, own-SID DACL and remote-client refusal. The
query remains 59 bytes (64-byte listener limit), with one JSON reply of at most
4096 bytes including LF. The service obtains the actual server PID from the
kernel and retains its process handle and creation-time identity; a JSON PID
cannot substitute for that evidence. A full/slow connection has a bounded
exchange and cannot keep the listener alive during shutdown. No extra pipe or
probe-time helper is created.

Windows ancestry retains the original process objects and checks at most 32
same-user ancestors within the original two-second authorization deadline.
Environment reads require the native pointer-width layout, verified against the
Win32 environment API, and a double-NUL terminator within 64 KiB. Marker keys
use Windows ordinal case comparison, including empty values. Process birth,
image, parent and environment are checked before admission and again at use.
The walk checks same-user ancestry and ends at a different token user SID.
Windows retains the old parent PID after a launcher exits: a missing/exited
parent or a candidate created at or after the child's creation FILETIME ends the
walk as departed. A query-only
`OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` returning `ERROR_ACCESS_DENIED`
also ends the walk.

Access denial does not prove a different principal. A same-user process can
protect its own process object with a restrictive DACL; an OWNER RIGHTS ACE can
even remove `READ_CONTROL`. Both access denial and the departed-parent boundary
(the Windows analogue of a Unix double fork) address unintentional agent
descent. Deliberate same-user code remains outside this application IPC check.

Creation times come from retained original handles; a later-created candidate
is never inspected as an ancestor. These are parent-only boundaries, never a
caller/GUI liveness exception or authority from roles, names, PIDs or caller JSON.

Readable same-user agent names/markers, other open/query errors, changed
identity, the ancestor cap and the original deadline still refuse. Held parent
proofs and the original same-user chain are rechecked at use. No failed
Toolhelp lookup is converted to a clean PID-zero root. This does not change the
separately unsupported Windows owner-status probe.

Only the service actor commits policy and read intents, and responses claim
`durable:true` only after that commit. The delivery worker observes successfully
applied service-owned fleet events; passive UI projections cannot authorize an
Answer. Approval policy remains default-off and existing writer/handoff checks
still precede delivery. `delivery_result` is an indexed read-only query available
even while the service is passive after revocation. Retry an unknown interaction
with the same logical request UUID; do not fabricate a new delivery receipt.

Policy mutation also requires an opaque actor-side permit at the delivery API;
passing policy JSON to its generic request method refuses. Ancestry checks reject
advertised loader-injection variables, and Linux requires a readable, zero
`TracerPid`; Windows requires a successful negative native debugger query.
Other-platform tracing checks remain platform work. Executable
names supplement the session/run markers: a renamed or reparented unmarked
launcher cannot be identified from these checks. This is an application IPC
check, not an OS isolation boundary against arbitrary same-user code. Restart
ordering and a complete native positive-path regression remain cutover work.

## Maintenance configuration

`Desktop maintenance ABI: desktop-maintenance-v1` is required in addition to the
frontend-request marker. The native sender uses
`ui_requests::Request::maintenance_configure(request_id, Configuration)` with
both `update` and `recovery` present (explicit null disables a subsystem).
It uses the same original-child registration, request fingerprint, retained
resources and fresh actor permit as approvals. Generic `desktop request` cannot
configure timers or policy. Update identity must match the authenticated GUI's
actual executable, version and source, never the CLI version. The native handler
supplies that identity from its own build; callers cannot select another app.

The service owns maintenance load, timers, original fleet observations and
actual worker drain. Its closed `maintenance::Projection` appears as
`value.maintenance` on existing watch hello/status/heartbeat frames and on
`desktop status --json`. The UI keeps one subscriber; missing ABI/projection is
unavailable, with no legacy timer fallback. No software installation or native
notification is triggered by this lane.

A generic read-only `maintenance_result` request contains a fresh `request_id`
and the `original_request_id`. It works after revocation/passive restart without
starting an executor. `durable:true` describes the original committed configure,
not a new write by the query. Unknown recovery edits remain consumed and are
never recreated by a timer or a result lookup. Approval and maintenance ordering
are separate within one UI instance, so an automatic configuration cannot
silently obsolete a human approval-setting change.
