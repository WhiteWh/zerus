# Bounded producer control-line I/O

The dormant internal `src/desktop/producer_io.rs` helper reuses the existing
`protocol::line` and owned desktop stream/reader. It adds no production caller,
controller, thread, listener, resolver, FD clone, semantic frame, command, store
or positive API. Existing desktop and passive feed framing/timeouts are unchanged.

Producer control lines are capped at 16 KiB including the final LF. A write
requires one nonempty line; it never splits, retries or truncates an oversized
payload. Reads preserve following bytes in the original BufReader rather than
discarding/replacing it or opening another reader. The host still bounds that
reader and validates UTF8, its closed schema, sequence, identity and phase.

Every operation takes one absolute monotonic deadline, with at most fifteen
seconds remaining (the agreed producer idle limit). It checks before each
partial read/write and after completion, and applies only the remaining time
to each syscall. Partial progress cannot renew the deadline. Already buffered
data is rejected after expiry without consumption. The dedicated control stream
retains its last timeout setting; each later admitted operation must set its own
deadline. Windows refuses these new operations before I/O pending qualification.

A successful write means local bytes were written, never peer acceptance,
current ownership, a native outcome or a durable receipt. A failed/late write
may have sent a prefix or a complete frame; it is no replay or resume permission.
An EOF means this connection ended, not that no instance exists or resources
drained. Errors leave ownership with the host; this helper never closes a gate,
releases a charge, checkpoints a store or restores GUI writers. Future control
must retain the actual reader/writer/worker through completion and apply the
existing unknown/prepared/frozen/revoke rules and original-ID reconciliation.

Private Unix-pair tests cover exact/over-bound lines, retained buffered tails,
expired buffered data, EOF/truncation, slow progress, blocked writes and refusal
before payload I/O. They are framing coverage only, not a producer handshake,
authenticated current lease, role proof or controller/native qualification.

## Terminal connection ownership

Any read error may leave a frame partially consumed, including timeout,
over-bound data or interruption after earlier progress. It is terminal for that
connection: never call `line` again or resume with another deadline. A write
error likewise leaves its prefix uncertain. The raw framing functions above
remain unchanged; the dormant `Channel` wrapper enforces this lifetime rule.

`Channel::new` consumes the host's existing `BufReader<Stream>`, retains its
buffered tail and reuses the existing same-user transport check. It resolves no
path, connects to nothing and clones no FD. There is no extract, reset, Clone or
serialization API. An error in either direction, or clean EOF, permanently
fences both directions before shutting down the supplied stream. Later calls
refuse even if bytes remain buffered or a new deadline is supplied. Drop shuts
down the stream and drops the owned reader. No production caller installs this
wrapper, and ordinary desktop/feed connections keep their current behavior.

The future off-actor host must supply a dedicated blocking stream, retain and
account any pre-existing aliases and actual worker/reader/writer lifetime, and
keep this terminal-error rule and the residual timeout requirements beside its
stream setup. Shutdown is never proof that other aliases, callbacks or workers
have exited. The same-user observation is not canonical-instance, installed
code, configuration, profile, role or current lease proof. No frame contents
are interpreted and no producer handshake, semantic request, gate or ownership
transition follows from constructing this wrapper. Windows refuses construction
before transport I/O. The five added private-socket tests qualify ownership,
buffer retention and terminal reuse refusal only; selected exact merged Mac
checks remain deferred under the standing internal-helper exception.

On Apple targets only, a valid read-timeout setup that returns `EINVAL` is
allowed to proceed to the read. XNU's
[sosetoptlock shutdown check](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/uipc_socket.c#L4491-L4496)
rejects socket options after both directions shut down; the receive path can
still drain queued bytes and report EOF without blocking. The original absolute
deadline checks remain before and after the operation. Other setup errors and
all write-timeout errors still refuse. This preserves clean EOF versus a
truncated frame, including through `Channel`, rather than treating the timeout
setup error as a frame result. The macOS socket test explicitly checks the raw
shutdown `EINVAL`; Linux tests exercise the narrow mapping and final-frame drain.
The local Mac is unreachable, so the new macOS behavior awaits the recut CI gate;
Linux checks are not native macOS qualification.
