# Explicit waits between independent sessions

Agents can read compact coordination instructions with `hgs guide` and identify
their verified current session with `hgs self --json`. The optional fixed
[static hint](reference.md#static-agent-guidance) points tracked Claude Code and
Codex main conversations to that guide through SessionStart context. Tracked
Kimi Code main conversations receive one UserPromptSubmit JSON `message` per
conversation; its native replay retains the hint on resume and compaction,
while clear starts a new conversation and emits once. SessionStart output is
not used for Kimi. DeepSeek hosts started by hgs carry the same sentence for
hgs-owned main agents; native subagents and sessions opened in the host's own
UI do not receive it. An already-running host is never hot-reloaded. One
effective copy means the latest prompt, not every hint retained in history:
opting out does not remove old hint-bearing system messages, and re-enabling
can retain them beside a new one. Fresh and still-empty, unmarked OpenCode
conversations use an experimental per-session instruction entry, not
SessionStart or an Activity message. After-turn adoption
remains unqualified because OpenCode would persist an `Instructions updated`
system row in native history. Global and exact-account opt-outs affect new
injection attempts, while `guide` and `self` remain available. Hint policy is
resolved for each new process, including resume, and inherited environment
overrides are a process snapshot, not live configuration reload. Native
children and arbitrary commands are outside this hint channel.

An independent session is not a native subagent. Declare dependencies through
`hgs wait`. Prose, a submitted message, Beads status, a native Stop and an idle
producer do not declare or complete a wait. The protocol supports every provider.

The exact live, online waiting run stays in Active. Its open header shows
**Waiting for acceptance** or **Waiting for result** and elapsed time. Cards use
a compact **Waiting** clock badge, with the phase, producer and computer in the
description. Native work, live commands, approvals and errors keep priority.
Progress replies retain their unread identity while reply-only completion badges
and notifications wait. The composer stays available. Offline, stopped, archived
and reused identities never establish current waiting work.

Dependencies retain exact native identities at both ends. A producer reference
without a run names that exact conversation across its runs. Snapshots supply
four open-dependency previews per waiter; the durable journal provides bounded
pages for the complete record. No message body is parsed to infer a dependency.
The former desktop Graph visualization is [retired](retired-features/session-graph.md).

## Declare and hand off

Check the selected installed CLI's help for `hgs [@host] wait`. Older versions
cannot track waits. Remote calls probe the same installed executable before
dispatch, preventing an unknown command from launching an agent.

Copy a confirmed producer reference through the GUI or `hgs [@producer]
reference <session> --json`. Before sending an assignment, run `hgs wait begin
--json` on the waiting computer with JSON stdin:

```json
{
  "request_id": "33333333-3333-4333-8333-333333333333",
  "producer_reference": "zerus://session?machine=22222222-2222-4222-8222-222222222222&host=peer&name=claude%2Fexample%2Fworker&conversation=native-conversation&run=producer-run",
  "label": "Review the patch"
}
```

Use a new canonical UUID per declaration and retain it for a deliberate retry.
Normally hgs proves the caller against its native process ancestry; `hgs self
--json` returns that proven reference. An operator or API-host agent adds its
copied `waiter_reference`; hgs verifies the local computer, provider, saved run
and conversation. This is an explicitly authored dependency, not observed native
work. Archives, children, unconfirmed identities and self-waits are rejected.
Renames retain the dependency; a new waiter run or cleared conversation does not
inherit it.

The response contains public `wait` metadata, a `reply_host` routing hint and a private `reply_ticket` with
`wait_id`, `waiter_machine`, `expected_run_id`, `expected_conversation_id` and
`reply_token`. Include that ticket and the requester's configured computer route
in the assignment sent through ordinary guarded `hgs send`. Declaration sends
nothing. Preserve failed or uncertain delivery and inspect the original receipt;
never resend automatically. Cancel an abandoned declaration explicitly.

The ticket authorizes acknowledgement and completion of this one dependency.
It grants no native input or permission change. Share it with the intended
producer. Snapshots, metadata indexes, journal pages and public receipts never contain it.
An idempotent `begin` can recover its own ticket.

## Acknowledge, deliver and finish

The producer runs `hgs [@waiter] wait ack --json` with the ticket fields and a
fresh operation `request_id`. The phase changes from `awaiting_ack` to
`awaiting_result`. Use the waiter's configured SSH alias; its actual machine UUID
must match the ticket even when the alias differs from its reported host name.

Deliver the actual result to the original waiting conversation through guarded
`hgs send`. After confirmed submission, run `hgs [@waiter] wait finish --json`
with the ticket, a fresh operation `request_id` and that send's
`result_request_id`. The waiting computer checks its own durable receipt: exact
run and conversation, parent recipient, submitted state and a submission after
the declaration. Missing, uncertain, older or wrong-scope receipts keep the wait
open. Only body-free delivery identity enters this journal.

A result can finish a wait before a separate acknowledgement. Repeated
acknowledgements cannot reopen completed waits; cancelled waits reject late
results. Operation UUIDs bind exact payloads and produce transactional idempotent
receipts. Settlement never sends, resumes or retries a native conversation.

## Cancel and inspect

`hgs [@waiter] wait cancel --json` takes `request_id`, `wait_id`, `waiter_machine`,
`expected_run_id` and `expected_conversation_id`. This deliberate owner operation
needs no producer ticket and changes only the exact dependency.

`hgs [@waiter] wait list --json` takes the exact `expected_run_id` and
`expected_conversation_id`. It returns up to 50 records and `next_cursor`; pass
that cursor as `before` for older entries. An immutable insertion sequence keeps
pages stable during new declarations and state changes. Completed entries retain
only body-free result delivery evidence.

The private SQLite journal lives under the existing session state directory.
Listings share a read-only snapshot with indexed owner lookups, exact counts and
four-record previews. There are at most 1024 open dependencies per waiter.
Settled details expire after 30 days in bounded maintenance batches. Small
declaration tombstones and operation receipts preserve UUID no-replay guarantees.
Storage has a 1 GiB ceiling and fails explicitly at capacity. No GUI cache loads
the full journal.
