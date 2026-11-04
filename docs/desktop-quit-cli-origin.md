# Proposed native CLI and configuration origin

Status: design only for `zerus-5wmh.66.2.34.3.5`. No admission exception,
production anchor, helper launch, pairing build or installer change is implemented
or authorized here. The [quit-store design](desktop-quit-store-design.md) keeps
its initial explicit-selector refusal until a replacement is independently
qualified. R3's unresolved legacy-mutator exclusion remains a separate blocker.

## One shared pairing contract per platform

Coordinator direction is one native trust root per platform, shared with G1.
No version string, help marker, JSON response, path name, copied installation
record or user-writable provenance file can supply that root. Pair identity
supplies identity evidence only: it cannot mint a G1 role, TaskStart ticket,
quit-store capability, configuration origin, current lease or delivery authority.

### macOS: the existing G1 signed bundle

Reuse G1's [own-bundle selector](desktop-mac-installation.md), merged in #803,
and the accepted [paired local install design](superpowers/specs/2026-10-31-local-macos-paired-bundle.md)
under `.66.3.25.2`. There is no second Mac digest-pairing implementation. The
native selector independently verifies its own genuine, sealed signed pair at
`Contents/MacOS/hgs-tray` and `Contents/MacOS/hgs`, mapped-self identity and
unchanged generation, with exact kernel-peer observations. Its `Selection` and
borrowed `Observation` remain passive, point-in-time evidence, not an exec fence
or an upgrade lock. Use the same native source contract; do not extract a path,
requirement or serialized observation to reconstruct an equivalent permit.

Mac availability depends on `.25.2` landing and genuine running-self/signing
qualification `.25.1.2`, plus the separate origin, resource and current-connection
checks required by the quit-store design. Negative bare/ad hoc tests or a passed
signature command do not qualify that positive. No literal Team ID, alternative
installed-app scan or PATH candidate supplies a fallback. A stale, external,
unpaired, changed or unverifiable selection refuses durable quit. Existing
ordinary GUI/CLI choices retain their behavior.

The old maintained LaunchAgent passes `--hgs`; the paired-install proposal omits
that argument and keeps implicit sibling selection inside GUI state. This does
not erase the old original argument or create a general override exception.
The paired install's per-child stable `HGS_EXECUTABLE` rule still protects agents,
hooks and services from retaining a swappable bundle path. Those ordinary child
calls do not establish native helper origin or admission. The future native
quit helper must have its own qualified parent/child/connection custody within
the same G1-selected pair; do not whitelist an inherited marker by name.

### Linux: one build-paired image identity for G1 and quit

Proposed shared native module: a private constructor for a build-paired CLI image,
owned jointly with G1. G1 must reuse this contract for its later Linux pairing;
quit does not create a parallel G1 identity domain. No G1 operation ticket or
installation record is converted into quit authority.

The trusted build first produces the exact locked-dependency `hgs` artifact,
then embeds its SHA-256 and bounded format/version descriptor in the GUI build.
The embedding consumes the actual artifact, not a runtime digest override or
unverified digest supplied independently of that artifact. An unpaired source
build has an explicit unavailable value, never an ambient environment fallback.
Future build integration needs its own pinned workflow/security review. It is
not part of this documentation change.

Choose descriptor-based execution with a post-exec image check as one admission
transaction:

1. Capture the original raw OS environment and arguments before parsing,
   loader mutation, threads or selector I/O. Preserve presence, empty/non-UTF8
   values and all relevant prefixes. Keep this opaque in native memory, with no
   `Debug`, serde constructor or raw-value diagnostics.
2. Resolve the selected candidate using the original selection. Open the exact
   regular ELF image without following a replaced leaf, pin its directory/file
   identities and bound reads before hashing. Proposed limits are 256 MiB per
   image and 64 KiB streaming chunks; an oversized, special, unreadable or
   unsupported file refuses. Validate metadata/identity before and after hashing
   against the embedded expected artifact. A missing expectation refuses.
3. In an owned child, execute the pinned descriptor using Linux `execveat` with
   `AT_EMPTY_PATH`; never reopen the pathname or fall back to PATH. Unsupported
   descriptor execution refuses. An open fd alone does not exclude in-place
   modification, so it is not sufficient image proof.
4. Before allowing the child production anchor access, independently pin its
   birth and actual executed image through kernel evidence, including its
   `/proc/<pid>/exe` identity and bounded hash. Match it to both the pinned source
   descriptor and embedded expectation, with fresh generation checks. A hash at
   observation time does not prove the pages mapped at exec unless writes were
   excluded from exec through the complete check. That write-exclusion assumption
   must be proven on the actual running kernel and filesystem; descriptor exec
   and generic ETXTBSY expectations do not establish it. Unsupported or
   unqualified filesystems/kernels make this binding unavailable. Qualification
   includes a modify-then-restore writer spanning exec and hash, replacement,
   exec failure, child exit, hidden image and updater races. Keep the owned child
   unreaped during the check, with one custody owner for reaping, no auto-reap or
   SA_NOCLDWAIT, and a pinned pidfd for observations retained beyond the wait.
   Successful spawn or main-ELF hash alone is not successful loaded-image binding.
5. The genuine child captures its received environment before `Config::load`
   and cannot proceed to any production probe merely because it was exec'd.
   Its initial mode parks behind the reviewed one-use stdin/parent-observed birth
   and sealed-registration protocol. Actual worker, child, fd and pipe custody
   remains charged through timeout/cancellation until real drain, as the
   quit-store design requires. A decoded hello or claimed PID cannot open this
   gate. The exact wait/release handshake is a separate implementation review.

The image's descriptor, native observations and constructor remain private to
the shared module. Tests use a distinct typed fixture result that cannot create
native admission. No public function takes a digest/path/JSON and returns an
admitted identity. Hashes are comparison evidence, not caller-supplied authority.

There is an unresolved **initial admission ordering** gate on both platforms.
The quit-store design requires the GUI's initial summary/generation/resource
transaction before helper preparation or spawn. Image or configuration proof
must not be obtained by an uncharged origin-probe child before that transaction;
calling it a version check, using another ledger or reusing a G1 ticket does not
remove the resource requirement. Conversely, proof required before the GUI's
first anchor probe cannot depend on that not-yet-admitted helper. The descriptor
execution sequence above describes later charged execution and cannot by itself
authorize the initial anchor open. The concrete pre-anchor source/image/config
evidence and its relation to later actual execution need explicit G3/native
agreement and qualification. No already-live peer, static selection or passive
observation is presumed to solve that gap. Until it is resolved, initial durable
admission stays unavailable; this proposal authorizes no origin-only process,
connection or exception to the initial transaction order.
The next proposed boundary is finite in-process acquisition/read-set evidence
with zero child, connection or anchor operations: immutable raw inputs, pinned
image descriptor and bounded captured configuration bytes. Whether that evidence
can authorize anything remains a separate G1/G3/native decision. Later charged
execution must bind those exact inputs, never re-resolve them; no configuration
derivation is presumed available without its own shared bounded native contract.

The maintained Arch service currently launches `hgs-tray` without `--hgs`, and
the desktop entry adds `--sessions`. Current core discovery uses the explicit
environment choice, then PATH and installer folders, then a bare-name fallback;
none currently supplies the above native capability. Preserve that ordinary
behavior. A future qualified selector must pin its actual chosen image and
refuse any mismatch; it cannot silently choose a different CLI for durable quit.

## Original configuration and fleet derivation

Pair identity is necessary and insufficient. A future replacement for the
strict selector fence must bind the original selected image and actual immutable
configuration/fleet used by this entry. It may not strip overrides and take a
later permissive snapshot. Keep raw OS input classification before the GUI's
`--hgs` to `ZERUS_CLIENT_HGS` conversion and before `Config::load` writes
`HGS_STATE_DIR`.

Proposed native stages are `OriginalScope`, `PairedImageObservation` and
`ConfigurationObservation`; names describe private construction boundaries,
not current APIs. None is deserializable or constructible from peer input. An
observation alone cannot produce `ProductionAnchor`: that constructor still
requires the entire native origin and resource graph.

- Original HOME/XDG/default-profile checks and the complete selector/marker
  inventory in the quit-store design still apply. Explicit state/config/install,
  fleet, test, replay, lifecycle and unknown prefixed inputs remain refusals.
  No default-looking/empty/non-UTF8 override is reclassified as implicit.
- The only candidate future selector relaxation is an exact originally selected
  CLI already proven to be this platform's native pair, with the same qualified
  configuration/fleet derivation. Flag presence, equal version, equal path or a
  serialized digest is insufficient. Mac external overrides cannot substitute
  for G1's own sealed bundle. This proposal does not activate that relaxation.
- The parent records its native default derivation separately from original user
  observations. The parent constructs its child's arguments/environment. The
  child's pre-loader capture is only a consistency check that its received scope
  equals the parent's sealed native derivation; it is not independent evidence
  of the GUI's original inputs. That original provenance and the parent/child
  custody graph must be established separately. Caller JSON or an environment
  allow flag cannot label an override as a native derivation.
- Build an immutable native configuration object from a complete, bounded input
  manifest. Include state/config root derivation, original working directory
  where relevant, configuration and machine-catalog file identities/digests,
  effective local identity and fleet routing. The loader consumes the pinned
  captured inputs; a later reopen cannot silently select another configuration.
  Proposed admission budget is 64 input files and 4 MiB total, at most 128 KiB per
  file. Excess, uncertain includes, unreadable inputs or incomplete routing
  provenance refuse durable admission while ordinary CLI behavior remains.
- Configuration is user-editable data, not an authority file. Do not persist raw
  values in a public receipt or upload them. The native binding must also qualify
  indirect SSH/configuration sources and actual endpoint identity; an alias or
  self-reported fleet JSON is not proof. Until that full provenance is concrete
  and qualified, the affected route is unavailable. This document grants no
  local or remote positive exception from an absent field.
- Revalidate the original classification, pinned image/generation, configuration
  and native derivations at every startup, recovery and real-Quit entry, before
  any production anchor parent/stat/open/bootstrap probe. Revalidate again at
  each qualified helper/dispatch boundary after slow I/O. Never cache a permissive
  Boolean or borrow an earlier entry's observation. Changed/unknown context
  refuses; already charged resources follow existing unknown-custody retention.

The complete input-manifest acquisition and cross-process configuration
handshake still require a concrete pinned implementation design and fault/race
proof before positive admission. This proposal supplies no parser bypass,
installer authority, config migration or remote dispatch permission.

## Updates, refusal and presentation

A CLI-only update with a stale paired GUI makes durable quit unavailable. The
ordinary GUI and CLI remain usable; no mismatch triggers a downgrade, automatic
repair, fallback executable, agent restart or service change. Linux installers
and coordinator rollouts must install the built pair together once the mechanism
exists. Mac uses the existing paired installer and its signed bundle swap and
rollback; no second updater or pairing record is introduced.

This includes a running GUI whose embedded expectation becomes stale after a
CLI-only install or on-disk Linux pair change. Durable quit stays unavailable
until the pair matches and the GUI restarts. CLI-first coordinator rollouts may
pass through this truthful unavailable window. Do not add a restart prompt,
automatic update/restart or new skew surface.

There is no existing visible Quit notice in today's Tauri client. Current
drop-on-Quit logs through `action_delay::cancel_all` only, and a hidden window
cannot display a notice before the exit-flush acknowledgement or timeout closes
it. Current toasts and the pre-existing local stderr drop line, including its
bounded five-name group preview, stay unchanged until the qualified caller lands.

For that future caller, evaluate native availability freshly at group registration
and use the group's existing Undo-window toast note line for one fixed core Text
sentence, translated through the catalog including Russian, such as
"Quitting Zerus before this runs cancels it." It applies to the whole original
group and is chosen from that fresh evaluation, never a cached Boolean. Undo
windows registered during running-GUI pair skew use this same sentence. Do not
omit members, repack a group, show success or imply an action will survive Quit.

A refusal first discovered at Quit is diagnostic only: new origin reasons use
bounded closed codes, counts and digests, without paths, PIDs, process names or
private configuration values, alongside the existing local drop line. No new
pre-exit dialog/confirmation, exit delay or next-start notice is introduced;
pre-hold refusal has no durable record. A confirmation would need owner direction
and is outside this scope. After any partial hold or charge, retain denial/manual
recovery and unresolved custody; mismatch is not repair.

## Independent gates and proposed review sequence

1. Agree this shared Linux contract with desktop-u, G3 and webui/native/security;
   confirm reuse of the existing Mac contract. This is design agreement only.
2. Review the concrete native raw-input capture, bounded build embedding,
   Linux descriptor/actual-image binding and complete configuration handshake
   separately before code. Run private decoy/fixture controls first; no test
   opens the owner's OS-profile anchor or installed CLI/configuration.
3. Qualify both actual maintained launch shapes with private real built apps,
   immutable pre-loader inputs, fake/old/changed/alternate images, unavailable
   kernel observations, file/update races and zero **attempted** production
   parent/bootstrap/anchor probes on every refusal. Mac additionally waits for
   `.25.2` and genuine `.25.1.2`; private HOME alone is not keychain isolation.
4. Keep the quit store's namespace/VFS, helper/drain, R3 legacy exclusion and full
   cleanup matrix, same-or-earlier agreed-and-shipped release surface and exact
   native caller reviews independent. Completing `.34.3.5` cannot waive them or
   activate G1 current roles/producers. Windows remains fenced.

No positive availability, native execution or kernel qualification is claimed
by this source-only proposal.
