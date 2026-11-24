Test policy for a team (U62)

A task runs only the tests its change touches: hgs test affected names them
with their time; run them with hgs team run (the command hgs guide tests
gives), never a full suite. Ask the lead before a run over 10 minutes.

A hand-out names this rule. A phase integration runs the affected tests of
the phase's range.

Merges (task 174): with [merge_queue] on, an accept merges the reported
commit into the lead branch (the owner's integration_folder, else the team's
folder), runs its affected tests there and keeps it or returns the task with
the failure or the conflicting files. Nobody merges, builds or tests a
member's work by hand. A merger reviews the diff and the findings and
resolves the conflicts the queue returns; a phase integration reads the
queue's results (hgs test affected MERGE_TESTED..HEAD) and the release
candidate. Tauri: tauri-*; release-candidate.
The full run happens only before a release: ask the owner
first with the estimate; the owner sets the full_tests policy in the team
settings (never, ask, allowed) or grants one run for a release.

Weigh every test by real need before you plan or run it (owner,
2026-10-31): speed of delivery comes first. A test earns its run only if it
can fail for this change. Skip suites that cannot see the change, lint per
task (the integration runs it), a separate screenshot run (take shots in the
test run) and anything a later run repeats (the integration's or the
release's full run). Write the code first; build and test once at the end
of the task. A mechanical change (moves, renames, splits) builds and tests
once, after all its parts. The lead plans each task's tests this way.

A split keeps every test, counted before and after. A test that repeats
another becomes a case of a table test; a test that guards nothing (it
cannot fail, or its code is gone) is removed, with the reason in the report.
Never weaken a test or criterion to pass.


A performance run looks at the machine first: a perf suite (a `performance = true`
provider, or a bench/perf name) samples CPU, disk and the running Zerus builds
before and during. Over the `[performance]` limits it waits for a quiet window
(bounded by `wait_seconds`), then runs marked `loaded` with the numbers in its
result. Compare binaries alternating rounds (A, B, A, B), never all of one then
the other.
