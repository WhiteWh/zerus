import { countedHeaderAttention, isWorking, needsAttention, quietAttention, visibleUnreadReply } from "../../store/attention";
import type { CreationCard, GroupingOut } from "../../types";
import type { AgentKind, Session, Subagent } from "./types";
import { currentChildren } from "./subagents";
import { sessionIs } from "../../store/teamsView";
import type { TeamSummary } from "../../types";

export type Row =
  /** `project` is the project's id; `name` and the colour come from the organization's header. */
  | { type: "header"; id: string; project: string; name: string; color: string | null; palette: number; collapsed: boolean; total: number; attention: number; unread: number; working: number }
  | { type: "card"; id: string; session: Session; expandable: boolean; expanded: boolean; compactRow: boolean;
      /** Nesting under parent sessions (zerus-sa9qn.2) and under a team lead: 0 at top level. */
      depth?: number;
      /** The nested card's parent row key and whether it is its parent's last shown child. */
      parentKey?: string; lastChild?: boolean;
      /** A parent's child sessions: all descendants in this group and those working. */
      team?: Team;
      /** A child whose parent is not shown above it: the parent's name. */
      fromParent?: { name: string; key?: string };
      /** A Teams lead's team (its strip); a member under its lead is `teamFirst` and `teamLast` at the ends. */
      teamSummary?: TeamSummary; teamFirst?: boolean; teamLast?: boolean;
      /** Task 455: a lead's member rows (the strip counts exactly these). */
      teamMembersShown?: number }
  | { type: "child"; id: string; parent: Session; child: Subagent; first: boolean; last: boolean }
  | { type: "creation"; id: string; creation: CreationCard }
  /**
   * An ended parent that still has live child sessions (zerus-sa9qn.17): a
   * presentational container its children nest under. It is never a
   * session row: it grants no selection, drag, attention or action of its own.
   */
  | { type: "container"; id: string; project: string; container: Container; depth?: number; parentKey?: string; lastChild?: boolean;
      team?: Team; fromParent?: { name: string; key?: string } };

/**
 * An ended parent shown only for its live children (zerus-sa9qn.17). `key` is
 * its listed row when the filters hide a stopped or paused parent; a parent
 * known only from its children's links (terminated, archived or no longer
 * listed) has none.
 */
export interface Container {
  key?: string;
  name: string;
  title: string;
  agent: AgentKind;
  computer: string;
  machine: string;
  /** "archived": its computer lists it in Archive; "ended": stopped, paused or no longer listed. */
  state: "ended" | "archived";
}

/**
 * A parent's child sessions: all, working, quiet requests its children hold
 * for it (`asking`) and requests only the person can answer (`needsYou`).
 * Informational: it never raises the parent's own attention (spec 2.3).
 */
export interface Team { total: number; working: number; collapsed: boolean; asking?: number; needsYou?: number; errors?: number }

/** Nesting stops after this many levels; deeper children start at top level (spec 1.4). */
export const MAX_DEPTH = 8;

export interface ListInput {
  grouping: GroupingOut;
  /** Each project's rows in list order (groupMembers). */
  members: Map<string, Session[]>;
  /** The state, machine and search filters. */
  visible: (s: Session) => boolean;
  /** Whether some filter or search is active: an empty project is then always hidden (SessionsWindowRows.cpp:559). */
  filtering?: boolean;
  /** "Hide projects without active sessions" (SettingsPage.h:232), on by default. */
  hideEmpty?: boolean;
  /** The window's online state of a row's computer (Qt's `entry.online`). */
  online: (s: Session) => boolean;
  /** The project filter's project ids; empty is every project. */
  projects: readonly string[];
  expanded: (key: string) => boolean;
  compact: (key: string) => boolean;
  /** Archives never expand recorded subagents. */
  children?: boolean;
  includeCompleted?: boolean;
  selected?: { key?: string; subagent?: string; conversation?: string };
  roster?: (s: Session) => Subagent[];
  childDraft?: (parent: Session, child: Subagent) => boolean;
  /** The listed row key of a session's parent session, when the catalog has it. */
  parentKey?: (s: Session) => string | undefined;
  /**
   * The parent a child really depends on (task 467): only such a link nests.
   * Absent, every resolved `parentKey` nests (an older list).
   */
  nestKey?: (s: Session) => string | undefined;
  /** A parent whose child sessions are collapsed. */
  teamCollapsed?: (key: string) => boolean;
  /** An explicit own expansion that can open a retained ancestor's branch. */
  teamExpanded?: (key: string) => boolean;
  /** Favorites bypass ancestor collapse, then use their own child toggle, after every ordinary filter. */
  favorite?: (s: Session) => boolean;
  /** The Teams team a row belongs to, when its computer's team list is loaded. */
  teamOf?: (s: Session) => TeamSummary | undefined;
  /** Whether this Teams team's members are folded under their lead (workspace/collapsedTeams). */
  teamFolded?: (team: string) => boolean;
  /**
   * Keep an ended parent as a container of its listed children (zerus-sa9qn.17):
   * the live list only. Archive rows never use it.
   */
  containers?: boolean;
  /** The machine UUID of a row's computer, to tell a parent gone from this computer from one on another. */
  machineOf?: (s: Session) => string | undefined;
}

/** A listed parent that has ended: not running (stopped or paused) or its agent ended, and not archived. */
export const endedParent = (s: Session) => s.state !== "archived" && (s.state !== "running" || s.status === "ended");

const AGENTS: readonly AgentKind[] = ["claude", "codex", "kimi", "qwen", "hermes", "deepseek", "opencode", "sh"];

/** The title of a session name `agent/project/tag`: its tag. */
const tagOf = (name: string) => name.slice(name.lastIndexOf("/") + 1) || name;

/** Qt's grouped rows (SessionsWindowRows.cpp:289-338, header counts 393-411). */
/**
 * A parent's team with its shown native subagents counted in (zerus-sa9qn.16):
 * they add to all and working. Their requests and errors are the parent's
 * own and already show on its card, so they are not counted twice.
 */
export function withSubagents(team: Team | undefined, kids: readonly Subagent[], collapsed: boolean): Team | undefined {
  if (!kids.length) return team;
  return {
    ...(team ?? { asking: 0, needsYou: 0, errors: 0 }),
    total: (team?.total ?? 0) + kids.length,
    working: (team?.working ?? 0) + kids.filter((child) => child.state === "working").length,
    collapsed: team?.collapsed ?? collapsed,
  };
}

/**
 * One project group's shown rows with the containers of their ended parents
 * (zerus-sa9qn.17), in rank order. A shown child whose parent is
 *  - a listed row of this group that the filters hide and that has ended
 *    (`endedParent`) reveals that row as a container;
 *  - not listed, and its link says `archived` or `saved`, or `missing` on the
 *    child's own computer (terminated, archived or forgotten there) gets one
 *    container per recorded parent, built from the link.
 * A parent that runs, or is listed in another group, leaves its children at
 * top level with their **from PARENT** chip, as before. A revealed parent's
 * own parent is resolved the same way, so ended chains nest. One worklist
 * over the shown rows and one map per kind: no extra read per row.
 */
export function withContainers(listed: readonly Session[], all: readonly Session[], input: Pick<ListInput, "parentKey" | "machineOf">) {
  const containers = new Map<string, Container>();
  const ghostOf = new Map<string, string>();
  if (!input.parentKey) return { ordered: listed, containers, ghostOf };
  const shown = new Set<string>();
  for (const s of listed) shown.add(s.key);
  let byKey: Map<string, Session> | undefined;
  const member = (key: string) => (byKey ??= new Map(all.map((s) => [s.key, s]))).get(key);
  const revealed = new Set<string>();
  const ghosts = new Map<string, Session>();
  const pending = [...listed];
  while (pending.length) {
    const s = pending.pop()!;
    const parent = input.parentKey(s);
    if (parent) {
      if (parent === s.key || shown.has(parent) || revealed.has(parent)) continue;
      const row = member(parent);
      if (!row || !endedParent(row)) continue;
      revealed.add(parent);
      containers.set(parent, { key: row.key, name: row.name, title: row.title, agent: row.agent, computer: row.computer, machine: row.machine, state: "ended" });
      pending.push(row);
      continue;
    }
    const link = s.parent;
    if (!link?.machine || !(link.session_id || link.conversation_id)) continue;
    const gone = link.state === "archived" || link.state === "saved" || (link.state === "missing" && input.machineOf?.(s) === link.machine);
    if (!gone) continue;
    const key = `ended:${s.computer}\n${link.machine}\n${link.session_id || `c:${link.conversation_id}`}`;
    if (!containers.has(key)) {
      const prefix = link.name.slice(0, Math.max(0, link.name.indexOf("/")));
      const agent: AgentKind = prefix === "dsh" ? "deepseek" : (AGENTS as readonly string[]).includes(prefix) ? prefix as AgentKind : s.agent;
      containers.set(key, { name: link.name, title: tagOf(link.name), agent, computer: s.computer, machine: s.machine,
                            state: link.state === "archived" ? "archived" : "ended" });
      ghosts.set(key, { key } as Session);
    }
    ghostOf.set(s.key, key);
  }
  if (!containers.size) return { ordered: listed, containers, ghostOf };
  // In rank order: a revealed parent at its own position, a recorded one just
  // before the first row that names it.
  const ordered: Session[] = [];
  const placed = new Set<string>();
  const place = (s: Session) => {
    const ghost = ghostOf.get(s.key);
    if (ghost && !placed.has(ghost)) { placed.add(ghost); ordered.push(ghosts.get(ghost)!); }
    ordered.push(s);
  };
  for (const s of all) if (shown.has(s.key) || revealed.has(s.key)) place(s);
  return { ordered, containers, ghostOf };
}

export function groupRows(input: ListInput): Row[] {
  const out: Row[] = [];
  // A Teams team's members are not separate list rows: they sit under their
  // lead (Qt's SessionRoles::TeamMember tree). Only a visible lead takes them,
  // so a member filtered into view on its own still lists normally.
  const everyone = [...input.members.values()].flat();
  const teamFor = new Map<string, TeamSummary>();
  for (const s of everyone) {
    const team = input.teamOf?.(s);
    if (team) teamFor.set(s.key, team);
  }
  const leadOf = new Map<string, Session>();
  const rosterOf = new Map<string, Session[]>();
  for (const team of new Set(teamFor.values())) {
    const leadMember = team.members.find((m) => m.lead);
    const lead = leadMember ? everyone.find((s) => sessionIs(leadMember.session, s.computer, s)) : undefined;
    if (!lead) continue;
    leadOf.set(team.id, lead);
    rosterOf.set(team.id, team.members.filter((m) => !m.lead)
      .map((m) => everyone.find((s) => sessionIs(m.session, s.computer, s)))
      .filter((s): s is Session => !!s));
  }
  const wanted = (id: string) => !input.projects.length || input.projects.includes(id);
  const projectOf = new Map<string, string>();
  for (const [id, list] of input.members) for (const s of list) projectOf.set(s.key, id);
  /**
   * Task 455 (owner, 2026-11-09): an unfolded team shows every live member
   * under its lead, in team order, whatever project folder a member's session
   * is filed in (members run in their own worktree folders), and nowhere else.
   * This replaces task 431's rule, which kept only the members of the lead's
   * own folder under it and split the team. A lead that is not shown (filtered
   * out, or in a project the filter leaves out) takes no members: they stay in
   * their own folders. A member that leads its own team (a sub-team) keeps its
   * own place with its team.
   */
  const leadKeys = new Set([...leadOf.values()].map((lead) => lead.key));
  const membersOf = new Map<string, Session[]>();
  const nested = new Set<string>();
  for (const [team, lead] of leadOf) {
    if (!teamFor.has(lead.key) || !input.visible(lead) || !wanted(projectOf.get(lead.key) ?? "")) continue;
    const shown = (rosterOf.get(team) ?? []).filter((member) => member.key !== lead.key && !leadKeys.has(member.key) && input.visible(member) && !nested.has(member.key));
    membersOf.set(team, shown);
    for (const member of shown) nested.add(member.key);
  }
  const card = (s: Session, extra: Partial<Extract<Row, { type: "card" }>> = {}, collapsed = !!input.teamCollapsed?.(s.key)) => {
      const current = currentChildren(s, input.online(s));
      const roster = input.children === false ? [] : (input.roster?.(s) ?? s.subagents ?? []);
      const kids = roster.filter((child) =>
        input.includeCompleted
        || (input.selected?.key === s.key && input.selected.subagent === child.id
          && (!input.selected.conversation || input.selected.conversation === (s.conversationId ?? "")))
        || (!child.group && input.childDraft?.(s, child))
        || child.inactive === false).map((child): Subagent => !current && child.state === "working" ? { ...child, state: "unknown" } : child);
      // One counter and one toggle for a parent's subagents and child
      // sessions (owner, 2026-11-07, zerus-sa9qn.16): the toggle shows or
      // hides both, and is remembered across relaunches (zerus-sa9qn.15).
      const team = withSubagents(extra.team, kids, collapsed);
      const expanded = kids.length > 0 && !team?.collapsed;
      const changed = kids.length !== roster.length || kids.some((child, index) => child !== roster[index]);
      const shown = changed ? { ...s, subagents: kids } : s;
      const summary = teamFor.get(s.key);
      const teamMembers = summary && leadOf.get(summary.id)?.key === s.key ? membersOf.get(summary.id) ?? [] : [];
      const isLead = !!summary && leadOf.get(summary.id)?.key === s.key;
      const membersShown = isLead && teamMembers.length > 0 && !(input.teamFolded?.(summary!.id) ?? false);
      const row: Extract<Row, { type: "card" }> = { type: "card", id: s.key, session: shown, expandable: isLead && teamMembers.length > 0, expanded: expanded || membersShown, compactRow: input.compact(s.key), ...extra, team,
                 teamSummary: isLead ? summary : undefined, teamMembersShown: isLead ? teamMembers.length : undefined };
      out.push(row);
      if (expanded) kids.forEach((child, i) => out.push({ type: "child", id: `${s.key}/${child.id}`, parent: shown, child, first: i === 0, last: i === kids.length - 1 }));
      if (membersShown) {
        teamMembers.forEach((member, i) => out.push({
          type: "card", id: member.key, session: member, expandable: false, expanded: false,
          compactRow: false, depth: (extra.depth ?? 0) + 1, teamFirst: i === 0, teamLast: i === teamMembers.length - 1,
        }));
      }
      return row;
  };
  // One project group's visible cards in rank order, child sessions nested
  // under a parent shown in the same list. Only a real dependence nests
  // (`nestKey`, task 467): a session another one merely launched keeps its
  // card and only names who started it. Nesting never hides a row: a loop
  // or a parent elsewhere leaves the card at top level. A Teams member
  // listed under its lead is not a card of its own.
  const cards = (members: readonly Session[], project = "", all: readonly Session[] = members, favoritesOnly = false) => {
    const listed = members.filter((s) => !nested.has(s.key));
    const resolved = input.containers ? withContainers(listed, all, input) : undefined;
    const list = resolved?.ordered ?? listed;
    const containers = resolved?.containers;
    const parentOf = new Map<string, string>();
    const nests = input.nestKey ?? input.parentKey;
    if (nests) {
      const shown = new Set(list.map((s) => s.key));
      for (const s of list) {
        const parent = resolved?.ghostOf.get(s.key) ?? nests(s);
        if (parent && parent !== s.key && shown.has(parent)) parentOf.set(s.key, parent);
      }
    }
    if (!parentOf.size) {
      for (const s of list) {
        if (favoritesOnly && !input.favorite?.(s)) continue;
        const from = s.parent && input.parentKey ? input.parentKey(s) : undefined;
        card(s, s.parent ? { fromParent: { name: s.parent.name, key: from } } : {});
      }
      return;
    }
    const children = new Map<string, Session[]>();
    for (const s of list) {
      const parent = parentOf.get(s.key);
      if (!parent) continue;
      const siblings = children.get(parent);
      if (siblings) siblings.push(s);
      else children.set(parent, [s]);
    }
    // One depth-first pass from each root in rank order; a loop's first card
    // in rank order becomes its root.
    interface Node { s: Session; depth: number; parent?: string; last: boolean }
    const nodes: Node[] = [];
    const seen = new Set<string>();
    const walk = (root: Session) => {
      const stack: Node[] = [{ s: root, depth: 0, last: true }];
      while (stack.length) {
        const node = stack.pop()!;
        if (seen.has(node.s.key)) continue;
        seen.add(node.s.key);
        nodes.push(node);
        if (node.depth >= MAX_DEPTH) continue;
        const kids = (children.get(node.s.key) ?? []).filter((c) => !seen.has(c.key));
        for (let i = kids.length - 1; i >= 0; i--) stack.push({ s: kids[i], depth: node.depth + 1, parent: node.s.key, last: i === kids.length - 1 });
      }
    };
    // Cards below a session without a shown parent; the rest sit in a loop.
    const rooted = new Set<string>();
    for (const s of list) {
      if (parentOf.has(s.key)) continue;
      const stack = [s];
      while (stack.length) {
        const at = stack.pop()!;
        if (rooted.has(at.key)) continue;
        rooted.add(at.key);
        stack.push(...(children.get(at.key) ?? []));
      }
    }
    // In rank order: a real root, or the first card of a loop, starts a walk.
    for (const s of list) if (!seen.has(s.key) && (!parentOf.has(s.key) || !rooted.has(s.key))) walk(s);
    // Below the depth bound a card starts at top level again; nothing is hidden.
    for (const s of list) if (!seen.has(s.key)) walk(s);
    // Each parent's descendants, summed once from the leaves up (linear).
    const teams = new Map<string, Required<Omit<Team, "collapsed">>>();
    for (let i = nodes.length - 1; i >= 0; i--) {
      const { s, parent } = nodes[i];
      if (!parent) continue;
      const below = teams.get(s.key), sum = teams.get(parent) ?? { total: 0, working: 0, asking: 0, needsYou: 0, errors: 0 };
      const up = input.online(s);
      // A quiet request waits for the parent; any other one needs the person (spec 2.3).
      const asks = up && s.status === "attention" && s.state !== "archived";
      sum.total += 1 + (below?.total ?? 0);
      sum.working += (isWorking(s, up) ? 1 : 0) + (below?.working ?? 0);
      sum.asking += (asks && s.quiet ? 1 : 0) + (below?.asking ?? 0);
      sum.errors += (up && s.status === "error" && s.state !== "archived" ? 1 : 0) + (below?.errors ?? 0);
      sum.needsYou += (!s.muted && (asks ? !s.quiet : !s.quiet && needsAttention(s, up)) ? 1 : 0) + (below?.needsYou ?? 0);
      teams.set(parent, sum);
    }
    // Retain only favorite paths through collapsed ancestors, in one reverse
    // pass. Counts still cover the ordinary filtered hierarchy above.
    const retained = new Set<string>();
    for (let i = nodes.length - 1; i >= 0; i--) {
      const { s, parent } = nodes[i];
      if (input.favorite?.(s) || retained.has(s.key)) {
        retained.add(s.key);
        if (parent) retained.add(parent);
      }
    }
    const hidden = new Set<string>();
    // A connector ends at the last *shown* sibling, even when later siblings
    // stayed collapsed. The previous sibling's row is still local to this pass.
    const lastShown = new Map<string, Extract<Row, { type: "card" | "container" }>>();
    const connect = (row: Extract<Row, { type: "card" | "container" }>, parent?: string) => {
      if (!parent) return;
      const previous = lastShown.get(parent);
      if (previous) previous.lastChild = false;
      row.lastChild = true;
      lastShown.set(parent, row);
    };
    for (const { s, depth, parent, last } of nodes) {
      // Hidden with a collapsed parent, unless it is a favorite or leads to one.
      const onlyFavorites = parent ? hidden.has(parent) : favoritesOnly;
      const isFavorite = !!input.favorite?.(s);
      if (onlyFavorites && !retained.has(s.key)) {
        hidden.add(s.key);
        continue;
      }
      const counts = teams.get(s.key);
      // Retained context starts shut, but its own explicit expansion works
      // independently of the ancestor/project that hid the surrounding rows.
      const collapsed = !!input.teamCollapsed?.(s.key)
        || onlyFavorites && !isFavorite && !input.teamExpanded?.(s.key);
      if (collapsed) hidden.add(s.key);
      const from = !parent && s.parent ? { fromParent: { name: s.parent.name, key: input.parentKey?.(s) } } : {};
      const container = containers?.get(s.key);
      if (container) {
        const row: Extract<Row, { type: "container" }> = { type: "container", id: `container:${s.key}`, project, container, depth, parentKey: parent,
                   lastChild: parent ? last : undefined, team: counts && { ...counts, collapsed }, ...from };
        out.push(row); connect(row, parent);
        continue;
      }
      connect(card(s, { depth, parentKey: parent, lastChild: parent ? last : undefined, team: counts && { ...counts, collapsed }, ...from }, collapsed), parent);
    }
  };
  const headers = input.grouping.loaded ? input.grouping.headers : [];
  if (headers.length <= 1) {
    // One project, or none presented yet: no header, and nothing collapses (293, 324, 326).
    for (const [id, list] of input.members) if (wanted(id)) cards(list.filter(input.visible), id, list);
    return out;
  }
  for (const h of headers) {
    if (!wanted(h.id)) continue;
    const all = input.members.get(h.id) ?? [];
    // Task 455: members shown under a lead elsewhere are not this folder's rows.
    const list = all.filter((s) => input.visible(s) && !nested.has(s.key));
    // A project without visible members stays hidden while filtering, when
    // the owner asked to hide empty projects (the default), and when the
    // organization cannot reach it (SessionsWindowRows.cpp:559, 323).
    if (!list.length && (input.filtering || input.hideEmpty !== false || !h.accessible)) continue;
    // The counters cover members the filters hide; muted members never count as attention or unread (398-404).
    // A computer that is not online counts only Review later as attention and nothing as working.
    let attention = 0, unread = 0, working = 0;
    for (const s of all) {
      const up = input.online(s);
      if (countedHeaderAttention(s, up)) attention++;
      // An offline computer's unread replies count too (visibleUnreadReply, HgsClient.h:104-105).
      if (!quietAttention(s) && visibleUnreadReply(s, up)) unread++;
      if (isWorking(s, up)) working++;
    }
    out.push({ type: "header", id: `h:${h.id}`, project: h.id, name: h.name, color: h.color, palette: h.palette,
               collapsed: h.collapsed, total: list.length, attention, unread, working });
    // A collapsed project still lists its favorites (and the paths to them).
    cards(list, h.id, all, h.collapsed);
  }
  return out;
}
