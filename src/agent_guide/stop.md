Interrupt or stop a session (hgs guide stop)

Ask the owner before stopping, killing or archiving a session you did not
start for this task. Check every exit code.

  hgs [@h] interrupt SESSION --json
      End the current turn and keep the session. JSON stdin: request_id,
      expected_run_id, expected_conversation_id and expected_turn_started
      (turn_started from inspect).
  hgs [@h] pause SESSION
      Idle agents only. Closes the agent and keeps the conversation under
      Saved; hgs [@h] resume SESSION -d restores it.
  hgs [@h] archive SESSION
      Move an already stopped conversation to Archive.
  hgs [@h] terminate SESSION --with-children
  hgs [@h] archive SESSION --with-children
      After the session itself, the same action for each of its live child
      sessions on that computer, each by its own rules; a refused child is
      reported, never retried. Without the flag no child is touched.
  hgs [@h] kill SESSION
      Last resort: ends the terminal session at once and forgets its binding.
      Native history stays, but Zerus no longer lists the session.
