# Host access contract, version 1 (`zerus-host-access-v1`)

Implementation epic `zerus-un7n`. Design:
`docs/superpowers/specs/2026-11-26-shared-host-personal-accounts-design.md`.

This folder is the language-neutral contract that full Zerus (`src/host_access/`)
and the model-free artifact executor (`scripts/experiments/desync-artifact/host_access.go`)
both implement. The two implementations must agree on every fixture in
`fixtures/`. `generate.py` writes the fixtures from synthetic, deterministic
keys; `python contracts/host-access-v1/generate.py --check` verifies that the
committed fixtures match it. Every key and seed here is fixture data. No
production code reads a seed, and there is no configuration or environment
switch that selects fixture keys or a fixture isolation backend.

## Identities

| Field | Encoding | Meaning |
| --- | --- | --- |
| `host` | 64 lowercase hex | SHA-256 of the host's TLS certificate (DER). Pinned on admission. |
| `person` | 64 lowercase hex | SHA-256 of the person's raw 32-byte Ed25519 public key. Derived, never claimed. |
| `public_key` | base64url, no padding, 32 bytes | The person's Ed25519 key. Canonical (y < p) and not of small order. |
| `device` | 64 lowercase hex | SHA-256 of the device's TLS client certificate (DER). The TLS handshake is the device proof. |
| `project`, `context`, `backend` | `[a-z0-9][a-z0-9._-]{0,63}` | Host-local names. |
| `label` | 1–160 characters, no control characters | Display only; never an identity. |
| `signature` | base64url, no padding, 64 bytes | Ed25519, verified strictly. |

The account namespace is `(person, context, provider, account)`. Two people can
both have `native-codex`; their account references never compare equal. This
contract never carries auth homes, credential paths or provider tokens.

## Device enrollment statement

The person key signs these exact bytes (UTF-8, LF line ends, no trailing
spaces, decimal integers without leading zeros or signs):

```
zerus-host-access-v1/device-enrollment
host=<host>
person=<person>
device=<device>
issued=<unix seconds>
expires=<unix seconds>
```

Each line, including the last, ends in a single LF. Every field has a fixed,
validated alphabet, so no field can contain a separator. The statement binds the
device to one person on one host. It proves that the person's key holder enrolled
that device. It does not grant project membership.

## Policy

The host's operator maintains the policy in a private operator-owned file,
outside shared source, artifact and scenario trees. The policy is the
membership authority. Its JSON object accepts exactly these members; unknown
members, duplicate members, trailing data and documents over 1 MiB are refused.
A missing member reads as its empty value, then fails the checks below.

```
contract         "zerus-host-access-v1"
host             host ID; must equal the verifying host
revision         integer 1..2^53-1; never decreases
persons          [{person, public_key, label}]              at most 256
devices          [{device, person, issued, expires, signature}]  at most 1024
projects         [{project, members: [{person, role}]}]     at most 256 x 256
operators        [{person, rights: ["resource-cancel"]}]    at most 64, at most 4 rights each
contexts         [{person, context, backend}]               at most 256, one per person
revoked_devices  [device]                                   at most 4096
revoked_persons  [person]                                   at most 256
```

`role` is `reader` or `contributor`. The only operator right is
`resource-cancel`: stopping another person's job for resources. It grants no read
of results, no acknowledgement and no provider authority.

### Verification codes

A verifier reports the first failure, in this order:

| Code | Failure |
| --- | --- |
| `too-large` | More than 1 MiB. |
| `malformed` | Not one JSON object; unknown, duplicate or mistyped member; trailing data. |
| `contract` | `contract` is not `zerus-host-access-v1`. |
| `host` | `host` is not a host ID. |
| `host-mismatch` | `host` is not the verifying host. |
| `revision` | `revision` out of range. |
| `limits` | A list exceeds its bound. |
| `identifier` | A malformed ID, name or label. |
| `person-key` | A public key that is malformed, non-canonical or of small order. |
| `person-id` | `person` is not the SHA-256 of its `public_key`. |
| `duplicate` | A repeated person, device, project, project member, operator, operator right, context person or revocation entry. |
| `unknown-person` | A device, member, operator, context or person revocation names a person not in `persons`. |
| `window` | `issued >= expires`, or a time outside 0..2^53-1. |
| `signature` | The enrollment signature does not verify strictly for that host, person and device. |
| `role` | An unknown role. |
| `right` | An unknown operator right. |

The exact order, which both implementations follow:

1. size (`too-large`), then parsing (`malformed`);
2. `contract`; `host`, then `host-mismatch`; `revision`;
3. `limits`: every list, the members of each project and the rights of each
   operator (at most 4);
4. each person: `identifier` (person, label), `person-key`, `person-id`,
   `duplicate`;
5. each device: `identifier` (device, person), `duplicate`, `unknown-person`,
   `window`, `signature`;
6. each project: `identifier`, `duplicate`; then each member: `identifier`,
   `unknown-person`, `duplicate`, `role`;
7. each operator: `identifier`, `unknown-person`, `duplicate`; then each
   right: `right`, `duplicate`;
8. each context: `identifier` (person, context, backend), `unknown-person`,
   `duplicate` (one per person);
9. each revoked device: `identifier`, `duplicate`;
10. each revoked person: `identifier`, `unknown-person`, `duplicate`.

## Authorization

`authorize(policy, device, now, operation, project)` resolves the caller from
the device alone. A `person`, `owner` or `role` field in a request is never
consulted.

| Operation | Requires |
| --- | --- |
| `capabilities`, `jobs.list` | An enrolled, valid device |
| `jobs.read`, `artifacts.read` | Project membership, any role |
| `jobs.submit`, `jobs.control` | Project contributor |
| `jobs.cancel` | Project contributor (`via: member`), or the `resource-cancel` right (`via: operator`) |

Refusal codes, in order: `operation-unknown`, `device-unknown`,
`device-revoked`, `person-revoked`, `enrollment-not-yet-valid` (`now < issued`),
`enrollment-expired` (`now >= expires`), `project-unknown`, `not-a-member`,
`role-insufficient`. A refusal reveals nothing about other people's projects
or jobs to the caller beyond its code.

Ownership of a job, result, log, evidence or receipt is a separate check by the
service: the owner is the person who admitted it. Project membership alone never
grants another person's job controls.

## Person operations

A full-Zerus caller identified as a person (an OS account the operator bound
to a policy context), not through a device, is authorized with
`authorize_person(policy, person, operation, project)`:

| Operation | Role needed |
| --- | --- |
| `history.read`, `memory.read` | any member role |
| `history.publish`, `memory.write`, `handoff.continue` | `contributor` |

Order of checks, first failure reported: `operation-unknown`,
`person-unknown`, `person-revoked`, `project-unknown`, `not-a-member`,
`role-insufficient`. An operator right is never membership. Fixture:
`fixtures/person-decisions.json`.

## Device enrollment requests

A device asks its host operator for enrollment with this JSON object (no
other members):

```json
{"contract": "zerus-host-access-v1", "kind": "device-enrollment-request",
 "host": "<64 hex>", "person": "<64 hex>", "public_key": "<base64url>",
 "label": "<1-160 characters>", "device": "<64 hex>",
 "issued": 0, "expires": 0, "signature": "<base64url>"}
```

The signature is the person key's signature of the device enrollment
statement above, so an admitted request becomes a policy device entry
unchanged. The person's private key never leaves the person's device.

Checks against the operator's current verified policy at time `now`, first
failure reported:

1. `request-contract`: wrong contract or kind;
2. `request-host-mismatch`: another host;
3. `request-identity`: invalid person key, `person` not its SHA-256, invalid
   device ID or label;
4. `request-window`: `issued >= expires`, longer than 400 days, or beyond
   the maximum time;
5. `request-stale`: issued more than 300 seconds in the future, expired, or
   issued more than 7 days ago;
6. `request-signature`;
7. `request-device-revoked`, then `request-person-revoked`;
8. `request-duplicate-device`.

An operator admits each request once (a ledger of admitted request digests
refuses replays) and writes the next policy revision to a new file. Project
roles and execution contexts come only from the operator's admission input,
never from the request. Fixture: `fixtures/enrollment-requests.json`.
