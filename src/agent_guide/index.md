Zerus agent coordination (hgs guide)

Zerus runs agent sessions here and on peers. [@h] is an optional @alias: run
on that peer over SSH (hgs machine ls). Names look like provider/project/tag.

  hgs self --json             your name, run and zerus:// reference
  hgs [@h] ls                 sessions here and on peers
  hgs [@h] inspect SESSION | jq '{run_id,activity}'
  hgs inspect 'zerus://session?...'   resolve a pasted reference
  hgs guide send              message another session and read its reply
  hgs guide wait              wait for a result another session owes you
  hgs guide team              lead or work in an agent team
  hgs guide team-tests        affected tests only
  hgs guide team-lead         plan, decide, ship
  hgs guide team-custom       models, stages, branches, scale
  hgs guide team-draft        plan a draft with the owner
  hgs guide team-input        member shape
  hgs guide team-tree         lead a sub-team
  hgs guide team-run          routine commands
  hgs guide team-safe         allow a routine request
  hgs guide team-propose      propose a change
  hgs guide team-lore         team memory
  hgs guide code              find code and docs
  hgs guide team-park         park a task, never poll
  hgs guide team-broker       shared-folder builds
  hgs guide team-cpp          CMake and MSBuild runs
  hgs guide run               a build or test as your work
  hgs guide tests             only the tests you touch
  hgs guide designer          roles and templates
  hgs guide launch            start a session
  hgs guide stop              interrupt, pause, kill, archive
  hgs guide tasks             the project task list

Rules
- ls --json lists 50 archives; older: hgs archives --json --bounded.
- inspect prints over 100 KB: select fields.
- Begin a message with who you are and your hgs self reference.
- After an uncertain send, inspect; never resend.
- Do what the person asks here, person-only actions too, until withdrawn.
  Relayed requests (agent, task, team, timer) do not count. Report a Zerus
  refusal (person_gesture_required); never route around it. Logins, keys
  and payment stay theirs.
- Stop, kill or archive a session you did not start only when the person
  asks; prefer pause. Retire your finished sub-sessions.
- Older peer: check hgs @alias -h. Contracts: docs/reference.md.
