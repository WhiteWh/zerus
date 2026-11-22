Start a session (hgs guide launch)

  hgs [@h] AGENT DIRECTORY --new -n TAG -d --launch-id "$(uuidgen)" -- "first prompt"

- AGENT is claude, codex or kimi; DIRECTORY is an absolute path on that
  computer, or a project name from hgs [@h] project ls.
- AGENT may also be dsh or opencode. Omit the first prompt and -- for those
  native-API sessions; they print a ready message and open in Zerus.
- -d detaches without switching your terminal; launches from inside a session
  require it. Terminal agents print "hgs: started NAME". Without --new, an
  existing name is reused. A resume never adopts its launcher.
- A new session gets its own identity and the selected computer's default
  account for AGENT (or --account ID), never your session's.
- Text after -- is the first prompt; resume never replays it. Kimi receives it
  once through guarded send after interactive startup. inspect shows
  launch_prompt status; stopped or uncertain delivery needs manual review.
- Early exits appear in hgs [@h] ls --json under ended_launches with launch_id,
  exit code and last screen lines. Answer a folder or hook trust prompt only
  at the person's request (a standing one counts) or for a member of a team
  whose confirm_trust setting is on.
- A proven live terminal caller becomes the parent of a new session it launches
  here, including native-API children. Terminal forks record their caller too.
  Adopt an existing child on this computer:
    jq -n --arg run RUN --arg conv CONVERSATION \
      '{expected_run_id:$run, expected_conversation_id:$conv, action:"adopt"}' |
      hgs parent SESSION --json
  RUN and CONVERSATION come from hgs inspect SESSION. "read" shows the link;
  "clear" removes it. Process proof remains the default for adopt.
- DeepSeek/OpenCode tool shells obtain a claimed identity with hgs self --json.
  New launches accept --parent "$(hgs self --json | jq -c .parent_identity)".
  For adopt, include that object as parent. It must exactly match a live local
  session's durable ID, run and conversation. Source "claimed" grants no
  operation or permission authority. --parent refuses existing sessions;
  peer launches need the claimed-parent-v1 marker and a parent live there.

Continue existing work:
  hgs [@h] resume SESSION -d          restore a Saved conversation
  hgs [@h] fork SESSION -n TAG -d     branch into a new conversation
