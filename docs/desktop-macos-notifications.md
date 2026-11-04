# macOS durable notifications and closed-app activation

G4 design for `zerus-5wmh.66.6`, `.43` and the durable extension of `.44`.
This is a proposed integration contract, not executable admission. The current
GUI still owns its poster. G1's producer drain and native adapter admission
remain prerequisites; no import, marker, parsed receipt or document grants them.
The completed Mac switch is not reopened.

## Current boundary and ownership

Today, `client/src-tauri/src/platform/macos_notify.rs` posts through
`UNUserNotificationCenter` inside the running app. Its delegate routes tokens to
the GUI desk. `tray/src/MacAttentionNotifier.mm` defines the original behavior:
Qt's texts, default sound, `zerus.attention.<token>` request IDs, and removal from
both pending and delivered notifications. Existing focus, mute, reduction and
reply-deferral policy stays in the shared attention reducer.

The maintained `com.hgdev.hgs-tray` LaunchAgent runs the installed bundle's
`Contents/MacOS/hgs-tray --hgs <configured CLI>`. `RunAtLoad`,
`ProcessType=Interactive` and crash-only KeepAlive stay intact. Normal Quit does
not restart it. The present single-instance path can return before the
notification delegate exists; setup alone cannot prove cold-click delivery.

| Owner | State and responsibility after qualified handoff |
| --- | --- |
| G1, the existing per-configuration `hgs desktop` actor | One attention source/reducer; durable intents, baselines, reservations, native mappings, withdrawal tombstones and pending activations; writer/service epochs, adapter leases, admission and actual drain. Uses the existing private desktop database. |
| G4, inside the preserved signed `com.hgdev.hgs-tray` bundle | One leased native adapter: authorization inspection, native calls and bounded callback ingress. No detector, fleet, durable effect queue or database writer. |
| Zerus Next frontend | Existing list/window and exact-session presentation after authenticated activation; no notification fallback writer after handoff. |
| G3 / G5 | G3 retains immutable historical evidence; G5 owns reviewed registration, signing and installer changes. Neither source evidence nor an installed bundle supplies current effect authority. |

This extends the [desktop service design](superpowers/specs/2026-10-29-hgs-desktop-service-design.md)
and [producer-drain contract](desktop-producer-drain.md). Attention, posting and
badges move together under `g1-background-v1`; this design implements only the
Mac Post/Withdrawal and activation portion. Recovery, voice, swarm, connector,
native agents and terminals keep their existing owners.

## Signed host and launch path

Use a windowless mode of the existing app executable, not a second notification
daemon or a CLI pretending to be the bundle. A production no-argument login
start may enter this mode only after current G1 ownership is authenticated.
It has an AppKit event loop and notification delegate, but no webview, Dock icon,
menu-bar takeover, focus steal, legacy fleet or attention writer. It remains
windowless after posting. A validated click or `--sessions` promotes that same
canonical instance to the frontend. `show` keeps its existing no-visible-effect
semantics.
A missing/incompatible service after handoff leaves the migrated roles paused;
it never restarts the old GUI producers.

After an explicit Quit, only a **fresh attention edge recorded by hgs after
that Quit** may request a missing host. Startup, reconnect, re-read, old intent
and imported/legacy evidence never wake it. G1 requests a non-killing start of
the installed `com.hgdev.hgs-tray` LaunchAgent, then awaits a checked hello.
A wake creates no notification authority. No `kickstart -k`, `open -n`,
alternate bundle copy, unconditional KeepAlive, installer call or process kill
is part of delivery. G1 admits at most one host launch per attention burst and
at least 30 seconds between attempts, retaining the attempt/cutoff across its
own restart. Edges arriving while a launch is pending coalesce into that burst.
A failed, exited or crashed bootstrap never loops or retries that burst;
rate-limit expiry alone is not a new edge. The unchanged crash-only launchd
policy is not authority for another notification; the host bootstrap must
consume an actor-owned, one-use launch permit and exit cleanly on refused
setup. A crash during bootstrap leaves that permit spent, so launchd's next
crash restart refuses the same bootstrap and exits cleanly. Qualify that exact
crash/clean-exit sequence before admission; changing KeepAlive is not the fix.
Reserve/spend the durable permit **before** the launch side effect, binding
configuration, trusted installed host, current ownership, cutoff and burst.
Unknown launch outcome is status-only, with no replacement for that burst.
Bootstrap state is separate from the Post reservation and callback lease:
launching never authorizes a post or consumes a notification identity.
Deliberate frontend or promoted-host Quit remains a clean exit; a second
explicit Quit also advances the wake cutoff and cannot resurrect that burst.
G1 owns the durable close cutoff, manual-launch state and launch permit;
uncertain exit, startup or elapsed time never manufactures a fresh edge.
An unverified cutoff refuses wake admission. An automatic login/crash bootstrap
after Quit must exit cleanly without a fresh permit. Explicit desktop-service
revocation prevents admission. No plist/CLI behavior changes ship with this
document; the startup mode needs its own reviewed implementation.

The approved setting is **Notify when Zerus is closed**, default on. Webui-ac
placed it in **Settings > Sessions**, in the existing Group immediately after
**Notify about optional questions**; no new Settings section. Use the existing
ToggleRow hint: **On by default. After you quit Zerus, new replies and questions
still show system notifications. Clicking one opens Zerus at that session.**
It is macOS-only; hide it entirely on Linux/Windows. Persist it through the
existing settings writer and synchronize the durable G1 policy. While the
service is unavailable, keep it visible but disabled with **Needs the Zerus
background service, which is not running.** Never change the stored value due
to an outage. Its durable G1 policy gates post-Quit
wake/posting; it grants no lease or retry. With it off, Quit stops this native
notification flow until the next manual launch. Restart/reconnect does not
count as that launch, and manual reopening never reposts suppressed old edges.
It does not stop unrelated hgs services or agents. This document adds no UI.

macOS can [launch the registered app for a response](https://developer.apple.com/documentation/usernotifications/handling-notifications-and-notification-related-actions).
Install and retain the notification delegate **before application launch
finishes**, including transient starts, before a forwarding-only instance can
exit. Apple's [delegate contract](https://developer.apple.com/documentation/usernotifications/unusernotificationcenterdelegate)
requires this ordering. The early callback receiver forwards to G1, requests the
maintained LaunchAgent path and yields canonical ownership to it. It must not
become an unmanaged second frontend. The existing lock, socket and updater
discovery remain; the bootstrap must qualify concurrent launchd/LaunchServices
starts rather than assuming which process receives the callback.

## Admission, correlation and outcomes

Reuse `crates/desktop-ledger` native `Identity`, `Reserve` and exact `finish`
semantics. Only G1 can construct a verified native job after fresh producer
drain, current writer/service epochs, a live adapter lease and a committed
`fresh=true` reservation. `fresh=false` is status-only. The adapter checks the
lease again immediately before issuing the native method. Only the latest
verified **before-call** Refused outcome permits an explicit `retry_of`.
Timeout, disconnect, cancellation or error after possible issue is Unknown;
Unknown never becomes permission to post again, including after restart.

The service actor settles a matching receipt by its original completion time,
independently of native resource release. An on-time receipt can be durably
Delivered while its callback, child or pipe still holds the original operation
charge. It cannot become Deadline merely because those resources outlive the
reply. Resource accounting, capacity and shutdown still wait for every original
release and actual host/worker join. A late or missing receipt remains Unknown;
neither a receipt nor SDK submission success proves an observed OS banner/click.

The proposed private mapping binds configuration, original intent/revision and
exact target identity, attention token, native request ID, original Post ticket
and adapter/lease epochs, a random
per-post correlation UUID, and verified installed signing identity. New
`userInfo` keeps `hgsAttentionToken` and adds a versioned correlation value.
The OS request ID stays `zerus.attention.<token>`. Neither value is executable
input or a target-selection authority. Full native envelope is at most 64 KiB;
title/body are each at most 16 KiB, token at most the existing 8192 bytes.
Receipts/control are at most 16 KiB, with allowlisted metadata and no private
notification body or system error text. G1 owns the eventual schema/API.

The independent pure component is `client/core/src/macos_notify.rs`.
`Envelope::new` retains the exact Qt-compatible token and a canonical non-nil
correlation UUID; `request_id` and `user_info` produce the request ID and the
two-string dictionary (`hgsAttentionToken`, `hgsAttentionCorrelationV1`).
`response` bounds that dictionary's JSON representation before parsing and
returns ignored legacy/non-default data or a candidate envelope. Null,
duplicate/unknown fields and mismatched request IDs refuse. A candidate proves
no signed origin, delivered mapping, current revision or target availability.
`post_outcome` classifies supplied observations; `WithdrawalObservation`
retains which void calls were issued and always remains Unknown. These are
data helpers, not a new transport, receipt validator or ledger. No GUI or root
runtime uses them; the current poster and default builds stay unchanged.

The separate default-off `macos-native-peer` feature exposes `macos_notify::peer`
only on macOS. It enables the existing Foundation NSData binding, with no new
package or lockfile change. `observe(&UnixStream, &str)` synchronously checks
kernel audit data and live Security code against G1's pinned installed
requirement, bounded to 16 KiB and no NUL. Opaque borrow-bound observations
contain only the supplied requirement's digest and have redacted diagnostics;
they authenticate neither that requirement's source nor a current lease.
Full-token consistency and repeated live checks are point-in-time observations,
not exec fences. The host must own the actual check/connection lifetime beyond
an outer timeout, off its actor/UI thread. The helper starts no worker or
transport. `client/scripts/macos-peer-fixtures.py` qualifies private synthetic
signed peers under the Mac heavy lock. Ad-hoc C peers cover mechanisms and
exec/exit; digest-pinned private Node/Deno copies run synthetic socket scripts
for distinct-team checks. The fixture operator derives the expected requirement
from pinned input-file metadata before launch, never from a peer message.
No installed signing identity or Notification Center is used.

Authenticate the native peer using kernel-bound live-process identity
(audit-token-equivalent, with PID-reuse protection) and live code verification
against the maintained trusted installed bundle's designated requirement,
plus the current configuration, canonical instance, service challenge and
ingress lease. Capture trusted installation identity separately; a peer-supplied
requirement, bundle ID, PID/path string, same UID or token is insufficient.
The present Unix transport's
same-user check is not that signed-peer proof; the concrete macOS transport
and upgrade-continuity extension must be agreed with G1 before implementation.
Unavailable audit-token binding refuses; it never falls back to PID lookup.
Keep private signing identifiers out of source. Do not invent another service
or reuse the native ledger for G2 jobs.

The [Linux lineage lesson](desktop-linux-notifications.md) applies without
inventing a Mac bus ID: keep the original durable post binding separate from
the **current authenticated callback connection**. A banner may survive the
posting process or service epoch. A fresh lease may report its original
correlation, but cannot rewrite the original reservation, adopt arbitrary OS
IDs or turn Unknown into Delivered. Matching designated signing requirements
permit maintained upgrades; a changed/unverified requirement refuses adoption.

`addNotificationRequest` with no trigger preserves immediate delivery. Its
successful [completion](https://developer.apple.com/documentation/usernotifications/unusernotificationcenter/add(_:withcompletionhandler:))
means scheduled/accepted by the framework, not proof of a visible banner.
Permission and OS presentation policy still apply. Existing grants are reused;
an undetermined authorization must take the existing explicit app authorization
path, not a background surprise prompt. No owner permission store is reset.

Coordinator-approved migration rule: legacy token-only banners lack a G1 post binding.
Retain historical suppression until that original attention edge withdraws;
do not repost, activate from their clicks or remove an unbound OS request.
This is an accepted parity
tradeoff, not an already shipped Mac policy.
The current GUI compatibility path stays unchanged until handoff. G3 evidence
does not manufacture a delivered mapping or current signed-peer authority.

## Default click while the frontend is closed

1. The early delegate accepts only the default action and a bounded, well-formed
   correlation envelope. It completes the OS callback exactly once, after a
   bounded submission attempt; OS completion is not a durable G1 receipt.
2. Over current authenticated ingress, G1 looks up the original **Delivered**
   post mapping. It checks exact correlation/token/request ID, current
   unwithdrawn intent/revision and configuration. Missing, legacy, Unknown,
   revoked, stale or conflicting bindings select nothing.
3. G1 durably admits one activation identity for that exact post before waking
   the frontend. Duplicate callbacks/status inquiries cannot create another
   activation or choose a different session. This is actor-owned presentation
   state, not a new native effect kind or local app spool.
4. G1 starts the maintained tray LaunchAgent if absent, or promotes its existing
   windowless instance. The canonical `show`/`sessions` messages remain compatible.
   An additive bounded, authenticated exact-session delivery carries the G1
   activation identity; the old two lines do not convey a token. This specializes
   the service design's proposed instance-channel extension, which is not yet
   implemented. No free-form path, executable or latest-session route is accepted.
5. Persist the current canonical consumer's exact claim **before presentation**.
   Once ready, the existing desk resolves the original stable computer, run and
   conversation against G1's current catalog, including rename, and presents
   exactly that target. Recheck the tombstone/revision immediately before
   selection and acknowledge the same activation. Missing/offline/replaced
   identity stays pending/reviewable; display aliases never substitute identity.
   An acknowledgement never sends agent input or independently marks a reply read.

Webui-ac's presentation ruling uses the existing notice bus, with no pending
list: **That session is no longer available.** for missing/replaced identity,
or **<computer> is offline. The session opens when it reconnects.** for offline.
These presentation states still require the exact claim/revalidation rules;
the wording does not authorize a second presentation or an alias fallback.

Proposed bounds: one native method in flight, one queued job, at most eight
callback envelopes and 64 KiB total queued callback bytes; a full queue refuses
new ingress without evicting accepted work. Each native/control attempt has a
positive absolute budget of at most five seconds; window readiness has the
existing 30-second total startup bound. Pending activations use indexed pages
of at most 128 and the desktop storage-pressure rules, not per-session caches.
Lost acknowledgement reconciles the same persisted claim, never a blind second
presentation, fresh target or notification. Accepted pending identities are not
evicted under pressure. Restart does not blindly present old pending clicks;
it revalidates their current intent and exact identities first. Durable admission
and consumer deduplication must be qualified before claiming restart delivery.

If G1 is unavailable, the callback cannot claim durable acceptance. Keep the
bounded failure explicit and do not create an app-side journal or start a
second actor. The normal closed-frontend guarantee assumes the registered G1
service is available; service outage is a distinct incomplete outcome. No OS
callback redelivery is assumed.

## Withdrawal, races and shutdown

G1 commits the original intent's tombstone and invalidates pending activation
before reserving a native withdrawal. The signed adapter removes only its
exact bound request ID from both
[pending](https://developer.apple.com/documentation/usernotifications/unusernotificationcenter/removependingnotificationrequests(withidentifiers:))
and [delivered](https://developer.apple.com/documentation/usernotifications/unusernotificationcenter/removedeliverednotifications(withidentifiers:))
notifications. Both APIs are asynchronous and return no completion receipt.
Record both methods as issued metadata and finish the native attempt Unknown,
never Delivered or visually removed. This is the agreed native outcome contract.
Do not enumerate all Notification Center items to invent a confirmation.

Serialize calls per exact post; an already-issued Post retains its callback
charge before the matching withdrawal can run. A late callback can settle only
its still-reserved original attempt, never clear the tombstone or change an
immutable Unknown. A banner left by a race is inert after withdrawal. New
attention requires a new edge; service/host restart alone never creates one.

Revoke fences new jobs first. Retain admission through actual native method,
completion block, callback ingress and queued-result lifetimes; a bounded wait
or socket close alone is not drain. Preserve `.9.30`'s Exit/`exit_work` fallback
for Cocoa/system termination without requiring a page acknowledgement, and
the updater's explicit `quit_with_flush` path. An app exit neither cancels
hgs intent nor authorizes another post. Rollback still requires G1's qualified
revoke/drain gate before Qt starts.

## Reviewable implementation slices and proof

1. Agree this design and the G1-owned native lease/mapping/activation extension.
   Add pure envelope/outcome policy and fake-center tests independently.
2. Qualify signed windowless bootstrap, early delegate and LaunchAgent promotion,
   initially passive. Obtain pinned pre-merge review for startup, window,
   transport, signing, manifest or packaging changes.
3. After G1's positive producer/native admission exists, fence the current GUI
   poster, integrate exact Post/Withdrawal reserve/finish and actual drain.
4. Integrate durable click admission and exact frontend claim/ack, with private
   kill/revoke/restart and duplicate/stale-click tests at every durability edge.

Headless Mac tests use fake native centers and private state under the owned
heavy lock; they never call the owner's notification center, prompt, alter
LaunchAgents or restart agents. Cover two configurations, replaced signing
identity, late old leases, same token with wrong correlation, Unknown, pressure,
post/withdraw races, lost acknowledgements, concurrent starts and clean Quit.
Compile plus existing macOS window tests is explicit mechanism evidence, not
proof of OS cold launch. Signed notification posting/click/withdrawal, no-window
and no-Dock behavior, and launchd/LaunchServices routing need a separate real
Mac acceptance window arranged by design-ac with the owner. G2 `.66.4.2`
preempts these slices when its parent `.66.3.22` becomes executable.
