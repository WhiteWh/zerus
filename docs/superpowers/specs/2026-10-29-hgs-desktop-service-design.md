# Durable desktop service in hgs

**Status:** Direction approved, 2026-10-29; revised for coordinator and webui review before final fold.

**Tracking:** `zerus-5wmh.66.3` (G1), including `.54` and `.65`.

**Baseline:** `integrate/client-webui` at `0bc1eb87`. G1 is off the Mac-switch bar. Implementation starts after the switch; owners are decided then by budget. Until then, core finishes `.35` after the New session fold, then stands by.

## Scope and ownership

The owner's chosen topology is one new durable **per-user `hgs desktop` service**. It owns the fleet source, desktop presence, attention detection and posting intent, queued read delivery, badge projections and automatic approval delivery after every window closes. Zerus Next owns presentation. Recovery, voice, swarm synchronization, native agents and the mobile connector retain their existing processes and ownership.

This implements [inventory G1](2026-10-29-qt-retirement-inventory.md#g1-durable-fleet-presence-and-notification-services-in-hgs) and its [migration contract](2026-10-29-qt-retirement-inventory.md#migration-contract). Preserve [desktop attention](../../reference.md#desktop-workspace), [automatic approvals](../../reference.md#desktop-workspace), the presence/read identities and watch-v1 contracts in `docs/reference.md`. Review rulings accept desktop-v1, the command group, SQLite and the Windows logon task. They are future interfaces, not implemented ones. Budgets remain qualification proposals until measured.

## Registration and lifecycle

| Platform | New registration | Execution and boundary |
| --- | --- | --- |
| Linux | `hgs-desktop.service`, systemd **user** unit | `hgs desktop serve`, restart on failure with bounded delay; graphical-session environment supplies native presence/posting access. No system unit or root service. |
| macOS | `com.hgdev.hgs-desktop`, LaunchAgent in the user's login session | Plain CLI LaunchAgent; it owns only durable intent. G4 posts and owns the badge inside signed `Zerus.app` (bundled helper or windowless agent mode), under `com.hgdev.hgs-tray`. No LaunchDaemon. |
| Windows | Per-user logon task named `hgs-desktop` | Starts `hgs.exe desktop serve` without a console, with bounded task restart-on-failure. Next may request the same registered start if absent. No elevated service or second watchdog tree. |

On macOS the service reaches G4's adapter over private IPC; if absent, it requests the maintained signed bundle's agent-only launch path, waits for its version/epoch hello, then delivers intent. That entry must create no window or Dock icon. The CLI cannot post or badge as `com.hgdev.hgs-tray`. G4 must prove this while Next's window is closed.

The proposed [macOS notification contract](../../desktop-macos-notifications.md)
details signed-host ownership, cold-click delivery through the tray LaunchAgent,
withdrawal and stale-click fencing. Its native admission and activation APIs
remain unimplemented; the current GUI poster retains authority until handoff.

`hgs desktop status --json` reports generation/version, ownership, upstream freshness, pending counts and component errors. G5 adds maintained installation/removal with configured CLI/state paths. The dev identity `dev.zerus.next` and replay never hand off. Non-default configurations never silently register or take production ownership. Existing units and application identifiers stay intact.

The service holds an OS-backed exclusive lock scoped to the user and canonical configuration identity; a PID or heartbeat alone never proves ownership. EOF from a client detaches only that client. Graceful service exit stops admissions, checkpoints accepted work and closes its own read streams. It never terminates agents, terminals, native hosts or their process trees.

## One source and bounded fanout

Reuse the Rust watch supervisor, legacy scheduling and pure attention/read reducers already in `client/core`; share or relocate these without introducing a Tauri/Qt dependency into hgs. Maintain exactly one upstream `hgs watch --json` per configured computer, with peers reached through `hgs @alias` and its established SSH transport. A capable source has no parallel listing poll. Older peers use the same supervisor's bounded legacy schedule; automatic approvals retain Qt's five-second peer freshness cap while enabled.

Stage catalog/meta pages in an indexed projection by source generation; publish replacement only after `last`. Gaps/reconnects retain last-known display but remove current-action authority. Updates carry upstream status/generation: local IPC connectivity never means a peer is online. Route peers by configured alias, independently of reported host name.

Merge Next's Activity/Git/Processes interests into those same handles, keyed by exact computer/run/conversation or checkout; choose the shortest permitted interval. Keep advertised subscription limits and explicit refusals. Archives stay bounded separate reads. For `.65`, the catalog reducer projects existing `process_work.count` additively, preserving unknown/zero and freshness; no extra inspection or journal. The UI uses Qt's idle activity/phase plus positive background count (`SessionsWindow.cpp:5298-5299,5463-5465`).

Process state in source order; publish snapshots and sequenced deltas through byte-bounded queues. A slow subscriber receives a gap/resnapshot or disconnects without blocking other consumers. Durable commands are refused before acceptance or journaled, never dropped or coalesced like display updates.

## Writers and durable intent

`desktop/state.sqlite3` lives below the existing `HGS_STATE_DIR`, with a private directory/file and existing link/permission checks. Version its schema and transactional migrations; a newer schema is preserved untouched and refused, never reset or downgraded. It holds indexed projections/checkpoints. One database writer actor serializes transactions; only the logical owners below write their tables. Host read state and the private approval journal retain their existing APIs/stores; no duplicate authority. Diagnostics exclude message bodies, questions and notification tokens.

| Logical owner | Durable authority and delivery rule |
| --- | --- |
| Presence | Sole automatic producer of existing `desktop-presence.json`, through the existing presence implementation. Sample aggregate idle/lock only: 30-second periodic writes, five-second transition checks, one sample at a time with the existing ten-second timeout. Preserve 120-second listing expiry. On Linux the user service has no display connection: no KIdleTime/compositor listener. Use `LinuxPresenceSource.cpp:64-80`'s session D-Bus ScreenSaver idle/lock, GNOME/Mutter and KDE paths (including its units check), plus logind IdleHint where available. No answer means unknown, never active; a lock/idle hint alone is not precise input evidence. Input hints never turn missing Linux D-Bus evidence into active presence; no keys or pointer history are recorded. |
| Attention and posting intent | Reuse Qt/Rust attention identities and suppression semantics (`AttentionTracker.cpp:12-287`, `client/core/src/attention.rs`). Atomically persist observed identity and its posting/withdrawal intent before dispatch. Preserve focus, mute, optional-question baselines and deferred reply notices while native/child/background work continues. Offline is not resolution. An imported or focus-suppressed edge never becomes a fresh banner later. |
| Read queue | Persist the exact computer/name/run/conversation and merged reply/signature acknowledgement before reporting it queued. Existing host `hgs read` remains authoritative. Retire only when a matching host snapshot settles it; partial/unknown results remain pending. Preserve `ReadMarkSync.h:23-26` and `core/src/readsync.rs`: four in flight, fair host choice, failure backoff 10–300 seconds, confirmed resend 120 seconds, partial-application resend capped at 960 seconds. These identity-bound acknowledgements may retry; this permits no general action replay. |
| Badge/count projection | Commit counts and freshness against the catalog/marks revision; retain an explicit incomplete indicator on offline data. No independent poller, full-history scan or second notification detector. After handoff, Next's Sessions counters (`.46`/`.58`, Qt online rule), the macOS menu bar and desk badge consume this service projection, including incomplete/offline flags. They never recompute attention; scoped counters request the service projection. G4 only applies the newest revision. |
| Automatic approvals (`.54`) | Sole discovery/delivery worker, **off after migration until a new explicit local opt-in**. Reuse `approval-log begin/finish/list/detail` and exact native verification. Scan at most 512 rows per 250 ms tick, two concurrent targets, 4096-entry cooldown. Reserve durably before native input, recheck policy and exact run/conversation/question, then send once. Existing, failed, cancelled or unknown reservations never authorize another delivery. Journal failure pauses admissions; turning off stops new admissions. Ordinary questions, startup trust and persistent grants stay manual (`AutomaticApprovals.cpp:28-128`, `src/approval_log.rs`). |

G4 supplies one leased native posting/badge adapter, with no detector. Tokens bind durable intent to exact activation identity. Confirmed pre-delivery refusal may retry; uncertain posting uses read-only status or stays unknown. Withdrawal invalidates stale clicks even if the OS keeps the banner. Resolve run/conversation across rename before activating Next, within the existing bounded wait. Routing survives window closure.

## Zerus Next interface

`desktop-v1` uses a user-private Unix socket or a named pipe restricted to the current user's SID. Refuse remote/cross-user clients and oversized inputs. CLI proxies never start their own peer readers.

Proposed `hgs desktop watch --json` envelopes carry epoch, sequence, computer, upstream generation/status and watch-v1 frames/desktop projections. Peer watch-v1 is unchanged. Next's Rust adapter feeds its existing pump, subscriptions and 50 ms batches; JavaScript gets no new state engine. A new epoch resubscribes/reconciles snapshots, never guarded actions.

Proposed `hgs desktop request --json` permits only subscriptions, focus/input fallback, exact read acknowledgements, delivery receipts/activation and explicit approval settings. Durable operations use request IDs and acceptance/refusal; uncertain replies are checked by ID. Subscription/focus heartbeats are transient, coalesced leases, not durable journal entries. Existing CLI outputs stay compatible. Other interactive writes retain their guarded lanes and G2 ownership.

Focus means **application focus**: Next reports one flag for its active, non-minimized native window, never per-session focus (`AttentionTracker.cpp:51,124,226`). The lease expires after five seconds without renewal. Suppressed edges are recorded and never replayed on blur/disconnect.

Before a configuration's handoff marker, Next keeps its existing in-process attention, read-sync, counts and approval UI unchanged; `.54` delivery remains unavailable until implemented. After handoff, absent/incompatible service means read-only for these roles, never fallback writers or a second fleet. Sessions shows the persistent notice **"Background service is not running. Notifications, read marks and approvals are paused."**, with **"Start service"** asking the maintained registration to start it, never spawning a private copy. Settings > About shows the same status beside build info, without separator dots.

A banner click or menu/badge route when Next is closed starts it through the maintained launch path, waits for the window to be ready and sends the exact target through its existing single-instance channel (`src-tauri/src/instance.rs`), never a second interactive instance. At baseline that channel accepts only `show`/`sessions`; G4 extends it with bounded open-session requests and preserves those existing messages.

## Handoff, coexistence and upgrade

Qt remains on **arch until the Linux phase**. Installing/upgrading its desktop unit stays passive until the handoff marker: subscriber only, no writers and no second fleet; if the owner exposes no feed, wait idle. Today's arch behavior is unchanged.

The Mac switch deliberately shares Qt's `hgs-tray` instance name (`identity.rs`, `instance.rs`); probing a socket cannot identify the owner. Require a versioned **desktop-v1 delegation hello** with build/configuration identity and writer epoch. Next answers it; an instance without a valid reply is legacy and keeps the service passive. Keep Qt/Next's ordinary frontend single-instance exclusion. Service writer exclusion uses the durable revocation and exact-effect guards below; refuse unproven quiescence. Compatible Qt/Next can coexist only through this handoff protocol and the same fleet.

G3 imports bounded read-only snapshots after old writers drain/stop. Record source/schema identities and a durable handoff marker before new writers start. Preserve tokens, pending reads and reservations; baseline historic alerts without posting. Quarantine malformed/unsupported input, fail closed and leave all sources unchanged. G3 explicitly reads both Qt's former opt-in and **Next's interim automatic-approval setting** (its settings store, if enabled before G1). Neither carries consent across the move. Settings says **"Automatic approvals were turned off by the move."** A new local opt-in is required. Reuse G3 importers; G1 takes no draft/run-layout/connector writer.

The G1/G3 agreement of 2026-10-30 is specified in [the desktop migration contract](../../desktop-migration.md). The small shared `zerus-desktop-migration` crate validates marker/manifest structure, fixed digest references and the existing configuration fingerprint. G3 owns immutable import evidence. G1 alone writes `desktop/handoff-v1.json` phases under `desktop/handoff.lock`, which it retains while migrated writers run. G3 never takes the service's `owner.lock`. No parsed phase or evidence field proves current quiescence; G1 still verifies fresh delegation, durable drain and current writer/effect guards. The shared crate performs no registration, marker writes or takeover.

The coordinator's later 2026-10-30 ruling adds the closed v1 `revoked` phase without changing the marker payload: G1 durably revokes before normal rollback/Qt start and records reason/history in its own journal. Exact shared effect identities guard correctness, and live legacy-GUI detection revokes the abnormal path. This supersedes a requirement for service-owned long-lived Qt Unix socket exclusion; Windows permanent lock exclusion remains separately unqualified. Production takeover stays fenced until revocation and exact effect guards exist.

The accepted effect audit reuses `approval-log begin`'s per-host unique durable
question reservation and `hgs read`'s serialized exact-identity merge. Presence
needs epoch-bound producer admission, with legacy refusal naming the revoke
command. Posting/withdrawal need durable edge/attempt reservations and exact
leased receipts; badge application needs newest-only epoch/revision receipts.
The service probes the GUI socket's live owner before each effect batch and at a
bounded interval, revoking for legacy Qt. It never owns the canonical socket.
Unmodified Qt directly calls native notification/badge APIs and bypasses a new
hgs journal: cosmetic duplicates or visible cleanup races are acceptable only on
the abnormal manual-Qt-start path until stand-down. No global exactly-once
guarantee is claimed.

Maintained upgrades stop admissions, checkpoint, publish the executable through the maintained platform installer and restart **only desktop**, preserving source identity/signing. Reconcile receipts/current snapshots before effects resume; unknown delivery is not replayed. Verify native-agent birth identities survive. Recovery, voice, swarm and the connector are never restarted as a side effect. Preserve the Windows staged session-host images and in-use binaries (`tests/test_windows_installer_survival.py`); an in-use replacement is deferred rather than killing its owner.

Rollback never re-seeds imported stores. Before `update-installed.py --rollback`
or an install selecting `HGS_MAC_GUI=qt` / `HGS_LINUX_GUI=qt` starts Qt, G1 must
durably revoke: stop admissions, record `revoked`, drain and checkpoint. Failed
or uncertain revocation stops rollback before Qt starts with an actionable
diagnostic. Retain exact delivery identities against replay. G5 preserves the
rollback copy and leaves takeover disabled until this gate exists. Qt removal
still requires platform G6 acceptance.

## Budget and proof

These are approved qualification targets to measure, not established capacities:

| Resource | Bound / acceptance target |
| --- | --- |
| Framing | Reuse watch's 1 MiB upstream frame, 512 KiB snapshot page and 64 KiB control-line limits. A local envelope allows at most another 4 KiB of metadata. Never assemble a whole journal/snapshot in memory. |
| Fanout memory | At most eight local subscribers, 4 MiB queued each; global upstream ingress 16 MiB. Backpressure/resnapshot isolates a slow client. Catalog and pending work are indexed on disk, not unbounded resident per-session caches. |
| Work | One upstream per configured computer; at most four connection/probe attempts and four read deliveries concurrently, plus two approval targets and one presence sample. Subscription limits remain the advertised watch limits. Capacity exhaustion is explicit, with incomplete freshness and no automatic grants for uncovered sources. |
| Storage | Proposed 512 MiB projection/intent budget, 64 MiB WAL/checkpoint budget and 10 MiB rotated diagnostic logs. Reclaim settled projection/history first; never evict unresolved intent, read marks or approval reservations to make room. If durability cannot be admitted, refuse new effects and report pressure. Existing approval-journal retention remains its own policy. |
| Runtime target | At 100,000 live rows across eight computers: at most 256 MiB steady RSS for the service, local transport children and native adapter; remote reader cost is reported per host. Idle below 0.5% of one core; no busy-loop retries. Measure burst recovery and queue high-water marks before accepting these numbers. |

Measure main-database and WAL allocated bytes separately. Above 80% of the proposed 512 MiB projection budget, reclaim toward 60% using indexed batches of at most 256 settled rows, yielding between transactions: obsolete catalog generations and resolved intent payloads first. Preserve live dedup baselines, exact unknown/pending work and approval reservations. Receipt reclamation requires epoch/revision fencing so an expired receipt can only be queried, never admitted again as a fresh effect. Checkpoint WAL without long reader transactions; do not count free pages as reclaimed disk until measured. At the hard budget, if safe reclaim cannot free space, refuse new durable effects and report pressure. Qualification records bytes before/after, WAL peaks and writer latency under the stated workload and disk-full injection; tune the proposal from those measurements.

Worker errors stay component-local: presence expires, posting failure does not stop reads, and failed approval reservation sends nothing. Registration recovers a desktop crash; it holds no recovery/voice/swarm locks or agent shutdown hooks.

Required tests before implementation can be called complete:

- Shared Qt/Rust fixtures for attention, mute/focus/offline/reply deferral, exact click targets, presence expiry, read settlement and `.65` idle/background facts.
- Mac signed-agent launch without a window/Dock icon; Linux D-Bus absence reports unknown; old Qt versus switch Next delegation hellos; passive arch installs/upgrades before the marker.
- Closed Next plus banner/menu/badge activation starts once, waits for readiness and opens the exact session through its instance channel. Application-focus leases suppress globally, not per session.
- Two frontends, zero windows, repeated reconnects and a slow subscriber: one reader per computer, one poster, bounded queues, gap/snapshot recovery and no duplicate banners.
- Kill/fail injection before and after every reservation, checkpoint, native send and receipt; lost replies, disk-full and newer-schema cases retain unknown outcomes without replay. Without new consent, imports/restarts cannot enable approvals.
- Qt/Next/service handoff, explicit revocation and installer rollback interrupted at every marker; unchanged source bytes, identity-guarded effects, preserved tokens and no historical alert replay. Uncooperative legacy Qt must refuse initial cutover; abnormal manual Qt startup revokes the service, with only the acknowledged cosmetic native-post/badge overlap.
- Fake peer outages, fair read retries and the resource workload above; no parsing or durable I/O on a UI thread. Prove failure isolation with recovery/voice workers active.
- Native Linux/macOS/Windows startup, idle/lock, posting/withdrawal/click/badge, service upgrade and running-agent survival. Windows runtime acceptance stays blocked until the owner's PC is reachable; cross-target Clippy alone does not prove it.

Finalize this document first. After the Mac switch, assign implementation owners by available budget; no G1 code is started here.
