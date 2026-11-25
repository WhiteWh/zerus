# Zerus artifact executor (Windows x86_64)

`hgs-executor.exe` receives a verified build and a versioned scenario bundle,
runs an explicit PowerShell or native entry point, and returns logs, observations
and hashed evidence. It runs without Zerus's GUI, a model, a conversation, a
source checkout, Go, Python or Node on the test host. Windows PowerShell or the
scenario's native runtime must already be available. Console runs use a small
Windows host with the vendor SDK installed; no executor binary is installed on
the console itself.

The source and synthetic qualification harness live in
`scripts/experiments/desync-artifact/`. The fork's CLI integration is in
`src/state/teams/artifacts.rs`. It is not a physically qualified console
release.

## Released layout

WhiteWh fork releases (`whitewh-<build>`, docs/fork-updates.md) ship it twice,
built by `scripts/build-artifact-executor.py` from pinned Go 1.27.4 and desync
`717d63b1756554abc470671ea591ad6d25e6059d`:

- inside `zerus-windows-x86_64.zip`, as `hgs-executor.exe` beside `hgs.exe`,
  with this README and the examples under `executor\` and its linked-license
  notices under `licenses\executor\`. `build-info.json` names its
  `artifact_executor` binary and source SHA-256 and desync revision;
- as the standalone `hgs-executor-windows-x86_64.zip` release asset, for a test
  host that needs no desktop.

A builder profile may omit `helper`: `hgs` then uses the `hgs-executor.exe`
installed beside itself. An explicit absolute `helper` still selects another
installed copy. Neither package contains identities, state or payloads.

## Delivery and disk use

The only backend is pinned desync. The publisher indexes original files and
serves requested ranges directly; it creates neither an archive nor a complete
chunk store. The receiver changes owned files in place. File extensions have no
meaning to this protocol: pak, ucas, DLL and other containers are opaque bytes.
Identical renamed files move on the same volume, with no payload copy. Added,
changed, removed and file/directory transitions belong to one durable target.

There is one payload tree on each side. Small indexes, receipts and bounded
scratch data are additional. Growth of the final build still needs real disk
space. Interrupted updates repair toward the pinned target; they do not provide
rollback to an older build. Ready requires the entire expected file set, exact
hashes and supported Windows attributes/last-write times. Unknown extra files
or replaced native objects are preserved and block admission.

Current admission limits: 4096 files, 4 MiB tree metadata, 500000 chunks and
32 MiB index per file. A 4 MiB average chunk is the example for very large
containers; memory and chunk-count limits are checked before writes. A build
outside these bounds is refused. The approximately 400 GB use case still needs
a complete hardware qualification; metadata-scale tests do not establish its
wall time or disk/memory peak.

## Pairing and starting

1. Extract the package on both Windows computers. Use a dedicated local state
   directory on a fixed disk. Keep state, source indexes and evidence outside
   source repositories and external sync folders.
2. On each computer, create a new private identity by placing `{}` in
   `identity-init.json` in its configuration directory and running:

   ```powershell
   .\hgs-executor.exe identity-init C:\ZerusExecutor\identity-init.json
   ```

   This creates sibling `identity/`. `identity/public.json` holds its public
   certificate fingerprint. Pair the public fingerprints through your existing
   trusted channel. Keep `identity.key` on its originating computer. Existing
   identities are never silently replaced. Certificates currently expire after
   one year; renew and pair explicitly.
3. Create the executor's `state/` directory. Copy `examples/executor.json`, set
   its local paths, builder fingerprint, private IPv4 listener and resource
   budgets. Remove the example console device if unused. Start:

   ```powershell
   .\hgs-executor.exe executor-serve C:\ZerusExecutor\executor.json
   ```

   One service owns this state root. The example runs up to seven days, then
   drains the active scenario while keeping observation/result requests alive.
   A host supervisor may restart it after exit; restart reconciles durable jobs
   and never reruns an admitted script automatically.
4. Configure the builder's local profile from `examples/profile.json` under
   Zerus's existing state root, `artifact-executors/profiles/stand.json`. Set the
   exact executor fingerprint, endpoint and builder identity. The example omits
   `helper`, so a fork installation uses its own `hgs-executor.exe`. The executor must
   be able to reach both builder publication listeners. With port `0`, Windows
   assigns private ephemeral ports; configure the local firewall for this
   executable and paired private hosts. No firewall rule or public service is
   installed automatically.

The protocol uses mutually pinned TLS 1.3, literal private IPv4 addresses,
bounded authenticated requests, no proxies, DNS, redirects or public CA lookup.
The unauthenticated-to-TLS loopback HTTP mode exists only in lower-level local
qualification commands. `controller-*` and the service require pairing.
A receiver's per-file `result.json` names the transport its bytes used, derived
from the validated offer: `paired-tls-private-network` (`external_network`
true) for a private IPv4 peer, `paired-tls-loopback` for 127.0.0.0/8 and
`authenticated-loopback-only` for the local plain qualification modes.

Paired clients fail a blocked read after 30 seconds without incoming bytes.
Set `peer.read_idle_seconds` locally to 1..300 seconds; omitted or `0` uses
30 seconds. The executor setting applies to its downloads of both the build
and scenario bundle, including separately approved data peers. The builder
profile setting applies to its control and result reads from the executor.
Neither setting is sent in publication offers or selected by the sender.
Listeners validate the value but do not apply a server-side inactivity timer.
The lower-level plain loopback mode refuses a nonzero setting.

Each TCP read with data starts a new interval, so a progressing transfer may
last longer than this bound. The existing five-second connection/TLS handshake
limits and 15-minute overall client request limit also apply. A timeout leaves
delivery incomplete; it never authorizes a scenario launch or automatic replay.
Reconnect through the existing explicit publication repair flow.

Windows metadata replacement can temporarily be refused while an observer
holds the old record open. The adapter waits at most two seconds for an exactly
bound metadata commit, with its source pinned against writers and both native
name bindings rechecked after refusal. Changed or unknown bindings stop the
commit. This mechanism never retries a scenario launch or a payload operation.

## Teams and test broker

The profile selects `source: snapshot` for the member's existing build-broker
snapshot, or `source: shared` for an output below the shared folder.
`artifact_relative` names that output; it is not copied. Build first with the
existing build template. Then use the normal guarded member run:

```json
{"team":"TEAM","attempt":"ATTEMPT","request_id":"NEW-LOWERCASE-UUID","template":"artifact-test","args":{"profile":"stand","job":"unique-build-test-job"}}
```

Submit this JSON to `hgs team run --json`. `hgs run` can also use the template
with a shared-output profile. Validation creates no directories or payloads.
The dispatcher locks the relevant build queues until both input trees are
verified on the executor. It releases those queues before waiting for the test.
The session retains the existing Building status for its active run. Teams >
Builds displays the bounded last twenty delivery pointers, selected device and
phase, including waiting for observation and Unknown.

A queued `.zerus/tests.toml` provider may use
`command = "zerus-artifact:stand"`. Its selected profile takes precedence over
OS-based source-runner routing; it is never passed to a shell or silently run
on the builder. Such providers use `source: shared`, run the profile's complete
scenario, have no local-result reuse and no automatic test retries. Parameters
are pinned in the profile, so use distinct profiles for distinct cases. Remote
host performance providers use the executor-side admission described below;
builder CPU samples are not remote evidence.

Every new intended execution needs a new job label. Reconnect with the same
label to inspect, finish delivery or collect an existing job. Reusing a job
with different inputs is refused. A controller timeout ends only its wait; the
remote test continues. Never interpret that timeout as a failed assertion.

## Executor-host performance admission

Queued providers marked `performance` use the local executor's load evidence.
The controller checks the `host-performance-v1` capability before publication.
This admission currently supports `device_id: host`. A console requires its
own SDK load adapter: CPU and disk activity of the small Windows host do not
qualify the console's load.

The controller profile may declare this policy:

```json
"performance": {
  "cpu_percent": 70,
  "disk_percent": 90,
  "disk_busy_percent": 20,
  "wait_seconds": 600,
  "on_busy": "run_loaded"
}
```

`cpu_percent` limits overall host CPU activity. `disk_percent` retains the
existing meaning of used capacity, taking the largest percentage across the
executor, artifact and scenario volumes under the executor account's quota.
`disk_busy_percent` separately limits the busiest local physical disk. All
three percentages must be explicitly supplied in a profile policy. Without
one, the queued route uses the project's CPU/capacity/wait settings and a
20 percent physical-disk activity limit. The service runs one scenario at a
time; builder-side build counts do not establish executor load.

After both input trees are verified, the executor waits for two consecutive
one-second quiet samples. Additional waiting is bounded by `wait_seconds` and
the executor's local overall scenario budget. The receipt records the effective
wait bound. `on_busy: not_run` refuses execution after that bound; `run_loaded`
explicitly permits execution with a loaded flag. Missing counters and invalid
samples are never interpreted as idle or as permission to run loaded.

The native sampler uses local language-neutral PDH counters, with at most
64 physical disk instances and 64 KiB counter data, plus fixed local volume
capacity reads. It starts no monitoring shell, WMI process, agent or model.
See Microsoft's [English counter API](https://learn.microsoft.com/en-us/windows/win32/api/pdh/nf-pdh-pdhaddenglishcounterw)
and [rate-counter sampling contract](https://learn.microsoft.com/en-us/windows/win32/perfctrs/collecting-performance-data).

The final hashed receipt includes before/after samples, sampled running peaks,
sample counts, validity, scope, limits and native execution duration. A short
scenario with no complete running interval has no running sample. Missing data
stays unavailable. The controller keeps this summary after verified collection;
the test record and Tests view carry executor load and label capacity and
activity separately. Performance duration excludes artifact delivery, the quiet
wait and result collection. A counter failure during execution preserves the
test's one-time execution but marks its performance result unavailable.

Restart during `waiting_for_quiet` remains Unknown and does not launch the
scenario on resume. A known refusal before native creation produces a
collectable `not_run` receipt and can release its generated result spool.

## Scenario and console adapter contract

Start from `examples/scenario/`. `scenario.json` and its entry point must be
members of the verified scenario tree. Scripts receive a sanitized environment:

| Variable | Meaning |
| --- | --- |
| `ZERUS_ARTIFACT_ROOT` | Verified installed build, read as input |
| `ZERUS_SCENARIO_ROOT` | Verified versioned scripts |
| `ZERUS_RESULT_DIR` | Evidence to return |
| `ZERUS_PARAMETERS_FILE` | Pinned JSON parameters |
| `ZERUS_DEVICE_ID` | Exact locally configured device |
| `ZERUS_OBSERVATION_FILE` | Explicit human verdict, when guided |

The runtime current directory and HOME/TEMP/APPDATA point to the job's generated
runtime folder. Ambient model, SSH and Zerus credentials are not inherited.
Scripts are trusted programs under the configured Windows account, not a
security sandbox. The account and local firewall must provide any required
isolation from other data or services. The transport itself has no telemetry
exporter or cloud store.

For a console, configure a device ID and adapter in executor `devices`, set the
same `adapter` in the scenario, and select that device in the builder profile.
The versioned script invokes the locally installed vendor SDK by explicit path,
deploys the verified build, runs it, and collects evidence into RESULT_DIR.
Device custody prevents updating its build during that scenario. This generic
adapter contract has no vendor SDK implementation or hardware qualification
bundled; use an actual SDK-specific scenario before claiming console support.

For user instructions, write the concrete steps in `instructions`, set
`guided: true`, and have the script wait for/read OBSERVATION_FILE as needed.
`controller-status` writes `controller-status.json`; its remote result contains
the exact job and request hash. Supply an `observation` object in a sibling copy
of the controller config, then run `controller-observe` on that copy:

```json
{"request_id":"32-LOWERCASE-HEX-DIGITS","job_id":"REMOTE-JOB-ID","request_sha256":"EXACT-REMOTE-REQUEST-HASH","verdict":"passed","notes":"The person's actual observation"}
```

Only `passed` or `failed` is accepted. No model invents a verdict. A script
exiting zero without its required observation is an infrastructure error.

## Results, recovery and bounds

The API is mutually authenticated under `/executor/v1`. It exposes capabilities,
bounded retained jobs, per-ID status, publication refresh, observation,
acknowledgement, admission release, log pages (64 KiB) and hashed evidence pages (4 MiB). There is no
arbitrary file-read or arbitrary shell endpoint. Payload publication credentials
may be refreshed only for the same held job and artifact identities.

The controller persists `controller-state.json` and the complete remote receipt
in its private job directory, downloads evidence under hashed local names,
verifies full evidence and log hashes, flushes them, and only then acknowledges
the result. ACK removes only exact unchanged generated objects on the executor,
retaining a compact deduplication receipt. A lost ACK reply can be reconciled.
Modified or unknown outputs are preserved for local review.
An infrastructure refusal proven to precede core execution admission can release
its queue capacity with a durable bound receipt; the job identity is retained.
Active or Unknown jobs cannot use that release operation.

At most six jobs with retained output are admitted. Logs retain 8 MiB while
continuing to drain excess output; evidence is capped at 128 files/64 MiB.
Runtime/output enumeration is bounded at 512 objects/80 MiB and sampled during
execution. These are admission/monitoring limits, **not an instantaneous disk
quota**. Native Job Objects enforce configured memory, child count and timeout,
and terminate only the executor's owned job tree. Root process exit alone does
not prove that its descendants finished.

Process creation starts suspended inside the native job. Durable creation-issued
and accepted PID/birth receipts precede execution. An unresolved creation window
is quarantined; an absent process name, timeout or restarted service does not
authorize replay. Unknown jobs and uncertain custody require local review. Do
not delete state or reuse their job IDs to bypass it.

## Build, license gate and qualification

Use `scripts/experiments/desync-artifact/build.py` with the pinned public desync
source, local Go executable and isolated work directory. It builds offline by
default, records the source identity, and collects notices from the **actual
linked binary**. New or changed license texts fail the reviewed-hash gate.
Package the resulting binary and notices with:

```powershell
python services/artifact-executor/package.py --binary PATH\artifact-sync-prototype.exe --notices PATH\licenses\desync-artifact --output PATH\hgs-executor-windows-x86_64.zip
```

The packager verifies binary/inventory/notice hashes, writes only a new archive,
and includes no local configuration, payload, certificate keys, toolchain or
runtime job data. Zerus remains MIT; desync's BSD-3-Clause and all linked
dependency notices remain in `licenses/`. No project relicensing is necessary.
Only the explicit reviewed example files are included; a local identity or
runtime file created beside an example is not traversed into the package.

Synthetic native tests cover tree generations, renames, corruption repair,
interruption boundaries, paired TLS, one-time execution, guided observation,
timeouts, restart reconciliation and verified collection. A physical weak host,
slow LAN, full-size installed build, physical power loss and actual console SDK
are separate qualification requirements. No private build data is needed in
source, task reports or public CI.
