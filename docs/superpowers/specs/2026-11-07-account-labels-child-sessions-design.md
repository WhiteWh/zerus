# Account marks and child sessions

**Status:** Design, 2026-11-07. Owner request relayed by the coordinator session
`design-tima2`; implementation proceeds without a separate approval round.
**Epic:** `zerus-sa9qn`
**Scope:** the Tauri client (`client/`), the `hgs` CLI where the link is
recorded, and the desktop service where it projects notifications. The Qt GUI
is out of scope. Android and iOS follow in separate tasks (see Follow-ups).

## Goal

1. Every account has a label, a color and an optional emoji mark. A session
   card shows its account as a colored circle with the label's first letter,
   or the emoji when that option is on.
2. A session started or adopted by another session is its child. The session
   list nests children under their parent, several levels deep. Children work
   quietly: only requests that a person alone can answer draw attention.

The live example: the coordinator `design-tima2` directs `desktop-u`,
`fixes-u`, `migrate-u`, `review-win-c` and others. They appear under it, and the
owner is not notified when one of them becomes Ready.

---

## Part 1. Architecture

### 1.1 The parent link

A child session's record on its own computer gains one optional object:

```json
"parent": {
  "schema": 1,
  "machine": "11111111-1111-4111-8111-111111111111",
  "host": "arch",
  "session_id": "22222222-2222-4222-8222-222222222222",
  "name": "claude/example/coordinator",
  "conversation_id": "native-conversation",
  "run_id": "native-run",
  "source": "launch",
  "at": 1762500000
}
```

`machine` and `session_id` are the identity. `name`, `conversation_id` and
`run_id` are hints for display and for matching a parent record that an older
`hgs` rewrote without its `session_id`. `source` is `launch` or `adopt`.

**Single writer.** The child's computer is the only writer of the link: it lives
in the child's record, like `session_id`, `launch_id` and `fork_parent_id`.
There is no concurrent multi-writer state, so the link needs no replicated
document; `crates/replica` and the Loro project catalog are untouched. Every
desktop reads the link from the computer's normal listing; phones read it from
the host snapshot. Collapsing a parent's children is device-local UI state.

**Authority.** The link is presentation and attention routing only. It never
authorizes a send, answer, approval, stop, archive or removal, and it never
moves an existing session's project. Since zerus-d5twt (2026-11-11) a new
child's initial placement follows the project of a parent this computer proved
from process ancestry; see the project inference rules in `docs/reference.md`.

### 1.2 Where the link comes from

| Source | Recorded | Proof |
| --- | --- | --- |
| `hgs [@h] AGENT DIR … -d` (a launch) run from an agent's shell | at the new session's first record, `source: launch` | the launching `hgs` resolves its caller with the same native process-ancestry proof as `hgs self` |
| `hgs parent SESSION --json` with `adopt`, run by an agent | on SESSION's record, `source: adopt` | the caller is proven the same way; the target is selected by name plus its expected run and conversation |
| `hgs [@h] parent SESSION --json` with `clear` (the desktop **Detach** entry is a follow-up) | removes the link | an explicit person or agent action |
| `hgs wait` producer/waiter links | never | a wait is a declared dependency, not ownership: reviewers, peers and a waiter's own parent are producers too |
| Native subagents (Claude Task, Codex subagents) | never | they are not independent sessions; they keep their existing compact child rows |
| Message prose, session names, folders, accounts | never | no inference |

**Launch.** Today `hgs launch` already blanks `HGS_RUN_ID`, `HGS_SESSION_ID` and
related variables for the new pane so the child gets its own identity. The
launching `hgs` now also sets `HGS_PARENT` itself: the caller's proven reference
as compact JSON, or empty when no live tracked agent owns the launching process.
An inherited `HGS_PARENT` is always overwritten, so a person typing a launch in a
plain shell, or a script setting the variable, records no parent. The session
host's first `lifecycle` write records `parent` only for a new session
(`startup_kind == "new"`); a resume or attach never adopts the launcher of that
moment. A fork run by an agent records both its fork lineage (`fork_parent_id`)
and its parent; they are independent facts.

**Remote launch.** `hgs @peer AGENT …` (and `hgs @peer fork …`) run by a proven
agent forwards that agent's link as `--parent-link`, hex-encoded JSON, behind
the `launch-parent-v1` help marker, so an older peer launches without a parent
(zerus-sa9qn.7). Remote adoption is not forwarded. A
cross-computer link is a claim by the SSH-authenticated caller; the child's
host records it with `source: forwarded` and cannot verify the parent on
another computer, so its listing always reports that parent as `missing`, even
when the IDs match a local session, and never treats it as live for attention
(1.5, zerus-l4ehe).

**Adopt.** Existing sessions such as `desktop-u` predate the feature. The
coordinator runs `hgs parent desktop-u --json` with `{"action":"adopt"}` and
the target's expected run and conversation on stdin, like other guarded
session commands. It refuses an unproven caller, self-adoption, archived
targets, a target that is an ancestor of the caller on this computer (a cycle)
and a target with another parent unless `"replace": true`. `read` returns the
link and `clear` removes it.

### 1.3 Durability

The link is carried on every path that keeps `session_id`: resume, Clear, fresh
start, rename, pause, archive and restore (the `lifecycle` resume carry list,
`clear_context::carry_fresh_start`, archive copies and restore). Renames of
either side do not matter: the identity is the parent's `session_id`, which a
rename keeps, and the displayed name comes from the parent's current row, falling
back to the recorded hint. An older `hgs` that rebuilds the child's record on
resume drops the link; the child then lists at top level, as before this
feature.

### 1.4 Resolving a parent in the list

A desktop resolves a child's parent among the rows it currently lists:

1. the row on the computer whose `machine_id` is `parent.machine` and whose
   `session_id` is `parent.session_id`;
2. only when that parent row lists no `session_id` (older `hgs`): the row on that
   computer with the recorded `conversation_id`, then `run_id`.

Names never match. The resolution is a single pass over listed rows with one
hash map, rebuilt with the list; there is no per-session cache and no extra read.

| Parent | Child shows |
| --- | --- |
| Listed in the same project group and visible under the current filters | nested under it |
| Stopped or paused (or its agent ended), in the same project group, hidden by a filter | nested under a dimmed **Ended** container of the parent's own row (zerus-sa9qn.17) |
| Running but hidden by a filter, or listed in another project | at top level of its own group, with a **from PARENT** chip that selects the parent |
| Archived, including terminated (its link says `archived`) | in the live list nested under a dimmed **Archived** container built from the link; nested in Archive (zerus-sa9qn.17) |
| Not listed on the child's own computer: forgotten or removed (link `missing` there, or `saved` but unlisted) | nested under a dimmed **Ended** container built from the link (zerus-sa9qn.17) |
| Not listed elsewhere: on another computer, that computer offline, or unknown | at top level with **from PARENT** using the recorded name; the tooltip says the parent is not listed |

**Ended parents (zerus-sa9qn.17, owner 2026-11-11).** Ending a parent never
orphans its live children in the list: the ended parent stays as their
container until no live (non-archived) child remains, then leaves the list by
itself. The container is presentation only and grants nothing: it is not a
session row, takes no selection, keyboard step, drag target or attention, and
its menu offers only Open, Resume or Restore, and Move to archive of the
parent's own listed row, or Open in Archive for a parent known only from the
link. It appears only for children the filters show, and is built in the same
single pass with one hash map per kind, without extra reads.

**Cycles** can only come from `adopt` across computers or from a rewritten
record. The writer refuses local cycles. The list builder walks parents with a
visited set and a depth limit of 8; a session whose parent chain loops or runs
deeper renders at top level. Nesting never hides a row.

**Project groups.** A child nests only inside its parent's project group. A link
never moves a session between projects, so project counts, drag-and-drop
organization and the project filter keep their meaning. Children launched in
worktrees of the parent's repository already infer the same project.

### 1.5 Supervision on the host

The host computes one more fact per listed row with a parent: `parent.state`,
one of `live` (the parent record is running and its process has not exited),
`saved` (paused or stopped), `archived` or `missing` (this listing has no such
session: gone, or on another computer). Session IDs are UUIDs, so this needs
only the rows of the same listing and no machine identity; it is computed in
`finish_rows` with one map keyed by `session_id`. A listing never reads or
waits on the shared catalog, and a launch reads its machine identity without
the catalog lock, only after proving its caller.

A row is **supervised** when `parent.state == "live"`. It is **quiet** only
when its host also reports `human_request: false` and the core sees no
person-only request in the row itself (an approval of the session or a native
subagent, a failed or required sign-in); a host that does not classify keeps
every request visible. Every consumer, the
desktop list, the desktop notification tracker in `client/core`, the desktop
service (which projects each row through the same tracker) and later Android,
reads the same fact from the row. When the parent is unknown, saved, archived or
missing (including a parent on another computer), the child is not supervised and behaves like any session.
This fails open: an unverifiable parent never silences a request.

### 1.6 Account label, color and mark

* **Label.** The existing per-computer catalog label (`hgs account ls --json`
  `label`, edited by **Rename…** on the Accounts screen). A grouped account
  without a name falls back to its email, as today.
* **Color and emoji.** Desktop preferences, like machine colors
  (`machines/colors/<alias>`): `accounts/color/<computer>/<profile id>` and
  `accounts/mark/<computer>/<profile id>`. Editing an account on the Accounts
  screen writes every member profile of that grouped account. Without a chosen
  color, the color is a stable palette slot derived from the label, so every
  desktop shows the same default without setup.
* **Option.** Settings > Sessions > **Account marks**: Letters (default) or Emoji.
  Emoji applies to accounts that have one; the others keep their letter.
* **Letter.** The first grapheme of the label that is a letter or digit,
  uppercased (`Intl.Segmenter`). An empty label uses the provider's initial.
* **Rows.** A session row resolves its mark by its computer and recorded
  `account_id`. A row without a recorded binding, or whose profile is not in the
  loaded catalog, shows no mark rather than a guess.

Color and emoji are kept on the host since zerus-sa9qn.6, so phones and other
desktops show them: `accounts.json` refuses unknown fields in older `hgs`
versions, so they live in a sibling `account-marks.json`, changed by `hgs account
mark` and listed as `mark` on account rows and `account_mark` on session rows.
The desktop preferences above remain for computers with an older `hgs` and move
to the host once (`--if-unset`, the host value wins). Hiding one account's mark
stays a desktop preference.

---

## Part 2. Attention policy

### 2.1 The rule

The owner's rule: only an explicit human request draws attention. A request the
parent agent can answer is not a human request; one only the person can answer
is. A supervised child therefore treats its signals as **quiet** or **human**:

* **Quiet** is the existing mute treatment: no system notification, no rail,
  tray or project-header count, neutral gray badges and edge mark, and a crossed
  bell replaced by the supervised mark. The session's actual state, Activity,
  unread identity, questions and approvals are unchanged. Nothing is marked
  read. Unlike a muted session, a quiet child leaves the Needs attention filter,
  and Active unless it is working, in Background, Scheduled or has a draft; only
  the person's own Review later mark keeps it in both. Each status filter
  lists exactly what its tab counts, through one predicate, and nesting never
  adds a row that fails it (owner report, 2026-11-11, zerus-sa9qn.18).
* **Human** behaves as for any session: badges, counts and notifications.

Status captions do not change. A child still reads Ready, Needs input or
Background; only who is alerted changes. The Background contract (an idle agent
with owned jobs shows Background and stays in Active) is preserved.

### 2.2 Every signal

| Signal today | Supervised child | Why |
| --- | --- | --- |
| Ready, idle reply, new-reply badge | quiet | the parent reads results through `hgs inspect`, `read` or a wait |
| Unread badge and reply count | quiet (gray) | as above |
| Background completion | quiet | the parent's work, not the person's |
| Waiting, Scheduled | quiet (never attention today) | unchanged |
| Optional questions | quiet | the agent continues without an answer |
| Blocking question (AskUserQuestion, Codex input, plan question) | quiet | the parent can answer with `hgs answer`; the parent card shows it (2.3) |
| Tool or permission approval, the session's own or a native subagent's | **human** | an approval is the person's security gate; an agent never approves another agent's tool use (automatic approvals are a separate desktop opt-in) |
| Startup folder trust prompt (`*-trust:` questions) | **human** | only the owner may confirm folder trust (`hgs guide launch`) |
| Codex hooks trust, Sign in, sign-in failed, expired login | **human** | only the person can trust hooks or sign in |
| Usage limit reached, rate limit, model unavailable | quiet | the parent moves the work to another account or model |
| Provider error, retrying, recovery blocked or exhausted | quiet | the parent inspects and resumes or relaunches |
| Agent exited, crash, interrupted | quiet | as above |
| Stuck (no progress) | quiet | there is no separate stuck signal today; the parent owns follow-up |
| Review later (a person's own mark) | **human** | the person set it |
| A local unsent draft | unchanged | the person's own text |
| A child's native subagent asking | as the child's own question: quiet | the same parent can answer |

Human signals keep the child's own card amber, appear in counts and notify. Its
nested place under the parent is unchanged, so the person sees whose child asks.

### 2.3 What the parent shows

A parent card aggregates its children (all depths, bounded by the rows already
listed). The Team toggle reads **2 of 5** (working of all children). When a
child holds a quiet request, the toggle adds a gray **Asking: 1**; when a
child holds a human signal, an amber **Needs you: 1**. The aggregate is
informational: it never raises the parent's own attention, notification or
count, and it does not mark anything read.

### 2.4 When the parent is gone or stuck

* **Parent missing, archived, saved (paused or stopped) or its process exited:**
  the child is no longer supervised (1.5). Its pending quiet signals become
  ordinary: a pending question notifies, an unread reply counts. Unmute-style
  replay protection applies: the transition raises the current request once and
  does not replay older ones the tracker already settled.
* **Parent on another computer:** never supervised in this version (its host
  lists it as `missing`); nested in the list, ordinary attention. Follow-up: resolve remote parents in the desktop
  catalog.
* **Parent live but stuck:** the parent's own state surfaces it (its question,
  approval, error or idle reply alerts the person normally). Children stay quiet
  while the parent process is live. No timers escalate quiet signals in this
  version: a clock-driven escalation would notify differently on each device.
* **Parent muted:** children stay quiet; muting the parent is the person's choice
  about the parent only.

### 2.5 Contract alignment

Session status semantics are an owner-directed contract (CLAUDE.md, the
Background section). This request is that direction for supervised children.
This change keeps desktop, the desktop service, shared fixtures and docs aligned:

* `client/core` owns one `quiet(row, signal)` predicate used by the list
  projection and the notification tracker; the service uses the same crate.
* A shared fixture, `client/core/tests/fixtures/supervised-attention.json`,
  lists signal and parent-state cases; Android adopts it in its follow-up.
* `docs/reference.md` documents the link, `adopt`/`parent`, and the policy.
* Android (zerus-sa9qn.4) and iOS (zerus-sa9qn.5) apply the policy, nesting
  and account marks from the shared fixture.

---

## Part 3. UI

### 3.1 Cards

The card adopts the large agent tile of the owner's prototype:

* **Agent tile.** A 34 px rounded tile at the card's left, replacing the provider
  badge in the first line. Since the owner's 2026-11-07 feedback it shows the
  prototype's agent artwork (20 px; 14 px on the 22 px compact tile) on a tile
  with a 1 px border, loaded lazily outside the entry bundle; the provider
  glyph in the agent's color is the fallback. The tile colors are skin tokens
  (`list.agent-tile`, `list.agent-tile-border`): the prototype's `#0f1318` and
  `#232932` in the dark skin, white and `#d3dae1` in the light skin. The account mark stays in the footer with its
  label. Compact rows use a 22 px tile before the mark and title.
* **Card surface.** Rows stay transparent; hover and selection paint the
  surface, as before. An always-painted surface shipped in #1644 and was
  removed at the owner's request on 2026-11-07 as too heavy. Edge marks
  (attention, unread, draft, error) stay as the inset left bar.

Expanded card (94 px), left to right:

```
[tile 34]  title                 [status badge] [unread] [questions]
 (mark)    folder / branch           git marks  [machine]
           current action
           model  effort  [A Work]            [kids 2/3] [team 3 of 5]
```

Compact row (40 px): `[tile 22 + mark] title [status] [questions] [mute] [expand]`.
A compact parent (owner feedback 2026-11-07) takes a second line, 62 px in all:
`[team] 3 of 8 working [Needs you: 1] [Asking: 2] [Errors: 1] [chevron]`, the
same collapse toggle; the summary joins the row's accessible description.

### 3.2 Account mark

* A 16 px circle (14 px on compact rows), filled with the account color, the
  letter in 10 px semibold with an automatically contrasting ink; an emoji mark
  renders at the same size with a tinted ring instead of a fill.
* Expanded cards also show the label in the footer: `[A] Work` ellipsized to
  120 px. Compact rows show the circle only.
* Tooltip and accessible name: "Account: Work (claude on arch)".
* **Accounts screen:** next to **Rename…**, **Color and mark…** opens a dialog
  with the 8 entity palette swatches plus a custom color, an emoji field
  (one grapheme, cleared by **No emoji**), a live preview of the circle, and
  **Reset to default**.

### 3.3 Nesting

* Child cards are full session cards, indented 26 px per level with a 1 px tree
  connector (vertical line and elbow) in `--z-border`. Visual indentation stops
  at 3 levels (78 px); deeper descendants keep the third level's indent and
  their level in the accessible description.
* A parent card's footer shows a **Team** toggle: the people glyph, `3 of 5`
  (working of children) and a chevron. It collapses or expands the parent's
  child sessions; the choice defaults to expanded and this device keeps a
  collapsed parent across relaunches by machine UUID and session ID
  (owner, 2026-11-07, zerus-sa9qn.15). Since zerus-sa9qn.16 the counter and the
  toggle also cover native subagents (one count, one toggle), and a subagent
  row is a compact session row that never expands; their separate toggle is
  gone.
* Children sort by their rank in the project group, as top-level cards do.
  Dragging a child organizes it within its group exactly as before; nesting is
  derived, not a position.
* **from PARENT** chip on a child whose parent is not shown nested (1.4): link
  style, opens the parent when listed.
* A new child row starts compact (owner, 2026-11-07, zerus-sa9qn.14): when a
  link's `at` is newer than what this device recorded for the computer that
  stamped it (`sessions/childRowsSeen`), the row is made compact once. A
  computer seen for the first time is only recorded, so existing rows never
  change; a row the person expands stays expanded. Settings > Sessions >
  **Start new child sessions as compact rows**, on by default, turns it off.

### 3.4 Supervised indicator

A supervised child shows `corner-down-right`, 13 px, before its title in
`--z-color-dim`: handled by the parent above (owner, 2026-11-07,
zerus-sa9qn.16; a plain bell read as "notifications coming", the crossed bell
as muted). Tooltip: "Supervised by PARENT. Only requests that need you notify
you." Its quiet badges are gray as for muted rows; a human signal keeps the
amber badge and edge.

### 3.5 Session menu

* **Detach from PARENT** (follow-up): removes the link (asks for confirmation
  that names the parent; reversible by the parent's adopt). Until then
  `hgs parent SESSION --json` with `clear` does it.
* **Open parent**: the from-PARENT chip opens it; a menu entry is part of the desktop follow-up.
* Mute and Unmute stay independent of supervision.
* **Terminate** and **Move to archive** of a session with live child sessions on
  its computer list them and offer **Also end N child sessions** (Archive:
  **Also archive N child sessions**), off by default (zerus-sa9qn.17). Children
  the action does not apply to now are listed as staying. Only that choice ends
  children: each one runs after the parent's own outcome is done, through its
  own guarded action and never with the parent's Remove working folder choice,
  one at a time, rechecked as the same instance and never retried; one notice
  counts done, failed or changed, and unknown outcomes. A parent that failed or
  whose outcome is unknown keeps every child. Move to archive asks only for such
  a session; any other archives at once, as before. The command-line form is
  `hgs terminate|archive SESSION --with-children`. Nothing propagates
  implicitly: the link names the children, the person's choice is the authority.

### 3.6 Keyboard and screen readers

* The list stays a single tree of `treeitem` rows; a child row gets `aria-level`
  and its accessible description adds
  "child of PARENT, level N". The parent's adds "N child sessions, collapsed" or
  "expanded".
* Left on an expanded parent collapses its children; Right expands; Left on a
  child moves focus to its parent. Existing Up and Down order follows the
  visible rows.
* The account mark is part of the row's accessible name only once ("Account:
  Work"); the letter itself is `aria-hidden`.
* Colors meet 3:1 against the card for the circle and 4.5:1 for the letter ink;
  the letter ink switches between dark and light by luminance.

### 3.7 Text

No UI text separates values with dots (`scripts/check-ui-separators.py`): the
team strip says `3 of 5`, the footer uses spacing, and tooltips use line breaks.

---

## Delivery

1. **Account marks.** Client only: mark resolution, card circle, Accounts
   dialog, Settings option, tests.
2. **Parent link and nesting.** CLI: record at launch, `adopt`, `parent`,
   carry on all paths, listing field, remote forwarding with the help marker.
   Client core: view row fields. Client: tree rows, connector, Team toggle,
   card tile, from-PARENT chip, Detach.
3. **Attention policy.** Host `parent.state`; core `quiet` predicate shared by the
   list and tracker; service projection; fixture; docs.

## Follow-ups (filed as beads)

* Android: nesting, supervised quiet attention and account marks from the shared
  fixture.
* iOS: the same.
* Host-stored account color and emoji (sibling file, mutation, phones): done in
  zerus-sa9qn.6.
* Resolve cross-computer parents for supervision in the desktop catalog.
* Route a supervised child's blocking question to its parent session as a
  guarded message (needs owner direction: it sends text into an agent).
* Drag to nest, **Set parent…** and **Detach from PARENT** from the desktop.
* Forward the proven caller on remote launches (`--parent-reference`).
