# Remote build delivery and test execution plan

Extend the WhiteWh fork's existing Teams build broker and test queue so that a powerful computer can build a project and a small executor can test the resulting artifact on another computer or a connected console. The main delivery case is an approximately 400 GB installed build containing large pak files, over a slow or unreliable connection.

The selected implementation is desync's existing reconstruction engine with a source adapter that reads chunks directly from published binary containers, regardless of extension. The owner selected desync as the sole backend. The earlier candidate comparison below remains reference material; implementation and qualification use desync. Its BSD-3-Clause notices and the actual linked dependency notices accompany the resulting binaries. Zerus original code retains MIT.

The Windows adapter now implements full-tree generations, paired private-address TLS, a durable headless executor and a builder controller. The fork routes explicit artifact tests through its existing run and test brokers and exposes bounded delivery state in Builds. The standalone package and setup contract are described in [the executor README](../../../services/artifact-executor/README.md). Source and synthetic qualification harness remain in [the desync implementation](../../../scripts/experiments/desync-artifact/README.md). Content inputs and measured reports remain outside the repository. WhiteWh fork releases ship the executor beside `hgs.exe` and as a standalone package (docs/fork-updates.md); it is not a full-size or hardware-qualified release.

## Current implementation boundary

The transfer/control/scenario chain is implemented on Windows. It keeps one
payload tree per side, serializes files, preserves native destination custody,
repairs interrupted or corrupt owned data, and stages identical renames by
native moves. Publishers retain source leases until deployment; the broker
releases its build queues before waiting for the scenario. Remote jobs pin
artifact, script, executor and device identities and never replay an admitted
script after an uncertain result. Explicit observation and hash-verified
collection/acknowledgement are available through the controller and paired API.

The scenario adapter can call a locally installed console SDK, but no vendor
implementation or physical console qualification is bundled. Full-size builds,
slow physical networks, weak hosts and physical power-loss tests remain
qualification gates. Current tree/index/resource limits and all setup details
are in the executor README. The stage descriptions below record the design and
its earlier qualification steps; their historical prototype limitations are
superseded only by this explicit implementation boundary and current source.

Executor-host performance admission now measures local CPU, volume capacity and
physical disk activity through a bounded native sampler, records the exact
policy and measurement validity, and reports native execution duration through
the test broker. Console performance still requires SDK-side load evidence.

Paired transport clients also bound silent reads independently of transfer
duration: a local `peer.read_idle_seconds` setting selects 1..300 seconds,
with 30 seconds as the default. The executor applies its setting when receiving
both input trees; the controller applies its own setting to control/result
reads. Progress resets the interval, while existing connection, handshake and
overall request limits remain in force. A transport timeout leaves delivery
incomplete and grants no scenario execution or replay authority. Synthetic
loopback qualification covers fragmented/rate-limited delivery, interrupted
index/chunk responses, native inactivity expiry, repair and refusal to run an
incomplete tree. It does not qualify physical network or hardware performance.

## Requirements

- Run no model or agent conversation on the test computer. An executor receives concrete jobs, transfers data, runs versioned scripts and reports evidence.
- Keep one complete build on the publisher and one on the executor. Neither historical builds, a second complete output tree, a duplicate compressed archive nor a complete chunk store is mandatory.
- Reuse matching ranges inside large pak files, including ranges whose offsets changed. Unchanged files remain in place.
- Restore the exact published file bytes. A game-engine patch overlay is an optional separate deployment mode, not automatically equivalent to a freshly packaged build.
- Account separately for transfer bytes, publisher reads, executor reads and writes, peak RAM, metadata and temporary storage.
- Repair interrupted updates toward a pinned target artifact. A partially updated tree cannot run tests. In-place delivery does not guarantee rollback to the old build.
- Enforce bounded working memory, request sizes, concurrent transfers, journal reads and output capture. Index size can scale with artifact size on disk; it must not force an unbounded RAM map.
- Preserve current local broker behavior and the distinction between member working-copy checks and integration queue checks.
- Initial provisioning still requires obtaining all missing data, through the network or a preloaded disk.

No fixed allowance of several spare gigabytes is assumed. Qualification must establish the smallest practical service footprint. Physical growth of the final build is separate from temporary space and cannot be eliminated by a delta algorithm.

## Existing fork integration points

The fork guide explains snapshot/shared build execution and the test queue in [the Teams help](../../../client/src/ui/teams/help/team-guide.ru.md). CodeLore records `build-broker-members-sharing-one-folder` and `affected-tests-test-map-selection-and` preserve the related contracts; their historical details must be checked against current source.

| Existing component | Current responsibility | Planned addition |
| --- | --- | --- |
| `src/state/teams/broker.rs` | Shared-folder build locking, snapshots, readiness and result reuse | Publish a completed output generation and hold source leases during delivery |
| `src/state/teams/dispatcher.rs` | Admit and execute typed member runs; operate the test queue | Dispatch artifact and executor jobs while retaining the original task and attempt identity |
| `src/state/teams/test_queue.rs` | Scheduling, resource classes, bounded result records | Explicit executor/device selection and delivery prerequisites |
| `src/state/teams/test_runners.rs` | Platform-based remote execution from an exact source commit | Keep the existing path; introduce a distinct prebuilt-artifact capability |
| `src/state/teams/test_results.rs` | Input identity, reuse, flaky and budget policy | Include artifact, scenario and relevant device/environment identity |
| `client/core/src/teams/read.rs` and `view.rs` | Typed Tests/Builds projections | Paged delivery and executor projections |
| `client/src/ui/teams/plan-panels.tsx` | Existing Tests and Builds displays | Show delivery stages, target hardware, throughput and failures |

The current queue partitions requests by whether their platform runs locally. Explicit hardware selection must take precedence for a new artifact-backed request, even if the local computer has the same operating system. Do not silently redirect a low-spec measurement to a faster host.

Some repository documentation describes a different Teams implementation where Tests/Builds are advisory metadata. This plan targets the executable fork mechanisms above; it does not silently enable execution in that separate contract.

## Ready made components

These are source-level findings, not measurements on the project's pak files. Qualify an exact revision and its dependencies; README claims and another project's benchmarks do not establish 400 GB performance.

### desync

[desync](https://github.com/folbricht/desync) provides content-defined chunk indexes, local seeds and remote chunk retrieval. Its current reconstruction code recognizes a seed that is the output file, orders overlapping moves and handles dependency cycles. A bounded stash can fall back to fetching a chunk from the store. The extracted file can be kept after an interrupted operation. See the [extract interface](https://github.com/folbricht/desync/blob/717d63b1756554abc470671ea591ad6d25e6059d/docs/cli/desync_extract.md), [planner](https://github.com/folbricht/desync/blob/717d63b1756554abc470671ea591ad6d25e6059d/assemble-plan.go), [in-place copy](https://github.com/folbricht/desync/blob/717d63b1756554abc470671ea591ad6d25e6059d/assemble-inplacecopy.go) and [stash](https://github.com/folbricht/desync/blob/717d63b1756554abc470671ea591ad6d25e6059d/assemble-stash.go).

Ordinary publication creates a chunk store. Zerus needs an adapter mapping chunk identity to offsets in its one current output, with bounded optional caching. Recovery, metadata memory and durable readiness remain qualification work. A Go helper is acceptable for the first experiment; it requires no Go toolchain on the executor. License: BSD-3-Clause. Source reference: `717d63b1756554abc470671ea591ad6d25e6059d`.

### bita and bitar

[bita](https://github.com/oll3/bita) and its Rust library [bitar](https://github.com/oll3/bita/tree/main/bitar) support HTTP range delivery, content-defined chunks and output-as-seed reconstruction. Standard publication creates an additional archive, which violates our publisher constraint unless replaced with a direct-range or virtual-archive adapter. The inspected [reordering implementation](https://github.com/oll3/bita/blob/f3686bc424f9ef078c9c6e82f07bdfb2a8317d8e/bitar/src/clone_output.rs) keeps displaced chunks in a memory map without an explicit cap in that function. It needs a bounded strategy and crash-repair qualification. The inspected CLI CI covers Linux/macOS; Windows must be qualified explicitly. License: MIT. Source reference: `f3686bc424f9ef078c9c6e82f07bdfb2a8317d8e`.

### hsynz and libhsync

[hsynz](https://github.com/sisong/hsynz) can publish sync metadata for the current file and fetch missing ranges from the original, without a second compressed publication. Its `hsign_diff` also permits a new file plus an old signature instead of both versions. This is attractive for retaining work on the powerful publisher. However, the documented `hsync_http` interface explicitly rejects identical input and output paths. It is a comparison candidate for matching/transfer, not a complete low-disk installer. License: MIT. Source reference: `fc977068385e7375ab34be1bf99708897d50df34`.

### rsync and librsync

[rsync](https://download.samba.org/pub/rsync/rsync.1) already supports distributed delta comparison and `--inplace`, making it a useful functional and performance baseline. Its manual explicitly allows inconsistent output after interruption and notes that overwriting basis data can reduce transfer efficiency. An unmodified invocation is not our transaction/recovery protocol. Windows packaging and process integration also need review.

[librsync](https://github.com/librsync/librsync) provides signature/delta/patch primitives without requiring both source versions on the publisher, but supplies neither a network protocol nor directory lifecycle. Treat it as a matching alternative; do not assume its patch operation safely aliases its basis file. License metadata identifies LGPL-2.1; verify exact distribution terms if selected.

### itch butler and wharf

[butler's offline commands](https://itch.io/docs/butler/offline.html) can generate a block patch from an old signature and the new build and resume applying through a staging directory. Its [apply specification](https://itch.io/docs/wharf/algorithms/apply.html) reconstructs changed files in staging for safe commitment. Thus the name "in place" does not establish bounded extra space for one huge changed pak. Useful as a reference for directory and checkpoint handling, not the first candidate under our storage constraint.

### Pairwise binary patchers

Conventional HDiffPatch, xdelta and bsdiff workflows normally use old and new data to generate a patch. They are optional experiments when both happen to be available, not a baseline requiring old builds to be retained. HDiffPatch's documentation specifically redirects the no-old-version case to hsynz. Do not extrapolate its separate embedded in-place patch mode to a 400 GB directory update.

### Selection

Use desync as the sole backend. Its inspected code addresses rearranging a file under a bounded displacement buffer, and the experimental adapter exposes an explicit stash cap. Continue qualifying total memory, storage and recovery on increasing data sizes. The other tools above document alternatives already considered and are not planned implementation paths.

## Publication without a second build

A completed build is published as a generation. An artifact manifest identifies the source snapshot including uncommitted changes, target/configuration, relative paths, lengths, final file hashes and the chunk index format. Separate artifact identity from a mutable local path.

Build an index that maps each required chunk hash to a current pak and byte range. Serve those ranges directly. Coalesce adjacent reads and network requests. Avoid mandatory creation of `.cba`, `.hsynz`, tar/zip or `.cacnk` payload copies beside the current build. A bounded cache of recently requested/compressed data is optional.

Use a standard cryptographic digest for final artifact verification and qualify the selected backend's chunk identity and truncation settings. A fast matching checksum is not the final authority for file identity. Preserve pak bytes, signatures and platform metadata exactly; transport compression is independent and should be disabled when it wastes CPU on already compressed data.

The publisher must keep a served generation stable. Hold verified read leases on its output files and coordinate those leases with the build broker. Open file handles alone do not make in-place writes immutable on every operating system. Source deletion, truncation, replacement or modification invalidates publication; never silently serve the new bytes under the old artifact identity.

With one physical copy, slow delivery can postpone a subsequent build that would overwrite leased outputs. There is no simultaneous promise of unchanged old bytes, unrestricted overwrites and zero extra storage. Start with explicit broker waiting. Release source leases after delivery and full target verification; testing itself does not need to retain the source lease. Indexing and delivery must not hold the team store's global writer lock.

## Delivery protocol

Keep control messages separate from large binary transfers. Reuse Zerus machine identity and existing connection routing where possible. Use authenticated HTTP(S) range or chunk requests for data, or an existing hgs-owned tunnel when direct connectivity is unavailable. Never pipe Windows OpenSSH directly or carry pak bytes in JSON/base64 messages.

Each delivery binds `delivery_id`, immutable `artifact_id`, exact executor identity, managed destination identity and manifest digest. The executor receives a bounded manifest page or one file index at a time, reports its installed artifact and capabilities, and prepares a plan before any destructive write.

The plan accounts for unchanged files, chunks already at the correct offset, movable local chunks, required downloads, displacement dependencies, file growth/shrinkage and obsolete paths. Benchmark content-defined and rolling-signature matching rather than committing to one boundary scheme before measurement.

Support a preloaded first build, an older-than-previous version, partial output after interruption and an empty destination. Same-named files, timestamps or a remembered artifact label do not prove correct contents. Independently verify reused ranges as required by the selected backend and final verification policy.

Transport retries may retrieve immutable bytes again. They do not grant permission to repeat a test, SDK install or other external effect. An unsupported peer reports an unavailable capability; it does not reinterpret the new job as an old source-runner request.

## Applying updates with one installed copy

Take exclusive custody of the managed deployment slot and wait until its prior test or deployment has physically settled. A locked file or a known user of the tree holds delivery with an actionable reason. Do not obtain custody by terminating unrelated programs.

Mark the slot unavailable before the first write. Preserve unchanged files and correct-offset chunks. Read needed old data before an overlapping write. Break cyclic moves with a bounded buffer; when a source can no longer be preserved, retrieve its required final data from the publisher. Calculate that additional traffic separately from the ideal missing-chunk volume.

Persist the desired artifact and update phase before writes. Keep durable readiness separate from progress counters. After a crash, regard the tree as mixed data, verify its actual ranges and build a fresh repair plan toward the same target. Never replay an old COPY instruction against offsets that may have changed. A compact receipt journal assists recovery, but correctness must not depend on volatile displacement buffers surviving.

Group target flushes and durable progress checkpoints without reporting bytes as durable before the file writes have settled. Handle torn writes by verification and refetch. Prefer simple sequential per-pak execution initially; defer cross-file source reuse until its dependency and memory costs are justified.

Do not shrink a file while its trailing bytes remain a source. Remove obsolete managed build files only when no pending operation needs them, and never remove files outside the manifest-owned deployment tree. Treat path aliases, links, case collisions and destination replacement as native file-identity concerns.

Once every required file has its final verified content and metadata, durably publish the slot's new artifact identity and readiness. This is a logical availability transition, not an atomic filesystem replacement of all 400 GB. Cancel during in-place writes leaves an unavailable repairable slot; it does not promise rollback. A test holds the slot against another update.

The executor must have space for final size growth and bounded metadata/buffers. If the final build does not fit, refuse with the actual shortfall. No algorithm can store a larger final build on a physically full device.

## Resource accounting

Measure all index and planner costs. For a sizing example of 400 GiB, 64 KiB average chunks imply about 6.55 million entries; 1 MiB implies about 409,600. At an illustrative compact 48 bytes per entry, that is about 300 MiB versus 18.75 MiB before database, object, hash-map or dependency-graph overhead. These are arithmetic sizing examples, not measured library footprints.

Compare average chunk sizes of 64 KiB, 256 KiB, 1 MiB and 4 MiB. Larger chunks shrink metadata and requests but can increase retransmission around small changes. Process files or bounded windows and spill searchable metadata to an indexed local database where necessary. Neither desync's limited stash nor a streaming API alone proves the total planner fits a weak machine.

Measure pak layout effects with the project's actual packaging settings. Independent unchanged compressed blocks can remain reusable; a broad recompression or encryption change can destroy byte similarity. Reading pak bytes does not require decrypting game assets. Format-aware packaging changes are optional, separately evaluated improvements.

A full sequential 400 GB read at an assumed 100 MB/s already takes about 67 minutes; at 500 MB/s about 13 minutes, before writes, seeks or CPU. These estimates explain why disk I/O must be tracked alongside network bytes. Avoid repeated full scans of unaffected pak files, but retain actual content verification before declaring a ready artifact.

## Minimal executor

Build a small headless executable with durable job state, transfer backend, bounded log/result spool and native process supervision. Its capability response names supported protocol/backend revisions, operating system, device adapters and resource limits. It contains no model runtime, model credentials, desktop webview, team designer or source checkout requirement.

An executor job names an artifact, a versioned script bundle and parameters. Support prepare, deploy, run, collect and cleanup phases. Local script runtime or console SDK requirements are explicit capabilities. Scripts and their input files are prepared on the working computer. If a scenario needs a human observation, record that state rather than inventing a verdict.

Bind submission and status to one durable request ID. A repeated observation does not start the script again. A connection loss after admission produces a status lookup for the original run, not a new execution. Retryable read-only data fetching and policy-controlled test retries have separate identities. Keep logs and result files bounded/paged and upload large evidence through the same data transport with result manifests.

Model an attached console separately from its host and reserve its identity exclusively for deployment and testing. The host-to-console installation path is a separate adapter: saving traffic to the host does not prove that the SDK can avoid a full transfer to the console.

## Teams behavior

Extend existing Builds/Tests views with the artifact, builder, executor, device, delivery phase, bytes reused/downloaded, source wait reason and verified result. Show indexing, planning, transferring, applying, verifying and ready as job details, without introducing unapproved changes to session-state semantics.

An artifact-backed test requires a ready matching artifact on its reserved slot. Include scenario digest, parameters, artifact identity and relevant device/software identity in result reuse. Performance runs should normally measure anew and must not share the measured device with transfer/apply work. Return infrastructure/setup failures separately from assertion failures and user observations.

## Implementation sequence

### Stage 1 Measure the data and qualify desync

Inspect neighboring real pak versions on separate computers, retaining only one version per computer. Include no-change, code-only, one changed asset, package insertion/reordering, packaging-option changes and a materially changed build. Temporary signatures/indexes may be retained; another full build is not required by the test procedure.

Run the pinned desync engine and raw-container adapter. Exercise the package-boundary and in-place paths rather than accepting a favorable small-file result. Use modest synthetic files first, then one large real container, then the full approximately 400 GB deployment. Use quota-limited real filesystems for storage claims; sparse zero-filled data is not network or allocation evidence for compressed game data.

The standalone experiment includes a separate memory-scaling driver. It tests
larger read-only content ranges and feeds synthetic whole-container metadata
into desync's actual planner without creating a corresponding large payload.
Windows Job Objects enforce committed-memory limits before a startup gate
admits configuration/input reads; per-phase deadlines bound time as well.
Allocation and deadline refusals are failed admission configurations. Synthetic
metadata probes do not qualify payload correctness, full-build performance or
the intended weak hardware. All measurements and input metadata stay in local
private reports outside the repository and trackers.

Production admission must budget actual per-file chunk counts, all simultaneously
live lookup tables and planner structures, concurrency, and non-heap overhead.
The displacement stash and Go GC target alone cannot admit a job. Process files
serially within a deployment budget by default. If the largest individual file
does not fit, qualify a bounded extension to desync before admitting it; arbitrary
independent window writes must not bypass overlap safety or whole-file verification.

Report payload bytes plus metadata, peak resident memory, peak allocated temporary disk, source/target read and write bytes, seeks where measurable, CPU, elapsed time, binary/runtime footprint and restart amplification. Run shaped 20/100 Mbit/s links with latency and disconnects, and separate HDD/SSD profiles when available. Cross-check exact final hashes.

Exit criterion: qualify the desync integration for one-copy storage on Windows and representative weak hardware. If the current index or graph does not fit, bound that part of the desync integration and repeat the affected qualification. Do not silently switch backends or use two-copy staging.

### Stage 2 Define identities and durable lifecycle

Implement shared manifest/job types, version negotiation, artifact/destination identity, source leases, indexed receipts and lifecycle transitions. Review source-writer exclusion and post-crash recovery before integrating remote execution.

The standalone Windows prototype now exercises typed, versioned single-file
manifests and bounded replacement receipts. It binds artifact/index contents,
request and native destination identities, serializes slot custody, excludes
source writers during each invocation and verifies readiness independently of
publisher availability. It reconciles repeated preparation without reseeding
mixed data. Abrupt-exit tests cover reconstruction and readiness boundaries;
Windows null-seed temporary files have process-owned delete-on-close lifetime.
See the experiment README for the exact contract and limits.

This remains a local qualification implementation. Its combined local manifest
is not a remote wire format. Authenticated executor identity, production shared
types, durable job admission, indexed multi-file receipts and generation leases
across broker operations remain integration requirements. In particular, no
script execution or exactly-once external-effect claim follows from a delivery
receipt. Production admission retains Stage 1's resource/hardware qualification
gate.

Exit criterion: source replacement, repeated submissions, expired connections and mixed target data cannot produce a false ready state or duplicate execution.

### Stage 3 Deliver one pak end to end

Serve chunks/ranges from one current raw pak without a second archive/store. Integrate the qualified backend behind a narrow adapter. Implement direct/tunnel connectivity, batching, rate limits, repair and complete target verification.

The qualification helper now separates publisher and receiver into independent
processes. The receiver consumes a path-free, pinned artifact descriptor and
downloads a bounded index and missing chunks over authenticated loopback. The
publisher holds the source lease for its full server lifetime. An offer can
rotate connection credentials after restart while keeping the artifact and
delivery binding unchanged; changing the target of mixed data is refused.
Both modes use one reconstruction/readiness adapter. Synthetic native tests
exercise process interruption, corruption, preloaded/empty receivers, offline
verification and graceful server expiry under separate hard process budgets.
This does not qualify LAN/WAN connectivity, authenticated peer identity, tunnel
routing, request batching or full-build/weak-hardware operation.

Exit criterion: update an existing large pak on a constrained disk, interrupt at several stages, recover without stale COPY replay, and prove exact output bytes and measured allocation limits.

### Stage 4 Deliver complete build trees

Add per-file manifests, additions, removals, renames, metadata, multiple pak files and whole-build readiness. Coordinate subsequent builds through output leases and independent destinations through deployment slots.

Exit criterion: the full build transfers with one complete source and destination copy, no hidden full payload cache, correct source waiting and correct behavior for a skipped version.

### Stage 5 Add the executor and script scenarios

Package the headless runner, identity binding, process lifecycle, result spool and scenario bundles. Begin with a desktop script and add a console-host adapter once its SDK contract is available. No model is installed on the target.

Exit criterion: a job survives loss of the controlling connection, yields one execution receipt and returns logs, metrics and a result for the exact artifact and script version.

### Stage 6 Integrate the fork brokers and UI

Connect artifact publication to the build broker and ready-slot prerequisites to the test queue. Preserve existing member-check/integration-check semantics and local defaults. Add typed client projections and user-visible diagnostics, documentation and narrow compatibility tests.

Exit criterion: a Teams request builds on one computer, incrementally delivers to a selected low-spec host, executes the scenario and shows evidence in the original task/attempt without creating an agent session on the executor.

## Qualification cases

| Case | Required result |
| --- | --- |
| No-op update | No payload download or pak rewrite; bounded metadata/validation work is reported |
| Small edit inside a huge pak | Matching content is reused; actual transfer and disk costs are shown |
| Shifted or cyclically moved ranges | Exact reconstruction without unbounded buffer growth |
| Recompressed or encrypted layout change | Correct result with honest traffic estimates; no promised savings |
| Empty or preloaded executor | Complete provisioning or adoption after content verification |
| Older or corrupted local build | Verified reuse and repair toward the requested artifact |
| Disconnect or helper crash during a move | Mixed output stays unavailable; repair uses actual data |
| OS restart or torn write | No stale readiness/checkpoint trust; exact output after repair |
| ENOSPC and final build growth | Bounded failure, accurate shortfall and a repairable slot |
| Source changed during indexing/delivery | Invalidate or hold the generation; never mix source identities |
| Next build requested during delivery | Explicit output-lease wait, without locking unrelated team actions |
| Repeated run request or lost start reply | Reconcile the same request; never launch another script implicitly |
| Two tests targeting one console | Device reservation admits one at a time |
| Target-root replacement or unsafe manifest path | Refuse access outside the original managed destination |
| Unknown/older protocol | Clear capability refusal while legacy local behavior remains usable |

Run focused synthetic and native filesystem tests for the new mechanism. Do not run the repository's full regression suite for a documentation proposal. Production adoption requires real pak measurements and Windows execution; source inspection is sufficient only for this plan and shortlist.

## Decisions still requiring measurement

Desync is selected. Average chunk size, actual minimum RAM and service disk footprint still require qualification. Examine actual container compression/encryption settings, executor filesystems, source output paths and console deployment behavior. Retaining a prior complete build, allocating another 400 GB archive or making reflinks mandatory is not an acceptable way to pass qualification.
