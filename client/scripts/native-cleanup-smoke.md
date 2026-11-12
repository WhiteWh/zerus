# Native cleanup smoke

`native-cleanup-smoke.py` exercises the built production GUI's ordinary
Terminate / Remove working folder path. It complements the replay smoke in
`termination-cleanup-smoke.py`, whose fake CLI deliberately cannot establish a
native ownership origin.

Run on Linux with bubblewrap, util-linux `unshare`, Xvfb, D-Bus, tmux,
ImageMagick, Tesseract and its English model. Supply prebuilt, same-source
CLI and canonical `com.hgdev.hgs-tray` GUI binaries from a clean commit. Build
outside the namespace, using only your own worktree's Cargo target directory.
The smoke never builds or installs toolchains.

```sh
python3 client/scripts/native-cleanup-smoke.py \
  --app /path/to/your/prebuilt/hgs-tray \
  --hgs /path/to/your/prebuilt/hgs \
  --tessdata /usr/share/tessdata \
  --output artifacts/test-results/native-cleanup/run-1
```

The output directory must be new. Repeat with a new output directory for an
independent walk. An English model can also be supplied from a private directory;
the input paths and SHA-256 hashes are recorded.

The outer `gui_sandbox` masks `/tmp` and `/run/user/<uid>`, binds the person's
home (by `HOME` and by the passwd entry) read-only, and isolates network and
IPC. Only the walk's own scratch root stays writable. Inside it, `unshare --user --map-current-user --pid --fork --mount-proc`
starts a dedicated PID-1 reaper with umask `0077`. Its children receive an
`env -i` environment, private HOME/config/state/tmux and a short TMPDIR below
`/run/user/<uid>`. X11 is mandatory; the owner Wayland runtime is hidden. A
verifier checks the namespace identities and reaper before launching hgs or the
GUI. It also checks that `DISPLAY` is a local display served by this
namespace's own Xvfb behind the private `/tmp`, and that the person's home is
not writable. There is no unsandboxed mode. The script refuses `python -O` and
`PYTHONOPTIMIZE`, and its safety checks are explicit, never bare asserts.

It runs as a manual qualification, not in CI. See
[Manual native cleanup qualification](../../docs/ci-and-aur.md#manual-native-cleanup-qualification)
for when to run it and which evidence to keep. CI's `test_native_cleanup_smoke.py`
pins what the walk reads and calls, and its safety margins, to the product
sources.

The maintained updater stages the GUI into the private HOME's installed path.
The updater runs only inside the namespace. Its pre-staging census records that
no owner GUI is visible. Read-only outer witnesses hash the owner's real
`~/.local/bin` and `~/.local/share/hgs/build-provenance.json` before and after
each walk, and record the running installed GUI's PID, birth and executable
hash. A walk fails if any of those witnesses changes.
This matters for sealed own-helper authentication: a memfd-executed helper has
no installation sibling. A real private passive desktop service establishes its
store, then normal GUI startup performs a real native takeover. The smoke
checks the resulting durable native ownership status and the same original
request/journal/revision in every program-stop record. No ownership database,
receipt, manifest or native permission is fabricated. Replay and PerfHost guards
are unchanged. XTest and screenshot OCR operate the ordinary confirmation.

Real hgs creates four owned generated worktrees and launches a hook-speaking
fake Claude through its production launch path. Each checkout also has a private
sleeping occupant. A tracked Cargo manifest and ignored `target/CACHEDIR.TAG`
declare the synthetic generated cache; naming a directory `target` alone does
not authorize deletion.

The walk checks:

- A conversation change after the offer refuses the captured action, preserving
  the current agent, occupant and working folder.
- The confirmation shows the other-program count and its expandable PID list.
  Confirmed Stop ends the proven occupants and removes the generated checkout,
  preserving its branch and archive ownership identity.
- Local data introduced after the offer keeps the folder after program stop.
- A fixture Git wrapper delegates real deletion, then returns failure after its
  effect. The native cleanup result remains Unknown. Original-ID status reads
  and reopening review leave the journal and deletion-attempt count unchanged.

Before and after every confirmation the smoke saves the complete bounded PID
census, including birth, cwd, argv and PID namespace IDs. Native original/result
journals bind all program targets to the checkout. A proof file verifies that
every protected identity survives; transaction helpers that exit naturally are
listed separately. Synthetic agents and occupants log received SIGTERM with
their PID, birth and checkout. No Force is requested. A bounded 100 ms monitor
checks all visible processes' PID namespaces and `pid_for_children` throughout
the walk, refusing any observed inner PID namespace. Processes outside the
private PID namespace have no routable PID within it.

Evidence includes screenshots/OCR, private state, native journals, census and
signal proofs, input hashes, a summary and `SHA256SUMS`. The namespace exits
before scratch is copied and removed on success or failure. The script does not
touch the owner's GUI, display, CLI installation or live sessions.
