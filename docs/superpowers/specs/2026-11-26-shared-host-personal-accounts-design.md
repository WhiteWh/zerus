# Shared remote hosts with private provider authority

Status: implementation authorized by the owner on 2026-11-26, through
claude/Zerus/BrokerWorks. Joint source review by codex/Zerus/Broker and
claude/Zerus/BrokerWorks. See "Implementation status and plan" for what
exists in source; isolation is not qualified and multi-person mode is not
available. It is not part of fork
build 225's feature set. Design tracking: `zerus-vtyx`; implementation epic:
`zerus-un7n`.

## Required behavior

A person connects their existing desktop Zerus to a remote full Zerus host or
a model-free build/test broker. Their provider sign-in on their workstation
remains useful: connecting must not require another provider login on the host,
nor retire their local sign-in. Another person can connect independently to the
same host and use explicitly shared project history and memory. Neither person
can read, select, use, export, refresh or spend the other's provider account.

One-time admission to a host is distinct from signing into a model provider.
An invitation or administrator approval establishes host access once; subsequent
connections prove the same person and device without another provider login.
The default sharing scope is an explicitly selected project, not a merger of
the participants' personal Zerus installations or swarms.

## What exists today

| Current mechanism | Evidence | Consequence |
| --- | --- | --- |
| Machine connections choose an SSH user | `src/machines.rs`, `ssh_options`; `docs/reference.md`, Machines and accounts | Two clients using one SSH login share its OS authority. |
| Account catalogs and native homes belong to an OS user | `src/accounts.rs`, `Catalog`, `native_home`, `profile_home`, `select`, `run_environment` | Different `CODEX_HOME` or account labels under one OS user do not isolate secrets. |
| Session records retain account and native home | `src/state/conversation_history.rs`, `src/state/agent_hint.rs`; `src/accounts.rs`, `run_script` | Access to the current control interface can reuse the original person's account. A shared history projection needs a separate API. |
| Account copy exports selected native credentials | `src/accounts.rs`, `export`, `import`, `copy_account` | Copying a rotating grant is not a solution to multi-person authorization. Claude export is refused; other adapters have different restrictions. |
| Native CLIs own provider refresh behavior | `src/accounts/inspection.rs`, Claude/Qwen/Kimi inspection paths | A second remote refresh writer must not be created implicitly. |
| Swarm replication shares catalog/project metadata | `src/swarm/mod.rs`, `store.rs`, `transport.rs`; `crates/replica` | Node/swarm membership is not a person-specific access policy. Credentials stay local, but that alone does not isolate control authority. |
| Mobile enrollment identifies workspace devices | `services/mobile/relay/src/store.rs`, `authorized`; `store/enrollment.rs` | Existing signatures and revocation are useful patterns; device roles and workspace membership do not establish ownership of a provider account. |
| Mobile launch validates the gateway account catalog | `services/mobile/connector/src/launch.rs`, `Launch::execute` | Checking that an account is installed is insufficient when a gateway serves several people. |
| The artifact executor pins one control client and approved data peers | `scripts/experiments/desync-artifact/peer.go`, `dataListener`; `executor.go`, `validateRequest` | `DataPeers` authorizes data publishers, not additional control users. Jobs currently share the executor's OS identity. |
| Project memory has a stable project identity | `src/state/project_memory.rs` | Reuse the identity, but add authorization before queries, indexing, writes and exports. |

The current implementation is suitable for its existing personal-machine trust
model. None of these mechanisms alone establishes the requested multi-person
boundary. No real credentials were read during this design review.

## Identity and authority

Keep these identities separate:

| Identity | Meaning |
| --- | --- |
| Person | A stable Zerus identity backed by the person's key, independent of provider email, account label and SSH login. |
| Device | A client installation with its own key, enrolled for that person. Several devices may belong to one person. |
| Host | The remote service identity, pinned on admission. Its physical computer may serve several isolated contexts. |
| Execution context | The actual UID, SID, sandbox or VM and its private filesystem/process boundary. A directory name is not a context. |
| Provider account | A private reference owned by one person, with a separately verified provider identity. |
| Project membership | Rights over one shared project and its published resources. It grants no provider authority. |

Use a private account reference scoped by person, execution context, provider
and stable account ID. Existing local account IDs remain compatible; they are
never treated as globally unique. Two people can both have `native-codex`.
Matching email, label or quota snapshots never merges their ownership.

Bind a running session to its person, execution context and account reference.
Bind a job to its submitting person and project. Bind each request to a device,
operation, target, request ID, current policy revision and deadline. The service
derives the person from verified enrollment; a request's `person_id` field is
never proof of identity.

## One host-access layer, two execution modes

Both full Zerus and the headless broker expose the same versioned host-access
contract. The desktop presents one connection flow. A broker advertises job and
artifact capabilities; a full host additionally advertises supported native
session capabilities. Both enforce the same person/project policy.

1. The host creates a short-lived single-use invitation, scoped to projects and
   permitted operations. Its operator can approve a person without receiving
   that person's provider account or private key.
2. The desktop verifies the host identity and proves its person/device keys.
   Accepting an invitation binds that device to a membership. Keys stay on
   their originating device; a shared password or copied client identity is
   unnecessary.
3. The host issues a device-bound authorization grant. It includes host,
   person, device, projects, allowed operations, expiry and policy revision.
   A copied grant cannot be used without its device proof. TLS identity and
   person authorization are checked independently.
4. Reconnection renews the authenticated connection using that membership.
   Every operation is checked against current policy, including on existing
   connections and immediately before a mutation.
5. Selecting a project shows its authorized history, memory and jobs. Selecting
   an account shows only the caller's private provider catalog.

Existing SSH remains a bootstrap or transport option. Giving several people the
host owner's unrestricted SSH login is not the new access mode. Local IPC must
verify the calling OS identity and execution binding too; it cannot trust
environment variables, a JSON role or a caller-supplied owner ID.

The host-access service must not run submitted project scripts itself. A small
privileged launcher may provision contexts and apply exact ACLs. The ordinary
control service and the execution workers remain unprivileged.

### Model-free broker

Provider authorization stays entirely on each person's workstation. The broker
receives concrete build/test/scenario jobs and authorized artifact references.
It needs no provider sign-in, provider credential forwarding or model runtime.

Run arbitrary submitted code under isolated credential-free worker identities.
Separate personal spools, outputs, temporary directories and job-control rights.
A Windows Job Object supplies process-tree and resource limits; it does not
replace a SID, sandbox or VM security boundary.

One project can retain one shared managed artifact tree with authorized
read-only execution leases. Updating it needs an exclusive generation lease;
another person's job cannot overwrite a generation being tested. Personal
receipts and metadata do not require duplicating the full payload. Unrelated
projects and private artifacts remain inaccessible.

Scheduling accounts for the submitting person, with bounded per-person queues,
fairness and resource budgets. Knowing another job's ID grants no read, cancel,
result acknowledgement or repair authority. A host operator may receive an
explicit resource-cancellation right without provider-account authority.

### Full remote Zerus

Each person's remote native session needs a genuinely isolated execution
context, private state, native runtime home, temporary directories, sockets and
process access. On Windows the initial implementation should use separate
non-administrator users/SIDs; on Unix, separate non-root users. An equivalent
qualified sandbox or VM is possible. An elevated shared SSH login is unsuitable.

No-reauthentication native mode requires a verified provider-specific adapter:

- The workstation retains the refresh-capable provider authorization and its
  native refresh owner. The host receives only the authorization needed for
  the explicitly selected person's session, through a protected channel.
- Prefer a non-persisted handle/pipe or a verified owner-side API transport.
  Such an adapter must use the existing account; it must not silently replace
  subscription access with a separately billed API account.
- A Zerus access grant is not a provider access token. An environment-variable
  name in `AUTH_ENV` proves sanitization, not that a native CLI accepts it,
  avoids persisting it or supports subscription authentication through it.
- Qualify native launch, continued interactive use, refresh, restart and secret
  persistence against the exact supported native release before advertising
  this mode. No provider adapter is declared qualified by this document.
- If an adapter cannot satisfy these rules, show the capability as unavailable.
  Do not fall back to copying a refresh grant, borrowing the host's default
  account, requesting another remote provider login or retiring the local login.

Local agent plus remote filesystem/process tools is a separate useful mode:
provider authorization stays local and the remote host supplies computation.
Native local filesystem/shell tools must be correctly replaced or constrained;
this is not automatically transparent for every native agent. It must be
presented and qualified separately from a fully remote native session.

Private worktrees are the default for concurrent contributors. Shared project
code and memory are untrusted inputs, not account permissions. Project builds,
tests and arbitrary shell code must execute in credential-free workers, without
provider homes or credential handles. A native adapter that cannot separate its
arbitrary tool execution from credential-bearing authority is not certified for
the stronger hostile-project-code threat model. UI filtering and prompts cannot
substitute for that boundary.

## Shared history and memory

Publish a typed project projection rather than exposing native session stores,
runtime configuration or provider homes. It can include approved messages,
instructions, decisions, results, artifact references and provenance. Provider
credentials, native auth files, account usage, refresh state, private account
identifiers and authorization/debug payloads are excluded.

| Operation | Project reader | Project contributor | Private execution owner |
| --- | --- | --- | --- |
| Read published project history and memory | Yes | Yes | Yes, within membership |
| Add project memory and submit authorized jobs | No | Yes | According to membership |
| Inspect private runtime/account state | No | No | Their own context only |
| Send to, resume or approve a private native session | No | No | Their own session, subject to existing native proofs |
| Change, copy, export or inspect credentials | No | No | Their own account under its separate account policy |
| Continue from published history | No execution right | New session using their own account | New session or authorized own resume |

When another person chooses **Continue with my account**, create a new session
with a new owner/account binding and a link to the published context. Never
reinterpret that action as native resume or account-switch on the original
person's session. Cross-person continuation imports only authorized context;
it does not copy native auth/home metadata.

Enforce scope before storage/export as well as before displaying results.
Partition shared projections and private documents: hiding fields after a Loro
merge would leave them in replicated operations. `crates/replica` remains the
only Loro importer. Shared memory indexes, semantic search, counts, attachments,
event feeds and audits all need the same scope. Use indexed, bounded pages and
bind cursors/cache entries to the permission revision. A legacy peer must not
receive a private document it cannot safely interpret.

Zerus-controlled secrets never enter this projection. General message text is
not a guarantee against a person deliberately pasting a secret; do not claim
that a display redaction can undo a credential disclosure already replicated.

## Disconnect, revocation and restart

- A workstation disconnect does not stop credential-free broker jobs already
  admitted. Results wait for their authorized recipient.
- A remote native runtime may continue using an already-issued provider access
  token until its actual expiry or confirmed process stop. A Zerus lease does
  not magically revoke that token. No new lease, account fallback or automatic
  restart is inferred while the workstation is unavailable.
- Revoking a person/device grant immediately refuses new Zerus requests,
  subscriptions and credential issuance. Revoking a provider grant and stopping
  an already running native process are distinct operations. Any stop retains
  exact process/session binding and the existing lifecycle confirmation rules.
- Previously downloaded shared history cannot be remotely erased by revoking
  future access. Revocation stops subsequent synchronization and operations.
- Restart requires fresh authorization where needed. Preserve stopped, held and
  unknown execution outcomes; never report a process as resumed from stale
  history or mint another job ID to bypass uncertainty.
- Admission and credential delivery reserve a durable request receipt before
  effect. Lost replies reconcile the original request; unknown effects are not
  replayed. Unknown credential delivery does not imply an unused credential.
- Person/device key rotation changes enrollment explicitly. The host operator
  can restore project membership but cannot manufacture another person's
  provider authorization or silently claim their identity after key loss.

## Implementation order and compatibility

1. Add the person/device/host-context policy model, typed grants and synthetic
   isolation fixtures. Keep existing session IDs, references, `hgs` identifiers
   and private personal-machine state intact.
2. Implement the shared host-access layer and broker multi-person admission.
   Add credential-free execution identities, project artifact leases, ownership
   checks, fair scheduling and bounded audit/results APIs.
3. Add the common desktop connection/invitation UI and shared history/memory
   projections. Enforce the policy across all CLI, desktop and mobile routes;
   direct legacy endpoints must not bypass the new gate.
4. Add full-host private contexts and individually qualified provider/native
   launch adapters. Unsupported native capabilities remain unavailable.
5. Qualify reconnect, revocation, rotation and mixed-version upgrades before
   enabling multi-person mode on an existing host.

A new `multi-person-access-v1` capability is negotiated independently from old
machine pairing. Old single-person clients retain their existing isolated
personal context; they never silently gain access to a shared multi-person
host through its owner login. Enabling sharing requires an explicit reviewed
binding of existing resources to a person and project. Unknown legacy ownership
stays private and unavailable to new members. Preserve person-gesture, native
caller proof and unknown-delivery protections; neither a device signature nor
a replicated operation turns an agent request into the person's native action.

## Implementation status and plan

This section is maintained with the implementation (epic `zerus-un7n`; native
task ZERUS/T-34gfjn). It records what exists in source, not what is
qualified. Multi-person mode stays unavailable in every build until a
distributable qualification record exists for the exact binary (below).

Contract `zerus-host-access-v1` (`contracts/host-access-v1/`, Go in
`scripts/experiments/desync-artifact`, Rust in `src/host_access`), with
shared fixtures that both implementations must match:

- policy verification and device authorization (`policies.json`,
  `decisions.json`);
- person operations for operator-bound OS accounts (`person-decisions.json`:
  `history.read` and `memory.read` for any role; `history.publish`,
  `memory.write` and `handoff.continue` for contributors);
- device enrollment requests (`enrollment-requests.json`). The person's key
  stays on their device; a request is host-bound, signed, at most 400 days
  long, admitted within 7 days and once only.

Model-free broker (the artifact executor):

| Area | State in source |
| --- | --- |
| Multi-person admission, ownership, fair per-person queues | done |
| Operator policy trust: owner and DACL on held handles, installer-trusted bootstrap configuration, named non-service operator, explicit shared entry `executor-serve-shared` | done |
| Job, handle and environment hardening (protected Job DACL, exact inheritance, credential-free allowlisted environment) | done |
| AppContainer candidate `windows-appcontainer-v1`: plan from the verified policy, profile origin records, grant matrix with read-back, token verification before resume, launch envelope checked against current policy, runtime probe | done in source, not qualified |
| Generation leases on the single installed tree (same-generation share, advance held, unknown receipts pin, no age release) | done |
| Owner and operator cancel with request identity; "cancelled" only after whole-tree zero | done |
| Revocation: new launches and access refused immediately; a running job is marked `revoked_while_running`, not killed | done |
| Enrollment administration (`access-request-device`, `access-admit-device` with rotation, `access-revoke`, `access-show`) | done |
| Distributable qualification `appcontainer-qualify` (Q0-Q10) and activation by a trusted record bound to binary, source, matrix, configuration and runner/OS provenance | done in source; never run (needs the disposable-runner workflow) |

Full Zerus (`src/host_access`, delegated to claude/Zerus/Worker): OS-identity
callers mapped through an operator-owned binding file to a person and
context; a fail-closed gate on every local `hgs` route; explicit state-folder
binding; a typed shared history and memory projection with bounded,
revision-bound pages and owner-free handoffs; and a provider support matrix in
which only Claude local-agent with remote compute is implemented (not live
qualified). Remote native subscription and API-proxy modes are unsupported.

Activation remains blocked until the reviewed workflow
(`scripts/experiments/desync-artifact/qualification/appcontainer-qualification.yml.draft`)
runs the exact packaged binary on a disposable runner as a non-administrator
account and every Q0-Q10 case passes. A failed, skipped or unsupported case
keeps the backend unregistered.

## Acceptance tests

Use two synthetic people, device keys and non-admin OS identities, duplicate
account labels and fake provider sentinels. Never use real accounts in tests.

1. Both clients reconnect without another provider login; each sees only its own
   account references. A third device with copied public metadata cannot join.
2. Both read permitted project history and memory. Private projects do not leak
   through listing, search, counts, attachments, cached pages or replica deltas.
3. Every CLI/API/UI path refuses cross-person account inspection, export, copy,
   selection, native send, resume, fork, switch and approval. Raw IDs, forged
   actor fields and a shared-history link grant no additional authority.
4. **Continue with my account** creates a distinct native session/account
   binding. Replaying the same request cannot create another session.
5. Code under either worker cannot read the other person's files, process
   memory/environment, private sockets or credential handles. A malicious build
   from a shared project cannot read either person's provider sentinel.
6. Each proposed native authorization adapter proves its supported credential
   type, no second refresh writer, no remote sign-in, no local sign-out and no
   persistence in argv, native homes, shared outputs, logs or replica documents.
   A skipped native test is not compatibility evidence.
7. A broker holds no provider sentinels at all. Job controls and acknowledgements
   are owner-bound; project artifact reads follow explicit membership and active
   generations cannot be overwritten by another job.
8. Device/person revocation applies on an already open connection. Verify the
   separate actual behavior of outstanding provider tokens and running jobs.
9. Disconnect/reconnect and service restart preserve exact sessions/jobs and
   outstanding observations without borrowing another account or replaying work.
10. Kill components between durable reservation, credential delivery and job
    admission. Original receipts reconcile known outcomes and preserve unknown
    ones. Repeated requests never double-launch or double-spend authorization.
11. Validate large, paginated histories and simultaneous users without loading
    complete journals or sharing per-person caches accidentally.

## Security boundary references

The host operator/administrator is part of the trust boundary for fully remote
credential-bearing processes. Separate users do not protect process memory
from a privileged administrator. The credential-free broker and local-agent
mode keep provider secrets off that host; the broker still necessarily sees
authorized project inputs and results.

Windows checks access using the process token and object ACLs, not the selected
Zerus account name: [Access tokens](https://learn.microsoft.com/en-us/windows/win32/secauthz/access-tokens)
and [process security](https://learn.microsoft.com/en-us/windows/win32/procthread/process-security-and-access-rights).
If Windows DPAPI protects private context data, use user-scoped protection and
appropriate ACLs; machine-scoped protection permits other users on the machine
to decrypt: [CryptProtectData](https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata).
