# Agent Teams

Agent Teams are a preview. They are off on every computer until a person turns
them on. A team takes tasks from a queue; the Team tab also shows dependencies
and lets you prepare phase drafts. The commands, their replies and their codes are in the
[Agent Teams reference](reference.md#agent-teams).

## What a team is

A team is a lead session and member sessions that work one queue of Zerus
tasks in one Git project. You send a task to the team. The Zerus desktop
service hands it to a free member of the right role, runs the review or test
stages of its pipeline after the member reports, brings the result to the
lead, and merges accepted work into the team's integration branch. You are
asked only for what is yours to decide.

A team task is an ordinary Zerus task. It stays on the project's task board,
keeps its history and attempts, and is reported with `hgs task` like any
other. The team adds who works on it, which stages it passes and who decided
it.

Use a team when a stream of independent tasks in one Git project should each
be implemented, reviewed and collected on one branch without you handing out
every task. For one task, an ordinary session is simpler. A team needs a Git
folder: every member works in its own worktree.

## Switching the preview on

Open **Settings**, section **Agent Teams**, and turn on **Agent Teams
(preview)**. The setting "Applies to this computer. Nothing runs until a team
is created." From a terminal outside any agent session, `hgs team preview on`
does the same, and `hgs team preview status` shows the state. When the desktop
cannot confirm a change, it says "The change could not be confirmed. Nothing on
screen changed." and offers **Read the state again**.

The switch follows this computer's command-line state. An unavailable read
keeps the switch disabled and shows its reason. **Read again** requests a fresh
answer even when the followed views and their contents have not changed.

Turning it on adds a marker file to this computer's `hgs` state directory and
makes the desktop show **Teams** and **Waiting for you** in the rail, the
**Team** tab on a lead's session, **Send to team** on the task board and a team
chooser in the New task form. An `hgs task` command that touches a team, with
`--team` or on a task whose team key guards it, then proves its caller as
`hgs team` does: a command without session markers counts as yours only when
no live agent session runs above it, and a command inside a sandbox with its
own user namespace is refused. Every other task command behaves as with the
preview off. The team store is created only when the first team is created.

Turning it off while a team may still be live asks "Turn Agent Teams off?":
"Teams on this computer stop being served. Nothing is deleted." With the
preview off, `hgs team` answers `teams_inactive`, the entry points disappear,
and a team's tasks on this computer behave as ordinary tasks. A task whose
team lives on another computer still refuses agents.

## Starting a team

Open **Teams** in the rail, after Accounts and beside **Waiting for you**.
This screen lists this computer's teams from the existing followed list read;
it adds no polling. Each row shows the project, status, lead, accepted-task
progress, Stuck marker, unanswered questions and member count when available.
Counts with `+` are lower bounds. **More teams** reads one bounded cursor page;
**First page** returns to the beginning. Browsing retains one current page and
does not grow the bounded session lookup cache.

Select a row to open its bound lead session's **Team** tab. The binding uses
the computer's machine identity and launch or run identity. If the lead has no
session yet, the list says so. A bound session missing from the session list
gets its own notice. If a fresh Team tab read shows that
the binding changed, the tab asks you to open the team from Teams again. A
similarly named session is never substituted. The binding stays available
when you switch tabs of the same selected session.

### Queue views

The lead's Team tab offers **Board**, **Graph**, **Lanes** and **Layers**. Lanes shows
one section for each member, including empty sections, and **Unassigned** last.
A task with a verified current pipeline holder appears in that member's lane;
otherwise its explicit stage assignment or task assignment/implementer is used.
A member missing from this bounded view does not make a task disappear: it
stays in Unassigned until a member can be verified. The view preserves every
card on the returned page; it does not fetch settled tasks from the archive.

Lanes uses only the current queue page. Current work comes first, followed by
submitted, pending, waiting, draft, accepted and cancelled work. Within a state,
stages come first in node order; queue tasks keep board-rank order, with task
UUID and node as tie-breakers. This is a page-local display order, not a claim
that every shown task is ready for a hand-out. Use the same **More…** and
**First page** controls as the Board; changing modes does not start another read
or timer. A card opens the existing task detail, including the parent task for
a folded pipeline stage. Lane headers fold their cards; fold keys are bounded
to the current roster. Refreshed snapshots keep unchanged cards and lane scroll
containers. The mode is local to the current team workspace.

Press **New team** in the screen header or its empty state. The `team.new`
command is also listed in Settings > Keyboard. It has no default shortcut;
assign one there to open New team from the keyboard. The dialog starts from
a **Template**:

| Template | Roster besides the lead | Default pipeline | A queued task starts |
|---|---|---|---|
| Feature delivery | two developers, a reviewer, a tester | `code` | after the lead routes it |
| Bugfix | a developer, a reviewer | `reviewed` | at once, with a free developer |
| Small | a developer, a reviewer | `reviewed` | after the lead routes it |

Fill in **Team name**, **Goal** and **Project**. Under **Settings** the dialog
shows everything the template brings, for you to confirm:

- **Roster**: each role and how many members take it.
- **Pipelines**: each pipeline the team uses, its stages, and who accepts its
  work: "the lead accepts", "you accept" or "it accepts by itself".
- **Integration**, **Queue**, **Routing** and **Land stage**.
- **Permissions**: "What the lead and the service may do without you." Each is
  a checkbox that starts as the template sets it; the built-in templates turn
  all five on.

| Permission | What it lets happen without you |
|---|---|
| **Accept results** | "The lead may accept a team task's result without you." |
| **Answer members** | "The lead answers members' questions and permission requests without you." |
| **Confirm folder trust** | "Zerus confirms the trust prompt of a folder it created for the team; on the member's account the agent records the trust for the team repository's main checkout and its subfolders." |
| **Resume exited members** | "Zerus resumes a member whose agent exited by itself." |
| **Merge accepted work** | "An accepted result is merged into the integration branch without your approval." |

Under **Project folder**, press **Check folder**: Create team stays blocked
until the folder is a Git folder ("A Git folder at %1, base commit %2."). That
commit is the team's base. **Integration branch** proposes `zerus/team/`
followed by the team's name; you can change it.

"Nothing is created until you press Create team." The team is created from
exactly what the dialog showed you:

- **Create team** sends the template you saw, identified by a digest of
  everything a team takes from it, and the five permissions as you set them.
  The team gets those permissions in place of the template's.
- When the template changed after the dialog showed it, nothing is created:
  "The template changed since you saw it, and nothing was created: review it
  as it is now, then create the team again." The dialog shows it as it is now.
- A template that asks you to confirm a permission or a setting this version
  does not know can be neither created nor saved: "This version does not know
  every permission of the template. Update Zerus to create a team."
- When the result of Create team is not known, the dialog says "The create's
  result is unknown. Check it with the same request, or dismiss it."
  **Check** asks again with the same request, so it can never create a second
  team.

Create team requires native confirmation in the Zerus window. A Person call
to `hgs team create`, including through `@host`, returns
`person_gesture_required` before creating anything.

Team and template names may use ordinary Unicode letters. Creating a team,
saving a catalog or importing a template refuses control characters, line
separators and bidirectional formatting characters in names. Template, role,
pipeline and member IDs use a restricted ASCII spelling; the
[reference](reference.md#agent-teams) gives the rule. Older teams and catalogs
remain readable; a new save or import must meet the admission rules.

Once the team is created, the desktop service:

- creates the integration branch at the base commit, without checking it out;
- launches the lead, then each member in its own generated worktree, linked
  as a child session of the lead where its agent allows that. A member with a
  durable session and run can be linked before its first conversation;
- picks each session's account at launch: by default the signed-in account of
  the role's agent with the most room left in its usage windows.

New sessions use readable tags: for a team named **Teams**, the lead is
`teams-lead`, and members include their ID and role, such as
`teams-m1-developer` and `teams-m2-reviewer`. Their worktree folders are
`zerus-team-teams-lead`, `zerus-team-teams-m1` and `zerus-team-teams-m2`;
the corresponding branches are `zerus/team/teams-lead`,
`zerus/team/teams-m1` and `zerus/team/teams-m2`.

The team slug uses lowercase ASCII letters, digits and hyphens, at most 48
characters. If that slug is empty or already reserved for another team on this
computer, Zerus adds a short suffix from the team ID. The reservation remains
stable when the service restarts. Names are labels; launch and session IDs
still identify members. Existing sessions are not renamed, and saved worktree
paths, branches and bindings remain in use after an upgrade. New member
branches are refused before creation if an existing Git ref is equal, an ancestor
or a descendant; the reason names both refs. A team created before this fix whose
lead shows "Worktree creation refused" is archived and created again.

With the preview on, a team's members appear as ordinary child sessions beneath
the lead, ordered by role and member ID. The usual children chip folds or expands
them, with the same saved fold choice as other child sessions. The lead has a
**Team: name** tag instead of a separate heading, and each row shows its role.
Membership uses the team roster's computer and launch/run bindings, including
when a parent link is pending; session names do not establish membership. A
live, quiet member without its first conversation shows **Waiting for a task**.
An unknown job count does not establish work; only a positive count prevents
that waiting label.

Before its first conversation, a member the team launched can take its first task
when the native first-message gate verifies its empty composer. A pending question
or known background or subagent work keeps it from taking that task. If its
terminal cannot be verified as ready, the usual three-minute service card names
the member as "waiting for its first-message readiness". Members with a
conversation keep the usual idle and known-zero work checks.

The lead's strip shows accepted/total tasks, its team status (highlighting
**Stuck** when an explicit blocker remains), unanswered member questions as
**?N**, and live members including the lead. Accepted tasks include archived
work; cancelled tasks and internal review/test stage tasks are excluded from
the total. The Team tab bar also excludes cancelled tasks. Counts stop at 1000 and
show **+** when more exist; an approximate count does not draw a percentage.
The open queue is a single phase, so it has no phase bar. A team with multiple
phases also shows completed/total phases. Click the strip, or focus it and press
Enter or Space, to open the lead session on its Team tab. Only the usual
children chip folds the members; that choice survives a restart.

Questions count only the open member-question inbox items, not cards for the
person. Stuck comes from explicit launch/hand-out/unreadable-member blockers,
a silent lead, a blocked service card, or a team project-limit service item.
Ordinary work/integration approvals, questions and uncertain-delivery inbox
items do not establish Stuck by themselves. These summaries arrive with the
bounded team list; drawing a strip does not load each team's view.

Ordinary sessions and the Sessions list with the preview off keep their usual
status and grouping rules.

A member that cannot be started becomes a card in Waiting for you that says
why and whether Zerus tries again. When Zerus does not know whether a member
started, it checks before it starts anything again. **Save as template** keeps
the template under a name of yours in your own catalog, with the permissions
as you set them.

## When a change does not go through

A change you make on the Team tab, in Waiting for you or with `hgs team`
either goes through or ends in one of three ways:

- **Refused by a rule.** Nothing was written. The desktop shows the team's
  reason; the command line answers with a code and a sentence, for example
  `stale_revision` when the team changed after you read it. Read the team
  again before you decide anew.
- **The store is busy.** "The team store is busy. Nothing was written; try
  again." Nothing changed, and you may simply try again. A command waits 8
  seconds for the store before it answers this way.
- **The result is not known.** The write may have happened or not. The
  command line says "The result is not known; repeat the same request with the
  same request ID." The desktop keeps the change open with **Check**, which
  repeats it with the same request, and **Dismiss**. A repeated request ID
  never applies a change twice, so a change is never sent again as a new one.

## The Team tab

The lead's session has a **Team** tab. Its bar shows the team's status, its
active members, open inbox items and open cards, and the team's actions:
**Pause** or **Resume**, **Stop**, and **Archive** after a stop. Destructive
actions ask for confirmation first.

The bar also opens **Feed**, **Inbox**, **Reports**, **Cards** and **Lead chat**.
The first four read one page at a time, at most 200 rows. **Next page** keeps
the original page cursor, so later inserts do not move that snapshot;
**First page** starts a fresh snapshot, and **Refresh** rereads the selected
page. The feed follows the journal's oldest-first order. Each event reads as
a sentence: who acted (you, the team service or a member), what happened, and
the task number and member it names; the journal's exact text stays in the
event's tooltip. Inbox and card rows show their state and who wrote them the
same way. Reports are the
report items on the selected inbox page, not an exhaustive report search.
Only one page is retained, and closing the drawer drops it.

Reports offer **Accept** or **Reject** (return with a reason), open questions
offer **Answer**, and cards offer the decisions their writer recorded. A
changed team revision or an unreadable page blocks these actions until a
fresh read. An uncertain action remains in the existing Check/Dismiss flow;
opening or refreshing a drawer never repeats it.

**Lead chat** uses the selected lead's ordinary composer and its saved draft,
shared with Activity. The bound computer and session must match, and its run
and conversation must be available. Sends keep the normal guarded delivery
and durable request identity; an uncertain delivery is never replayed.

**Members** lists each member with its role, what it works on ("Working on
#%1", "Reviewing #%1", "Testing #%1") and whether its session is linked. The
member's name, **Session linked** in its details and **Open session** in its
menu open the member's own session, found by the same computer and launch
binding the lead's Team tab uses; when that session is not on the session list
the menu says so and nothing opens. A
member's actions are **Pause**, **Resume**, **Stop** and **Remove**; the lead
cannot be removed. Removing a member puts its open work back to the lead.

A member whose terminal holds input, such as an open question, an unsent
draft or a dialog waiting for the person, takes no task: it is a fact about
the member, not a failure, with one typed cause for all of them. The
service hands the task to another free member, and tries the member again
after one, two, four, eight and then fifteen minutes; a ready task that only
that member can take gets a card after the wait. A member the service cannot
read at all counts the same way, and a ready task that needs only such a
member gets one card that names it.

The **Board** groups the team's tasks, with the queue as its own group:
**Queue**, **In work**, **Submitted** for reported work, a group for each stage
such as **In review** or **In test**, **Accepted** and **Cancelled**. A task
already handed out stays held until its observation moves; it takes no free
member from a task that waits. When no member can take a ready task, the lead
gets one inbox item for the episode, and no second one while it lasts. A card
shows the task's role, pipeline, member, round and stages; a stage card says
which task it checks ("checks the work of %1"), and a card that waits for a
decision says "Decision needed".

When every eligible member for a ready task stays unreadable for three
minutes, the lead's inbox names those members. Zerus keeps checking and closes
the item after a member is readable for three minutes, or when the task stops
waiting. A brief change in readability keeps the current item. An unreadable
reviewer or tester leaves its stage in the pipeline. The person's existing
unreadable-member card still provides the specific reason and retry action.

A task's details show its evidence above the decisions: the member's latest
report with its commit, and each stage, newest first, with its latest verdict:
who gave it, whether it passed or returned the work, and the findings. The
details read it once for the task they show and again when the team changes;
a stage shows its own facts. While the evidence on screen is of another team
revision than the decisions, it stays readable, is marked as being read again,
and Accept and Return wait for the matching evidence. A read that fails says
why and leaves the decisions as they are.

A card's decisions are **Admit** for a task that waits for admission,
**Accept** and **Return** for reported work, **Hold** or **Release**, and
**Cancel task**. Return and Cancel task ask for a reason. **Reassign** asks
for another active member of the same role and a reason; a stage's Reassign
leaves out the members who implemented any round of its task. **Route**
selects a role and one of its pipelines for a waiting or pending queue task.
Routing keeps the dependencies and write paths, and the assignment while the
role stays the same and the assigned member can still take it. Changing the
role, or a removed assigned member, clears the assignment. Both actions use
the window's native confirmation, which lists what Route keeps and clears;
the selected desktop service must be updated for Route if it is older. An
open form stays open while the team changes and keeps its choices, but it
sends only the team revision you reviewed. When another change arrives, the
form says so and waits until you check the choices and press **Review
again**. The service refuses a request whose revision is no longer current.
A stage's role follows its pipeline. Routing an approved task requires a
logged plan edit.

**Add task** opens the New task form in the Tasks side panel with this team
preselected. If a previous draft is still saving, the team choice waits for
the reopened form's active editor. A reserved card keeps its original
destination. Stopped and archived teams offer no Add task.

On a stopped or archived team's board, a task also shows what the current
stop recorded: cancellation, release, key clearing, a refusal and its reason,
or work not yet reached. An older stop without a recorded identity shows no
invented outcome.

## Starting from a goal

In **New team**, choose **Plan a goal first**, enter a goal and review the
template and permissions. **Start planning** starts only the lead, in its own
worktree. Other members and the integration branch wait for your approval of
the initial plan. A Person call to `hgs team start-planning` is refused with
`person_gesture_required`; start planning in the window.

The lead receives a durable inbox notice and can read the original goal with
`hgs team current`. It submits drafts that you can inspect in **Layers**.
Review the initial draft with **Approve phase**. The review includes the roster,
account choices and working folders. The team stays in Planning until creation
of every approved task is confirmed; then it starts the other members.

**Pause planning** stops new launches and automatic messages; the lead's current
work may continue. **Resume planning** returns to Planning. A lead cannot
remove your pause. **Stop** ends the team. An uncertain launch remains visible
and is never automatically launched again after a restart.

Starting a planned team requires the new Teams engine. Older `hgs` versions
cannot open that computer's Teams store afterward; use the updated CLI and
desktop service together.

## Preparing phase drafts

Open **Layers** in the Team tab to read planned phases. **New phase draft**
creates a proposed phase; **Edit draft** changes its title, barrier and tasks.
Each task has a description, acceptance criteria, pipeline, optional assignee,
dependencies, write paths and estimate. Layers uses the shared dependency
projection; opening a task shows its original description and criteria.

A person or the team's bound lead may save a draft. Drafts reserve task IDs
and count against the team's planned-work cap, including when a pipeline
creates a stage. They create no task documents, launches or hand-outs.
Phase 0 remains the only intake queue. Approval freezes a draft and creates
its tasks before the phase becomes available for hand-outs.

The CLI reads are `hgs team phases --team TEAM` and
`hgs team phase --team TEAM --phase N`. Use the returned revision, next phase
and node IDs for `submit-draft-phase`; an `edit-draft-phase` also carries the
generation read from the draft. Both accept the complete draft as JSON on
stdin, with `request_id`, `team` and `expected_revision`; see the
[draft input contract](reference.md#agent-teams) for its fields and bounds.
Saved IDs stay attached to their tasks, and removed IDs are never reused.

A failed read shows its reason and **Read again**, never an editable empty
plan. A stale save retains the editor text for copying. An unconfirmed save
locks saving and points to **Check** in the team's unresolved actions; it is
never automatically sent again under a new request identity.

## Reviewing and accepting phases

**Approve phase** reviews the exact generation, its descriptions, criteria and
changes since the previous draft. **Reject phase** requires a comment for the
lead. **Undo rejection** restores only that rejection's draft; it does not
approve it. The lead can edit a rejected draft and submit a new generation.

The phase shows confirmed task-creation progress. A refusal or uncertain result
keeps the phase closed; an unknown result never becomes a successful task or
an automatic second creation. A frozen publication cannot be edited.
The person's Auto policy can approve later phases of a running team; the
initial roster and working-folder confirmation always requires the person.

Layers shows live and archived tasks after publication. The service records
**Awaiting acceptance** only when the work, pipeline stages, decisions and
integration are settled. **Accept phase** rechecks those facts and completes
the phase, allowing later work through its gate. It supplies no task or merge
receipt. Send back, delegated lead acceptance and phase controls follow in
later slices.

## Sending a task to a team

- On the task board, a task in Backlog or To do has **Send to team: %1** in
  its menu, one entry per running or paused team of its project.
- Drag a task card from the session's Tasks side panel onto the Team tab's
  **Board** view to send it to that team. Graph, Lanes and Layers have no task
  drop target. This uses the same **Send to team** command and refusal
  messages. Only writable Backlog and To do cards from the same project are
  accepted; the task's current stage is checked again on drop. HTML dragging
  and the macOS/Windows pointer path have the same checks. An in-flight send
  is not duplicated, and an uncertain result is never retried automatically.
- The New task form has a **Team** chooser ("Send the new task to a team's
  queue"), with the same running/paused choices. Stopped, archived, finished
  and not-yet-running teams are not offered. Identically named live teams
  include an identifying suffix in both menus.
- An independently proved agent may use
  `hgs task add --project PROJECT --team TEAM < prompt.txt`.
  `--role` and `--pipeline` retain their existing routing checks. A Person
  creates or sends a task through the window; Person intake from the CLI
  requires a native gesture. Ordinary commands on an existing team-owned
  task remain an interim path for S2b, as listed below.

The task stays an ordinary task with a `team` key that names its team and
home computer. A task you send, or the lead sends, is admitted at once. A task
sent by a member or by another agent waits until the lead admits it. The queue
is taken in the order of the board's To do column, and each member works on
one task at a time.

### Editing a team

In the **Team** tab, **Add member** chooses a role from this team's pinned
catalog, an agent, model, effort and account. The account can be named or
**The freest account**; empty model and effort use that account's defaults.
Launch effort is available for Claude and Codex; DeepSeek keeps its model in
its native account. The member gets its own readable session and worktree through the usual launch
service. Existing members keep their sessions and work.

**Rename team** and **Edit goal** change the team's name and goal. Clearing the
goal is allowed. A rename keeps existing session, folder and branch names.
All four edits are yours alone and refuse a stale team revision; reopening the
form after a refresh shows the current values.

The member menu's **Swap…** replaces the agent for a member you have already
stopped with **Stop**, after its open tasks and attempts settle. The menu shows
the open-work count when Swap is unavailable. A swap keeps the member ID,
role, task history, folder and branch. The old session stays outside the team;
Swap never stops, archives or adopts a session. The new session's readable
name gains a revision suffix. Unknown launch or delivery receipts refuse a
swap rather than repeating unfinished work. Active cutover and adoption are
follow-up work, not choices in this dialog.

Opening a form makes no write. A result that cannot be confirmed stays in the
existing action list: use **Check**, without submitting another edit.

## Routing

On a `routing: lead` team, admitted tasks wait until the lead chooses their
role and pipeline with `hgs team route`. Admission waits and routing waits
each have one count-only inbox item pointing to `hgs team queue`, so the
existing doorbell brings an idle lead back to the queue. Each item closes
when no tasks wait in that category.

When the team refuses a new task, for example because its queue is full, the
task is still created but not queued: it stays on the board as an ordinary
task, and its team key is left as it was before the send. The desktop says
"The task was created, but %1 did not queue it: %2" and offers **Send to
team** again or **Dismiss**; the command line says "Task REF exists and was not
queued (CODE): REASON". When the result is not known, the task keeps its team
key and the desktop says "The task was created; whether %1 queued it is not
known. Send to team queues it now, and never twice." Only a task in Backlog or
To do can be sent.

## Waiting for you

**Waiting for you** in the rail counts the open cards of every team on this
computer. The page lists "Cards only you can answer, oldest first." Each card
shows its kind, its team, its age, its text and its decisions; **Open task**
opens the task on the team's board.

A card offers the decisions it accepts. **Integration** and **Work
acceptance** take **Approve** or **Reject**. **Acknowledge** closes a card and
changes nothing else.

| Card | When it appears |
|---|---|
| **Integration** | The lead accepted a result, and merging it waits for you because Merge accepted work is off. **Approve**: the service merges it and finishes the task. **Reject**: no new merge is authorized; the task stays in review. A merge already recorded is not undone and opens the card below |
| **Rejected result is merged** | A result reached the integration branch before its Integration card was rejected. The card and the lead's inbox name the task, commit and branch. Rejection did not undo that merge; only a revert on the integration branch removes it |
| **Work acceptance** | Every stage of a pipeline whose acceptance is yours has passed. **Approve**: the task is accepted, and merged where the team integrates. **Reject**: the task goes to the lead |
| **Member not started** | A member could not be started; the card says why and whether Zerus tries again |
| **Start not confirmed** | Whether a member started is not known; Zerus checks before it starts anything again |
| **No room for members** | New members wait until there is room for another session or account |
| **Member needs you** | A member's session asks for something Zerus does not confirm on your behalf |
| **Team service blocked** | A step of the service failed three times, for example a merge that cannot run, and is tried again; the same card names a member a ready task needs that cannot be read. It also names the cause when an open hand-out's holder has no durable session handle or its session record cannot be read: after the same three-minute period, with the same deduplication and close on recovery or settlement. An undecidable binding never counts as a departure; decisively ended bindings keep the existing left-member settlement. It is also the card for the project limit: when the project's own settings refuse every hand-out, one such card per team appears at the first refusal, its text names the limit, and the queue waits until the project allows it |
| **Report waiting** | A member's report has waited for the lead's decision longer than the team allows, an hour by default |
| **Lead not working** | The lead has not worked for a while, 20 minutes by default, although items wait in its inbox |
| **Late session** | A member's session started after the team stopped or the member was removed; Zerus does not stop it |

The page also lists **Requests of team sessions**: a question or a permission
request of a lead or a member that only a person can answer. Choosing one
opens that session.

## The lead's decisions

The lead reads its inbox and decides each report the pipeline handed to it.
The lead decides and the service carries the decision out. A task has one
decision at a time.

A new Return, Reassign or Cancel can replace an earlier Return or Reassign
only before its reopen is reserved. Other unsettled decisions must finish
first; the queue says whether its pending decision can be replaced.

The member's report text stays separate from **Service notes**, such as
"The report text was shortened" or "The member's worktree had uncommitted
changes." The service keeps at most 15,000 bytes of report text. In prompts
for the lead, a stage or a dependent task, each line of the member's text is
quoted and the service notes have their own heading. Text written by a member
does not become a service note.

- **Accept.** With an integration branch, the task is Done once the service
  has merged it, or once you approved its Integration card. A merge conflict
  returns the task to its member with the list of conflicting files. Without
  an integration branch the service moves the task to Done at once. The
  service finishes or merges only while the task is still as the lead accepted
  it; see [You and the team on one task](#you-and-the-team-on-one-task).
- **Return**, with a reason. The service reopens the task to the same member
  with the reason, which starts a new round.
- **Reassign**, to another active member of the same role, with a reason. The
  service hands the task over to that member.
- **Hold** and **Release.** A held task is not handed out.
- **Cancel**, with a reason. Running work is cancelled, and its member gets
  the ordinary cancel notice of a task; the task leaves the queue once the
  service has seen it cancelled. A task with nothing running (Backlog, To do or
  In review) leaves the team and stays in its stage as an ordinary task. A task
  whose delivery is unknown, and a Done task, are left as they are, and the
  lead gets an inbox item about it.

A lead can accept only work its pipeline handed over, and cannot accept or
cancel a stage: a stage ends by its holder's verdict or by you.

**A member's question.** A member whose task is blocked asks with
`hgs team ask`, and the question waits in the lead's inbox. With Answer
members on, the lead answers with `hgs team reply`. The answer stays with the
question: the member reads it in `hgs team current`, which shows the member's
own questions about the task it now holds, newest first, with their answers.
A question from an earlier round is not shown there; what the next round must
know goes into the reason of a Return.

## Pipelines and stages

A pipeline says what happens between a member's report and the end of a task.
`code` has a review by a reviewer and a test by a tester, then the lead
decides; `reviewed` has a review, then the lead decides; `docs` has a review
and is accepted when the review passes; `check` goes straight to the lead.
Each stage is a task of its own, handed to a free member of the stage's role
and never to the member who implemented the work.

In Team settings, **Allow land stages** is off by default. After the update,
existing teams with that switch off skip Land steps in custom pipelines.
The Feed records why, and the pipeline continues to the next step or its
normal acceptance. Turn the switch on to create new Land stages. A Land stage
already created can finish. Creating a replacement for a retired Land stage
also requires the switch to be on.

The stage's holder answers with a verdict: pass, or return with findings. A
return reopens the task to its implementer without the lead. After the
pipeline's rounds, three unless it sets another number, the task goes to the
lead. Every reopen counts as a round, whoever caused it.

## Doorbells and timers

After its launch and after a delegated resume, a Claude, Codex or Kimi lead gets
one short brief naming its team, `hgs team current` and `hgs team inbox`.
The brief is launch context, not a task or an attempt. Unknown delivery is never
replayed. DeepSeek leads do not get this brief yet; their hosted delivery path
needs the same guarded first-send support (tracked in zerus-3tq9r).

The inbox is where the lead's work waits; a doorbell only tells a session to
look. When items wait, the service sends the lead one message that says how
many wait and names `hgs team inbox`. When answers to its questions wait, a
member gets one message that says how many and names `hgs team current`. A
doorbell names a count and a command and never the text of an item, a question
or an answer.

A doorbell rings only while the team is running and the session is live, idle
and has no pending request, and only for something that arrived after the
previous doorbell. It rings at most once per interval, 120 seconds unless the
team sets another, and goes to the exact run and conversation the service
read. A doorbell whose delivery is not known is never sent again; the next new
item rings a new one, and a resumed session gets a new doorbell, never the old
one. A member's doorbell is dropped once the member no longer holds that
attempt: after an accept, a return, a reassignment, a cancel or a stop.

The lead also gets one aggregate inbox item while tasks wait for its admission,
and, on a `routing: lead` team, one while admitted tasks wait for routing.
Each item names the current count and `hgs team queue`, never task text.
More waiting tasks update that item without another doorbell. Each item closes
when its category has no waiting tasks; a later arrival starts a new episode
and can ring a new doorbell. The episodes survive a service restart.

Timers turn waiting into an inbox item for the lead or a card for you:

| After | When | Becomes |
|---|---|---|
| 120 seconds | a member's turn ended on a task in progress without a report | an inbox item |
| 180 seconds | a handed-out task has not started | an inbox item |
| 15 minutes | a report waits for the lead's decision | an inbox item |
| 60 minutes | that report still waits | a **Report waiting** card |
| 20 minutes | the lead has not worked while its oldest inbox item waits | a **Lead not working** card |

The times are the team's settings. A timer fires once for its task and round,
or for its inbox item, and only while its condition still holds: what ends the
condition also closes the card it opened. No timer fires while the team is
paused or stopped. After a restart of the desktop service a timer may fire
late, never early.

## You and the team on one task

A team's task stays yours as well: you can start, cancel, close or move it on
the task board at any time, and the team follows what you did.

- **You start it yourself.** The team treats the run as yours: it ends its own
  attempt, settles any decision that was waiting, and goes on from your run as
  a new round.
- **A team's cancel never ends your run.** A cancel acts on the run the team
  knew. If you started the task again before the cancel was carried out, the
  cancel is settled and your run goes on.
- **Two runs at once.** While two runs claim the task, the team decides
  nothing about it: it finishes and merges nothing, and no cancel ends it,
  until one run remains. A task that has ended, cancelled or Done, counts as
  ended whoever ran it.
- **You take it out.** Retargeting the task away from the team takes it out of
  the queue; the team does not take it back.

## The integration branch

The integration branch collects accepted work. The service merges with Git
plumbing and moves the branch only if it still points where the merge
started. It never checks the branch out, never runs `reset`, never writes the
primary checkout or another branch, and never pushes. No hook runs on these
merges. Integration is unavailable while the integration branch is checked
out in a worktree, with Git older than 2.38, and in a repository that has
submodules, is a partial clone, or declares a hook or a merge driver in its
own Git configuration; the service then merges nothing, and the third failed
attempt opens a card in Waiting for you. Your own merge driver and signing
settings are honoured. Landing the branch on your base branch, or pushing
it, is your own step.

The permission **Merge accepted work** (`merge_accepted`) decides who starts a
merge: the lead's Accept while it is on, your approval of the Integration card
while it is off. Switching it off takes back every accepted result that has
not merged yet and that you did not accept or approve yourself: each goes back
to waiting for you with an Integration card.

## Pause, stop and archive

- **Pause** stops hand-outs and doorbells and sends nothing to any session;
  no timer fires while the team is paused. A paused member takes no new task.
  The lead can pause and resume the team and its members, but cannot resume
  what you paused or stopped.
- **Stop** a member: it takes no new task, and neither the lead nor the
  service resumes it. **Remove** takes it out of the team.
- **Stop** the team: every unfinished task is cancelled as after a lead's
  Cancel. Running work is cancelled with the ordinary notice; a task with
  nothing running stays in its stage; a task whose delivery is unknown, and a
  Done task, are left as they are; a task you started yourself after the stop
  is left to you. The service then removes the team key from every task that
  was open at the stop; each stays on the board as an ordinary task.
  In bounded background passes it also clears this team's keys from closed
  tasks that have no live team binding, without adding a history entry.
  A key for another team or a task bound again is kept. Archive also starts
  this closed-task cleanup.
- A member whose session starts after the team stopped, or after you removed
  it, is not taken into the team: a **Late session** card names it.
- **Archive** follows a stop and closes the team's open cards.

Zerus never stops a team's agents by itself. The sessions stay, and you end
them as any other session. Quitting a member's agent by hand is not a stop of
the team or of the member: with Resume exited members on, the member is
resumed. Use Stop on the member instead.

A cancelled task can be moved from In review or Done back to To do and sent
to a team again. Moving it back clears its cancelled mark and reason; a send
also clears an older stale mark in Backlog or To do. An attempt still being
cancelled is settled before the send creates its new queue entry; a task
still in In review cannot be resent.

## Hand-outs refused before delivery

A task-attempt fence can refuse a hand-out as `cutting_over`: nothing was
reserved or sent. That closes the effect as not delivered and frees the task
and member. The service checks again after its normal refusal backoff and
hands the task out under a new effect when the fence admits it. It never
replays the refused effect. An ordinary refusal before input follows the same
rule; an unknown or uncertain delivery keeps its reservation and is never
sent again automatically.

A proven non-delivery does not open a `delivery_uncertain` inbox item. The
service also reconciles such effects recorded by older versions and closes
only the false item bound to their original outcome receipt, including after
manual recovery. A genuine unknown delivery stays open. A refused reopen
hands the task back to the lead with a notice that explains the refusal.
The service closes that notice after the lead releases the hand-back or
newer work supersedes it, including for tasks recovered on older versions.

## Project limits

A hand-out is written with the lead as its actor and the member as the
executor, so the project's task settings for agents apply to it as to any
agent. With **Agents may start and cancel tasks in other sessions** off, or
with the hourly limit of agent starts reached, the refusal is a fact about the
team, not about a task or a member: the whole queue waits. One card in Waiting
for you, shown as **Team service blocked** and bound to no task, opens at the
first refusal and says that the team's tasks wait and are handed out by themselves when the project allows it; its
decisions are **Retry**, which checks on the next service pass, and
**Acknowledge**. While the fact stands the service hands nothing out, and it
tries one hand-out as a probe after one, two, four, eight and then fifteen
minutes; a probe the project admits ends the fact and the queue goes on. The
lead sees the fact among the service items of `hgs team status`. An hourly
limit hands the task out by itself at the first probe after the hour passes,
nobody doing anything. A refused hand-out is never forced.

## What the service confirms by itself

Two permissions of the team, **Confirm folder trust** (`confirm_trust`) and
**Resume exited members** (`resume_exited`), and nothing else, let the service
answer for you:

- **Folder trust.** The startup folder trust prompt of a Claude, Codex or Kimi
  member whose worktree Zerus created for this team is answered at once.
  On that member's account the agent records trust for the team's main Git
  checkout and its subfolders. Codex may name that repository root in its
  prompt; the service verifies it against Git rather than trusting the text.
  **Settings > Approvals** records the scope: only a confirmed grant says
  "Trusted for %1 and its subfolders on this account"; failed, cancelled or
  unconfirmed attempts say trust was requested but not confirmed.
- **Resume.** A member whose agent exited by itself is resumed in the same
  conversation, at most three times an hour, with a short brief that points
  it at `hgs team current`.

  An archived exit first waits for the original terminal name to be free.
  Measured from the recorded exit time, the wait uses the larger of the
  teardown time budgets plus one service observation interval and the
  ten-minute cap for cleanup I/O without an overall deadline. With the
  current budgets, the bound is ten minutes. While waiting, Zerus writes
  no resume attempt, approval audit or card, and reserves no hourly slot.
  A later observation resumes once the name is free. If it is still busy
  at expiry, Zerus writes one "did not settle" card and never automatically
  resumes that exit. Review the card and Restore manually once the original
  run has settled.

Each answer is checked against the exact run, conversation and prompt,
recorded in the approval journal before delivery, and never repeated when its
delivery is unknown. Any other prompt, a folder Zerus did not create, and a
member you stopped go to you. These confirmations do not turn on automatic
tool approvals.

## Limits of this milestone

- Linux and macOS only.
- On macOS the system hides the environment of its own programs, among them
  the shells and `ssh`, from other processes. A person's command started below
  such a program, for example in `tmux`, over `ssh` or in a terminal inside
  another application, is refused: "On macOS a person's team command cannot
  be checked below a program that hides its environment, usually a system
  shell; run it from the Zerus window." Commands that a lead or a member runs
  are not affected.
- A team lives on one computer: its lead, its members and its Team tab.
- Queue execution has Board, Graph and Lanes views; planned phases use Layers.
  A lead can prepare drafts from a goal. The person can approve, reject and
  accept phases; send-back rework and phase controls follow in later slices.
- DeepSeek can be a lead or member with a host that proves each tool call
  with its per-call token, witnesses task input and reports its lifecycle.
  If the host lacks those capabilities, stop idle DeepSeek sessions or restart
  the host yourself. After the host is back, remove the blocked member and add
  a replacement. For a blocked lead, stop the team and create a new one.
  Zerus does not retry the blocked start or restart a live host. OpenCode's
  shared-service sessions cannot be a lead or member in this milestone.
- On Linux, `hgs team` from a shell inside a sandbox with its own user
  namespace is refused, whether a person or an agent runs it, so a member
  whose agent runs its commands in such a sandbox cannot take part.
  A private process namespace also prevents the caller proof from seeing
  the host outside it: the refusal names that limit and the remedy. Run team
  commands outside that sandbox or turn its private process namespace off.
- Person team decisions and mutations require explicit native confirmation in
  the Zerus window on the computer that owns the team. This includes owner-card
  **Decide**, Create, Start planning, team settings and membership, dependencies,
  advance-base, queue and phase decisions (including Send back and correction
  approval/rejection), and sending or creating a task for a team. A person's CLI command for these actions returns
  `person_gesture_required`, including remote commands. Independently proved
  lead/member commands and service facts retain their existing scope.
- A CLI Person claim or renderer `confirmed` flag grants no permission. The
  native confirmation binds the exact payload, computer, configuration, store,
  team, revision and original UUID. After an uncertain result, Check reads only
  that original receipt; missing, corrupt or foreign evidence never replays the
  action. An unknown task intake preserves its original and cannot be resumed
  or replaced with a new UUID. A new confirmation is allowed only for a known
  closed refusal or a later explicit new action.
- Two Person writer groups remain interim until the following slices: ordinary
  task rename/edit/stage/target/send/cancel commands on a live team-owned task
  (S2b), and preview, computer configuration, catalog/template and legacy
  import/save/remove writers (S3). These still rely on process ancestry as
  defense in depth. That rule guards against accidents and a session merely
  dropping markers; it cannot stop same-user arbitrary code, detach/re-exec,
  replacement of trusted software or state redirection. The full Person
  authority migration is incomplete until both groups move to native admission.
- An accepted or cancelled queue entry is already decided and takes no
  Cancel. A task's later run must be observed as its own current attempt;
  the team's cancellation never takes authority over an unrelated run.
- Closed-task key cleanup is a bounded sweep. A task document that cannot be
  read is skipped, and a key synchronized back behind the cursor or after
  completion is not swept again until another Stop or Archive. A person may
  retarget a task whose old key remains; live bindings and other teams' keys
  are never cleared by this sweep.

## Origin

Agent Teams were designed and first built by WhiteWh in the `WhiteWh/zerus`
fork; pull request #322 is its first version. Zerus keeps its concepts, its
working process and its desktop views, and builds the runtime on Zerus tasks
and the desktop service. Files imported from the fork keep WhiteWh as their
Git author. See "Origin and credit" in the
[design](superpowers/specs/2026-11-11-agent-teams-design.md#origin-and-credit).

## Advancing the base of a live team

A person can use **Advance base…** in the Team tab's bar, or run
`hgs team advance-base --json` with a JSON request on stdin:

```json
{"request_id":"00000000-0000-4000-8000-000000000165","team":"00000000-0000-4000-8000-000000000001","expected_revision":7,"commit":"0123456789abcdef0123456789abcdef01234567"}
```

The form reads the current base and the available main commits once, then asks
for confirmation. Its revision remains the revision of that read. A stale
revision requires reviewing the updated team; an uncertain result keeps its
original request ID and offers the usual explicit Check and Dismiss actions.

This operation is person-only, including when a lead has permission to accept
results. It runs on the team's home computer and resolves the existing project
through the same local placement reader as member launch. Branch integration
must be enabled and the team must be running or paused. The full lowercase
commit hash must exist in that repository, be strictly newer than the current
base, and contain the current base in its ancestry. It must also be contained
by either the literal local `refs/heads/main` or locally fetched
`refs/remotes/origin/main`. There is no fetch, network lookup, HEAD fallback or
arbitrary ref selection. Both names are read exactly in one ref snapshot;
a branch whose name merely resembles either ref cannot substitute for it.
Missing main refs and backward, equal, unrelated or
non-main commits are refused. The journal records the old base, new base and
the ref that admitted it. Repeating the exact request returns its saved receipt
before rechecking the revision or repository.

The new base applies to future hand-out and reopen reservations. Each effect
pins its assigned base; its prompt and that generation's integration ancestry
check use that pin even after another advance. The task detail and queue show
`assigned_base` separately from the team's current default. On upgrade, older
effects keep the pre-advance base through a frozen default and indexed
high-water mark; upgrading does not scan or rewrite the effect history.

The first successful advance also raises the computer's private Teams store
compatibility mark. Older builds cannot reopen that store. An older engine
that already holds a connection cannot write hand-out, effect or integration
rows for an advanced team: its transaction fails without partial rows, with
a cause naming the required newer Zerus writer. Those guards do not cover
other teams or unrelated settings/status writes on an already-open connection.
Use the updated engine to continue the affected work.

Advancing writes no Git objects or refs. It does not merge into, reset or
rewrite the integration branch or any member branch. Existing member
checkouts and saved launch plans keep their identities. A newly selected
member launch uses the current default as its initial checkout base. The
hand-out tells the member to start from the integration branch and merge the
assigned base into its own work branch before implementing. The member owns
that merge and any conflicts. The service continues its ordinary acceptance
merges using the base actually assigned to the reported task generation.

### Tests and Builds provider metadata

Team settings can inspect the native-selected project folder and save a
person-reviewed provider selection. Opening the section and **Check** only read
bounded metadata; neither saves a choice nor starts a program. Tests and builds
remain ordinary member-agent tasks. Direct runners, executable templates,
validation requests, report import and mandatory gates are unsupported.

The version-1 file is `.zerus/tests.toml` (at most 64 KiB, at most 16 providers):

```toml
schema = 1
[[provider]]
name = "cargo-test"
provider = "cargo"
kind = "test"
scopes = ["full", "focused"]
reports = ["junit"]
```

Known provider metadata IDs are `cargo`, `npm`, `pytest`, and `cmake`; kind is
`test` or `build`, scopes are `full`/`focused`, report formats are `junit`/`json`,
and optional `disabled = true` keeps a provider listed but disabled. These are
metadata declarations, not proof of installed tools, supported focused commands,
executed suites or imported results. Unknown fields, providers and schemas are
refused; command/program/template fields are not accepted.

A present file supplies the base configuration. Without it, the fixed bounded
marker reads `Cargo.toml`, `package.json`, `pytest.ini`, and `CMakeLists.txt` suggest
known providers, without running discovery. A person's JSON override has the same
shape and replaces entries by name. Empty uses the file/detected base. An override
never hides a malformed or unsafe source file. Both `.zerus` and its file, and
all detection files, refuse symlinks and irregular inputs below the configured
root. The trusted configured root itself may use a symlink spelling.

**Save provider selection** is a separate explicit settings action, enabled only
after a successful Check. The operation rechecks the exact project computer,
configured checkout object and source/config digests under the team writer lock.
A changed checkout/config or stale revision needs a new review. The selection is
advisory companion settings: it neither changes legacy settings JSON nor enables
execution/acceptance policy, so older clients/services keep their existing task
behavior. Old hgs versions show inspection as unavailable, never a successful
empty configuration. An uncertain save keeps its original action for Check in the
Team tab; it is never automatically reposted.

## Correcting an awaiting phase

**Send phase back** requires a comment and keeps the phase awaiting acceptance.
Its earlier accepted tasks and commits stay unchanged. The person can send back
a hard gate; the exact lead can decide only a Lead gate with the Accept results
delegation. **Draft corrections** opens a separate nonempty proposal with new
task IDs. **Edit corrections**, **Approve corrections**, rejection and undo
retain that proposal's exact publication, episode and generation. The phase
returns to Approved only after every new task is confirmed and publication
settles. A partial or unknown outcome never causes an automatic replay.

Corrections preserve the existing acceptance, approval policy, barrier, pause,
order, kind and mode. The barrier shown in their editor is read-only. The
existing phase reader/poller keeps the retained base snapshot separate from
the proposal, then switches to the new frozen publication after confirmation.
Older unreturned publications remain unavailable to estimate views; the whole
phase live scope still includes earlier accepted work. No task is mass-reopened
and no integration history is rewound. Read `hgs guide team-rework` for the
strict command fields and original-request Check behavior.

### Read-only implementation estimates

The selected phase's **Observed timing** panel shows implementation estimates
through its existing lazy read. An unapproved draft is labelled as a preview;
a valid empty observation page can still show its known estimates. A reserved
publication is a frozen selected identity, and its numbers stay unavailable
until publication settles and the complete live membership is read. Published
values come from that publication's frozen draft nodes, joined to their exact
live task identities. Later edits, correction proposals and earlier tasks in
the whole-phase live snapshot never replace those values.

Zero or missing estimates, values over 525600 minutes, incomplete reads and
prerequisites outside the selected cohort remain unknown. Estimate membership
completeness does not certify observation coverage. Each observed unit retains
its exact publication, task, attempt, effect and binding identity; a page's
selected publication cannot fill missing original membership. Schema 1 records
points and gaps, so implementation actuals remain unavailable, including when
task states or delivery/report events are present. Estimates are not phase
elapsed time, an ETA, or a completion promise. Hiding or changing the selected
scope drops the derived page; a failed or mismatched read cannot retain its
numbers.

## Controlling a planned phase

The phase header's **More** menu offers a barrier, pause/resume, movement and
acceptance policy. These controls retain the phase's stable ID, task membership
and history. The person or exact current lead can set a barrier or pause a
nonterminal planned phase. A pause requires a reason; its owner and reason stay
visible. The lead can remove or replace only the pause owned by that same lead
session. A person's pause, a predecessor's pause and an unattributed legacy pause
require the person. Existing reports and work already in hand continue, while
the pause blocks new ordinary hand-outs.

**Move earlier/later** uses the exact neighboring identity returned by the
phase read, not a visible row position. The source, target and all crossed
phases must be positively unstarted. Publishing, awaiting, terminal, incomplete
or previously attempted work cannot move. Changing the phase's acceptance
policy is Person-only and uses the same positive unstarted check; it never
changes the team default or softens a waiting gate. Current task documents are
checked as well as retained live/archive history, so an unobserved send
reservation cannot be mistaken for an unused Todo task.

Unavailable controls show their reason. Missing, stale or partial reads do not
enable an action. An unknown result retains the exact original request for
**Check**; it never automatically sends a replacement. During corrections,
permitted barrier/pause changes preserve the BASE/proposal/NEW identities and
all acceptance, approval, title, order and historical publication facts.
