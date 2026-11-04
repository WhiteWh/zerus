# Encrypted workspaces

An unconfigured computer has no workspace. Settings → Mobile connection offers
Create workspace and Join by code. Creating generates an independent computer
identity and a signed workspace genesis locally. Joining requires a computer
invitation from an already enrolled computer; phone invitations have a different
purpose. An active trusted computer must remain online to verify the joining
device's proof and approve its immutable public keys.

Create and Join ask for the relay and, for Join, a computer invitation code.
The computer's system hostname supplies its private display name automatically;
neither a workspace-name field nor a computer-name field is required. Existing
saved labels are preserved. A phone's Rename machine action remains a separate
display preference on that phone.

Every computer and phone has its own bearer credential, signing key and
encryption key. A workspace is the shared authorization namespace, rather than
one physical computer. A phone can reach each enrolled gateway and that
gateway's directly configured SSH peers. A direct peer remains a native route;
it does not become an enrolled gateway automatically. Windows gateways execute
local Windows operations only.

A computer has one active workspace. A phone can retain several separate
workspace connections. A computer already reachable through a gateway's SSH
configuration does not need to join separately; enrolling it as another gateway
is useful when it must remain reachable independently of the first gateway.

Private workspace/computer labels live at endpoints. The public enrollment
service stores generic labels and opaque identities. Changing a local label
does not rename another device's local connection.

## Confidentiality and trust

| Private encrypted content | Service metadata visible to the relay |
| --- | --- |
| Messages, answers, commands and arguments | Opaque workspace/device/gateway/route/request IDs |
| Names, paths, account information and histories | Protocol, operation class, size, time and expiry |
| Snapshots, native errors, terminal/process output | Queue/claim/receipt state and server-assigned service class |
| Files, attachments and recordings | Bounded presence and opaque notification categories |

The relay retains offline queues, routing, durable receipts, quotas and push
delivery. Encryption ends at the trusted gateway, which decrypts, validates and
executes the request or uses its existing authenticated SSH route to a peer.
Neither an executable path nor a process command becomes public metadata.

The [wire specification](../services/mobile/crypto/README.md) defines RFC 9180
HPKE using X25519/HKDF-SHA256/AES-256-GCM, with an independent Ed25519 signature
over the complete header, encapsulation and ciphertext. Recipient keys come
from verified membership. Each recipient gets a separate envelope. The copied
invitation contains a secret that the relay never receives; its HMAC binds the
exact joining device and keys to the inviter's pinned membership head.

Membership is a signed linear log. Any active computer can add or remove a
device, including the founding computer. Endpoints persist their verified head
and reject rollback or a conflicting branch. A delayed envelope may use a
previously verified head only while both devices remain active with the same
immutable keys. Locally accepted revocations serialize with authorization and
private-content release.

A relay operator can observe metadata, delay/drop delivery or partition
devices. The protocol does not promise availability or global consensus against
that operator. A valid signed snapshot can be replayed until its expiry;
authenticity alone does not prove the newest native state. New request receipts
bind their exact request identity, and native target/run/conversation/question
checks remain authoritative for actions. A removed device may retain plaintext
it previously received.
Compromising a recipient's long-term private key can expose retained historical
ciphertext; this single-shot protocol does not provide recipient-compromise
forward secrecy or post-compromise recovery.

## Persistence and switching

The endpoint saves keys, credentials, exact enrollment candidate and outgoing
operation identity before sending them. Retrying an operation uses its retained
ciphertext and original route. Reading a receipt never generates a replacement
request. Every API v2 request keeps a permanent identity tombstone, including
requests labeled as reads. Cleanup may remove payloads, never permission to
execute the same identity again. An interrupted admission returns an
authenticated uncertain outcome when execution cannot be established.

The connector keeps its native journal and adds separate indexed encrypted
transport tables. Its private state uses owned files, bounded reads, durable
replacement and compare-and-swap checks. Mobile keys use Keystore/Keychain;
membership and outgoing-envelope state are committed in private encrypted
SQLite records. Forgetting a protected phone connection erases its local
wrapping key; global device removal requires a computer-signed membership
change. The connection stays visible until idempotent protected cleanup and
the final connection-index save succeed. A failed Forget can be retried,
including after key deletion succeeded but journal cleanup or index saving failed.

Change workspace requires confirmation. The connector stops taking new work,
finishes its current native call and releases the old journal. A protected
switch retires the old gateway using a signed membership change, preserves its
journal, and activates a fresh identity and receipt directory for the destination
workspace. Native sessions and desktop projects remain local. Existing phones
of the destination workspace do not need another pairing ceremony.

An enrollment may already be saved when its activation response is lost. The
desktop then refreshes local status without repeating Create or Join. A saved
pending connection is resumed with Check connection or Continue connecting;
its exact device identity, destination and request identities are retained.
Confirmed pending enrollment also clears the consumed invitation from the form.

An explicitly confirmed legacy switch on Linux can hand over a verified Rust
connector service automatically. It verifies the service, running process,
configuration and journal binding, then reserves one graceful stop request for
that connector alone. It waits for its current phone request and journal lock
to finish before activating the destination. A private, marked service drop-in
updates the executable used for subsequent starts without replacing the base
unit or unrelated settings. Native agents and tmux are never stop targets.
An already stopped connector can still use the established manual activation
path on other platforms or when no service can be admitted; this grants no
service control or autostart guarantee. A running unsupported or unsafe service
retains an actionable pending connection. Opening settings and reading status
never authorize a handover; uncertain delivery never authorizes a second stop
signal.

There is no account/password recovery service. A machine with lost local state
uses a fresh invitation from another trusted computer, or creates a new
workspace if none remains. An HTTP authorization error alone is not proof of a
signed retirement. Legacy migration first requires the old connector to stop
and release its journal. If the old relay cannot confirm retirement, the new
identity remains pending and the old configuration stays in place. A separate
confirmation can disconnect that legacy configuration locally and activate the
new workspace; it explicitly reports that old relay records and credentials may
remain. This local disconnect option never substitutes for signed retirement
when switching from a protected workspace.

Protected connector configurations omit the legacy `node_token` field. Protected
mobile connections use a separate versioned index. Older binaries therefore
cannot silently reinterpret the credential as a plaintext API v1 connection.
Malformed markers, missing private state and unavailable keys fail closed.
API v1 remains supported for explicitly configured legacy connections; its
hashes, native journals and historical plaintext are not rewritten.

## Security verification map

The following maps Zerus-specific controls to the
[OWASP API Security Top 10](https://api-security.owasp.org/editions/2023/en/0x11-t10/).
It is a review map, not a certification.

| Risk area | Zerus control and evidence |
| --- | --- |
| Object, property and function authorization (API1/3/5) | Workspace-scoped database checks plus signed sender/recipient/role/route binding; native target/run/conversation checks; cross-workspace and wrong-route tests. |
| Authentication (API2) | Independent random credentials, typed expiring invitations, secret-bound key admission, verified roster history and signed revocation; forged-key/bootstrap/signature tests. |
| Resource use and automated flows (API4/6) | Bounded enrollment, roster/history pages, ciphertext bodies, queue bytes, retained identities and shared resource leases. Operator-assigned team reserves stay inside declared totals; public credentials cannot self-promote. |
| Outbound requests (API7) | The relay never fetches a private command's URL or executes its payload. Connector/native admission remains authoritative for any requested network operation. |
| Configuration and API inventory (API8/9) | Public enrollment is explicit; TLS is required outside isolated loopback tests; API v1/v2 have separate transport and downgrade guards. Packaging includes the reviewed sibling crypto crate. |
| Untrusted upstream responses (API10) | Bound parsing, exact wire framing, strict signature/key checks, pinned history and endpoint-authenticated native receipts. Outer completion hints cannot establish completion. |

Resource tests must exercise actual saturation, body sizes and memory ceilings,
not only request ordering. OWASP's
[resource-consumption guidance](https://api-security.owasp.org/editions/2023/en/0xa4-unrestricted-resource-consumption/)
also covers process/file limits, timeouts and response-page bounds. Deployment
qualification is specific to its declared configuration and budget.

Protocol/key tests live in `services/mobile/crypto`; adversarial connector and
crash tests in `services/mobile/connector`; real PostgreSQL workspace controls
in `services/mobile/workspace-tests`; service isolation/priority tests in the
relay suite; and native wire/storage/HTTP tests in Android and ZerusKit. Source
changes and test builds do not migrate a live gateway or publish a release.

Desktop Stop admission is advertised as `Workspace stop ABI: service-preflight-v1`
in connector help and `capabilities.workspace_stop` in `--check --json`. Before
writing a generation-bound Stop marker, Linux verifies the saved managed unit,
its effective `KillMode=process` policy, unchanged definition, exact MainPID,
connector image, journal lock and generation. Extra processes or delegated
cgroups refuse. A foreground connector needs the exact journal-owning process
and no active service; unreadable supervision refuses. Platforms without a
verified installer service admission refuse Stop. Clients must require the ABI
marker before offering Stop or a running workspace change; there is no legacy
Stop fallback. Installing a new connector on a live gateway remains explicit.
