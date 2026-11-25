import { For, Show, createEffect, createSignal, on, untrack } from "solid-js";
import type { JSX } from "solid-js";
import { tr } from "../../i18n";
import { Icon } from "../lib/icons";
import { PROFILE_PREVIEW_ROWS, profileSectionFolded, setProfileSectionFolded, type ProfileSection } from "../../store/profileSections";
import type { TeamBuildItem, TeamBuildsView, TeamProfileRole, TeamProfileView, TeamTestsView, TeamTimelineView, TeamView, TimelineLane } from "../../types";
import { createTimelineReads, defaultPhase, scopeChoices } from "./timeline";
import type { PlacedNode } from "./plan";
import { KIND_STYLES, keyName, lossKey, styleOf, swatchBackground } from "./profile-palette";
import { taskHeat, type TaskHeat } from "./heat";
import { downloadJson, profileExport, profileFileName } from "./profileExport";
import { skin } from "../../skin/runtime";
export { runStatusLabel } from "./planModes";
import {
  boardColumns,
  formatSeconds,
  formatTokens,
  layerRows,
  runTime,
  stageChips,
  stageName,
  testStatus,
  runStatusLabel,
  testsRows,
  testSuites,
  suiteRuns,
  forcedWaiting,
  reportCounts,
  caseName,
  caseFailed,
  type PlanMode,
} from "./planModes";

const MODES: { mode: PlanMode; label: string; icon: string }[] = [
  { mode: "graph", label: "Graph", icon: "network" },
  { mode: "board", label: "Board", icon: "rows-3" },
  { mode: "layers", label: "Layers", icon: "layers" },
  { mode: "lanes", label: "Lanes", icon: "layout-dashboard" },
  { mode: "profile", label: "Profile", icon: "activity" },
  { mode: "tests", label: "Tests", icon: "list-todo" },
  { mode: "builds", label: "Builds", icon: "hard-drive" },
];

/**
 * Task #436 (owner, screenshot of package 167): the plan's view group, one icon
 * per view, inside the toolbar's own segmented group. The toolbar's other
 * groups and toggles live in PlanView, which owns their state.
 */
export function PlanModeBar(props: {
  mode: PlanMode;
  onMode: (mode: PlanMode) => void;
}): JSX.Element {
  return (
    <div class="pm-modes pm-group" role="tablist" aria-label={tr("Plan view")} data-plan-group="view">
      <For each={MODES}>
        {(item) => (
          <button
            type="button"
            role="tab"
            class="pm-mode"
            data-mode={item.mode}
            aria-selected={props.mode === item.mode}
            classList={{ on: props.mode === item.mode }}
            onClick={() => props.onMode(item.mode)}
          >
            <Icon name={item.icon} size={13} />
            <span class="pm-mode-label">{modeLabel(item.mode)}</span>
          </button>
        )}
      </For>
    </div>
  );
}

function modeLabel(mode: PlanMode): string {
  if (mode === "graph") return tr("Graph");
  if (mode === "board") return tr("Board");
  if (mode === "layers") return tr("Layers");
  if (mode === "lanes") return tr("Lanes");
  if (mode === "profile") return tr("Profile");
  if (mode === "builds") return tr("Builds");
  return tr("Tests");
}

export function stageLabel(kind: string): string {
  if (kind === "review") return tr("Review");
  if (kind === "test") return tr("Test");
  if (kind === "command") return tr("Command");
  if (kind === "implement") return tr("Implement");
  if (kind === "land") return tr("Land");
  return kind;
}

function lossLabel(kind: string): string {
  if (kind === "rework") return tr("Rework");
  if (kind === "member") return tr("Waited for a member");
  if (kind === "deps") return tr("Waited for dependencies");
  if (kind === "lead") return tr("Waited for the lead");
  if (kind === "person_wait") return tr("Waiting for a person's answer");
  return kind;
}


function columnTitle(key: string): string {
  if (key === "todo") return tr("To do");
  if (key === "active") return tr("Active");
  if (key === "review") return tr("In review");
  if (key === "test") return tr("In test");
  if (key === "done") return tr("Done");
  return tr("Cancelled");
}

function Card(props: { placed: PlacedNode; heat?: TaskHeat | null; onOpen?: (id: number) => void }): JSX.Element {
  const task = () => props.placed.node;
  return (
    <button
      type="button"
      class={`pm-card pm-tone-${tone(task().status)}`}
      style={props.heat ? { "--heat": String(props.heat.tint) } : undefined}
      data-task={task().id}
      title={props.heat?.tip || undefined}
      onClick={() => props.onOpen?.(task().id)}
    >
      <span class="pm-card-line">
        <span class="pm-id">#{task().id}</span>
        <b class="pm-title">{task().title}</b>
      </span>
      <span class="pm-card-meta">
        <span>{task().status || tr("pending")}</span>
        <Show when={task().assignee}><span>{task().assignee}</span></Show>
      </span>
      <Show when={props.placed.task?.origin || props.placed.node.origin}>
        {(origin) => <span class="pm-origin">{tr("Spawned from #%1", origin())}</span>}
      </Show>
      <HeatLine heat={props.heat} />
      <Show when={stageChips(props.placed.task?.pipeline_run).length > 0}>
        <span class="pm-stages">
          <For each={stageChips(props.placed.task?.pipeline_run)}>
            {(stage) => (
              <span class={`pm-stage pm-stage-${stage.state || "waiting"}`} title={stageLabel(stage.kind)}>
                {stageLabel(stage.kind)}
                <Show when={stage.attempt > 0}> {stage.attempt}</Show>
              </span>
            )}
          </For>
        </span>
      </Show>
    </button>
  );
}

function tone(status: string): string {
  if (status === "accepted" || status === "done") return "done";
  if (status === "cancelled" || status === "rejected") return "danger";
  if (status === "in_progress" || status === "working") return "working";
  if (status === "in_review" || status === "submitted") return "review";
  if (status === "in_test") return "test";
  return "muted";
}

export function BoardView(props: {
  phases: { nodes: PlacedNode[] }[];
  includeCancelled: boolean;
  profile?: TeamProfileView | null;
  onOpen?: (id: number) => void;
}): JSX.Element {
  const columns = () => boardColumns(props.phases, props.includeCancelled);
  return (
    <div class="pm-board">
      <For each={columns()}>
        {(column) => (
          <section class="pm-column" aria-label={columnTitle(column.key)}>
            <header class="pm-column-head">
              <b>{columnTitle(column.key)}</b>
              <span class="pm-count">{column.nodes.length}</span>
            </header>
            <div class="pm-column-body">
              <For each={column.nodes.slice(0, 200)}>
                {(placed) => (
                  <Card placed={placed} heat={taskHeat(props.profile, placed.node.id)} onOpen={props.onOpen} />
                )}
              </For>
              <Show when={column.nodes.length > 200}><p class="pm-more">{tr("%1 more not shown", column.nodes.length - 200)}</p></Show>
            </div>
          </section>
        )}
      </For>
    </div>
  );
}

function columnLabel(key: string): string {
  if (key === "todo") return "To do";
  if (key === "active") return "Active";
  if (key === "review") return "In review";
  if (key === "test") return "In test";
  if (key === "done") return "Done";
  return "Cancelled";
}

export function LayersView(props: {
  phases: { nodes: PlacedNode[] }[];
  includeCancelled: boolean;
  profile?: TeamProfileView | null;
  onOpen?: (id: number) => void;
}): JSX.Element {
  const rows = () => layerRows(props.phases, props.includeCancelled);
  return (
    <div class="pm-layers">
      <For each={rows()}>
        {(row) => (
          <section class="pm-layer" aria-label={tr("Layer %1", row.layer)}>
            <header class="pm-layer-head"><b>{tr("Layer %1", row.layer)}</b><span class="pm-count">{row.nodes.length}</span></header>
            <div class="pm-layer-body">
              <For each={row.nodes.slice(0, 200)}>
                {(placed) => <Card placed={placed} heat={taskHeat(props.profile, placed.node.id)} onOpen={props.onOpen} />}
              </For>
            </div>
          </section>
        )}
      </For>
    </div>
  );
}

/** Task #79: a kind's swatch from the shared profile palette (a wait shows its hatch). */
function Swatch(props: { kind?: string; strip?: boolean }): JSX.Element {
  const style = () => (props.kind ? styleOf(props.kind) : undefined);
  const dark = () => skin().appearance === "dark";
  return <span class="pm-swatch" classList={{ strip: props.kind === "build", none: !style() }}
    style={style() ? { background: swatchBackground(style()!, dark()), "border-color": dark() ? style()!.dark : style()!.light } : undefined} aria-hidden="true" />;
}

/** Task 82: a card's lateness and, beside it, its overrun badge in its own colour. */
export function HeatLine(props: { heat?: TaskHeat | null; compact?: boolean }): JSX.Element {
  return (
    <Show when={props.heat && (props.heat.label || props.heat.badge)}>
      <span class="pm-heat-line">
        <Show when={props.heat!.label}>
          <span class="pm-late" classList={{ "pm-late-muted": props.heat!.muted }}>{props.heat!.label}</span>
        </Show>
        <Show when={props.compact ? props.heat!.badgeCompact : props.heat!.badge}>
          <span class="pm-overrun" style={{ "--over": String(props.heat!.over) }} title={props.heat!.badge}>
            {props.compact ? props.heat!.badgeCompact : props.heat!.badge}
          </span>
        </Show>
      </span>
    </Show>
  );
}

/** Task 82: what the heat shows: the lateness tint and its scale, and the overrun's own scale. */
export function HeatLegend(props: { profile?: TeamProfileView | null }): JSX.Element {
  return (
    <div class="pm-heat-legend" aria-label={tr("Heat")}>
      <span>{tr("Late against the plan: own lateness tints, inherited is muted (from #N)")}</span>
      <span class="pm-heat-scale">
        <span>0</span>
        <i class="pm-heat-ramp" />
        <span>{tr("2h or more late")}</span>
      </span>
      <span>{tr("Overrun, own work against the estimate:")}</span>
      <For each={[0, 0.5, 1]}>
        {(over) => <span class="pm-overrun" style={{ "--over": String(over) }}>{over >= 1 ? "x2+" : `x${(1 + over).toFixed(1)}`}</span>}
      </For>
      <Show when={!props.profile}><span class="pm-heat-note">{tr("Reading the profile…")}</span></Show>
      <Show when={props.profile?.task_rows_more}>
        <span class="pm-heat-note">{tr("Lateness of the first %n tasks", props.profile?.task_rows.length ?? 0)}</span>
      </Show>
    </div>
  );
}

/** Task #79: every span kind of the profile, its name in full; the items wrap. */
export function ProfileLegend(): JSX.Element {
  return (
    <div class="pm-legend" aria-label={tr("Span kinds")}>
      <span class="pm-legend-title">{tr("SPANS")}</span>
      <For each={KIND_STYLES}>{(style) => <span class="pm-legend-item" data-kind={style.key}><Swatch kind={style.key} /><span>{keyName(style.key)}</span></span>}</For>
    </div>
  );
}

/**
 * Task #339: the totals come first, above the lanes, as Qt's summaryTiles sits
 * before the timeline card in TeamProfile::timePage; the export button stays
 * in the same bar.
 */
export function ProfileSummary(props: { profile?: TeamProfileView | null; team?: string; scope?: string }): JSX.Element {
  const [status, setStatus] = createSignal("");
  const exportJson = () => {
    const team = props.team ?? "";
    const scope = props.scope ?? "plan";
    const name = profileFileName(team, scope);
    try {
      downloadJson(name, profileExport(team, scope, props.profile));
      setStatus(tr("Exported to %1", name));
    } catch (error) {
      setStatus(tr("Not exported: %1", error instanceof Error ? error.message : String(error)));
    }
  };
  return (
    <div class="pm-profile pm-profile-summary">
      <div class="pm-profile-head">
        <span class="grow" />
        <Show when={props.team}>
          <button type="button" class="btn small" data-action="profile-export" onClick={exportJson}>{tr("Export JSON")}</button>
        </Show>
        <Show when={status()}><span class="pm-note" role="status">{status()}</span></Show>
      </div>
      <Show when={props.profile} fallback={<p class="pm-empty">{tr("Reading the profile…")}</p>}>
        {(profile) => (
          <div class="pm-tiles">
            <div class="pm-tile"><span>{tr("Elapsed")}</span><b>{formatSeconds(profile().elapsed_s ?? 0)}</b></div>
            <div class="pm-tile"><span>{tr("Worked")}</span><b>{formatSeconds(profile().worked_s ?? 0)}</b></div>
            <div class="pm-tile"><span>{tr("Accepted")}</span><b>{profile().accepted}/{profile().tasks}</b></div>
            <div class="pm-tile"><span>{tr("Tokens")}</span><b>{tokenValue(profile().tokens, tr("The provider did not report this profile's tokens"))}</b><Show when={profile().tokens_partial}><i class="pm-note">{tr("partial")}</i></Show></div>
          </div>
        )}
      </Show>
    </div>
  );
}

/**
 * Task #339/#410: the Top losses and the Roles are two collapsible sections
 * side by side under the lanes, each folded to its first rows with its count
 * in the header and the owner's choice remembered per Profile view. The spans
 * legend stays with the lanes (TimeView).
 */
export function ProfileDetails(props: { profile?: TeamProfileView | null; team?: TeamView; onOpenReport?: (id: number) => void }): JSX.Element {
  const view = () => props.team?.id || "profile";
  // Task #462 (Qt TeamProfile.cpp:917): the losses order, By time or By tokens.
  const [order, setOrder] = createSignal<"time" | "tokens">("time");
  const losses = () => order() === "tokens"
    ? [...(props.profile?.losses ?? [])].sort((a, b) => (b.tokens ?? -1) - (a.tokens ?? -1) || b.seconds - a.seconds)
    : (props.profile?.losses ?? []);
  return (
    <div class="pm-profile pm-profile-details">
      <Show when={props.team?.tasks.some((task) => task.archived)}>
        <h3>{tr("Archived tasks in this window")}</h3>
        <For each={props.team?.tasks.filter((task) => task.archived)}>{(task) => (
          <div class="pm-role-row">
            <button class="btn small" onClick={() => props.onOpenReport?.(task.id)}>{`#${task.id} ${task.title}`}</button>
            <span>{tr("Phase %1", task.phase)}</span>
            <span>{task.status}</span>
            <span class="grow" />
            <span>{task.worked_s == null ? tr("Time unknown") : formatSeconds(task.worked_s)}</span>
          </div>
        )}</For>
      </Show>
      <Show when={props.profile}>
        {(profile) => (
          <>
          <div class="pm-loss-order">
            <div class="pm-segment" role="radiogroup" aria-label={tr("Top losses")} data-loss-order>
              <button type="button" role="radio" aria-checked={order() === "time"} classList={{ on: order() === "time" }} onClick={() => setOrder("time")}>{tr("By time")}</button>
              <button type="button" role="radio" aria-checked={order() === "tokens"} classList={{ on: order() === "tokens" }} onClick={() => setOrder("tokens")}>{tr("By tokens")}</button>
            </div>
          </div>
          <div class="pm-details-columns">
            <ProfileSectionBox id="losses" view={view()} title={tr("Top losses")} count={profile().losses.length}
              items={losses().slice(0, 20)} empty={<p class="pm-empty">{tr("No losses recorded")}</p>}
              row={(loss) => (
                <div class="pm-loss-row" data-kind={lossKey(loss.kind) ?? ""}>
                  <Swatch kind={lossKey(loss.kind)} />
                  <span class="pm-loss-kind">{lossLabel(loss.kind)}</span>
                  <Show when={loss.task > 0}><span class="pm-id">#{loss.task}</span></Show>
                  <span class="grow" />
                  <span>{formatSeconds(loss.seconds)}</span>
                  <span class="pm-note">{tokenValue(loss.tokens, tr("This loss's tokens were not reported"))}</span>
                </div>
              )} />
            <ProfileSectionBox id="roles" view={view()} title={tr("Roles")} count={profile().roles.length}
              items={profile().roles.slice(0, 50)} empty={<p class="pm-empty">{tr("No roles recorded")}</p>}
              row={(role) => (
                <div class="pm-role-row">
                  <span>{role.key || role.kind}</span>
                  <span class="grow" />
                  <span>{role.accepted}/{role.attempts}</span>
                  <span class="pm-note">{role.returned} {tr("returned")}</span>
                  <span class="pm-tokens">{tokenValue(role.tokens_per_accepted, tr("No accepted task of this role reported tokens"))}</span>
                </div>
              )} />
          </div>
          </>
        )}
      </Show>
    </div>
  );
}

/**
 * Task #410: one folded section: its title and count open or close it, and
 * "Show all" / "Show fewer" sit at the header's end. Folded by default.
 */
function ProfileSectionBox<T>(props: {
  id: ProfileSection; view: string; title: string; count: number;
  items: readonly T[]; row: (item: T) => JSX.Element; empty?: JSX.Element;
}): JSX.Element {
  const open = () => !profileSectionFolded(props.view, props.id);
  const toggle = () => setProfileSectionFolded(props.view, props.id, open());
  const visible = () => (open() ? props.items : props.items.slice(0, PROFILE_PREVIEW_ROWS));
  const more = () => props.items.length > PROFILE_PREVIEW_ROWS;
  const countTip = () => (props.items.length < props.count ? tr("The first %1 of %2", props.items.length, props.count) : undefined);
  return (
    <section class="pm-section" data-section={props.id} aria-label={props.title}>
      <header class="pm-section-head">
        <button type="button" class="pm-section-toggle" aria-expanded={open()} onClick={toggle}>
          <Icon name={open() ? "chevron-down" : "chevron-right"} size={13} />
          <b>{props.title}</b>
          <span class="pm-section-count" title={countTip()}>{props.count}</span>
        </button>
        <span class="grow" />
        <Show when={more()}>
          <button type="button" class="pm-section-more" onClick={toggle}>{open() ? tr("Show fewer") : tr("Show all")}</button>
        </Show>
      </header>
      <Show when={props.items.length > 0} fallback={props.empty}>
        <For each={visible()}>{(item) => props.row(item)}</For>
      </Show>
    </section>
  );
}

/** Task #410: an unknown token value is a muted dash, its reason in the tooltip. */
function tokenValue(tokens: number | null, reason: string): JSX.Element {
  return tokens === null || !Number.isFinite(tokens)
    ? <span class="pm-unknown" title={reason} aria-label={reason}>—</span>
    : <>{formatTokens(tokens)}</>;
}

/** Task #462 (owner, package 168): the profile's four modes, as Qt's TeamProfile tabs. */
export type ProfileMode = "time" | "tokens" | "members" | "compare";
const PROFILE_MODES: { mode: ProfileMode; icon: string }[] = [
  { mode: "time", icon: "clock" },
  { mode: "tokens", icon: "gauge" },
  { mode: "members", icon: "users" },
  { mode: "compare", icon: "git-compare-arrows" },
];
function profileModeLabel(mode: ProfileMode): string {
  if (mode === "time") return tr("Time");
  if (mode === "tokens") return tr("Tokens");
  if (mode === "members") return tr("Members");
  return tr("Compare");
}

/** The profile's mode switch: one icon per mode, in the plan toolbar's own segmented style. */
export function ProfileModeTabs(props: { mode: ProfileMode; onMode: (mode: ProfileMode) => void }): JSX.Element {
  return (
    <div class="pm-modes pm-profile-modes" role="tablist" aria-label={tr("Profile")} data-profile-modes>
      <For each={PROFILE_MODES}>{(item) => (
        <button type="button" role="tab" class="pm-mode" data-profile-mode={item.mode}
          aria-selected={props.mode === item.mode} classList={{ on: props.mode === item.mode }} onClick={() => props.onMode(item.mode)}>
          <Icon name={item.icon} size={13} /><span class="pm-mode-label">{profileModeLabel(item.mode)}</span>
        </button>
      )}</For>
    </div>
  );
}

/**
 * Qt's tokensPage (TeamProfile.cpp:1274): the token totals, the roles that spend
 * them and the losses that carry a token count. The profile read gives the
 * team's totals, each role's tokens per accepted task and each loss's tokens;
 * a per-task tray of tokens is not in that read, so the losses stand in for it.
 */
export function TokensPage(props: { profile?: TeamProfileView | null; phase?: number | null }): JSX.Element {
  const byTokens = () => [...(props.profile?.losses ?? [])].filter((loss) => loss.tokens !== null).sort((a, b) => (b.tokens ?? 0) - (a.tokens ?? 0));
  const roles = () => [...(props.profile?.roles ?? [])].sort((a, b) => (b.tokens_per_accepted ?? -1) - (a.tokens_per_accepted ?? -1));
  const spent = (role: TeamProfileRole) => (role.tokens_per_accepted === null ? null : role.tokens_per_accepted * role.accepted);
  return (
    <div class="pm-profile pm-profile-tokens" data-profile-page="tokens">
      <div class="pm-profile-head">
        <h3>{props.phase ? tr("TOKENS PER TASK, PHASE %1", props.phase) : tr("TOKENS PER TASK, THE PLAN")}</h3>
        <span class="grow" />
        <Show when={props.profile?.tokens_partial}>
          <span class="pm-note">{tr("partial: some sessions are on another computer or not recorded")}</span>
        </Show>
      </div>
      <Show when={props.profile} fallback={<p class="pm-empty">{tr("Reading the profile…")}</p>}>{(profile) => (
        <>
          <p class="pm-note" data-profile-tokens-total>
            <b>{tokenValue(profile().tokens, tr("The provider did not report this profile's tokens"))}</b> {tr("Tokens")}
          </p>
          <h3>{tr("Roles")}</h3>
          <Show when={roles().length > 0} fallback={<p class="pm-empty">{tr("No roles recorded")}</p>}>
            <table class="pm-table" data-profile-roles>
              <thead><tr><th>{tr("Role %1")}</th><th>{tr("Accepted")}</th><th>{tr("Tokens per accepted")}</th><th>{tr("Tokens")}</th></tr></thead>
              <tbody><For each={roles()}>{(role) => (
                <tr data-role={role.key}>
                  <td>{role.key || role.kind}</td>
                  <td class="num">{role.accepted}/{role.attempts}</td>
                  <td class="num"><Show when={role.tokens_per_accepted !== null} fallback={<span class="pm-unknown">—</span>}>{formatTokens(role.tokens_per_accepted!)}</Show></td>
                  <td class="num"><Show when={spent(role) !== null} fallback={<span class="pm-unknown">—</span>}>{formatTokens(spent(role)!)}</Show></td>
                </tr>
              )}</For></tbody>
            </table>
          </Show>
          <h3>{tr("Top losses")}</h3>
          <Show when={byTokens().length > 0} fallback={<p class="pm-empty">{tr("No losses recorded")}</p>}>
            <table class="pm-table" data-profile-token-losses>
              <thead><tr><th>{tr("Task")}</th><th>{tr("Tokens")}</th><th>{tr("Worked")}</th></tr></thead>
              <tbody><For each={byTokens()}>{(loss) => (
                <tr data-kind={lossKey(loss.kind) ?? ""}>
                  <td><Show when={loss.task > 0} fallback={<span class="pm-unknown">—</span>}>{`#${loss.task}`}</Show> {lossLabel(loss.kind)}</td>
                  <td class="num">{formatTokens(loss.tokens ?? 0)}</td>
                  <td class="num">{formatSeconds(loss.seconds)}</td>
                </tr>
              )}</For></tbody>
            </table>
          </Show>
        </>
      )}</Show>
    </div>
  );
}

/** A lane's own spans of one kind, in seconds; its waits are separate spans. */
const laneSeconds = (lane: TimelineLane, kinds: readonly string[]) =>
  lane.spans.filter((span) => kinds.includes(span.kind)).reduce((total, span) => total + Math.max(0, span.to - span.from), 0);
const waitedSeconds = (lane: TimelineLane) => lane.waits.reduce((total, span) => total + Math.max(0, span.to - span.from), 0);

/**
 * Qt's membersPage (TeamProfile.cpp:1308): one row per member with the work the
 * timeline read counts (attempts, returned work, worked, utilization, waited,
 * work redone) and the profile's per-role numbers where the lane names a role.
 */
export function MembersPage(props: { profile?: TeamProfileView | null; team: TeamView; load: (phase: number | null) => Promise<TeamTimelineView> }): JSX.Element {
  const [phase, setPhase] = createSignal<number | null>(defaultPhase(props.team.phases));
  const reads = createTimelineReads((scope) => props.load(scope));
  createEffect(on(phase, (scope) => reads.show(scope, untrack(() => props.team.revision))));
  createEffect(on(() => props.team.revision, (revision) => reads.changed(revision), { defer: true }));
  const roleOf = (name: string) => props.profile?.roles.find((role) => role.key === name);
  const rows = () => reads.view()?.lanes ?? [];
  return (
    <div class="pm-profile pm-profile-members" data-profile-page="members">
      <div class="pm-profile-head">
        <h3>{tr("Members")}</h3>
        <select class="tl-scope" aria-label={tr("Scope")} value={phase() == null ? "" : String(phase())}
          onChange={(event) => setPhase(event.currentTarget.value ? Number(event.currentTarget.value) : null)}>
          <For each={scopeChoices(props.team.phases)}>{(choice) => <option value={choice.value}>{choice.label}</option>}</For>
        </select>
        <Show when={reads.loading()}><span class="pm-note">{tr("Reading the timeline…")}</span></Show>
      </div>
      <Show when={reads.error()}><p class="pm-empty warn">{reads.error()}</p></Show>
      <Show when={reads.view()} fallback={<Show when={!reads.error()}><p class="pm-empty">{tr("Reading the timeline…")}</p></Show>}>{(view) => (
        <>
          <table class="pm-table" data-profile-members>
            <thead><tr>
              <th>{tr("Member")}</th><th>{tr("Agent and model")}</th><th class="num">{tr("Accepted")}</th><th class="num">{tr("Attempts")}</th>
              <th class="num">{tr("Returned")}</th><th class="num">{tr("Worked")}</th><th class="num">{tr("Utilization")}</th>
              <th class="num">{tr("Its tasks waited")}</th><th class="num">{tr("Work redone")}</th><th class="num">{tr("Tokens per accepted")}</th>
            </tr></thead>
            <tbody><For each={rows()}>{(lane) => {
              const role = () => roleOf(lane.role);
              const work = () => laneSeconds(lane, ["work"]);
              const rework = () => laneSeconds(lane, ["rework"]);
              const redone = () => (work() + rework() > 0 ? Math.round((100 * rework()) / (work() + rework())) : 0);
              return (
                <tr data-member={lane.member}>
                  <td><b>{lane.member}</b></td>
                  <td>{lane.model || "—"}</td>
                  <td class="num"><Show when={role()?.accepted !== undefined} fallback={<span class="pm-unknown">—</span>}>{role()!.accepted}</Show></td>
                  <td class="num">{lane.spans.filter((span) => span.kind === "work" || span.kind === "rework").length > 0 ? new Set(lane.spans.filter((span) => span.kind === "work" || span.kind === "rework").map((span) => `${span.task}:${span.attempt}`)).size : 0}</td>
                  <td classList={{ num: true, warn: rework() > 0 }}>{rework() > 0 ? new Set(lane.spans.filter((span) => span.kind === "rework").map((span) => `${span.task}:${span.attempt}`)).size : 0}</td>
                  <td class="num">{formatSeconds(lane.busy_s)}</td>
                  <td class="num">{`${Math.round((lane.utilization ?? 0) * 100)}%`}</td>
                  <td class="num">{formatSeconds(waitedSeconds(lane))}</td>
                  <td classList={{ num: true, warn: redone() > 0 }}>{`${redone()}%`}</td>
                  <td class="num"><Show when={role()?.tokens_per_accepted !== undefined && role()!.tokens_per_accepted !== null} fallback={<span class="pm-unknown">—</span>}>{formatTokens(role()!.tokens_per_accepted!)}</Show></td>
                </tr>
              );
            }}</For></tbody>
          </table>
          <p class="pm-note">{tr("Utilization counts worked spans only; waits for a dependency, a free member or the lead are not work. A row opens the timeline.")}</p>
        </>
      )}</Show>
    </div>
  );
}

/**
 * Qt's comparePage (TeamProfile.cpp:1383) compares two phases. The plan's
 * profile read is one scope, so each side is the phase's timeline read: the
 * same lanes, elapsed and worked seconds the Time mode draws.
 */
export function ComparePage(props: { team: TeamView; load: (phase: number | null) => Promise<TeamTimelineView> }): JSX.Element {
  const phases = () => props.team.phases.map((phase) => phase.n);
  const [a, setA] = createSignal<number | null>(phases().length > 1 ? phases()[phases().length - 2] : (phases()[0] ?? null));
  const [b, setB] = createSignal<number | null>(phases()[phases().length - 1] ?? null);
  const left = createTimelineReads((scope) => props.load(scope));
  const right = createTimelineReads((scope) => props.load(scope));
  createEffect(on([a, () => props.team.revision], ([phase, revision]) => left.show(phase, revision)));
  createEffect(on([b, () => props.team.revision], ([phase, revision]) => right.show(phase, revision)));
  const rework = (view: TeamTimelineView | null) => (view?.lanes ?? []).reduce((total, lane) => total + laneSeconds(lane, ["rework"]), 0);
  const waits = (view: TeamTimelineView | null) => (view?.lanes ?? []).reduce((total, lane) => total + waitedSeconds(lane), 0);
  const tests = (view: TeamTimelineView | null) => (view?.tests ?? []).reduce((total, span) => total + Math.max(0, span.to - span.from), 0);
  const rows = () => [
    { key: "took", label: tr("Took"), unit: "s" as const, lower: true, value: (view: TeamTimelineView | null) => view?.elapsed_s ?? null },
    { key: "tasks", label: tr("Tasks"), unit: "n" as const, lower: false, value: (view: TeamTimelineView | null) => view?.tasks ?? null },
    { key: "worked", label: tr("Worked"), unit: "s" as const, lower: false, value: (view: TeamTimelineView | null) => view?.worked_s ?? null },
    { key: "rework", label: tr("Work redone"), unit: "s" as const, lower: true, value: (view: TeamTimelineView | null) => (view ? rework(view) : null) },
    { key: "waited", label: tr("Waited for a free member"), unit: "s" as const, lower: true, value: (view: TeamTimelineView | null) => (view ? waits(view) : null) },
    { key: "tests", label: tr("Tests"), unit: "s" as const, lower: true, value: (view: TeamTimelineView | null) => (view ? tests(view) : null) },
  ];
  const show = (value: number | null, unit: "s" | "n") => (value === null ? "—" : unit === "s" ? formatSeconds(value) : String(value));
  const phaseLabel = (n: number | null) => (n === null ? tr("The plan, every phase") : tr("Phase %1", n));
  return (
    <div class="pm-profile pm-profile-compare" data-profile-page="compare">
      <Show when={phases().length > 0} fallback={<p class="pm-empty">{tr("No phases to compare")}</p>}>
        <div class="pm-profile-head">
          <h3>{tr("Compare")}</h3>
          <select class="tl-scope" aria-label={tr("Phase %1", left.scope() ?? 0)} value={a() == null ? "" : String(a())} data-compare-a
            onChange={(event) => setA(Number(event.currentTarget.value))}>
            <For each={phases()}>{(n) => <option value={String(n)}>{phaseLabel(n)}</option>}</For>
          </select>
          <span class="pm-note">{tr("against")}</span>
          <select class="tl-scope" aria-label={tr("Phase %1", right.scope() ?? 0)} value={b() == null ? "" : String(b())} data-compare-b
            onChange={(event) => setB(Number(event.currentTarget.value))}>
            <For each={phases()}>{(n) => <option value={String(n)}>{phaseLabel(n)}</option>}</For>
          </select>
          <Show when={left.loading() || right.loading()}><span class="pm-note">{tr("Reading the timeline…")}</span></Show>
        </div>
        <Show when={left.error() || right.error()}><p class="pm-empty warn">{left.error() || right.error()}</p></Show>
        <Show when={left.view() || right.view()} fallback={<p class="pm-empty">{tr("Reading the timeline…")}</p>}>
          <table class="pm-table pm-compare" data-profile-compare>
            <thead><tr><th>{tr("Measure")}</th><th class="num">{phaseLabel(a())}</th><th class="num">{phaseLabel(b())}</th><th class="num">{tr("Delta")}</th></tr></thead>
            <tbody><For each={rows()}>{(row) => {
              const one = () => row.value(left.view() ?? null), two = () => row.value(right.view() ?? null);
              const delta = () => (one() === null || two() === null ? null : two()! - one()!);
              const worse = () => (delta() === null || delta() === 0 ? false : delta()! > 0 === row.lower);
              return (
                <tr data-measure={row.key}>
                  <td>{row.label}</td>
                  <td class="num">{show(one(), row.unit)}</td>
                  <td class="num">{show(two(), row.unit)}</td>
                  <td classList={{ num: true, warn: worse(), accent: delta() !== null && delta() !== 0 && !worse() }}>
                    {delta() === null || delta() === 0 ? "—" : `${delta()! > 0 ? "+" : "-"}${show(Math.abs(delta()!), row.unit)}`}
                  </td>
                </tr>
              );
            }}</For></tbody>
          </table>
        </Show>
      </Show>
    </div>
  );
}

export function TestsPanel(props: {
  tests?: TeamTestsView | null;
  onMore?: () => void;
  /** Task 199/#462: Force run of a suite (its refusal, if any); the builds have their own mode. */
  onForce?: (suite: string) => Promise<string>;
}): JSX.Element {
  const rows = () => testsRows(props.tests);
  // Task 324 (Qt TeamTests): "%1 suites failing" counts the suites whose latest
  // run failed or timed out, each suite once.
  const failingSuites = () => {
    const seen = new Set<string>();
    let failed = 0;
    for (const run of rows().runs) {
      if (seen.has(run.suite)) continue;
      seen.add(run.suite);
      const kind = testStatus(run.status);
      if (kind === "failed" || kind === "timed-out") failed += 1;
    }
    return failed;
  };
  const [open, setOpen] = createSignal("");
  const [forcing, setForcing] = createSignal<Record<string, boolean>>({});
  const [refused, setRefused] = createSignal<Record<string, string>>({});
  const force = async (suite: string) => {
    if (!props.onForce || forcing()[suite]) return;
    setForcing({ ...forcing(), [suite]: true });
    const error = await props.onForce(suite);
    setForcing({ ...forcing(), [suite]: false });
    setRefused({ ...refused(), [suite]: error });
  };
  return (
    <div class="pm-tests">
      <Show when={props.tests} fallback={<p class="pm-empty">{tr("Reading the tests…")}</p>}>
        {/* Task 324 (Qt TeamTests): the hint, the counters and the Queue section in Qt's order. */}
        <p class="pm-note" data-tests-hint>{tr("Test requests wait in a queue; Zerus runs them when the machine has room and keeps each result with its excerpt.")}</p>
        <div class="pm-tests-counts" data-tests-counts>
          <span classList={{ "pm-pill": true, "pm-count-on": rows().running.length > 0 }} data-count="running">{tr("%n running", rows().running.length)}</span>
          <span classList={{ "pm-pill": true, "pm-count-on": rows().waiting.length > 0 }} data-count="waiting">{tr("%n waiting", rows().waiting.length)}</span>
          <Show when={failingSuites() > 0}>
            <span class="pm-pill danger" data-count="failing">{failingSuites() === 1 ? tr("1 suite failing") : tr("%1 suites failing", failingSuites())}</span>
          </Show>
        </div>
        <Show when={testSuites(props.tests).length > 0}>
          <h3>{tr("Suites")}</h3>
          <For each={testSuites(props.tests)}>{(suite) => {
            const runs = () => suiteRuns(props.tests, suite);
            const waiting = () => forcedWaiting(props.tests, suite);
            return (
              <div class={`pm-run pm-suite pm-${runs()[0] ? testStatus(runs()[0].status) : "queued"}`} data-suite={suite}>
                <b class="pm-run-suite">{suite}</b>
                <Show when={runs()[0]} fallback={<span class="pm-note">{tr("no result yet")}</span>}>{(latest) => (
                  <>
                    <span class="pm-run-status">{runStatusLabel(latest().status)}</span>
                    <span>{runTime(latest().duration_ms)}</span>
                    <span class="pm-strip" aria-hidden="true">
                      <For each={[...runs()].reverse()}>{(run) => <i class={`pm-${testStatus(run.status)}`} title={runStatusLabel(run.status)} />}</For>
                    </span>
                    <Show when={latest().over_budget}><span class="pm-pill danger">{tr("Over budget")}</span></Show>
                  </>
                )}</Show>
                <span class="grow" />
                <Show when={refused()[suite]}><span class="danger pm-note" data-force-error>{refused()[suite]}</span></Show>
                <Show when={props.onForce}>
                  <button type="button" class="btn ghost small" data-action="force-run" disabled={!!forcing()[suite] || waiting()}
                    title={tr("Run %1 now: first in the queue, at day too, never a reused result", suite)} onClick={() => void force(suite)}>
                    {forcing()[suite] ? tr("Queuing\u2026") : waiting() ? tr("Forced, waiting") : tr("Force run")}
                  </button>
                </Show>
              </div>
            );
          }}</For>
        </Show>
        <Show when={rows().running.length > 0}>
          <h3>{tr("Running now")}</h3>
          <For each={rows().running}>{(item) => <TestQueueRow item={item} />}</For>
        </Show>
        {/* Task 324: Qt's Queue section is always there; "Nothing waits." is its empty line. */}
        <h3>{tr("Queue")}</h3>
        <Show when={rows().waiting.length > 0} fallback={<p class="pm-empty" data-tests-queue-empty>{tr("Nothing waits.")}</p>}>
          <For each={rows().waiting}>{(item) => <TestQueueRow item={item} />}</For>
        </Show>
        <h3>{tr("Recent runs")}</h3>
        <Show when={rows().runs.length > 0} fallback={<p class="pm-empty">{tr("No test runs recorded")}</p>}>
          <For each={rows().runs.slice(0, 50)}>
            {(run) => (
              <>
                <div class={`pm-run pm-${testStatus(run.status)}`} data-run={run.id}
                  role={run.report ? "button" : undefined} tabIndex={run.report ? 0 : undefined}
                  onClick={() => { if (run.report) setOpen(open() === run.id ? "" : run.id); }}>
                  <span class="pm-run-status">{runStatusLabel(run.status)}</span>
                  <b class="pm-run-suite">{run.suite}</b>
                  {/* Task 88: how its tests went, from the provider's report. */}
                  <Show when={run.report}>{(report) => (
                    <span classList={{ "pm-note": true, "pm-run-bad": report().failed + report().errors > 0 }} data-run-counts>{reportCounts(report())}</span>
                  )}</Show>
                  <span class="grow" />
                  <span>{runTime(run.duration_ms)}</span>
                  <Show when={run.runner}><span class="pm-note">{run.runner}</span></Show>
                  <Show when={run.executor_load}>{(load) => (
                    <span class="pm-note" data-executor-load={run.id}>
                      <Show when={load().valid} fallback={tr("Executor load measurement unavailable")}>
                        {load().sample_kind === "during_peak" ? tr("Executor sampled peak") : tr("Executor before run")}
                        {" "}{tr("CPU %1%", Math.round(load().cpu_percent ?? 0))}
                        {" / "}{tr("Disk busy %1%", Math.round(load().disk_busy_percent ?? 0))}
                        {" / "}{tr("Disk used %1%", Math.round(load().disk_percent ?? 0))}
                      </Show>
                      <Show when={load().loaded}>{" "}{tr("Run under load")}</Show>
                    </span>
                  )}</Show>
                </div>
                <Show when={open() === run.id && run.report}>{(report) => (
                  <div class="pm-cases" data-run-cases={run.id}>
                    <Show when={report().note}><p class="pm-note">{report().note}</p></Show>
                    <For each={report().results}>{(test) => (
                      <div classList={{ "pm-case": true, "pm-case-bad": caseFailed(test) }} data-case-status={test.status}>
                        <span class="pm-case-name">{caseName(test)}</span>
                        <span class="grow" />
                        <span class="pm-note">{runTime(test.ms)}</span>
                        <Show when={test.message}><p class="pm-case-message">{test.message}</p></Show>
                      </div>
                    )}</For>
                    <Show when={report().passed + report().failed + report().skipped + report().errors > report().results.length}>
                      <p class="pm-note">{tr("%1 more tests in the record", report().passed + report().failed + report().skipped + report().errors - report().results.length)}</p>
                    </Show>
                  </div>
                )}</Show>
              </>
            )}
          </For>
        </Show>
        <Show when={props.tests?.next != null && props.onMore}>
          <button type="button" class="btn ghost small" onClick={() => props.onMore?.()}>{tr("Load older")}</button>
        </Show>
      </Show>
    </div>
  );
}

function seconds(value: number): string {
  return runTime(Math.max(0, value) * 1000);
}

/**
 * Task 199 (Qt TeamTests builds): the build broker's shared folders, their build
 * folders, what builds, waits and finished. Task #462 (owner, package 168): its
 * own plan mode, with Qt's building and waiting counts (TeamTests.cpp:674-690).
 */
export function BuildsPanel(props: { builds?: TeamBuildsView | null; error?: string }): JSX.Element {
  const building = () => (props.builds?.folders ?? []).reduce((total, folder) => total + folder.build_folders.reduce((sum, group) => sum + group.running.length, 0), 0);
  const waiting = () => (props.builds?.folders ?? []).reduce((total, folder) => total + folder.build_folders.reduce((sum, group) => sum + group.queue.length, 0), 0);
  const line = (item: TeamBuildItem, kind: "running" | "queue" | "recent") => (
    <div class={`pm-queue pm-build pm-build-${kind}`} data-member={item.member}>
      <b>{item.member}</b>
      <span class="pm-note">{item.template}</span>
      <Show when={kind === "running"}>
        <span class="pm-pill">{item.snapshot ? tr("snapshot") : tr("shared")}</span>
        <span class="pm-note">{item.usual_ms > 0 ? tr("%1 of usually %2", seconds(item.seconds), runTime(item.usual_ms)) : tr("running %1", seconds(item.seconds))}</span>
      </Show>
      <Show when={kind === "queue"}>
        <span class="pm-note">{tr("waiting %1", seconds(item.seconds))}</span>
        <Show when={item.coalesced_with}><span class="pm-note accent">{tr("same tree as %1: one result", item.coalesced_with)}</span></Show>
        <Show when={item.waiting_for.length > 0}><span class="pm-note warn">{tr("waiting for edits of %1", item.waiting_for.join(", "))}</span></Show>
      </Show>
      <Show when={kind === "recent"}>
        <span class="pm-note">{runTime(item.ms)}</span>
        <Show when={item.hit}><span class="pm-note accent">{item.by ? tr("from the cache, built by %1", item.by) : tr("from the cache")}</span></Show>
        <Show when={!item.hit && (item.exit ?? 0) !== 0}><span class="pm-note danger">{item.timed_out ? tr("Timed out") : tr("Failed")}</span></Show>
        <Show when={item.error}><span class="pm-note danger">{item.error}</span></Show>
      </Show>
    </div>
  );
  return (
    <div class="pm-builds" data-builds>
      <Show when={(props.builds?.deliveries?.length ?? 0) > 0}>
        <section data-artifact-deliveries>
          <h3>{tr("Remote artifact tests")}</h3>
          <For each={props.builds?.deliveries ?? []}>{(item) => (
            <div class="pm-queue" data-artifact-job={item.job}>
              <b>{item.profile}</b><span>{item.device}</span><span class="pm-pill">{item.phase}</span>
              <span class="pm-note" title={item.executor}>{tr("Job %1", item.job)}</span>
              <Show when={item.phase === "awaiting_observation"}><span class="pm-note warn">{tr("Waiting for your scenario observation")}</span></Show>
              <Show when={item.instructions}><details><summary>{tr("Scenario instructions")}</summary><p class="pm-note">{item.instructions}</p></details></Show>
              <Show when={item.phase === "unknown"}><span class="pm-note warn">{tr("Execution outcome is unknown; review before starting a new job")}</span></Show>
            </div>
          )}</For>
        </section>
      </Show>
      <div class="pm-tests-counts" data-builds-counts>
        <span classList={{ "pm-pill": true, "pm-count-on": building() > 0 }} data-count="building">{tr("%n building", building())}</span>
        <span classList={{ "pm-pill": true, "pm-count-on": waiting() > 0 }} data-count="waiting">{tr("%n waiting", waiting())}</span>
      </div>
      <Show when={props.error}><p class="danger">{tr("Could not read the builds: %1", props.error!)}</p></Show>
      <Show when={props.builds} fallback={<Show when={!props.error}><p class="pm-empty">{tr("Reading the builds\u2026")}</p></Show>}>{(builds) => (
        <>
          <p class="pm-note">{builds().enabled
            ? tr("Members sharing a folder build through the build broker: each in its own snapshot, one build at a time per build folder.")
            : tr("The build broker is off: members sharing a folder build in it directly. The owner turns it on in the team's settings.")}</p>
          <Show when={builds().folders.length > 0} fallback={<p class="pm-empty">{tr("No two members share a folder.")}</p>}>
            <For each={builds().folders}>{(folder) => (
              <section class="pm-build-folder" data-folder={folder.folder}>
                <h3 title={tr("Shared by %1", folder.members.join(", "))}>{folder.folder}</h3>
                <Show when={folder.build_folders.length > 0} fallback={<p class="pm-empty">{tr("No build through the broker yet.")}</p>}>
                  <For each={folder.build_folders}>{(group) => (
                    <div class="pm-build-group" data-build-folder={group.name}>
                      <div class="pm-queue">
                        <b>{group.name === "." ? tr("the shared folder itself") : group.name}</b>
                        <Show when={group.usual_ms > 0}><span class="pm-note">{tr("usually %1", runTime(group.usual_ms))}</span></Show>
                        <Show when={group.hit_rate !== null}><span class="pm-pill">{tr("cache hits %1%", Math.round((group.hit_rate ?? 0) * 100))}</span></Show>
                      </div>
                      <Show when={group.running.length + group.queue.length === 0}><p class="pm-empty">{tr("Nothing builds or waits.")}</p></Show>
                      <For each={group.running}>{(item) => line(item, "running")}</For>
                      <For each={group.queue}>{(item) => line(item, "queue")}</For>
                      <Show when={group.recent.length > 0}><h4>{tr("Recent builds")}</h4></Show>
                      <For each={group.recent}>{(item) => line(item, "recent")}</For>
                    </div>
                  )}</For>
                </Show>
              </section>
            )}</For>
          </Show>
        </>
      )}</Show>
    </div>
  );
}

function TestQueueRow(props: { item: TeamTestsView["queue"][number] }): JSX.Element {
  return (
    <div class="pm-queue">
      <span class="pm-run-status">{props.item.state || tr("Queued")}</span>
      <b class="pm-run-suite">{props.item.suite}</b>
      <span class="grow" />
      <Show when={props.item.forced}><span class="pm-pill">{tr("Forced")}</span></Show>
      <Show when={props.item.reason}><span class="pm-note">{props.item.reason}</span></Show>
    </div>
  );
}
