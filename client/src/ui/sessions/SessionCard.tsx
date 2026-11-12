import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";
import { steadyTitle } from "../lib/steady-title";
import { tr, trNoop } from "../../i18n";
import { AGENT_LABEL, Icon } from "../lib/icons";
import { computerFresh, computerOffline } from "../../store/catalog";
import { quietAttention, visibleUnreadReply } from "../../store/attention";
import { optionalQuestionCount, optionalQuestionCountText } from "./optional-questions";
import { now } from "../../store/ui";
import { settings } from "../../store/settings";
import { machineAppearance } from "../../store/machineAppearance";
import { sessionMark, type AccountMark } from "../../store/accountMarks";
import type { AgentKind, Session, Status, Subagent } from "./types";
import type { Container, Team } from "./group-rows";
import type { LogoPath } from "../lib/agent-logos";
import { currentChildren, workingChildren } from "./subagents";

// Card anatomy follows tray/src/SessionCardDelegate.h paintCard() at the
// compact density (94 px rows, the only density in the web client):
//   line 1  title (14px/600) + status badge (20px) [+ unread] [+ questions] ... provider badge (18px)
//   line 2  branch/folder icon + "folder / branch" (11px) ... git marks, machine badge (18px)
//   line 3  current action (12px)
//   footer  model (10px) + effort chip (16px) ... agents counter (20px)
// Rows are transparent; hover and selection paint a neutral surface, and
// attention, unread, draft and error rows carry a 3px edge mark.

/** The row's status; rows of a computer whose stream is down show as last known (offline). */
export const statusOf = (s: Session): Status => (computerOffline(s.computer) ? "offline" : s.status);

/** Seconds of the current turn, while the row is working and its start is known. */
function elapsed(s: Session): number | undefined {
  const status = statusOf(s);
  if ((status !== "working" && status !== "background") || !s.turnStarted) return undefined;
  return Math.max(0, Math.floor(now() - s.turnStarted));
}

// The core's closed status vocabulary (SessionPresentation::status). Only these
// exact captions are translated: a composite caption keeps its English text.
const CAPTIONS = new Set<string>([
  trNoop("Agent exited"), trNoop("Archived"), trNoop("Background"), trNoop("Compacting"), trNoop("Connecting"), trNoop("Error"),
  trNoop("Interrupted"), trNoop("Model unavailable"), trNoop("Needs approval"), trNoop("Needs attention"), trNoop("Needs input"),
  trNoop("Not tracked"), trNoop("Paused"), trNoop("Provider error"), trNoop("Ready"), trNoop("Retrying"), trNoop("Scheduled"),
  trNoop("Sign in"), trNoop("Sign in required"), trNoop("Starting"), trNoop("Status unknown"), trNoop("Stopped"),
  trNoop("Using a tool"), trNoop("Waiting for acceptance"), trNoop("Waiting for result"), trNoop("Waiting to retry"), trNoop("Warming cache"), trNoop("Working"),
]);
export const captionText = (caption: string) => (CAPTIONS.has(caption) ? tr(caption) : caption);

/** The Qt caption (SessionPresentation::status) with the Working clock, as Qt shows it. */
/**
 * The status a row shows, with its Working time. `seconds: false` is for a
 * title or tooltip, which never carries a live seconds counter (zerus-5ryrx):
 * minutes only, as past the first hour.
 */
export function statusCaption(s: Session, seconds = true): string {
  if (statusOf(s) === "offline") return tr("Offline");
  if (s.status === "ready" && s.draft) return tr("Draft");
  const worked = elapsed(s);
  if (worked === undefined) return captionText(s.caption);
  const h = Math.floor(worked / 3600), m = Math.floor((worked % 3600) / 60), sec = worked % 60;
  return `${captionText(s.caption)} (${h ? `${h}h ${m}m` : seconds ? `${m}m ${String(sec).padStart(2, "0")}s` : `${m}m`})`;
}

/** Short state for strip tiles: elapsed time while working, otherwise the caption. */
export function stripCaption(s: Session): string {
  const seconds = elapsed(s);
  return seconds === undefined ? statusCaption(s) : `${Math.floor(seconds / 60)}m`;
}

// Badge precedence (SessionCardDelegate::statusOf): attention/error, working or
// background, draft, new reply, scheduled, waiting, paused, neutral.
export type BadgeKind =
  | "attention" | "error" | "working" | "background" | "draft" | "unread" | "scheduled" | "waiting" | "paused"
  | "neutral" | "muted-attention" | "muted-unread";

export function badgeKind(s: Session): BadgeKind {
  const status = statusOf(s);
  if (status === "attention") return quietAttention(s) ? "muted-attention" : "attention";
  // A supervised child's error is its parent's to handle: gray, like its other requests (zerus-rxz9k).
  if (status === "error" && s.quiet && !s.muted) return "muted-attention";
  if (status === "error" || status === "working" || status === "background") return status;
  if (s.draft && status !== "offline") return "draft";
  if (visibleUnreadReply(s, computerFresh(s.computer))) return quietAttention(s) ? "muted-unread" : "unread";
  if (status === "scheduled" || status === "waiting" || status === "paused") return status;
  return "neutral";
}

// Edge mark: attention/error > draft > unread; muted rows turn it grey.
export function emphasis(s: Session): string | undefined {
  const status = statusOf(s);
  if (quietAttention(s) && !(s.quiet && !s.muted && s.reviewLater) && (status === "attention" || visibleUnreadReply(s, computerFresh(s.computer)))) return "muted";
  if (status === "attention" || s.reviewLater) return "attention";
  if (status === "error") return s.quiet && !s.muted ? "muted" : "error";
  if (s.draft) return "draft";
  if (visibleUnreadReply(s, computerFresh(s.computer))) return "unread";
  return undefined;
}

// Status glyphs from SessionStatusBadge::paint, on a 12px box centred where
// Qt centres them (x+11 inside the badge).
const BADGE_GLYPH: Partial<Record<BadgeKind, string>> = {
  paused: `<path d="M4 3V9M8 3V9" stroke-width="1.5"/>`,
  draft: `<path d="M3 9L8.5 3.5M7.5 2.5L9.5 4.5M3 9L2.5 9.5" stroke-width="1.4"/>`,
  scheduled: `<circle cx="6" cy="6" r="4.5"/><path d="M6 6V3.2M6 6H8.2"/>`,
  waiting: `<circle cx="6" cy="6" r="4.5"/><path d="M6 6V3.2M6 6H8.2"/>`,
  unread: `<rect x="2" y="3" width="8" height="6" rx="1" stroke-width="1"/><path d="M2 3L6 6L10 3" stroke-width="1"/>`,
  "muted-unread": `<rect x="2" y="3" width="8" height="6" rx="1" stroke-width="1"/><path d="M2 3L6 6L10 3" stroke-width="1"/>`,
};

export function StatusBadge(props: { kind: BadgeKind; caption: string; iconOnly?: boolean }) {
  return (
    <span class={`sbadge k-${props.kind}`} classList={{ "icon-only": props.iconOnly }}>
      <Show when={props.kind !== "neutral"}>
        <Show when={BADGE_GLYPH[props.kind]} fallback={<i class="sb-mark" aria-hidden="true" />}>
          {(g) => <svg class="sb-glyph" width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" innerHTML={g()} />}
        </Show>
      </Show>
      <Show when={!props.iconOnly}><span class="sb-label">{props.caption}</span></Show>
    </span>
  );
}

// Provider glyphs from IdentityBadges::paint (pen 1.4), centred at x+10.
const PROVIDER_GLYPH: Record<AgentKind, string> = {
  claude: Array.from({ length: 8 }, (_, i) => `<path transform="rotate(${i * 45} 6 6)" d="M6 8V11"/>`).join(""),
  kimi: `<path d="M3 2V10M3 6L9 2M3 6L9 10"/>`,
  opencode: `<path d="M4 3L1 6L4 9M8 3L11 6L8 9"/>`,
  codex: `<path d="M2 3L5 6L2 9M7 9H10"/>`,
  deepseek: `<path d="M2 3L5 6L2 9M7 9H10"/>`,
  sh: `<path d="M2 3L5 6L2 9M7 9H10"/>`,
};

/** The provider glyph alone (IdentityBadges::paint), for menus and filter chips. */
export function ProviderGlyph(props: { agent: AgentKind; size?: number }) {
  return (
    <svg width={props.size ?? 12} height={props.size ?? 12} viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" innerHTML={PROVIDER_GLYPH[props.agent]} />
  );
}

export function AgentBadge(props: { agent: AgentKind; narrow?: boolean; max?: number }) {
  return (
    <span class="agent-badge" classList={{ narrow: props.narrow }} style={{ "--agent": `var(--z-agent-${props.agent})`, "max-width": `${props.max ?? 95}px` }} title={AGENT_LABEL[props.agent]}>
      <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" innerHTML={PROVIDER_GLYPH[props.agent]} />
      <Show when={!props.narrow}><span class="ab-label">{AGENT_LABEL[props.agent]}</span></Show>
    </span>
  );
}

/**
 * A supervised child: its parent handles its requests (spec 3.4). A dim
 * arrow turning down and right, before the title: handled by the parent
 * above (owner, 2026-11-07, zerus-sa9qn.16; a bell read as notifications, a
 * crossed bell as muted).
 */
export function SupervisedMark(props: { session: Session }) {
  const name = () => props.session.parent?.name.split("/").pop() || props.session.parent?.name || "";
  return (
    <span class="sc-supervised" role="img" aria-label={tr("Supervised by %1", name())}
      title={tr("Supervised by %1\nOnly requests that need you notify you.", props.session.parent?.name ?? "")}>
      <Icon name="corner-down-right" size={13} />
    </span>
  );
}

// The prototype's agent artwork loads once, outside the entry chunk; until
// then, and for agents without artwork, tiles show the provider glyph.
const [agentLogos, setAgentLogos] = createSignal<Readonly<Record<string, readonly LogoPath[]>>>();
let logosRequested = false;
function logoOf(agent: AgentKind): readonly LogoPath[] | undefined {
  if (!logosRequested) {
    logosRequested = true;
    void import("../lib/agent-logos").then((module) => setAgentLogos(module.AGENT_LOGOS)).catch(() => {});
  }
  return agentLogos()?.[agent];
}

/** The large agent mark at the card's left, on a bordered tile (spec 3.1, the owner's prototype). */
export function AgentTile(props: { agent: AgentKind; size?: number }) {
  const size = () => props.size ?? 34;
  // 20 px artwork on the 34 px tile, 14 px on the compact row's 22 px tile.
  const mark = () => (size() <= 24 ? 14 : Math.round(size() * 0.59));
  return (
    <span class="agent-tile" style={{ "--agent": `var(--z-agent-${props.agent})`, "--tile": `${size()}px` }} title={AGENT_LABEL[props.agent]}>
      <Show when={logoOf(props.agent)} fallback={
        <svg width={mark()} height={mark()} viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" innerHTML={PROVIDER_GLYPH[props.agent]} />
      }>
        {(paths) => (
          <svg class="agent-logo" width={mark()} height={mark()} viewBox="0 0 24 24" aria-hidden="true">
            <For each={paths()}>{(path) => <path d={path[0]} fill={path[1]} />}</For>
          </svg>
        )}
      </Show>
    </span>
  );
}

function TeamBadge(props: { kind: "asking" | "needs"; count: number }) {
  const asking = () => props.kind === "asking";
  const label = () => asking() ? tr("Asking: %1", props.count) : tr("Needs you: %1", props.count);
  const tooltip = () => asking() ? tr("Child sessions waiting for their parent's answer") : tr("Child sessions with a request only you can answer");
  return (
    <span class={`team-badge ${asking() ? "team-asking" : "team-needs"}`} role="img" aria-label={label()} title={tooltip()}>
      <Icon name={asking() ? "question" : "attention"} size={12} />
      <span aria-hidden="true">{props.count}</span>
    </span>
  );
}

/** A parent's child sessions: working of all, collapsing them (spec 3.3). */
export function TeamToggle(props: { team: Team; onToggle?: () => void }) {
  return (
    <button class="sc-team" classList={{ open: !props.team.collapsed }} aria-expanded={!props.team.collapsed}
      title={props.team.collapsed ? tr("Show subagents and child sessions") : tr("Hide subagents and child sessions")}
      onClick={(e) => { e.stopPropagation(); props.onToggle?.(); }} onDblClick={(e) => e.stopPropagation()}>
      <svg class="team-glyph" width="16" height="12" viewBox="0 0 16 12" fill="none" stroke="currentColor" stroke-width="1.2" aria-hidden="true">
        <circle cx="8" cy="3" r="2" /><circle cx="3" cy="5" r="1.6" /><circle cx="13" cy="5" r="1.6" />
        <path d="M4 11A4 3 0 0 1 12 11M0 10.5A3 2.4 0 0 1 4.5 9M16 10.5A3 2.4 0 0 0 11.5 9" />
      </svg>
      <span>{tr("%1 of %2", props.team.working, props.team.total)}</span>
      <Show when={props.team.asking}><TeamBadge kind="asking" count={props.team.asking ?? 0} /></Show>
      <Show when={props.team.needsYou}><TeamBadge kind="needs" count={props.team.needsYou ?? 0} /></Show>
      <svg class="chev" width="8" height="6" viewBox="0 0 8 6" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M1 2L4 5L7 2" /></svg>
    </button>
  );
}

/** A child shown away from its parent: the parent's name, opening it when listed. */
export function FromParent(props: { parent: { name: string; key?: string }; onOpen?: (key: string) => void }) {
  const short = () => props.parent.name.split("/").pop() || props.parent.name;
  return (
    <Show when={props.parent.key} fallback={<span class="sc-from" title={tr("Started by %1\nThat session is not listed.", props.parent.name)}>{tr("from %1", short())}</span>}>
      {(key) => <button class="sc-from link" title={tr("Started by %1\nClick to open it.", props.parent.name)}
        onClick={(e) => { e.stopPropagation(); props.onOpen?.(key()); }} onDblClick={(e) => e.stopPropagation()}>{tr("from %1", short())}</button>}
    </Show>
  );
}

/** A stable palette slot (0-7) for a name, until machine and project colours come from the listing. */
export function paletteIndex(name: string): number {
  let hash = 0;
  for (let i = 0; i < name.length; i++) hash = (hash * 31 + name.charCodeAt(i)) | 0;
  return Math.abs(hash) % 8;
}

export function MachineBadge(props: { alias: string; computer: string; vivid?: boolean; offline?: boolean }) {
  const appearance = () => machineAppearance(settings, props.alias, `var(--z-entity-palette-${paletteIndex(props.alias)})`);
  return (
    <span class="entity mbadge" classList={{ vivid: props.vivid ?? appearance().vivid, offline: computerOffline(props.computer) }}
      style={{ "--entity": appearance().color }} title={props.computer === "" ? tr("%1 (this computer)", props.alias) : props.alias}>
      {props.alias}{props.offline ? tr(" (offline)") : ""}
    </span>
  );
}

/** The account's colored circle: its letter, or its emoji with emoji account marks (spec 3.2). */
export function AccountMarkCircle(props: { mark: AccountMark; size?: number }) {
  return (
    <span class="acct-mark" classList={{ emoji: props.mark.isEmoji }} role="img" aria-label={tr("Account: %1", props.mark.label)}
      style={{ "--mark": props.mark.color, "--mark-ink": props.mark.ink, "--mark-size": `${props.size ?? 16}px` }}>
      <span aria-hidden="true">{props.mark.glyph}</span>
    </span>
  );
}

const markTooltip = (s: Session, mark: AccountMark) =>
  [tr("Account: %1", mark.label), mark.machine ? tr("%1 on %2", AGENT_LABEL[s.agent], mark.machine) : AGENT_LABEL[s.agent]].join("\n");

/** The expanded card's account: the circle and the label. */
export function AccountChip(props: { session: Session }) {
  const mark = () => sessionMark(props.session);
  return (
    <Show when={mark()}>{(m) => (
      <span class="acct-chip" title={markTooltip(props.session, m())}>
        <AccountMarkCircle mark={m()} />
        <span class="acct-chip-label ell" aria-hidden="true">{m().label}</span>
      </span>
    )}</Show>
  );
}

type Git = NonNullable<Session["git"]>;
interface GitMark { icon: string; count?: string; tone: "muted" | "attention" | "incoming" | "synced" | "conflict" }

// GitStatusBadge::marks: changed files, outgoing, incoming, then unverified or synced.
function gitFreshness(g: Git, at = Date.now()) {
  const age = g.receivedAt === undefined ? 0 : at - g.receivedAt;
  const fresh = (g.localFresh ?? g.state !== "unknown") && age >= 0 && (g.streamed === true || age < 30_000);
  const verified = fresh && (g.remoteVerified ?? g.state !== "unknown")
    && (g.remoteAge ?? 0) + age / 1000 < 60 && g.countsKnown !== false;
  return { fresh, verified };
}
export function gitMarks(g: Git, at = Date.now()): GitMark[] {
  const { fresh, verified } = gitFreshness(g, at);
  const count = (n: number) => (n > 99 ? "99+" : String(n));
  const marks: GitMark[] = [];
  if (g.changed) marks.push({ icon: "git-diff", count: count(g.changed), tone: !fresh ? "muted" : g.conflicts ? "conflict" : "attention" });
  if (g.ahead) marks.push({ icon: "git-push", count: count(g.ahead), tone: verified ? "attention" : "muted" });
  if (g.behind) marks.push({ icon: "git-pull", count: count(g.behind), tone: verified ? "incoming" : "muted" });
  if (!verified) marks.push({ icon: "git-unknown", tone: "muted" });
  else if (!g.changed && !g.ahead && !g.behind && !g.detached && !g.unborn) marks.push({ icon: "git-synced", tone: "synced" });
  return marks;
}

export function gitTooltip(s: Session, at = Date.now()): string {
  const g = s.git!;
  const { fresh, verified } = gitFreshness(g, at);
  const ref = verified ? tr("verified upstream") : tr("last known upstream; remote unverified");
  return [
    tr("Git status (current checkout)"),
    g.root || s.gitRoot || s.cwdPath || s.cwd,
    s.branch ? tr("Branch: %1", s.branch) : "",
    g.upstream ? tr("Upstream: %1", g.upstream) : "",
    g.changedKnown !== false ? tr("%1 changed or new files (including staged and untracked files)", g.changed) : "",
    g.conflicts ? tr("%1 files with merge conflicts", g.conflicts) : "",
    g.aheadKnown !== false ? tr("%1 outgoing commits (%2)", g.ahead, ref) : "",
    g.behindKnown !== false ? tr("%1 incoming commits (%2)", g.behind, ref) : "",
    !fresh ? tr("Git status is unavailable or out of date.") : "",
    !verified ? tr("Remote publication has not been verified recently.") : !g.changed && !g.ahead && !g.behind ? tr("Clean checkout. Branch matches the verified remote.") : "",
  ].filter(Boolean).join("\n");
}

export function GitMarks(props: { session: Session }) {
  const marks = createMemo(() => props.session.git ? gitMarks(props.session.git, now() * 1000) : [], [], {
    equals: (a, b) => a.length === b.length && a.every((m, i) => m.icon === b[i].icon && m.count === b[i].count && m.tone === b[i].tone),
  });
  const tip = steadyTitle(() => (props.session.git ? gitTooltip(props.session, now() * 1000) : ""));
  return (
    <span class="gmarks" title={tip.title()} onPointerEnter={tip.enter} onPointerLeave={tip.leave}>
      <For each={marks()}>{(m) => (
        <span class={`gmark t-${m.tone}`} classList={{ counted: !!m.count }}>
          <Icon name={m.icon} size={16} />
          <Show when={m.count}><span>{m.count}</span></Show>
        </span>
      )}</For>
    </span>
  );
}

/** Children counter: two-person glyph, compact count, chevron (SessionCardDelegate.h:520-543). */
export function AgentsCounter(props: { label: string; expandable: boolean; expanded: boolean; onToggle?: () => void }) {
  const glyph = (
    <svg class="kids-glyph" width="16" height="12" viewBox="0 0 16 12" fill="none" stroke="currentColor" stroke-width="1.2" aria-hidden="true">
      <circle cx="4" cy="3" r="2" /><circle cx="10" cy="4" r="2" />
      <path d="M0 9A4 3 0 0 1 8 9M6 9.5A3.5 2.5 0 0 1 13 9.5" />
    </svg>
  );
  return (
    <Show when={props.expandable} fallback={<span class="sc-kids static">{glyph}<span>{props.label}</span></span>}>
      <button class="sc-kids" classList={{ open: props.expanded }} aria-expanded={props.expanded}
        title={props.expanded ? tr("Hide subagents") : tr("Show subagents")}
        onClick={(e) => { e.stopPropagation(); props.onToggle?.(); }} onDblClick={(e) => e.stopPropagation()}>
        {glyph}
        <span>{props.label}</span>
        <svg class="chev" width="8" height="6" viewBox="0 0 8 6" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M1 2L4 5L7 2" /></svg>
      </button>
    </Show>
  );
}

// Qt keeps min(72, natural width) of the title before badges shrink (60 on
// compact rows). CSS cannot read the natural width, so estimate it from the
// 14px DemiBold average advance.
const titleReserve = (name: string, max: number) => ({ "min-width": `${Math.min(max, Math.ceil(name.length * 7.6))}px` });

function detailText(s: Session): string {
  if (s.summary) return s.summary;
  if (visibleUnreadReply(s, computerFresh(s.computer))) return tr("Response ready to read");
  if (s.status === "ready") return tr("Waiting for your next message");
  if (s.status === "paused") return tr("Resume to continue");
  return "";
}

export const agentsLabel = (s: Session) => {
  const kids = s.subagents ?? [];
  return `${workingChildren(s, !computerOffline(s.computer))}/${kids.length}`;
};

/** The checkout's folder name, as the card's second line shows it. */
export function folderOf(s: Session) {
  return s.cwd.split(/[\\/]/).filter(Boolean).pop() ?? "";
}

/** Optional questions remain separate from blocking input and session status. */
function OptionalQuestionsChip(props: { session: Session }) {
  const optionalCount = () => optionalQuestionCount(props.session, computerFresh(props.session.computer));
  return <Show when={optionalCount()}>
    <span class="qchip" classList={{ muted: quietAttention(props.session) }} title={tr(optionalCount() === 1 ? "%1 optional question waiting\nThe agent can continue without an answer. Answer or skip it in Activity.\nClick to open." : "%1 optional questions waiting\nThe agent can continue without an answer. Answer or skip it in Activity.\nClick to open.", optionalCount())}>
      <Icon name="question" size={16} />{optionalQuestionCountText(optionalCount())}
    </span>
  </Show>;
}

export function SessionCard(props: {
  session: Session;
  selected: boolean;
  expandable: boolean;
  expanded: boolean;
  onToggleChildren: () => void;
  team?: Team;
  onToggleTeam?: () => void;
  fromParent?: { name: string; key?: string };
  onOpenParent?: (key: string) => void;
}) {
  const s = () => props.session;
  const kind = () => badgeKind(s());
  return (
    <div class="scard tiled" classList={{ selected: props.selected, offline: statusOf(s()) === "offline", muted: !!s().muted }} data-emphasis={emphasis(s())}>
      <span class="sc-tile"><AgentTile agent={s().agent} /></span>
      <div class="sc-l1">
        {/* Supervision and Mute are independent: the arrow before the name,
            the crossed bell right after it (owner, 2026-11-08). */}
        <Show when={s().parent?.state === "live"}><SupervisedMark session={s()} /></Show>
        <span class="sc-title ell" style={titleReserve(s().title, 72)} title={s().name}>{s().title}</span>
        <Show when={s().muted}><Icon name="mute" size={14} class="sc-mute" /></Show>
        <StatusBadge kind={kind()} caption={statusCaption(s())} />
        <Show when={visibleUnreadReply(s(), computerFresh(s().computer)) && (s().status === "working" || s().status === "attention" || s().draft)}>
          <span class="sc-extra"><StatusBadge kind={quietAttention(s()) ? "muted-unread" : "unread"} caption="" iconOnly /></span>
        </Show>
        <OptionalQuestionsChip session={s()} />
        <span class="grow" />
      </div>
      <div class="sc-l2">
        <Icon name={s().branch ? "git-branch" : "folder"} size={14} class="sc-meta-ico" />
        <span class="sc-meta ell">{folderOf(s())}{s().branch ? ` / ${s().branch}` : ""}</span>
        <span class="grow" />
        <Show when={s().git && s().status !== "ended"}><GitMarks session={s()} /></Show>
        <MachineBadge alias={s().machine} computer={s().computer} offline={statusOf(s()) === "offline"} />
      </div>
      <div class="sc-l3">
        <Show when={props.fromParent}>{(parent) => <FromParent parent={parent()} onOpen={props.onOpenParent} />}</Show>
        <Show when={s().reviewLater}><Icon name="attention" size={14} class="sc-review" /></Show>
        <span class="ell">{detailText(s())}</span>
      </div>
      <div class="sc-foot">
        <span class="sc-model ell">{s().model}</span>
        <Show when={s().effort}><span class="sc-effort">{s().effort}</span></Show>
        <AccountChip session={s()} />
        <span class="grow" />
        <Show when={!props.team && (s().subagents ?? []).length}>
          <AgentsCounter label={agentsLabel(s())} expandable={props.expandable} expanded={props.expanded} onToggle={props.onToggleChildren} />
        </Show>
        <Show when={props.team}>{(team) => <TeamToggle team={team()} onToggle={props.onToggleTeam} />}</Show>
      </div>
    </div>
  );
}

const CHILD_STATUS: Record<Subagent["state"], { kind: BadgeKind; caption: string }> = {
  working: { kind: "working", caption: trNoop("Working") },
  ready: { kind: "neutral", caption: trNoop("Ready") },
  attention: { kind: "attention", caption: trNoop("Needs input") },
  error: { kind: "error", caption: trNoop("Error") },
  paused: { kind: "paused", caption: trNoop("Paused") },
  unknown: { kind: "neutral", caption: trNoop("Unknown") },
  ended: { kind: "neutral", caption: trNoop("Ended") },
};
export const childStatus = (c: Subagent, current = true): { kind: BadgeKind; caption: string } => current
  ? { ...CHILD_STATUS[c.state], caption: tr(CHILD_STATUS[c.state].caption) }
  : { kind: "neutral", caption: tr("Recorded") };

/**
 * Native subagent row (40 px), indented under its parent: a compact session
 * row that never expands (owner, 2026-11-07, zerus-sa9qn.16): the agent's
 * tile, the parent's account mark, the name and the status, then the expand
 * button's empty slot; the summary is its tooltip.
 */
export function ChildCard(props: { parent: Session; child: Subagent; last: boolean; first: boolean; selected: boolean }) {
  const status = () => childStatus(props.child, currentChildren(props.parent, !computerOffline(props.parent.computer)));
  return (
    <div class="child-row" classList={{ last: props.last, first: props.first }}>
      <i class="tree" aria-hidden="true" />
      <div class="scard compact child" classList={{ selected: props.selected }} title={props.child.summary || undefined}>
        <span class="cc-tile"><AgentTile agent={props.parent.agent} size={22} /></span>
        {/* A subagent runs on its parent's account (owner, 2026-11-08). */}
        <Show when={sessionMark(props.parent)}>{(m) => <span class="cc-mark" title={markTooltip(props.parent, m())}><AccountMarkCircle mark={m()} size={14} /></span>}</Show>
        <span class="sc-title ell" style={titleReserve(props.child.name, 60)}>{props.child.name}</span>
        <StatusBadge kind={status().kind} caption={status().caption} />
        {/* The expand button's slot, empty, so the status lines up with other rows. */}
        <span class="cc-expand cc-slot" aria-hidden="true" />
      </div>
    </div>
  );
}

/** The text of a parent's child sessions summary, for its compact row and its accessible description. */
export function teamSummary(team: Team): string[] {
  return [
    tr("%1 of %2 working", team.working, team.total),
    team.needsYou ? tr("Needs you: %1", team.needsYou) : "",
    team.asking ? tr("Asking: %1", team.asking) : "",
    team.errors ? tr("Errors: %1", team.errors) : "",
  ].filter(Boolean);
}

/**
 * A compact parent's second line: its child sessions at a glance, collapsing
 * them. The chevron keeps its place; on a narrow row the working and error
 * counters shorten to their numbers (zerus-sa9qn.11), and the button and row
 * descriptions keep every count in words.
 */
/**
 * Whether the summary shows short counters (zerus-sa9qn.12): it shortens once
 * its long labels overflow, in any language, and lengthens again only when
 * the width they needed is available. `needed` is that width (0 while long).
 */
export function fitCounters(state: { narrow: boolean; needed: number }, scrollWidth: number, clientWidth: number): { narrow: boolean; needed: number } {
  if (!state.narrow) return scrollWidth > clientWidth + 1 ? { narrow: true, needed: scrollWidth } : state;
  return clientWidth >= state.needed ? { narrow: false, needed: 0 } : state;
}

function CompactTeam(props: { team: Team; onToggle?: () => void }) {
  const title = () => (props.team.collapsed ? tr("Show subagents and child sessions") : tr("Hide subagents and child sessions"));
  // Short counters when the long ones do not fit; the container query below
  // about 300 px is the first approximation before any measurement.
  let body: HTMLSpanElement | undefined;
  let fit = { narrow: false, needed: 0 };
  const [narrow, setNarrow] = createSignal(false);
  const measure = () => {
    if (!body) return;
    fit = fitCounters(fit, body.scrollWidth, body.clientWidth);
    setNarrow(fit.narrow);
  };
  onMount(() => {
    if (!body || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(body);
    onCleanup(() => observer.disconnect());
  });
  // Other counts or language: measure the long labels again.
  createEffect(on(() => [props.team.working, props.team.total, props.team.needsYou, props.team.asking, props.team.errors, tr("Errors: %1", 0)], () => {
    fit = { narrow: false, needed: 0 };
    setNarrow(false);
    queueMicrotask(measure);
  }, { defer: true }));
  const counter = (kind: string, count: number | undefined, text: string) => (
    <Show when={count}><span class={kind} title={text}><span class="long">{text}</span><span class="short" aria-hidden="true">{count}</span></span></Show>
  );
  return (
    <button class="cc-team" classList={{ open: !props.team.collapsed, narrow: narrow() }} aria-expanded={!props.team.collapsed}
      title={title()} aria-label={[title(), ...teamSummary(props.team)].join("\n")}
      onClick={(e) => { e.stopPropagation(); props.onToggle?.(); }} onDblClick={(e) => e.stopPropagation()}>
      <span class="cc-team-body" ref={body}>
        <svg class="team-glyph" width="16" height="12" viewBox="0 0 16 12" fill="none" stroke="currentColor" stroke-width="1.2" aria-hidden="true">
          <circle cx="8" cy="3" r="2" /><circle cx="3" cy="5" r="1.6" /><circle cx="13" cy="5" r="1.6" />
          <path d="M4 11A4 3 0 0 1 12 11M0 10.5A3 2.4 0 0 1 4.5 9M16 10.5A3 2.4 0 0 0 11.5 9" />
        </svg>
        <span class="cc-team-count">
          <span class="long">{tr("%1 of %2 working", props.team.working, props.team.total)}</span>
          <span class="short" aria-hidden="true">{tr("%1 of %2", props.team.working, props.team.total)}</span>
        </span>
        <Show when={props.team.needsYou}><TeamBadge kind="needs" count={props.team.needsYou ?? 0} /></Show>
        <Show when={props.team.asking}><TeamBadge kind="asking" count={props.team.asking ?? 0} /></Show>
        {counter("team-errors", props.team.errors, tr("Errors: %1", props.team.errors ?? 0))}
      </span>
      <svg class="chev" width="8" height="6" viewBox="0 0 8 6" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M1 2L4 5L7 2" /></svg>
    </button>
  );
}

/**
 * Compact (collapsed) row, 40 px: title, then status, optional questions,
 * provider and mute at the right, then the expand button. A parent of child
 * sessions takes a second line with their summary (62 px).
 */
/**
 * What a compact row shows only in part (Qt's compactDescription,
 * SessionCardDelegate.h:125-128): its title, status and provider, which the
 * row draws as an icon only. The session's name follows a title that differs.
 */
export function compactDescription(s: Session): string {
  return [s.title, s.name !== s.title ? s.name : "", statusCaption(s, false), AGENT_LABEL[s.agent]].filter(Boolean).join("\n");
}

/** The ended parent container's tooltip (zerus-sa9qn.17): what it is and what it is not. */
export function containerDescription(c: Container): string {
  const what = c.state === "archived"
    ? tr("%1 is archived. It stays here while its child sessions are live.", c.name)
    : c.key ? tr("%1 has ended. It stays here while its child sessions are live.", c.name)
    : tr("%1 has ended and is no longer listed. It stays here while its child sessions are live.", c.name);
  return `${what}\n${tr("It is shown for its child sessions only and has no live actions.")}`;
}

/**
 * An ended parent kept as its live children's container (zerus-sa9qn.17): a
 * dimmed compact row with the parent's tile, title and Ended or Archived
 * state, its machine and its children's line. Presentational: no status,
 * attention, account or unread marks of a running session.
 */
export function ContainerCard(props: { container: Container; selected: boolean; team?: Team; onToggleTeam?: () => void }) {
  const c = () => props.container;
  const described = steadyTitle(() => containerDescription(c()));
  return (
    <div class="scard compact ended-parent" classList={{ selected: props.selected, two: !!props.team }}
      aria-description={containerDescription(c())}>
      <div class="cc-line">
        <span class="cc-tile"><AgentTile agent={c().agent} size={22} /></span>
        <span class="sc-title ell" style={titleReserve(c().title, 60)} title={described.title()} onPointerEnter={described.enter} onPointerLeave={described.leave}>{c().title}</span>
        <StatusBadge kind="neutral" caption={c().state === "archived" ? tr("Archived") : tr("Ended")} />
        <span class="grow" />
        <MachineBadge alias={c().machine} computer={c().computer} offline={computerOffline(c().computer)} />
      </div>
      <Show when={props.team}>{(team) => <CompactTeam team={team()} onToggle={props.onToggleTeam} />}</Show>
    </div>
  );
}

export function CompactCard(props: { session: Session; selected: boolean; onExpand: () => void; team?: Team; onToggleTeam?: () => void }) {
  const s = () => props.session;
  // Tooltips hold still under the pointer (zerus-5ryrx).
  const described = steadyTitle(() => compactDescription(s()));
  const expand = steadyTitle(() => `${tr("Expand session row")}\n${compactDescription(s())}`);
  return (
    <div class="scard compact" classList={{ selected: props.selected, two: !!props.team }} data-emphasis={emphasis(s())}
      aria-description={tr("Collapsed session row. Press Right to expand.")}>
      <div class="cc-line">
      <span class="cc-tile"><AgentTile agent={s().agent} size={22} /></span>
      <Show when={sessionMark(s())}>{(m) => <span class="cc-mark" title={markTooltip(s(), m())}><AccountMarkCircle mark={m()} size={14} /></span>}</Show>
      <Show when={s().parent?.state === "live"}><SupervisedMark session={s()} /></Show>
      <span class="sc-title ell" style={titleReserve(s().title, 60)} title={described.title()} onPointerEnter={described.enter} onPointerLeave={described.leave}>{s().title}</span>
      <Show when={s().muted}><Icon name="mute" size={14} class="sc-mute" /></Show>
      <StatusBadge kind={badgeKind(s())} caption={statusCaption(s())} />
      <OptionalQuestionsChip session={s()} />
      <button class="cc-expand" title={expand.title()} onPointerEnter={expand.enter} onPointerLeave={expand.leave} aria-description={compactDescription(s())}
        onClick={(e) => { e.stopPropagation(); props.onExpand(); }} onDblClick={(e) => e.stopPropagation()}>
        <Icon name="expand-row" size={16} />
      </button>
      </div>
      <Show when={props.team}>{(team) => <CompactTeam team={team()} onToggle={props.onToggleTeam} />}</Show>
    </div>
  );
}
