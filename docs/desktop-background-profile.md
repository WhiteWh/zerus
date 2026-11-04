# Canonical background profile definition

`client/core::desktop::profile::BACKGROUND_V1` is shared definition data for
the approved `g1-background-v1` profile. It has no producer/controller caller,
transport frame, CLI/status change, marker, store or migration. A matching
definition does not establish current ownership, consent, instrumentation,
source coverage, freshness, a drained role or permission to execute anything.

The profile contains exactly Fleet, Attention, NativePosting, Badges,
ReadDelivery and Presence. Its descriptive service permission names are
`fleet_source`, `attention_intent`, `agent_post`, `agent_withdrawal`,
`agent_activation`, `attention_badge`, `read_delivery` and `presence_publish`.
Native posting/withdrawal/activation is AgentAttention only. GUI software-update
notices remain outside this profile. Operations, Drafts, Providers, Downloads,
Cleanup and AutomaticApprovals have no membership or permission here; approvals
remain separate and off pending their own explicit qualification and consent.

The definition exposes one fixed 448-byte JSON representation, built from Rust
string literals so checkout line endings cannot change it. It has no whitespace
or final newline. The exact bytes, role order, permission order and version are
pinned in the compatibility tests. The lowercase digest is SHA256 of the ASCII
domain `hgs-desktop-profile-v1`, one NUL byte, and those canonical bytes:

`84ff9e4901c3df222b879922c756dcd364ab04300d5d3c83916320dd250b72ff`

No platform paths, clocks, caller values or runtime JSON formatting enter that
digest. Matching requires the exact version, profile ID and lowercase digest,
with no alias or unknown-version fallback. Role-list comparison accepts only
the full six-role set, rejecting missing, extra or duplicate declarations. It
does not inspect a gate and cannot turn an empty gate into known-zero coverage.

A future controller must bind this definition to its authenticated exact live
producer/configuration/build, current writer/service epochs, actual role
proofs, source-family allowlist and durable per-role owners/permissions. Every
selected role must have its own qualified proof, including the versioned
known-zero Presence producer capability. Attention, agent NativePosting and
Badges move together. The whole profile is journaled/returned atomically; its
identity and hash alone never perform that transition or authorize a marker.
Unrelated imports remain Staged, and historical data never supplies live
permissions. Source-family membership is not inferred by this helper.

Permission names describe the approved role scopes; they are not installed
policy or a new native job API. Changing profile meaning requires a new reviewed
definition/version and hash. Existing delegation/producer replies and positive
request refusal stay unchanged until their separate qualified integration.
