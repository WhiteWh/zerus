Interrupt or stop a session (hgs guide stop)

Stop, kill or archive a session you did not start for this task only when
the person asks you. Check every exit code.

  hgs [@h] interrupt SESSION --json
      End the current turn and keep the session. JSON stdin: request_id,
      expected_run_id, expected_conversation_id and expected_turn_started
      (turn_started from inspect).
  hgs [@h] pause SESSION
      Idle agents only. Closes the agent and keeps the conversation under
      Saved; hgs [@h] resume SESSION -d restores it.
  hgs [@h] archive SESSION
      Move an already stopped conversation to Archive.
  hgs [@h] terminate SESSION [--expected-run-id ID]
      Stop a tracked session and keep its conversation in Archive. Its child
      sessions are not touched.
  hgs [@h] terminate SESSION --with-children
  hgs [@h] archive SESSION --with-children
      Only when the person asks: after the session itself, the same
      action for every live child session under it on that computer, at every
      level. A parent link names them, not who started them, so they may not
      be yours; add --dry-run first to list them. Each child keeps its own
      rules (terminate also moves a stopped or paused child to Archive); a
      refused child is reported, never retried.
  hgs [@h] kill SESSION
      Last resort: ends the terminal session at once and forgets its binding.
      Native history stays, but Zerus no longer lists the session.

Retire a finished sub-session

Reuse an idle child for its own follow-up. Retire a sub-session you started
as soon as its result is accepted (report read or PR merged or closed) and
it has no next task planned. Also retire a child you started after 24 hours
idle with no task planned. Never retire a session you did not start, one
still working, or one the owner uses.

1. Finish or cancel its open hgs wait first.
2. Run hgs terminate SESSION to keep the conversation in Archive. Use
   hgs kill SESSION only where terminate is unsupported.
3. For a worktree you created for it, use hgs worktrees review --path PATH
   --worktree WORKTREE. Check the worktree is clean and its branch is
   merged or pushed. Move any report an open task still cites to a durable
   place, then run git worktree remove WORKTREE. Keep the branch.
