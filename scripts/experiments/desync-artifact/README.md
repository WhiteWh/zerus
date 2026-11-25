# Desync artifact delivery and executor qualification

The implementation now also provides complete-tree generations, mutually paired
private-address TLS, durable model-free scenario jobs, a builder controller,
verified evidence collection and fork Teams integration. See the current
[standalone executor setup and limits](../../../services/artifact-executor/README.md).
The original range/planner qualification modes below remain local experiments;
their loopback-only descriptions apply to those modes, not to the paired tree
and executor commands. Real content reports remain private and local.

This standalone Windows prototype evaluates the actual desync reconstruction
engine against one receiver file, with combined-process and separate-process
qualification modes. The fork connects its explicit `artifact-test` template
and artifact-provider route to the controller. The publisher reads ranges from a current raw container
through an index instead of storing a second archive or a complete chunk store.

Private inputs, paths, chunk hashes, logs and measurements stay outside the
checkout. The qualification driver only permits results under the local
application data directory's Zerus/private-benchmarks folder.

## Data flow

The publisher's source is opened read-only. Synthetic overwrite, insertion,
reordering, shrink and full-change views are generated as bounded reads. They
are byte-level test cases, not valid recooked or signed game containers.

The receiver starts with a bounded copy of a source range. Publication produces
only an index and a manifest. Desync updates receiver.bin directly, using
matching local chunks and authenticated chunk requests for missing data.
Unchanged source bytes are fetched directly from the original container.

The listener binds only 127.0.0.1. Its client accepts only that exact ephemeral
address, disables proxies and redirects, and authenticates with a per-process
random token. Configurations cannot choose another data endpoint. No cloud
store, telemetry exporter, model or agent session is instantiated. Network
payloads in this experiment remain on the local host.

Native Windows source leases exclude existing and new writers and deletion
during each preparation/application invocation. Source identity, metadata and
chunk hashes are checked before serving data. Receiver identity is recorded at
preparation. Native custody prevents that receiver and its directory ancestors
from being renamed or replaced during application. Reparse points, hard-linked
files and network drives are refused.

Readiness is written only after flushing and verifying the actual receiver.
On interruption, the next apply invocation reindexes its actual mixed contents.
It does not replay a previous sequence of COPY operations. The immutable target
manifest and publisher remain available during repair. Rollback to the old
container and recovery without the target's publisher are not promised.

## Separate publisher and receiver processes

The serve, receive and receive-status commands exercise delivery without
opening the source on the receiver. They use the same reconstruction and
durable readiness implementation as the original combined-process experiment.
They still communicate exclusively through authenticated IPv4 loopback on this
computer. They do not enable LAN/WAN delivery, a public listener or production
remote jobs.

    artifact-sync-prototype.exe serve PRIVATE_PUBLISHER_CONFIG_JSON
    artifact-sync-prototype.exe receive PRIVATE_RECEIVER_CONFIG_JSON
    artifact-sync-prototype.exe receive-status PRIVATE_RECEIVER_CONFIG_JSON

All configuration paths must be absolute paths on fixed local drives. The
publisher config uses the existing source/range/scenario/average fields and an
explicit serve_seconds between 1 and 300. It indexes the virtual published
bytes, writes publication.caibx and offer.json beside its config and holds the
source read lease for its complete server lifetime. It serves raw chunk ranges;
it creates no duplicate payload archive or full chunk store. On its deadline it
closes connections and settles admitted handlers before releasing the source.
The lifetime is a qualification bound, not a production lease/daemon policy.

The receiver config accepts only offer, delivery_id, stash_bytes, memory_bytes,
optional adopt_receiver_identity and the experimental crash_phase. Its offer
contains protocol, typed artifact identity, chunk count, serialized index length,
loopback endpoint, random session identity and random bearer token. Publisher
paths and native source identities are absent. Unknown fields, including a
source path in the receiver config, are refused. Both processes currently run
under the same local account; this is a separation of responsibilities and data
requirements, not an operating-system sandbox.

Pass an explicit caller-generated delivery_id and retain it across retries.
Offer/session credentials are trusted local control inputs and must remain in
private controlled storage; never log, commit or share offer.json. The receiver
persists artifact/request/native destination bindings without storing source
paths or connection tokens in its manifest or receipt. receive-status verifies
the exact requested artifact and delivery without contacting the publisher.
Its temporary custody ends when the command returns, as with the original
status command.

An empty receiver can be provisioned directly. To reuse a preloaded receiver.bin,
the caller must explicitly supply its freshly inspected native file identity in
adopt_receiver_identity. A same-named unknown file is never silently adopted or
truncated. An interrupted first manifest/receipt publication is reconciled only
against its pinned native receiver and matching index; an orphan receiver before
manifest publication still needs explicit local review. A mismatched existing
index is refused rather than overwritten for a new target.

The transport exposes only /v1/index and /v1/chunks/<digest>. It does not accept
file paths or arbitrary range source names. Every request needs the token,
artifact and publication session; the client checks response identities, exact
index length/digest, index framing and coverage, chunk lengths/digests and final
receiver bytes. DNS names, alternate loopback hosts, IPv6, proxies, redirects,
userinfo and URL paths/queries supplied as endpoints are refused before dialing.
The only admitted endpoint is a literal http://127.0.0.1:<port>.

This wire revision bounds the index to 500,000 chunks and 32 MiB, response headers
to 4 KiB, each chunk to the selected average times four, receiver connections and
admitted server requests to two, and client requests to 30 seconds. It admits only the
pinned raw .caibx layout before invoking the general desync decoder. These are
experiment admission limits, not evidence that the planner fits those maxima
on weak hardware; hard Job Object and deadline qualification remains required.

After a publisher restart, the endpoint, token and session change. A replacement
offer may reconnect an existing delivery only if its artifact remains exactly
the same. A different artifact or delivery ID cannot retarget a mixed receiver.
Interruption of either process leaves the receiver unavailable until a fresh
repair completes against its actual contents. A complete local target can be
revalidated and reapplied with no payload or index download even when the
publisher is stopped. An unavailable publisher cannot repair missing bytes.
This revision retains one pinned target per slot, including after readiness;
advancing an installed slot to a new generation is a later explicit lifecycle
transition, not an implicit retargeting of the current delivery.

Receiver results count chunk response bytes read, verified bytes, chunk request
attempts and downloaded index bytes separately. Bad chunk bodies count as
received bytes, not reused content. These counters exclude HTTP/TCP overhead
and do not substitute for full network/device measurements. Separate-process
native tests use synthetic containers with arbitrary extensions and individual
hard memory caps. They cover preloaded/empty receivers, edits and moves, sender
and receiver exits, token rotation, corrupt transport, unknown versions and
destinations, offline readiness, no-op downloads and graceful expiry while a
request is rate limited. Real builds are not inputs to these tests.

## Delivery identities and durable lifecycle

The standalone helper now has typed version-1 manifests and receipts. These
are a local, single-file contract experiment, separate from production Zerus
protocols. Old experimental manifests are refused; there is no implicit upgrade
or adoption of an unknown receiver. The local manifest contains publisher
configuration and native paths and must never be used as a remote wire record.

| Identity | Binding |
| --- | --- |
| Artifact | SHA-256 of the ordered typed artifact fields: protocol, pinned backend, complete content hash, serialized index hash, byte length and average chunk size |
| Delivery | Random identity retained across retries of this prepared request |
| Destination | Random slot nonce plus recorded native directory and receiver file identities |
| Publication | Native source identity, size, timestamp and exact virtual target bytes |
| Receipt | Protocol, manifest digest, artifact, delivery, destination, native identities, expected hash and monotonic transition sequence |

The artifact identity excludes local paths and delivery/destination identities.
The executor field deliberately admits only windows-local-qualification; it is
not a substitute for a production authenticated computer identity. Unknown
protocol/backend values, changed indexes, copied destination trees and receipts
for another binding fail before receiver writes.

The state sequence is prepared -> applying -> verifying -> ready. Entering
applying is persisted before desync can grow or write the receiver. Reapplying
from any known state starts with a fresh index of the actual receiver; no saved
COPY plan is trusted. Flushing and complete byte/length verification under a
writer-excluding receiver handle precede the ready receipt. result.json is
diagnostic data and never independently grants readiness. A crash after writing
a successful result but before the ready receipt leaves an unavailable slot.

Only one cooperating operation can own slot.lock. The Windows kernel holds
that exclusive handle; a timeout, stale PID or disconnected observer cannot
steal custody from a live owner. Abrupt process exit releases the handle. Every
directory component is held without delete sharing while path-based I/O runs.
Control records have a fixed input size bound. Each record replaces one
validated single-link .next file using flush plus Windows write-through replace;
there is no append-only progress journal to load or retain without a bound.
This qualifies process-crash ordering, not physical power-loss durability on
every device/filesystem.

Repeating prepare validates and reconciles the same publication and request
without reseeding mixed receiver bytes or issuing new identities. A complete
manifest followed by a missing first receipt can be reconciled only after its
native receiver and index are checked. Incomplete initial preparation before
publication refuses further writes; its orphan files need explicit local review,
rather than automatic deletion or adoption. Truncated or foreign receipts fail
closed.

The status command accepts an absolute local configuration path:

    artifact-sync-prototype.exe status PRIVATE_CONFIG_JSON

Success requires a matching ready receipt and complete verification of the
current receiver under a writer-excluding handle. It needs no source file or
publisher connection. Detected later content corruption invalidates readiness.
The internal ready admission function returns the held receiver handle: a future
runner must retain it and slot custody through execution. The status command
releases them on return and is an observation, not a lasting execution permit.
This experiment does not launch scripts, admit production remote jobs or claim
duplicate-execution protection for a runner that has not been implemented.

Source leases currently last for an invocation, not across separate prepare
and apply processes. Apply therefore rehashes the complete published virtual
view before changing the receiver, detecting same-size edits with restored
timestamps as well as native replacement. This extra source scan is deliberate
qualification overhead. Production source generation leases and builder wait
reasons still need integration with the fork's broker; it must not infer source
immutability from size/date alone.

## Dependency and local patch

Use desync revision 717d63b1756554abc470671ea591ad6d25e6059d from its
[public source repository](https://github.com/folbricht/desync).
Desync is BSD-3-Clause licensed. This directory contains the adapter rather than
a vendored copy of the dependency.

The build script adds an optional StashLimitBytes pointer to the dependency's
AssembleOptions and takes the smaller of that cap and its existing
runtime-derived budget. The default upstream behavior remains available when
the option is unset. This exposes an existing internal planner capability
without replacing its move ordering, cycle handling or reconstruction engine.
The patch is applied only to the separately prepared dependency source.

The build also copies planner_probe.go.txt into that external source as
zerus_memory_probe.go. This MIT-licensed experiment instrumentation feeds
synthetic metadata to the existing internal planner. It does not replace the
planner or execute its steps, validate payloads, open source files or construct
a large sparse output. It supplies synthetic verified skip positions and
disables reflinks explicitly. These differences are recorded when interpreting
metadata stress results.

An additional Windows-only patch creates desync's existing null-seed scratch
file with FILE_FLAG_DELETE_ON_CLOSE. Abrupt-exit qualification exposed that the
upstream close/remove path could otherwise leave .tmp-block files behind.
The operating system now removes the owned temporary object when its last
handle closes, including process exit. No directory sweep or filename-based
deletion of old/unowned files is introduced. Other platforms keep the original
temporary-file factory. The normal close path accepts an already removed file.

The explicit displacement limit does not cap the complete index or graph.
The Go memory limit is soft, so qualification samples actual process RSS as
well as heap statistics. Large-build qualification must measure these
structures rather than infer a hard bound from the stash cap.

## Build

Prepare a verified official Windows Go archive, version 1.27.4, and the
pinned public desync source in a separate tools directory. The current
experiment uses a portable toolchain and isolated caches; it changes neither
the user's PATH nor the installed Zerus.

The following PowerShell example refers only to public tooling:

    $tools = Join-Path $env:LOCALAPPDATA 'Zerus/public-tools/artifact-sync'
    python scripts/experiments/desync-artifact/build.py --source "$tools/source/desync-717d63b1756554abc470671ea591ad6d25e6059d" --go "$tools/toolchain/go/bin/go.exe" --work "$tools/helper-build" --test

Builds are offline by default. If public module dependencies have not yet been
cached, add --fetch-public-dependencies only during this tooling phase.
That flag permits downloads through the public Go proxy during compilation;
unit tests still run with module fetching disabled. This build command takes
no private content configuration. Do not introduce private source/module
dependencies into that public bootstrap.

The build writes artifact-sync-prototype.exe and its public-only build log
beside the isolated build directory. It compiles the library and this adapter,
not desync's cloud-store CLI drivers.

The same command collects complete license documents under
licenses/desync-artifact beside the binary, with NOTICES.md and inventory.json.
It checks the binary's actual module list, module versions, the Go toolchain
version, root license documents and package-ancestor overrides against
reviewed-licenses.json. Unknown dependencies or changed/missing/additional
documents fail the build and require review. This step runs offline and accepts
no private build-content configuration. The inventory contains public module
names and relative notice paths, without local source/cache paths.

Zerus keeps its MIT license. Retain the original desync copyright, full
BSD-3-Clause text and disclaimer, including when redistributing modified source
or binary packages, and do not imply endorsement. The repository retains an
exact copy in docs/licenses/desync-LICENSE.txt. The collected runtime and
dependency notices must accompany any redistributed helper. Composite upstream
documents are retained in full, including conditional notices for unlinked
source scopes. Production packaging must include this collection before the
helper is bundled; current desktop installers do not ship this experiment.

License gate tests use synthetic fixtures:

    python -m unittest discover -s scripts/experiments/desync-artifact -p test_collect_notices.py

## Local qualification

Create a private selection JSON with a files array. Each selected entry names
an absolute local source path and its starting offset using the fields path
and start. Do not commit this file. The source must be on a fixed local drive.

Run the driver with the binary, private selection and a new private output
directory:

    python scripts/experiments/desync-artifact/qualify.py --selection PRIVATE_SELECTION_JSON --binary ARTIFACT_SYNC_PROTOTYPE_EXE --out NEW_DIRECTORY_UNDER_LOCAL_PRIVATE_BENCHMARKS

The driver assigns each owned helper to a Windows Job Object with process and
job committed-memory limits before opening its startup event gate. The helper
reads no configuration or input before that gate opens. Assignment failure
refuses the run. Closing the job kills its owned children; per-phase deadlines
also stop only that run's helper. This is a hard committed-memory bound, not a
working-set reservation. Actual peak RSS and committed memory are sampled
separately. The Go memory limit remains a soft GC target.
The Job Object does not bound the operating system's file cache or establish
the whole machine's required physical RAM. These runs do not emulate a slow CPU
or replace qualification on the intended low-spec hardware.

The driver executes unchanged, small-overwrite, insertion, reorder, shrink,
full-change, zero-stash, interrupted-transfer, abrupt-process-exit,
corruption-repair and shaped-loopback cases. It also attempts a larger
one-GiB range when the input and free space permit. These sizes are bounded
test parameters, not assumptions about production hardware capacity.

Reports include fetched payload, process RSS, sampled physical allocation,
index/application time, exact-hash verdict and the readiness state after
failure. Logs and numerical results remain private. The driver prints only
public scenario names and completion status.

Offline Go tests additionally use small synthetic files to check native source
writer exclusion, concurrent slot ownership, ancestor rename refusal, repeated
preparation, copied/changed identities, receipt/index tampering, restored source
timestamps, readiness without the source, later receiver corruption, failed
receipt persistence and recovery after abrupt process exits. Boundary crashes
cover before application, after reconstruction, after flush, before ready and
after ready, as well as termination during a chunk request. Scratch cleanup
after abrupt exit is checked against a fixed allowed-file set. These are process
crash fixtures rather than a simulation of OS restart or torn disk writes.

Only one receiver payload exists per case, and it is removed after verification.
Small manifests, indexes, configuration and logs remain for inspection. Both
endpoints currently share one process, so RSS includes publisher and receiver
metadata. Disk allocation polling can miss very short-lived changes. Receiver
allocation is queried through a handle that shares read, write and delete;
small receipt/index allocations are estimated from directory metadata to avoid
interfering with atomic receipt replacement. The assembly code path is also
checked to avoid a staging writer.

## Memory scaling

Run the larger qualification separately; it uses the same private selection
and local-only transport restrictions:

    python scripts/experiments/desync-artifact/qualify_scale.py --selection PRIVATE_SELECTION_JSON --binary ARTIFACT_SYNC_PROTOTYPE_EXE --out NEW_DIRECTORY_UNDER_LOCAL_PRIVATE_BENCHMARKS

Defaults use a 512 MiB hard committed-memory limit and a Go GC target of three
quarters of that bound, leaving headroom for non-heap allocations. A soft GC
target below live metadata size can cause sustained GC pressure; this is not a
way to force the planner to fit. Metadata cases have a 90-second deadline;
allocation and deadline refusals are reported separately.
The real-content portion compares 64 KiB, 256 KiB, 1 MiB and 4 MiB chunk-size
settings over ranges up to eight GiB, with smaller growth baselines. It creates
only one receiver range at a time, verifies the exact result, then removes that
owned payload. Insufficient input or free disk space is an explicit skipped
case. The original source remains open read-only.

The metadata portion models a 400 GiB single container using deterministic
unique chunk IDs, plus a smaller baseline. It exercises rotations, reversed
chunk order, changed IDs and no-op placement. Both old/new indexes, publisher
lookup, receiver seed, self-seed and actual dependency graph remain live while
measuring. It writes phase snapshots, not payloads; each case runs in its own
hard-limited process. A detected allocation refusal is a recorded resource
failure, not a successful reconstruction. Unexpected errors stop qualification.

Synthetic fixed-size chunk distributions do not predict a real recook's chunk
counts, traffic savings or runtime. Metadata probes bypass payload discovery
and validation and do not execute the graph. Their purpose is to expose growth
and refuse unsuitable configurations without allocating another complete build.
Real range tests separately verify data correctness. Full-build, separate-host,
power-loss and console qualification remain outstanding.

Successful scaling runs leave REPORT.md and results.json in their private run
directory, and DESYNC-SCALE-REPORT.md / DESYNC-SCALE-RESULTS.json in its parent.
Do not copy those files into the repository, shared tracking or external tools.

Native resource-gate tests use synthetic metadata and an explicit helper path:

    $env:ZERUS_DESYNC_TEST_BINARY = 'ABSOLUTE_PATH_TO_ISOLATED_HELPER'
    python -m unittest discover -s scripts/experiments/desync-artifact -p test_windows_limits.py

## Tests and interpretation

Synthetic native tests cover exact reconstruction, no-op traffic, zero-stash
fallback, failure/corruption repair, source-publication changes, source aliases,
receiver replacement, authentication and refusal of other connection addresses.
The driver separately exercises real container bytes without emitting their
names or hashes to the console.

This establishes whether the candidate can implement the intended mechanism.
It does not establish full 400 GB memory/performance, real recook delta size,
power-loss behavior, WAN throughput, multi-file build readiness or console SDK
deployment. Those remain the subsequent qualification and integration work
described in the remote build delivery plan.
