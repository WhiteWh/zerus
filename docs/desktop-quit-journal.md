# Shared quit-journal planning data

`client/core::quit_journal` is the pure predecessor for durable delivery of Undo
actions at real Quit (`zerus-5wmh.66.2.34.3.2`). The CLI and Tauri already share
this crate. No runtime caller consumes these plans. Current real Quit still drops
pending actions; this prerequisite changes no shutdown, updater or delivery path.

The eventual delivery rollout is Linux and macOS first. Windows planning refuses
positive delivery, and Windows keeps the current drop diagnostics until its native
PC and custody qualification. The parent remains open for Windows. There is no
legacy dispatch fallback.

## Immutable record

Version 1 binds the original real-Quit `quit_confirmed_ns`, its boot identity,
checked `+5_000_000_000` cutoff, explicit platform, original configuration digest
and GUI birth observation. The clock values are untrusted nanosecond data. A host
must qualify a cross-process, boot-scoped monotonic clock; `std::Instant` and wall
clock serialization cannot provide that proof. An observation before the freeze,
at or after the cutoff, on another boot or on another platform starts nothing.

Groups and members retain their original frozen deadline ordinals. Each member
binds its original request UUID, full `end_action::Captured`, original native
removal choice, optional bounded cleanup evidence and mandatory `KeepForReview`
restriction. Choosing removal requires exact-target evidence; missing ownership
inside that evidence stays unknown. Keep choices can also retain this negative
evidence, including missing ownership, for later review. There is no conversion back to a live cleanup
Capture or permission. A later host must freshly establish exact ownership and
persist the central negative cleanup hold before any stop, including supervisor,
archive and maintenance paths.

The planning SHA-256 covers the descriptor, context, full group Action and its
parameters. It is a planning digest, not the root session-action canonical request
hash. Existing exact receipt APIs remain separate; Clear context still lacks its
own read-only recovery API.

The wire uses compact member keys: `d` descriptor and `h` planning digest;
descriptor keys `i` ordinal, `r` request UUID, `c` capture, `w` removal choice,
`e` cleanup evidence and `n` negative restriction. A capture is the tuple
`[[computer, name, run_id, conversation_id], Action, state]`. The Action uses its
existing shared shape with closed validation here. Unknown fields, duplicate
native action/target keys, future enum values, missing required fields and
oversized input refuse. The bounded decoder produces an immutable record; its
cached bindings avoid rescanning a whole batch for each member plan.

## Bounds and expected before-images

There are at most five groups and 50,000 members per group. The whole serialized
record, including UUIDs, descriptors, hashes, context and cleanup evidence, must
fit 16 MiB. All groups are accepted or refused together. Separate global planning
caps are 64 MiB charged row/payload bytes, 500,000 retained rows/tombstones, 16,384
summaries, one charged generation and four child exchanges. Each retained member
adds a 512-byte planning row charge. These are data-model bounds; physical SQLite,
WAL, file and pipe limits require the later shared-store design and measurement.

Reserve plans include immutable summaries and the generation charge before any
helper. Retained request and exact-target checks use indexed lookup callbacks; a
future store must perform those checks and read actual usage inside its own
transaction. Supplied observations and Usage never establish global uniqueness or
current admission. That store must enforce one global namespace across identities,
configurations and old GUI generations.

Every CAS plan binds the global revision and exact prior slot, immutable record,
group/member digest and expected prior member phase. A native-spawn-attempt plan
consumes Pending and charges an exchange. Spent and unknown members cannot issue
again. Closing admission closes new starts only; all charges remain. The model
provides no release, pruning, retry, receipt settlement, effect token or executor.
Deserializing historical phases cannot establish native currentness or custody.

The full shared store/private I/O/global namespace and inherited kernel custody
protocol still require final G3 agreement and pinned native/security review before
mutable delivery code. Actual exit and pipe drain must be verified across GUI and
worker generations; neither a deadline, GUI exit, PID age nor Unknown can release
resources. No new daemon, GUI self-helper or timeout that kills a detached action
is admitted by this predecessor.
