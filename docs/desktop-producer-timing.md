# Local producer timing bounds

`client/core::desktop::producer::timing` implements local monotonic timing for
the existing [producer contract](desktop-producer-drain.md). It adds no caller,
thread, timer, control frame, store, status or positive API. A `Timing` and its
`WithinBounds` observation establish no authenticated connection, current
lease, ownership, role drain, source coverage or execution permission.

At a new independently qualified drain preparation, the future host records its
own `Instant` once. Heartbeats are due every three seconds. A received current
heartbeat can move only the fifteen-second idle boundary. The ten-minute
absolute lifetime and 120-second drain deadline stay fixed at preparation;
equality with a deadline is expired. The helper reads no wall clock or message
timestamp and schedules no work. `observe_drain` is for the host while it is
still waiting for actual drain; after an independently proved drain, ordinary
`observe` still applies before capture, publication and effects.

Expiry, connection loss, revoke, owner change, logical proof invalidation and
unavailability are sticky. The first invalidation reason stays, and a late
heartbeat or out-of-order local observation cannot revive it. The value has
no clone, reset or serialization. Constructing a new one does not renew an old
context; a replacement needs a new authenticated connection/preparation and
its own distinct binding. Expiration never reopens a frontend gate, releases a
resource charge or restores GUI ownership.

The future host must bind the actual connection, configuration, clean build,
canonical instance, service/producer/writer epochs, exact profile and drain
identity independently. It must deliver negative changes to this helper and
fence real permissions; changing a local timer alone fences no native call.
Current role instrumentation, barriers, original source checks, publication
lock checks and whole-profile journal/rollback proofs remain required. The
existing ten-second one-use challenge and current GUI behavior are unchanged.
