# Bounded original launch-status observation contract

G1 accepted this contract on 2026-11-01 for G2 `zerus-5wmh.66.4.3.3`.
The command, capability response and caller described here are **not implemented**.
This document grants no current actor, source, kind, profile or executing
permission. Implementation needs separate exact source and private proof reviews.
Generic hgs configuration, help, remote routing, current Qt/creation listing and
launch producers retain their existing behavior.

## Full entry chain and deliberate first-slice narrowing

Current main handles only `--macos-session-service` and Windows `__host` before
`cli::dispatch`. An ordinary target read enters `cli::dispatch`, platform environment
preparation, desktop launch_configuration, `Config::load` and then its guard.
`Config::load` reads config with unbounded read_to_string, `machines::load` reads
machines.local.json with unbounded fs::read, sets HGS_STATE_DIR and may call
hostname. Generic --expect-machine uses local_node_id, which loads/saves Swarm.
The read-only lifecycle helper still allows 128 MiB of catalog.json before parsing.
Generic remote routing additionally loads source-host config/machines, then
launches SSH with its own configuration/key/known-host reads. None is covered by
a selected-session byte cap. The complete read contract must include preliminary
reads too.

The proposed command entry runs before those generic helpers, at the first line
of `cli::dispatch`, and returns before platform preparation,
desktop service configuration, `Config::load`, argument target routing, terminal
access or any generic guard. Main's two unrelated special forms do not match its
argv. No changes to generic loader, machine profile reader or existing guards.

Accepted exact target argv forms:

```text
  hgs --expect-machine M creation-launch-status --launch-id ID --json
  hgs --expect-machine M --client ORIGIN creation-launch-status --launch-id ID --json
```

M and ID are canonical non-nil UUIDs. ORIGIN is a bounded nonempty label <=80 bytes,
no controls/whitespace, validated but never identity/currentness evidence. It is
only the actual existing remote guard ordering, not a routing instruction.
No missing flags, duplicates, --dry-run, --read-only alias, stdin or trailing args.
Recognize the command by exact command/flag positions only. A keyword inside an
unrelated program or session argument must never select this entry.
Bare command / malformed recognized reserved forms fail with fixed diagnostics,
never fall through to default program launch. Direct `@host` forms for this command
are recognized and refused before `Config::load`. No SSH/remote controller exists
in this first slice. Supporting `hgs @alias ...` later requires its own bounded
source-host bootstrap/profile/SSH contract; ordinary existing @ routing remains
unchanged for all other commands. No remote end-to-end budget claim is made.
A separately reviewed caller may eventually use the target-side form on the
selected host; no caller/runner or remote invocation is added now.

## Safe additive capability preflight

The proposed exact `hgs --help creation-launch-status-v1` returns only
`creation-launch-status-v1\n`, success, <=256 bytes, with no config/environment
preparation/source opens. This reserved help form is intercepted at the same
entry. An older hgs sees a known-safe leading --help, never an unknown program.
Its ordinary help lacks this exact successful response. Ordinary --help behavior
is unchanged; its existing `Config::load` is NOT called by the new exact form.
A future automated caller must first run this on the SAME selected executable,
require exit 0, strict UTF-8, bounded output and an exact single marker line (no
substring/lossy decoding/malformed prefix), then invoke the target read within
its agreed 15-second operation budget. Failed/unsupported help means Unknown/no command
invocation/no ls fallback. Old help may internally read old config; that older
process has no new source-read qualification and its output/time must be bounded
by the future caller. This slice supplies no such caller or execution window.

## Explicit bootstrap and selected-source budgets

No config, machines.local.json, projects, alias catalog, SSH configuration,
provider home, native logs or arbitrary pathname is read by the new target entry.
Resolve only HOME (Windows USERPROFILE is not used: Windows is unsupported in
this first read), HGS_CONFIG_DIR, HGS_STATE_DIR, XDG_STATE_HOME and current_dir
through process environment/OS calls, using existing Config path formulas but
without `Config::load` or mutation. Config assignments currently consume only
HGS_SELF/HGS_PEERS/HGS_TAB/HGS_TAB_COLORS, none selects config/state roots, so
omitting that file does not replace an existing path override. Absent optional
overrides use the existing defaults; HOME must exist. Reject non-UTF8, empty or
oversized selected values rather than inventing a home.
Each selected env/path <=4096 bytes; aggregate selected path input <=16 KiB; resolved
absolute paths <=4096 bytes and <=32 normal components, no .. or symlink-following
canonicalization. Relative configured roots resolve against bounded current_dir,
then reject non-normal components. No hostname subprocess, process-wide env
mutation or default root creation. These restrictions apply only to the new command.

| Stage | Exact application metadata sources | Initial raw cap | Recheck/physical read cap |
| --- | --- | --- | --- |
| Capability | none | 0 | 0 |
| Bootstrap identity | config-root/swarm/catalog.json only | 1 MiB | same image at most once again, <=2 MiB + 2 sentinel bytes total |
| Selected observation | state-root/launches/ID.json and ID.end.json; state-root/name_routes/SHA256(start-observed exact name).json presence probe; state-root/SHA256(name).json only when route absent | shared 1 MiB (start/end each 8 KiB; binding <=1 MiB; route presence only, no route bytes parsed) | initial plus one whole-set recheck, <=2 MiB + 2 sentinel bytes total |

Total application metadata bytes physically read <=4 MiB + 4 sentinel bytes,
retained raw input <=2 MiB plus fixed 8 KiB recheck/hash scratch. Bootstrap and
selected caps are distinct; neither is omitted from a whole-read claim. Metadata
bounds are not decoded JSON/allocator/RSS/stack/kernel/page/whole-process bounds.
Every opened file's length and remaining raw budget is checked BEFORE parsing;
read remaining+one sentinel, refuse whole stage on excess, never accept a prefix.
No 128 MiB exception, unbounded read_to_string/fs::read, glob/read_dir, repair,
writer lock, directory creation, index rebuild or retry. At most ONE bootstrap
parse of already bounded bytes with existing `swarm::existing_node_id`, including
full catalog validation; no partial node-id extraction bypass. Serde duplicate
known fields/invalid/future schema refuse before projection; exact node_id must
match M BEFORE ANY selected-source open. Missing/oversized/unsafe/malformed or
mismatching identity is a fixed refusal with zero selected-source opens.

Each source is opened at most twice (initial and final recheck); no changed
source is refreshed, reparsed or retried. At most two identity-file opens, two
start, two end and two binding opens. Route is a no-follow held-directory
presence check; any present
route means Unknown, no route body/other binding/rename traversal. Pin at most
four existing directories: config-root/swarm, state-root, state-root/launches,
state-root/name_routes when present. Each path walk <=33 directory opens. At
most 16 calls to `Directory::open` (including every verify/recheck walk), charged
conservatively before each call against a total of 1024 application directory/file
open attempts; metadata/stat calls separately cap at 4096. Initial files stay open
through same-file/path checks; close-only handles release no lifecycle authority.
Any inability to stay within the counters refuses/Unknown, never silently omits
checks. Private proof will check exact instrumented/open counts and syscall
inventory against these ceilings, including empty roots and large sibling sets.

Reuse Directory's Unix no-link component walk; each opened selected directory
requires private/euid/type verification. read_private uses openat READONLY,
NOFOLLOW|NONBLOCK|NOCTTY|CLOEXEC then regular/private/euid/single-link checks.
Its opened metadata must be captured before/after bounded read, and original
file identity/mode/owner/links/length/ctime+mtime nanos/hash/presence compared with
the final path reopen, with held source roots verified before/after the whole
set. Recheck catalog identity/hash too; any observed drift means Unknown.
Missing paths are explicit and checked twice, never synthesized or created.
Ancestor path limits include verification walks, not only final leaf counts.
No transient-stability/atomic snapshot/writer quiet/CAS/current source claim.
All file operations must return; a 15-second invocation timeout is not kernel
cancellation or G1 reader-drain proof. No native agent/host/tmux/controller is called.

The initial reader is Unix (Linux/macOS) only. The Windows exact capability form
refuses Unsupported and emits no marker; the command refuses Unsupported with
zero source opens. No positive Windows filesystem
behavior or native execution claim. Linux actual path proof is required. Actual
private macOS path proof precedes any positive Mac qualification; cross checks
are not execution. No generic Windows/private-file helper behavior is broadened.

## Original index incompleteness and exact evidence

Index remains only the original canonical UUID start/end leaf names plus ONE
start-OBSERVED exact native name hash. No independent UI/current name, native
listing, host LaunchStatus call, archive/provider scan or persistent index.
The deliberately incomplete source scope is accepted: confirmed deletion,
pruning, rename route, host eviction or missing evidence remain Unknown, never
no-effect or retry proof.

Start requires exact ID, bounded nonempty native name<=512, finite nonnegative
started_at. Duplicate known keys and wrong/future consumed schema/types refuse.
Binding requires version 1, exact original launch ID and exact start/name/provider,
canonical non-nil run_id and actual nonempty confirmed conversation ID. First
binding projection conservatively accepts tracked root bindings of Claude and
Codex with canonical non-nil UUID conversation syntax only. Kimi/DeepSeek/OpenCode
and any other provider syntax/storage are Unknown in this first subset until
separately inventoried/agreed, never coerced into UUIDs or searched elsewhere.
Any expected_id/creation_pending/rename_shadow/rename/partial/future/contradictory
identity indicator present requires Unknown, not inferred confirmation. A start
locator alone cannot establish a binding. Existing read/read_run/repair are never
used. Matching binding together with ANY matching end remains conflicting/Unknown.

End observation requires exact ID; explicit i32-range integer code; finite
nonnegative ended_at; valid bounded NONEMPTY observed name (null stays Unknown);
draining ABSENT and private error ABSENT (false/null are not completion encodings).
Duplicate known fields/types refuse before Value. If start exists, ID and name
must agree. Private output/errors are discarded. No start means no binding can
be selected, NOT that a binding/session is absent. End observation is expressly
nonexclusive historical file evidence, never a G1 lifecycle/worker/FD/output drain
receipt, no-effect, settlement/release/rotation/relaunch permission.

Closed schema 1 reply variants, whole encoding <=16 KiB without clipping:

- Unknown: {schema,machine_id,launch_id,status:"unknown"}.
- Binding: {schema,machine_id,launch_id,status:"observed_binding",name,agent,
  run_id,conversation_id}; no current/running/alive/complete/effect permission.
- End: {schema,machine_id,launch_id,status:"observed_end",name,code,ended_at,
  binding_unresolved:true}; unresolved is ALWAYS true, never false. This explicit
  field prevents the historical exit observation from pretending exclusivity.

Fixed diagnostics <=256 bytes; no raw source/path/output/body/extras retained.
All missing/contradictory/partial/unsupported outcomes preserve original review;
no reducer/ID rotation/delivery/placement/release/current kind/profile/actor/caller.

## Proof before later exact implementation reviews

This is a source-only contract. Implementation qualification requires a private
scratch environment/HOME/HGS/XDG/TMPDIR/TMUX and private D-Bus/Xvfb for actual CLI
tests. No owner paths/bus/
installed apps/services/native agents/mobile/tmux. Verify early entry bypasses
`Config::load`, `machines::load`, generic guard and platform/desktop preparation for
both positive and malformed reserved forms. Huge/invalid config+machines decoys
must never be opened; oversized/bad/changed catalog must refuse before selected
opens. Empty dirs/missing catalogs stay byte/inode/path-identical with no new
state. Raw successful help strictly matches; old/malformed/failed/overcap probes
never qualify a future call (pure decoder tests only, no caller added).

Actual opened-source bytes/attempt counters, all metadata/sentinel boundaries,
unsafe files/links/FIFO/private ownership, duplicate keys, source/root replacement,
route presence/change, start deletion/pruning, null-name/draining/error ends,
conflicting binding/end, provider mismatch/unconfirmed binding. Large sibling
sets with count-independent source opens and zero enumeration. Retain raw hashed
logs/executed counts; pin exact optional syscall utility/script BEFORE injected
children. Current Qt/creation ls path and all producer bytes/writers unchanged.
Fresh pinned G1 and webui implementation **and** proof reviews precede an ordinary
merge into `integrate/design-polish`.
The narrowed target-side entry, full bootstrap/read counters and nonexclusive End
contract are fixed by the accepted agreement. A material boundary change needs
fresh agreement before code. An implementation is not qualified by this document.
