# Windows Projects folder model

Agreed by fixes and migrate on 2026-11-01 for zerus-5wmh.66.2.31 and
zerus-5wmh.66.5.3.10. Qt behaviour comes from `MachinePlatform.h`,
`SessionOrganization.cpp` and `SwarmController.cpp::presentation`.
The Projects screen and folder dialog retain the approved mockup's layout.

## Storage and ownership

The existing device-local Layout v1 file has one optional family:

```json
{
  "local_windows_folders": {
    "version": 1,
    "node_id": "10000000-0000-4000-8000-000000000001",
    "projects": {
      "example-project": [
        {
          "id": "original-folder-id",
          "machine": "example-computer",
          "machine_id": "10000000-0000-4000-8000-000000000001",
          "path": "C:/Example/Source",
          "name": "Source"
        }
      ]
    }
  }
}
```

Its owner is the current local catalog node: watch hello `machine_id`,
`swarm::local_node_id` and the current organization `node_id` must agree.
The computer UUID in a Zerus session URI is a different identity. An alias,
a drive letter or UNC spelling establishes no ownership. A different or missing
current node leaves the family dormant; it never rebinds it.

Supported family and record extensions round-trip unchanged. Imported IDs and
path spelling stay original; new user-created IDs must be canonical UUIDs.
Duplicate IDs across the family and Windows-normalized paths within a project
refuse the whole change. Windows comparison and cleaning use `paths`, including
case/separator equality and rejection of drive-relative, incomplete UNC and
device paths. A peer's POSIX folder retains its peer provenance even when its
spelling resembles a UNC path.

`MAX_PROJECTS`, `FOLDERS` and Layout `MAX_BYTES` bound reads and changes. The
combined presentation cannot exceed the folder bound or collide with a shared
folder ID. No initializer or edit truncates a prefix. Strict Layout loading
refuses null, malformed or unsupported families; the file owner retains the
original bytes and disables that writer. Compatibility `Layout::from_json` is
never an authority to replace a refused file.

## Pure API

`client/core/src/swarm/layout/windows_folders.rs` adds these Layout methods:

- `has_local_windows_folders`: presence independent of emptiness/support.
- `local_windows_folders_state`: absent, active or dormant for an observed node.
- `initialize_local_windows_folders`: checks target presence first. Only when
  absent does it validate the whole candidate, owner and live project mapping.
  Inserted/AlreadyPresent are RAM results, not validity/durability/handled claims.
- `overlay_local_windows_folders`: returns a presentation copy for the exact
  current node and live listed projects, without changing shared input.
- `edit_local_windows_folder`: bounded metadata changes using the existing
  folder-edit policy with explicit Windows path semantics. Set requires the
  explicitly supplied Place machine ID to equal the proven current node; a
  missing or foreign owner is refused before applying or assigning that ID.
- `remove_local_windows_project`: explicit deletion metadata only; a transient
  missing observation never invokes it.

These methods perform no I/O, import, folder creation, open or launch. The
shared `folder_edit::apply` remains POSIX and refuses Windows paths. Native
integration must use the existing organization/layout writer and never send
the local overlay as shared desired or `swarm apply` input. Original records
remain stored while their project is temporarily missing.

## G3 boundary and qualification

G3 owns the later source reader and existing missing-field importer. It admits
the **whole absent family only**, through its versioned handled record. Existing
explicit empty, null, unsupported or nonempty target data wins. There is no
per-record missing merge, deletion journal or automatic reseeding.

G3 checks target absence before reading Qt. Only an absent target admits a
candidate. Every candidate row, owner and project mapping must then be validated.
G3 must qualify a durable family checkpoint, then handled evidence and crash
retry through existing stores (or one existing Layout commit if that record
fits). This model implements no atomic two-file transaction or new marker. An original explicit folder
`machine_id` matching the proven source and current local node can qualify.
A missing ID or ambiguous ownership leaves the source unhandled and its private
evidence unchanged. Backups and observed caches do not establish current-source
authority. This model supplies no G3 reader, admission transaction or replay.

Synthetic tests and Windows cross-compilation qualify the data model only.
Windows PC runtime checks belong to a later coordinated slice. This family
grants no filesystem, chooser, agent or session-launch authority; those positive
paths retain their existing qualification gates.


## Native read projection

The existing organization worker passes a copy to Projects only after a current
local source observation was adopted. Its node, swarm, digest, base and source
generation must match the source snapshot. A replaced source, new hello,
disconnect, failed/ended read or held observation invalidates this read proof.
The accepted revision must still be current, with a ready, settled controller,
no external operation, pending edit or queued recovery, and a writable Layout.
Repeated startup calls retain the same source instead of invalidating its proof.
The existing worker idle pass rechecks a later Live status without reading or
adopting a cached source. The Pump must simultaneously report a live local hello, explicit Windows
platform, successful listing and the exact same node. A cached or polled card
cannot establish ownership. No default platform or host alias qualifies it.

Only the Projects callback gets the overlay. Controller base/desired, Swarm
snapshots, shared apply, the grouping model and New session placement retain
the original shared catalog. Reading never removes dormant metadata. All local
Windows Add, Edit, Remove, Open, Browse, Clone and launch paths, including
metadata-only UI writes, remain fenced pending separate Windows PC native
adapter qualification. The pure metadata APIs expose no native writer command.
