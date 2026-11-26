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

## Several people (in development, epic zerus-un7n)

Milestone 1 of `docs/superpowers/specs/2026-11-26-shared-host-personal-accounts-design.md`
adds an `access` section that lets one executor serve several people. It is
not ready for use yet: it requires an isolation backend that runs each
person's jobs under their own security principal, and this build provides
none. A configuration with `access` refuses to start, and refuses before the
executor writes anything, until such a backend exists.

```json
"access": {
  "policy": "C:\\ZerusOperator\\access-policy.json",
  "operator": "S-1-5-21-<domain>-<rid>",
  "isolation_backend": "<a qualified backend>",
  "per_person_jobs": 3
}
```

`policy` is the host operator's private policy file, in the shared contract
`contracts/host-access-v1` (also implemented by `src/host_access` in full
Zerus). It must be an absolute path outside the executor root and every shared
tree. It is read through one handle opened without following a link or
reparse point, while its ancestor folders are held open against renaming. A
link, a junction on the path, a second hard link or a final path other than
the configured one is refused. The policy must be on a fixed NTFS or ReFS
volume.

Multi-person mode starts only through its own service entry,
`executor-serve-shared <config>`. `executor-serve` refuses a configuration
with an `access` section, and `executor-serve-shared` refuses one without it,
so editing the configuration never moves a service between the modes. The
shared entry reads its configuration through one held handle (no link, one
hard link, within the control size bound, at exactly the configured final
path) and verifies that file and its folders against the fixed installer trust
(Administrators, SYSTEM and TrustedInstaller) before it uses any field; only
the bytes read through the checked handle are parsed. The configuration's
folder may receive the executor's own offer file, so it is held to the
higher-folder rule below.

`operator` is the SID of the named host operator. It is pinned in that
installer-protected configuration, never in the policy it authenticates, so a
configuration cannot make its own writer trusted by naming it. The operator
must resolve to an individual local or domain user account; groups, aliases,
well-known and capability SIDs and unresolved SIDs are refused. The executor's
own account may not be the operator (compared whatever the token's attributes:
a restricted token is still that account), Administrators, SYSTEM or a member
of them. The executor refuses to start, before it writes anything, rather than
adopt the account it runs under. Trusted principals for the policy are the
operator, Administrators, SYSTEM and TrustedInstaller. Through the held
handles, on every read:

- every object's owner must be trusted;
- the policy file grants no untrusted principal write, append, attribute,
  extended attribute, delete, permission or ownership rights;
- its folder grants no untrusted principal new entries, child deletion,
  deletion, permission or ownership rights;
- every higher folder grants no untrusted principal child deletion, deletion,
  permission or ownership rights (deleting a volume root is moot); adding
  unrelated entries beside the held chain replaces nothing;
- allow entries count even when a deny entry narrows them, entries of other
  kinds are refused, and a missing DACL is refused;
- finally Windows `AccessCheck` with the executor's own token must grant none
  of those rights, which covers inheritance, deny entries and owner rights.

A refusal names the object's role and the principal in the start log. A
missing, unreadable, invalid,
replaced (same revision, other content) or rolled-back policy refuses every
request. The highest accepted revision is kept in the executor root and in
memory, so a rollback stays refused after a restart. A lost or unreadable mark
on a root that already served several people refuses every policy until the
operator restores it. The mark, the mode record and the in-memory floor are
checked on every policy read, including when the policy bytes are unchanged
and their earlier verification is reused; a missing, unreadable, replaced or
rolled-back mark or mode record refuses. Once a root has run in this mode, it
never returns to the single paired client.

In this mode:

- The executor accepts any client certificate the current policy enrolls: a
  person's key signs each device's enrollment for this host, and the operator's
  policy grants project membership. Every request is authorized again against
  a freshly checked policy, so revoking a device or person takes effect on
  connections that are already open. A person, owner or role field in a
  request is refused, never trusted.
- Each job belongs to the person who submitted it, for one project. Status,
  logs, evidence, publication refresh, observations, acknowledgements and
  release answer only that person; to anyone else the job does not exist. The
  list shows only the caller's jobs in projects they may still read, and
  capabilities name only the caller. A
  job ID that another person already used is refused without its receipt.
- Mutations carry `Zerus-Policy-Revision` (the current revision) and
  `Zerus-Request-Deadline` (Unix milliseconds, at most five minutes ahead). A
  stale revision is refused; earlier receipts stay valid.
- A queued job is authorized again before it deploys, after each delivery,
  and at the launch boundary: after the scenario's verification and quiet wait,
  and inside the process creation callback before `CreateProcess`. A revoked
  device, person, membership or execution context marks it `access_refused`
  before launch (or `not_run` with an access reason at the boundary). Its owner,
  or an operator with `resource-cancel`, may release an `access_refused`
  receipt.
- Each person has a bounded queue (`per_person_jobs`, at most the executor's
  six) and the worker serves people in turn.
- Delivery slots belong to the job's project (`project-slots/<project>/<slot>`):
  members of one project share one managed copy, and another project's slot
  of the same name is a different folder. There is still only one installed
  tree per slot.
- Generation leases: before a job's delivery reads or advances a slot, a
  lease naming the slot, the generation it reads and the job is written under
  the slot's lock (`leases/<job>.json`). Jobs reading the same generation
  share it. The tree is never advanced to another generation while another
  job's lease names a different one: the later job is `held`. A lease is
  released only after its job's final receipt and verified whole-tree
  settlement; `unknown`, creation-issued, `awaiting_observation` and
  `cancel_unconfirmed` receipts keep it, also across restarts. Nothing is
  released by age.
- Cancellation: `POST /executor/v1/jobs/<id>/cancel` with a `request_id`
  (UUID), by the owner or by an operator with `resource-cancel`. A queued or
  held job becomes `cancelled` at once; a deploying job stops before launch; a
  running job's whole Job tree is terminated and the receipt says `cancelled`
  only after zero processes are verified, otherwise `cancel_unconfirmed`
  (never replayed, resources stay pinned). A repeated request returns the
  first request's identity. The answer carries only the job's phase; the
  operator's right never reads logs or results.
- Revocation takes effect at once for new requests, deployments and launches.
  A job already running is not killed for it (that would be a new kill
  authority); its receipt records `revoked_while_running` with the refusal,
  and the revoked person cannot read its results.

Isolation backend `windows-appcontainer-v1` (implemented, not qualified). Each
job runs in an AppContainer named for (executor root, host, person, context),
derived only from the current verified policy. Inputs (the project's
generation and scenario bundle) are readable through a per-project capability;
the job's runtime and output folders are its only writable places (Low label,
no permission or ownership rights); executor, operator, key, other people's
and human-profile objects carry no contained entry, and nothing is granted to
ALL APPLICATION PACKAGES. A regular AppContainer can still read objects that
grant ALL APPLICATION PACKAGES (much of Windows and Program Files); LPAC is
not supported. The suspended process's token, package SID, capabilities,
integrity, privileges and Job membership are verified before it resumes, and
its container must equal what the current policy gives the job's owner,
context and project. There is no uncontained fallback.

The backend registers only when `appcontainer-qualification.json`, placed
beside the binary by trusted packaging and protected by the installer, names
this exact binary, its source identity, grant matrix and configuration, a
`win25` runner on Windows 10.0 build 26100 or later, a non-administrator
service account and a pass for each of Q0-Q10, produced by the binary's own
`appcontainer-qualify` command on a disposable runner. This host must also be
Windows 10.0 build 26100 or later. The record's runner field is asserted by
the CI job, not signed; the packaging step and the installer's protection are
what vouch for it. Without such a record multi-person mode refuses to start.

Qualification status (disposable windows-2025 runner, 2026-11-26): every
isolation case Q0-Q9 passes (protected files, processes, IPC, handles,
environment, network, executor death, settled cancel, policy trust, leases).
Q10, real builds, does not: PowerShell scripts and MSBuild projects run, but a
C++ CMake build fails because MSBuild runs its build events as
`cmd /c <temporary .cmd>`, and a new `cmd.exe` running a batch file is refused
inside the container (`call` of the same file from a running `cmd` works).
Until that is solved the backend stays unregistered. Findings recorded for
contained jobs:

- Windows rewrites TEMP and LOCALAPPDATA of a contained process to the
  container's own storage (under the job-local LOCALAPPDATA) and refuses to
  create it without them; the executor points TMP there too.
- Windows PowerShell does not autoload modules such as
  Microsoft.PowerShell.Utility inside the container; the language and the
  built-in host work. The scenario environment carries `PSModulePath`.
- Named objects (`Local\...`) resolve in the container's private namespace.
- Contained processes run with strict handle checks.
- Network isolation drops connections to listeners silently (a timeout, not a
  refusal); a closed port is still refused.

Administration of people and devices (run explicitly; the service never
writes the policy):

- `access-request-device` (on the person's device) signs a device enrollment
  request for one host with the person's local key, which never leaves it.
- `access-admit-device` (operator) checks a request against the current
  policy, admits it once (a ledger refuses replays), adds project roles and a
  context from the operator's input only, optionally revokes devices it
  replaces (rotation), and writes the next policy revision to a new file.
- `access-revoke` revokes devices or people in the next revision.
- `access-show` prints the verified host and every person's devices, projects,
  roles, operator rights and context, without keys or signatures.

Data publishers (`data_peers`) remain a separate authorization from people's
devices. A Windows Job Object limits a process tree's lifetime and resources;
it is not a security boundary between people, and neither is a separate HOME.

### Operator setup, migration and rollback (multi-person)

Setup is manual and reviewed; no installer provisions accounts or ACLs yet.
The objects an operator creates, and removes on uninstall, are exactly:

1. a dedicated non-administrator service account that runs
   `hgs-executor executor-serve-shared <config>` (distinct from the named
   operator; never in Administrators);
2. the operator folder (for example `C:\ProgramData\ZerusOperator`) with a
   protected DACL: the named operator, Administrators and SYSTEM write; the
   service account reads; nobody else, and no ALL APPLICATION PACKAGES entry;
3. the executor configuration (installer-owned, same rules) naming the policy
   and the operator SID; see `examples/executor-shared.json`;
4. the policy, written only through `access-admit-device`, `access-revoke` and
   the operator's own reviewed edits; every change is a higher revision;
5. the qualification record beside the binary, copied by trusted packaging
   only, never edited.

The executor itself creates, inside its root only: `access-mode.json`,
`access-high-water.json`, `appcontainers/` (profile origin records and probe
folders) and per-job folders. AppContainer profiles it created are listed by
their origin records; remove them with the executor stopped and no unknown,
running or unconfirmed receipt, or leave them (they hold no data).

Migration and rollback:

- A root that has served several people never returns to single-person mode
  (`access-mode.json`; `executor-serve` refuses it). Roll back by stopping the
  shared service and starting a single-person executor on a NEW root.
- Rolling the policy back is refused (`access-high-water.json`); publish a
  new revision with the earlier content instead. A lost mark stops the
  service until the operator restores it.
- Single-person jobs must be collected or released before a root enables
  multi-person mode; ownerless receipts are never adopted.
- Upgrading the binary invalidates the qualification record (it names the
  binary's SHA-256): multi-person mode refuses to start until the new binary
  has its own record.

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

The runtime current directory and HOME, USERPROFILE, TEMP, TMP, APPDATA and
LOCALAPPDATA point to the job's generated runtime folder. The environment is
built from an explicit allowlist and nothing else: `SystemRoot`, `WINDIR`,
`COMSPEC`, a `PATH` of the system and Windows PowerShell folders, those
job-local folders, the variables above and the `GOTELEMETRY`/`OTEL_SDK_DISABLED`
opt-outs. The executor's own environment is never inherited, so provider keys
and homes, HGS account state, SSH agent sockets and proxy settings do not reach
a scenario. Names are unique regardless of case, contain no `=` or NUL, and
names, values and the whole block are bounded. The child inherits exactly two
handles, the log pipe's writer (standard output and error) and NUL (standard
input); policy, key, control, Job and process handles of the executor stay in
the executor. Scenario Job objects carry a protected DACL granting the
executor's account and SYSTEM only. All of this is hygiene under one account,
not isolation: scripts are trusted programs under the configured Windows
account, not a security sandbox. The account and local firewall must provide any required
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
