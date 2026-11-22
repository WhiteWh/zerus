# Person requests: request-bound person authority

Status: design accepted 2026-11-18 (`zerus-46l1q`); S0 (text) lands with this
file, later slices cite it. Owner direction 2026-11-18: an agent may change
anything at the person's request; the owner's decisions of that day are at the
end. References are to main `29b256361`, before S0 changed the guide text.

## Rule

When the person asks a session for something in their own message, that session
may do it as the person, person-only actions included. A message relayed by
another agent, a task dispatch, a team hand-out, a timer, recovery or a resume
grants nothing. A child or team member acts for the person only when the person
messaged it directly. Sign-in, passwords, keys and payment stay with the person.

A standing request in the person's own message ("from now on ...", "always
...") counts until the person withdraws it. A team's Confirm folder trust
setting (`confirm_trust`) is the person's standing choice for its members' trust
prompts: folder trust today, Codex hook trust with `zerus-f67qp` (decision 4).
The person may ask an agent to turn automatic tool approvals on (decision 2).

Until S1 the rule is an instruction (AGENTS.md, CLAUDE.md, `hgs guide`). Where a
product check below still refuses, the agent reports the refusal to the person
and never routes around it (no detaching, other service managers, accounts or
GUI helpers). A standing request is an instruction only: it makes no person
turn (below), so where a product check needs one the agent reports the refusal.

## What refuses today

P = product check, I = written rule only.

| # | Refused | Check | Where |
|---|---|---|---|
| T1 | any `hgs task` from an unmarked process below a marked agent | ancestry walk for `HGS_SESSION`/`HGS_RUN_ID` (kept) | `src/tasks/person.rs:7-18`; `src/tasks.rs:391`, `:421`; `src/cli.rs:2000` |
| T2 | an unproven DeepSeek tool call as the person (kept) | `hosted_agent_call()` | `src/tasks.rs:414-416` |
| T3 | `task cutover --hold` | actor must be Person | `src/tasks/cutover.rs:38-39`; `crates/replica/src/project.rs:168-169` |
| T4 | `task cancel-attempt` | same | `src/tasks/cancel_attempt.rs:64-69` |
| T5 | `task review-copy --confirm` | same | `src/tasks/copy_review.rs:29-31` |
| T6 | `task start-plan`/`start-deliver` (window Start protocol, kept) | same | `src/tasks/dispatch.rs:265-267` |
| T7 | `task cancel` of a task sent to the caller | executor is caller | `src/tasks.rs:1942-1948` |
| T8 | ordinary commands on a live team's task | `TaskActor::Agent`; a CLI Person passes at `:55-57` | `src/teams/tasks/policy.rs:69-74` |
| T9 | `--target` on a task an agent files | own session only | `src/tasks.rs:1162-1164` |
| M1 | 16 `hgs team` verbs, for every CLI caller | first word, `person_gesture_required` | `src/teams/cli.rs:34-55`; `src/desktop/teams/person_actions.rs:13-18` |
| M2 | other team mutations by a CLI Person | caller is Person | `src/teams/cli.rs:61-62` |
| M3 | a Person write without a window permit | writer gate | `src/teams/ops/apply.rs:119-120` |
| M4 | person-only operations | operation table | `src/teams/ops/authority.rs:21`, `:26-28`, `:35-36`, `:42`, `:71-85` |
| M5 | `team preview on/off` | caller is a Session | `src/teams/mod.rs:119-123` |
| M6 | `team catalog`, `template`, `base`, `waiting` | actor must be Person | `src/teams/cli/actor.rs:192-196` |
| M7 | ending a stage, the person's hold, resuming what the person paused or stopped, hard acceptance gate, initial plan | per-operation check | `src/teams/ops/decisions.rs:172-175`, `queue.rs:318-322`, `team.rs:114-115`, `members.rs:153-155`, `phase_controls.rs:147-150`, `rework.rs:25-28`, `publication.rs:90-92` |
| M8 | an unmarked command under a live agent or host (kept) | `agent_above` | `src/teams/caller.rs:241-249` |
| M9 | standing confirmation covers member folder trust and self-exit resume, not hook trust | delegation flags | `src/desktop/teams/confirm/policy.rs:90-99`, `engine.rs:223-232` |
| A1 | `hgs answer` to Codex sandbox setup | `agent_caller()` | `src/state/codex_sandbox.rs:194-197` |
| A2 | `hgs answer` to a Kimi cache choice other than Compact, or not by the launch parent | same | `src/state/kimi_cache_hint.rs:314-318` |
| A3 | forwarded answers to A1 and A2 | question ID prefix | `src/cli.rs:2033-2040` |
| F1 | `hgs @peer task`, `team`, `question-routing` | `PERSON_ONLY_FORWARDED` | `src/cli.rs:1935`, `:2003-2010` |
| F2 | one child's `question-routing` switch | `agent_refusal()` | `src/state/question_routing.rs:147-158` |
| F3 | `mobile-peer` forwarding (kept) | `agent_caller()` | `src/mobile_peers.rs:1436`, `:1461` |
| D | window-only controls: automatic approvals, question-route switch and latch, maintenance policy, Terminate+Remove other-program stop, keyless input release, notification control, ownership, cache maintenance | native frontend permit | `client/core/src/desktop/ui_requests.rs:24-138`; `src/desktop/ui_authority.rs:710-739` |
| I1 | "Only person accepts Done" (contradicted `src/tasks.rs:82`) | none for ordinary tasks (`crates/replica/src/task.rs:1102-1126`) | `src/agent_guide/tasks.md:28` |
| I2 | "Agents cannot cancel dispatched tasks" | only T7 | `tasks.md:33` |
| I3 | stop, kill, archive and `--with-children` need the owner's permission | none | `index.md:29-30`; `stop.md:3-4`, `:20` |
| I4 | folder trust needs the owner; only the person trusts hooks | none for `hgs answer` (`src/state/questions.rs:971-1020`) | `launch.md:18`; `docs/reference.md:836-837` |
| I5 | startup trust and permission changes remain manual | D only | AGENTS.md and CLAUDE.md, automatic approvals paragraph |
| I6 | Teams window confirmation and the team pages' Never lists | M1-M7, T8 | `team.md:32-37`, `:49`; `team-lead.md:42-43`, `:51-54`; `team-planning.md:42-43`; `team-rework.md:34-35` |
| I7 | cancel-attempt and plans are the person's | T4, T6 | `tasks.md:26`, `:35`, `:37` |
| I8 | commit, push, gateway, status semantics, Stable publication | none; already at request | AGENTS.md and CLAUDE.md |

Trust, approvals and sign-in are kept out of the parent question route
(`src/state/parent_link.rs:267-289`, `src/desktop/question_route.rs:16-18`); that
is routing, not a refusal.

## Who started a turn, today

Zerus cannot tell that a turn was started by the person.

- Every `hgs send` caller reaches one receipt writer. The request is
  `deny_unknown_fields` (`src/state/input.rs:47-59`); `Origin` is never stored
  (`:1724-1729`); the receipt has no sender (`:2194-2196`). The session record
  keeps only the last send (`:2219-2224`).
- An agent's send is recorded in `graph/sends.sqlite3` only when its caller
  proof holds (`src/state/session_graph/sends.rs:272-300`); otherwise it goes
  through with no sender (`src/cli.rs:885-895`) and looks like the person's.
- Positive person evidence exists at admission only: the window's deferred-send
  frontend admission (`src/desktop/deferred_send/runtime.rs:291`) and the phone
  connector's journal. The window's direct route
  (`client/src-tauri/src/composer/send.rs:1538-1543`) and terminal typing leave
  none. The deferred-send table also holds service question routes
  (`runtime.rs:420`).
- Prompt hooks set `input_seen_at`, `turn_started` and `prompt`
  (`src/state/journal.rs:1634-1657`) without a request ID. Only Kimi's first
  typed turn is bound to its request by text
  (`src/state/input/acceptance/kimi_order.rs:57-75`).

## Design

**Person message mark (S1).** Written only where Zerus admits a message the
person typed: at the deferred-send frontend admission, in the same transaction
as its original row; later the phone send (connector-service caller proof), the
window's direct route and the creation dialog's first prompt. Fields:
request_id, machine, session_id, run_id, conversation_id, origin
(desktop|phone), text SHA-256, time. Bounded and indexed. Question routes,
`hgs send`, launches, hand-outs, dispatches, timers, recovery and keep-warm
never write it.

**Person turn.** The prompt hook sets `person_turn` on the session record when
`input_pending_request` is a marked message for this run and conversation, the
prompt hash equals its `submitted_text` hash, and `input_seen_at` is later than
`input_pending_at`. It is cleared by the next receipt from anyone, by any later
prompt other than the agent's own subagent or background-job notice (a
`peer_message` counts as another sender), by a run or conversation change, and
after 12 hours at most (decision 1). Unknown cases fail closed.

**Use.** `caller::person_request()` runs the existing session proof, then
requires a live person turn and the matching mark, and returns a non-Clone,
non-Deserialize `PersonRequest`. Writers accept it where they now require
Person or the window permit and recheck it under their own lock, as the window
`Permit` is checked (`src/teams/ops/apply.rs:119`). Each writer stores
`person_request {machine, session_id, run_id, conversation_id, request_id,
origin}` with the acting session: tasks keep `Actor::Agent` plus this field (no
new Actor variant in replicated documents); Teams adds
`PersonOrigin::AgentRequest` (`src/teams/ops/types.rs:32-35`); answers add it to
the question receipt. `hgs self --json` shows `person_request`.

**Not passed on.** No environment variable, flag, file or `--agent` field
carries the token. A turn started by a parent's send or a hand-out has none.
Forwarding to a peer (F1) stays refused until S6.

**Limit.** Not a boundary against a same-user agent that writes Zerus state
directly; the same level as today's ancestry checks. It removes accidental use
and leaves evidence. The window gesture stays for decisions made in the window.

## After each slice

| # | After |
|---|---|
| T1, T2, M8, F3, T6 | kept |
| T3-T5, T7, T9, T8 | accept `PersonRequest` (S2); replaces S2b |
| M1-M7 | the same writers admit the window permit or `PersonRequest`; the CLI front door admits the person's own terminal outside agents, as for tasks (decision 3), and a proven Session holding a `PersonRequest` (S3) |
| M9 | Codex hook trust of Zerus-created team worktrees joins `confirm_trust` (decision 4, `zerus-f67qp`) |
| A1-A3 | accept `PersonRequest` (S4) |
| F1 | S6 (the request must cross SSH and be re-verified) |
| F2, D | CLI routes under `PersonRequest` (S5), turning automatic approvals on included (decision 2); Terminate+Remove other-program stop and keyless input release stay window-only |
| I1, I3-I5, I8 | text changed in S0 |
| I2, I7 | text changes with S2; I6 with S3 |

`zerus-63rru.31.4` (S2b) and `zerus-63rru.31.1` are superseded. The rest of
`zerus-63rru.31` becomes "window permit or `PersonRequest`" at the same writers.

## Slices

Sizes: S < 300, M 300-1000, L > 1000 changed lines.

1. S0, text (S): AGENTS.md, CLAUDE.md, `hgs guide` index, tasks, launch and
   stop, and `docs/reference.md`; this file.
2. S1, desktop mark and person turn (M): Linux and macOS, local sessions;
   tests through the real deferred-send and hook writers.
3. S2, tasks (S): T3-T5, T7-T9 with history fields; I2 and I7 text.
4. S3, Teams (M): writers, front door (the person's terminal and
   `PersonRequest`), preview, catalog, template; I6 text; closes
   `zerus-63rru.31`.
5. S1b, phone mark (S-M).
6. S4, answers (S).
7. S5, window-only settings, automatic approvals included (M).
8. S6, remote (L).

## Owner decisions (2026-11-18)

1. A person turn lasts until anyone else's input reaches the session, at most
   12 hours.
2. An agent may turn automatic tool approvals on at the person's request (S5;
   window-only until then).
3. The person's own terminal outside agents acts as the person for Teams again,
   as for tasks (S3).
4. Codex hook trust of Zerus-created team worktrees joins the `confirm_trust`
   standing confirmation (`zerus-f67qp`).
