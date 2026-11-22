import { Dropdown } from "../lib/Dropdown";
import { TextInput } from "../lib/TextField";
import { For, Match, Show, Switch, createEffect, createMemo, createSignal, on, onCleanup, type JSX } from "solid-js";
import { trText } from "../../text";
import * as bridge from "../../bridge";
import type { PathStyle } from "../../types";
import { AgentGlyph, Icon } from "../lib/icons";
import { composing } from "../lib/keys";
import { MachineTile, type MachineState } from "../components/MachineTile";
import { UsageMeters } from "../components/UsageMeter";
import { FolderBrowserDialog } from "../components/FolderBrowser";
import type { FolderBrowserCalls } from "../components/folder-browser";
import { closeNewSession, newSession, newSessionActions as act, startNewSession, type NewSessionState } from "../../store/newSession";
import { machineCards } from "../../store/machines";
import { catalog, agentKind } from "../../store/catalog";
import { connectionOf, osOf } from "../../store/overview";
import { now } from "../../store/ui";
import { skin } from "../../skin/runtime";
import { projectColor } from "./organization-ui";
import { launchWindows, type AccountRow } from "./newsession-ui";
import { providerName } from "../lib/usage";
import { accountKey, computerKey, createConfirm, dialogKey, folderKey, projectKey, type KeyControl } from "./newsession-view";
import { LaunchWarning } from "./LaunchWarning";
import { ModelPopover, type ModelPickerActions } from "./ModelPicker";
import { ChooseWorktreeDialog } from "./WorktreePicker";
import { NewWorktreeDialog } from "./NewWorktreeDialog";
import { ClaudeAppDialog } from "./ClaudeAppDialog";
import { sameSessions, type PickerSession } from "./worktree-dialogs";
import type { Json } from "../lib/json";
import { useLocalSpeed } from "../lib/localSpeed";
import { tr } from "../../i18n";
import "../styles/newsession.css";

// New session (tray/src/NewSessionDialog.cpp, constructor 136-475 and
// updateForm 660-742), from the prototype's layout
// (design/webui/src/dialogs/setup/NewSession.tsx) where Qt does not differ.
// It renders the store's state and calls its actions; the form's rules come
// from the core through the store (`form` is `launch::shown`). Opening and
// every choice here only read; Start hands the plan to background creation.
// The child dialogs (a peer's folder browser, Choose existing…, Create
// manually…, the model picker) open over it from `newSessionChild` and the
// store's `pickerOpen`.

/** A child dialog New session opens over itself, with the context it opened in (585, 1114-1143). */
export type NewSessionChild =
  | { kind: "browse"; target: string; computer: string; project: string; path: string }
  | { kind: "browse-repository"; target: string; computer: string; parent: string }
  | { kind: "choose-worktree"; target: string; computer: string; project: string; path: string; style: PathStyle }
  | { kind: "create-worktree"; target: string; computer: string; project: string; path: string; catalog: Json; branch: string; style: PathStyle }
  // WhiteWh fork: Continue a Claude app session (NewSessionDialog.cpp:246-254).
  | { kind: "claude-app"; target: string; computer: string; project: string; path: string };
export const [newSessionChild, setNewSessionChild] = createSignal<NewSessionChild>();

const AGENTS = new Set(["codex", "claude", "kimi", "qwen", "hermes", "dsh", "opencode", "ollama"]);
const sameKeys = (a: string[], b: string[]) => a.length === b.length && a.every((key, i) => key === b[i]);

/** The row whose key is `key`, keeping the last one found while a removed row is disposed. */
function rowOf<T>(find: () => T | undefined): () => T {
  return createMemo<T>((last) => find() ?? last, find() as T);
}

// Task #368: the fork's dense New session view ('B - Dense, labels left'), from
// tray/src/NewSessionDense.h and NewSessionDialog.cpp buildDenseLayout and
// refreshDense. The agent rail is derived from the account rows the classic list
// shows; the dialog renders it instead of that list when the skin's
// views.newSession is dense (the default).
export type DenseKind = "subscription" | "balance" | "local" | "terminal";
/** One account of a subscription agent: the chip's row. */
export interface DenseAccount { index: number; label: string; connected: boolean; fiveHour: number; week: number; reset: string }
/** One tile of the rail and what it shows over the next 12x2 meters (Agent in NewSessionDense.h). */
export interface DenseAgent {
  key: string;
  name: string;
  kind: DenseKind;
  /** The chosen account's row in the dialog's list, -1 for none. */
  row: number;
  absent: boolean;
  connected: boolean;
  fiveHour: number;
  week: number;
  reset: string;
  balance: string;
  balanceValue: number;
  accounts: DenseAccount[];
  /** The chosen one in `accounts`, -1 for none. */
  chosen: number;
  /** The rail's chosen tile. */
  selected: boolean;
}
/** The rail's agents in Qt's order: the six agents and the terminal (refreshDense). */
export const DENSE_AGENTS = ["codex", "claude", "kimi", "qwen", "hermes", "dsh", "ollama", "opencode", "sh"] as const;
const SIGNED_OUT = new Set(["signed_out", "expired", "credentials_locked", "credentials_unavailable", "desktop_session_unavailable"]);

/** A tile's kind: DeepSeek is a balance, Ollama and OpenCode are local, the shell is a terminal, the rest subscriptions. */
export const denseKind = (key: string): DenseKind =>
  key === "dsh" ? "balance" : key === "ollama" || key === "opencode" ? "local" : key === "sh" ? "terminal" : "subscription";

/** A window's reset as the tiles print it: hours:minutes left ("01:40"), "—" when unknown. */
export function denseReset(resetsIn: number | null | undefined): string {
  if (resetsIn === null || resetsIn === undefined || !Number.isFinite(resetsIn)) return "—";
  const minutes = Math.max(0, Math.round(resetsIn));
  return `${String(Math.floor(minutes / 60)).padStart(2, "0")}:${String(minutes % 60).padStart(2, "0")}`;
}

/** A window's reset in words for the line under the rail: "1h 40m", "—" when unknown. */
export function denseResetWords(resetsIn: number | null | undefined): string {
  if (resetsIn === null || resetsIn === undefined || !Number.isFinite(resetsIn)) return "—";
  const minutes = Math.max(0, Math.round(resetsIn));
  return `${Math.floor(minutes / 60)}h ${String(minutes % 60).padStart(2, "0")}m`;
}

const balanceText = (usage: { balances: { balance: string; currency: string }[] } | undefined) => {
  const first = usage?.balances?.[0];
  const amount = Number(first?.balance);
  if (!first || !Number.isFinite(amount)) return { text: "", value: -1 };
  const symbol = first.currency === "CNY" ? "¥" : first.currency === "USD" ? "$" : `${first.currency} `;
  return { text: `${symbol}${amount.toFixed(2)}`, value: amount };
};

/**
 * The rail's agents for `rows` and the chosen `agent` (Qt's refreshDense): the six
 * agents and the terminal, an agent without an account present as "no acct", the
 * chosen account's 5h and week windows and the 5h reset.
 */
export function denseAgentsOf(rows: readonly AccountRow[], agent: string, account: number, minute: number): DenseAgent[] {
  if (!rows.length) return [];
  const hasShell = rows.some((row) => row.provider === "sh");
  // OpenCode and Qwen Code show where an account of theirs is listed.
  const keys = DENSE_AGENTS.filter((key) => !["opencode", "qwen", "hermes"].includes(key) || rows.some((row) => row.provider === key))
    .filter((key) => key !== "sh" || hasShell);
  return keys.map((key) => {
    const found = rows.map((row, index) => ({ row, index })).filter((entry) => entry.row.provider === key);
    const chosenEntry = found.find((entry) => entry.index === account) ?? found[0];
    const usage = chosenEntry?.row.usage;
    const windows = launchWindows(usage, minute);
    const five = windows.find((window) => window.minutes === 300);
    const week = windows.find((window) => window.minutes === 10080);
    const balance = balanceText(usage);
    const accounts: DenseAccount[] = found.map((entry) => {
      const accountUsage = entry.row.usage;
      const accountWindows = launchWindows(accountUsage, minute);
      const accountFive = accountWindows.find((window) => window.minutes === 300);
      const accountWeek = accountWindows.find((window) => window.minutes === 10080);
      return {
        index: entry.index,
        label: entry.row.name,
        connected: !SIGNED_OUT.has(accountUsage?.status ?? ""),
        fiveHour: accountFive?.used ?? -1,
        week: accountWeek?.used ?? -1,
        reset: denseReset(accountFive?.resetsIn),
      };
    });
    const kind = denseKind(key);
    return {
      key,
      name: key === "sh" ? tr("Shell") : key === "dsh" ? tr("DeepSeek") : providerName(key),
      kind,
      row: chosenEntry?.index ?? -1,
      absent: found.length === 0 && key !== "sh" && key !== "ollama",
      connected: key === "sh" || !SIGNED_OUT.has(usage?.status ?? ""),
      fiveHour: five?.used ?? -1,
      week: week?.used ?? -1,
      reset: denseReset(five?.resetsIn),
      balance: balance.text,
      balanceValue: balance.value,
      accounts,
      chosen: chosenEntry ? found.findIndex((entry) => entry.index === chosenEntry.index) : -1,
      selected: key === agent,
    };
  });
}

/**
 * A choice list with rows that a label cannot carry in a native list (the
 * project colour, the computer's OS tile). Rows are keyed: relabelling keeps
 * them and the hovered one. Its keys are Qt's non-editable combo box
 * (`dialogKey`): on the closed button the arrows, PageUp, PageDown, Home and
 * End change the choice, Alt+Down, F4 and Space open it, and Enter goes to
 * Start; Escape closes an open list first.
 */
export function Choice(props: {
  id: string;
  label: string;
  keys: string[];
  selected: string | undefined;
  placeholder?: string;
  disabled: boolean;
  open: boolean;
  onOpen(open: boolean): void;
  onPick(key: string): void;
  option(key: string): JSX.Element;
  tip?(key: string): string | undefined;
  class?: string;
  initial?: boolean;
  /** Task #368: the dense view's menu: a header, a check at the current choice,
   * unavailable rows with their reason, and the view's own actions. */
  dense?: boolean;
  header?: string;
  check?: boolean;
  /** The dense menu's own actions, after a separator (New project…, Browse…, Clone repository…). */
  actions?: { label: string; icon?: string; disabled?: boolean; run(): void }[];
}) {
  let root!: HTMLDivElement;
  let button!: HTMLButtonElement;
  const [active, setActive] = createSignal<string>();
  // Task #433: the menu is fixed to the viewport, so the dialog's own overflow
  // never clips it and it always sits above the rows after its field.
  const [box, setBox] = createSignal<{ left: number; top: number; width: number }>({ left: 8, top: 8, width: 0 });
  const place = () => {
    const b = button.getBoundingClientRect();
    const width = Math.max(0, Math.min(b.width, Math.max(0, window.innerWidth - 16)));
    const below = Math.max(0, window.innerHeight - b.bottom - 12), above = Math.max(0, b.top - 12);
    const down = below >= 160 || below >= above;
    setBox({
      left: Math.max(8, Math.min(b.left, window.innerWidth - width - 8)),
      top: down ? b.bottom + 4 : Math.max(8, b.top - Math.min(280, above) - 4),
      width,
    });
  };
  // A row of the dense menu is unavailable when its reason is set; keys skip it.
  const reason = (key: string) => (props.dense ? props.tip?.(key) : undefined);
  const usable = () => (props.dense ? props.keys.filter((key) => !reason(key)) : props.keys);
  const step = (from: string | undefined, by: number) => {
    const keys = usable();
    if (!keys.length) return undefined;
    const at = from === undefined ? -1 : keys.indexOf(from);
    if (at < 0) return by > 0 ? keys[0] : keys[keys.length - 1];
    return keys[Math.max(0, Math.min(keys.length - 1, at + by))];
  };
  const show = () => { setActive(props.selected); place(); props.onOpen(true); };
  const pick = (key: string | undefined) => {
    props.onOpen(false);
    button.focus();
    // Committing the current item also records an explicit Destination choice.
    if (key !== undefined) props.onPick(key);
  };
  const onKey = (e: KeyboardEvent) => {
    if (props.disabled) return;
    const action = dialogKey("choice", e, props.open);
    const keys = usable();
    const to = (from: string | undefined) =>
      action === "first" ? keys[0] : action === "last" ? keys[keys.length - 1] : step(from, action === "next" ? 1 : -1);
    switch (action) {
      // Escape ("native") reaches the dialog's cancel, which closes the open list first.
      case "native": return;
      case "close": props.onOpen(false); return;
      case "start": e.preventDefault(); button.form?.requestSubmit(); return;
      case "ignore": e.preventDefault(); return;
      case "open": e.preventDefault(); show(); return;
      case "pick": e.preventDefault(); pick(active()); return;
      default: {
        e.preventDefault();
        if (props.open) { setActive(to(active())); return; }
        const next = to(props.selected);
        if (next !== undefined) props.onPick(next);
      }
    }
  };
  createEffect(() => {
    if (!props.open) return;
    const away = (e: PointerEvent) => { if (!root.contains(e.target as Node)) props.onOpen(false); };
    document.addEventListener("pointerdown", away, true);
    onCleanup(() => document.removeEventListener("pointerdown", away, true));
  });
  createEffect(() => {
    const key = active();
    if (!props.open || key === undefined) return;
    // Owner, 2026-11-10: only the list scrolls to its row; scrollIntoView also
    // scrolled the dialog and left it blank.
    const row = root.querySelector<HTMLElement>(`[data-key="${CSS.escape(key)}"]`);
    const list = row?.closest<HTMLElement>(".ns-choice-list");
    if (!row || !list) return;
    if (row.offsetTop < list.scrollTop) list.scrollTop = row.offsetTop;
    else if (row.offsetTop + row.offsetHeight > list.scrollTop + list.clientHeight) list.scrollTop = row.offsetTop + row.offsetHeight - list.clientHeight;
  });
  // Owner, 2026-11-10: the open dialog keeps its scale from the pop animation,
  // so it is the containing block of a fixed menu; the menu is moved by the
  // difference between where it landed and where it was placed.
  createEffect(() => {
    if (!props.open) return;
    const want = box();
    queueMicrotask(() => {
      const list = root.querySelector<HTMLElement>(".ns-choice-list");
      if (!list) return;
      const got = list.getBoundingClientRect();
      const dx = got.left - want.left, dy = got.top - want.top;
      if (Math.abs(dx) > 1 || Math.abs(dy) > 1) {
        list.style.left = `${want.left - dx}px`;
        list.style.top = `${want.top - dy}px`;
      }
    });
  });
  const optionId = (key: string) => `${props.id}-o-${props.keys.indexOf(key)}`;
  return (
    <div class={`ns-choice ${props.class ?? ""}`} ref={root}>
      <button type="button" ref={button} class="field ns-choice-btn" role="combobox" aria-label={props.label} aria-haspopup="listbox"
        aria-expanded={props.open} aria-controls={`${props.id}-list`} disabled={props.disabled} data-initial={props.initial || undefined}
        aria-activedescendant={props.open && active() !== undefined ? optionId(active()!) : undefined}
        title={props.selected !== undefined ? props.tip?.(props.selected) : undefined}
        onClick={() => (props.open ? props.onOpen(false) : show())} onKeyDown={onKey}>
        <span class="ns-choice-value">
          <Show when={props.selected !== undefined} fallback={<span class="ns-placeholder">{props.placeholder}</span>}>{props.option(props.selected!)}</Show>
        </span>
        <Icon name="chevron-down" size={14} class="ns-choice-chevron" />
      </button>
      <Show when={props.open}>
        <ul class="ns-choice-list" classList={{ "nsd-menu": props.dense }} id={`${props.id}-list`} role="listbox" aria-label={props.label}
          style={{ position: "fixed", left: `${box().left}px`, top: `${box().top}px`, "min-width": `${box().width}px` }}>
          <Show when={props.dense && props.header}><li class="nsd-menu-header" role="presentation">{props.header}</li></Show>
          <For each={props.keys}>{(key, index) => {
            const why = () => reason(key);
            return (
              <li id={`${props.id}-o-${index()}`} data-key={key} role="option" aria-selected={key === props.selected}
                aria-disabled={why() ? true : undefined}
                classList={{ on: key === props.selected, active: key === active(), unavailable: !!why() }}
                title={props.dense ? undefined : props.tip?.(key)}
                onPointerEnter={() => { if (!why()) setActive(key); }} onMouseDown={(e) => e.preventDefault()}
                onClick={() => { if (!why()) pick(key); }}>
                <Show when={props.dense && props.check && key === props.selected}><Icon name="check" size={13} class="nsd-check" /></Show>
                <Show when={props.dense} fallback={props.option(key)}><span class="nsd-row-main">{props.option(key)}</span></Show>
                <Show when={why()}>{(text) => <span class="nsd-row-why">{text()}</span>}</Show>
              </li>
            );
          }}</For>
          <Show when={props.dense && props.actions?.length}>
            <li class="menu-sep" role="separator" />
            <For each={props.actions}>{(action) => (
              <li class="nsd-action" role="menuitem" aria-disabled={action.disabled || undefined}
                onClick={() => { if (action.disabled) return; props.onOpen(false); action.run(); }}>
                <Show when={action.icon}>{(icon) => <Icon name={icon()} size={13} class="nsd-action-icon" />}</Show>
                <span>{action.label}</span>
              </li>
            )}</For>
          </Show>
        </ul>
      </Show>
    </div>
  );
}

/** One account row (AccountChoice::Delegate): the provider's logo, the bold name, then compact meters or the summary. */
function AccountLine(props: { row: () => AccountRow; selected: boolean }) {
  const minute = createMemo(() => Math.floor(now() / 60) * 60);
  const windows = createMemo(() => launchWindows(props.row().usage, minute()));
  const provider = () => props.row().provider;
  // A local model account: its model's speed instead of limits.
  const local = useLocalSpeed(() => props.row().shell ? [] : [props.row().profile, ...props.row().members]);
  return (
    <>
      <span class="ns-acc-logo" style={{ color: AGENTS.has(provider()) ? `var(--z-agent-${agentKind(provider())})` : undefined }} aria-hidden="true">
        <Show when={!props.row().shell && AGENTS.has(provider())} fallback={<Icon name="terminal" size={26} />}>
          <AgentGlyph agent={agentKind(provider())} size={26} />
        </Show>
      </span>
      <span class="ns-acc-text">
        <b class="ell">{props.row().name}</b>
        <Show when={!local.server() && windows().length} fallback={
          <Show when={local.server()} fallback={
            <span class="ns-usage ell" classList={{ [`tone-${props.row().tone}`]: !!props.row().tone, selected: props.selected }}>{props.row().summary}</span>
          }>
            <span class="ns-usage ell tabular" classList={{ selected: props.selected }} title={local.title()}>{local.text()}</span>
          </Show>
        }>
          <span class="ns-meters"><UsageMeters windows={windows()} compact /></span>
        </Show>
      </span>
    </>
  );
}

/** The colour of a used share: danger from 85%, warn from 65%, else the accent (NewSessionDense.h usageColor). */
export const denseUsageColor = (used: number) => (used >= 85 ? "var(--z-color-danger)" : used >= 65 ? "var(--z-color-warn)" : "var(--z-color-accent)");
/** Its text: the same tone from 65%, else the third text colour (usageText). */
export const denseUsageText = (used: number) => (used >= 65 ? denseUsageColor(used) : "var(--z-color-text-3)");

/** One tile of the dense agent rail (NewSessionDense.h AgentButton): the logo and what the agent can start with. */
function DenseTile(props: { agent: DenseAgent; machine: string; disabled: boolean; onPick(): void }) {
  const a = () => props.agent;
  const narrow = () => a().kind === "local" || a().kind === "terminal";
  const multi = () => a().kind === "subscription" && a().accounts.length > 1;
  const meters = () => a().kind === "subscription" && a().connected && !a().absent && !multi();
  const dimmed = () => a().absent || (!a().connected && !multi() && a().kind !== "terminal");
  // Two monospace lines: a balance, or why the agent cannot start yet.
  const lines = () => {
    if (narrow() || meters() || multi()) return [];
    if (a().absent) return [{ text: tr("no acct"), warn: false }, { text: tr("+ add"), warn: true }];
    if (a().kind === "balance" && a().connected)
      return a().balance
        ? [{ text: a().balance, warn: a().balanceValue < 5, danger: a().balanceValue < 2 }, { text: tr("bal"), warn: false }]
        : [{ text: tr("key"), warn: false }];
    if (a().connected) return [];
    return [{ text: a().kind === "balance" ? tr("no key") : tr("offline"), warn: false },
      { text: a().kind === "balance" ? tr("+ add") : tr("sign in"), warn: true }];
  };
  const tip = () => {
    if (a().absent) return tr("%1 — no account on %2. Add one in Agent accounts.", a().name, props.machine);
    if (a().kind === "terminal") return tr("%1 (interactive terminal)", a().name);
    return a().name;
  };
  return (
    <button type="button" class="nsd-tile" data-agent={a().key} classList={{ on: a().selected, narrow: narrow(), dim: dimmed() }}
      aria-pressed={a().selected} aria-label={tip()} title={tip()} disabled={props.disabled} onClick={() => props.onPick()}>
      <span class="nsd-logo" aria-hidden="true">
        <Show when={a().key !== "sh"} fallback={<Icon name="terminal" size={20} />}>
          <AgentGlyph agent={agentKind(a().key)} size={20} />
        </Show>
      </span>
      <Show when={multi()}>
        <span class="nsd-count">{tr("%1 acc", a().accounts.length)}
          <span class="nsd-dots"><For each={a().accounts}>{(account) => (
            <i classList={{ on: account.connected }} style={{ "background-color": account.connected ? denseUsageColor(Math.max(0, account.fiveHour)) : "var(--z-color-border)" }} />
          )}</For></span>
        </span>
      </Show>
      <Show when={meters()}>
        <span class="nsd-meters">
          <span class="nsd-meter"><i class="nsd-bar"><b style={{ width: `${a().fiveHour >= 0 ? Math.max(1, Math.min(100, a().fiveHour)) : 0}%`, "background-color": denseUsageColor(Math.max(0, a().fiveHour)) }} /></i><span style={{ color: a().fiveHour >= 0 ? denseUsageText(a().fiveHour) : "var(--z-color-dim)" }}>{a().fiveHour >= 0 ? `${Math.round(a().fiveHour)}%` : "—"}</span></span>
          <span class="nsd-meter"><i class="nsd-bar"><b style={{ width: `${a().week >= 0 ? Math.max(1, Math.min(100, a().week)) : 0}%`, "background-color": denseUsageColor(Math.max(0, a().week)) }} /></i><span style={{ color: a().week >= 0 ? denseUsageText(a().week) : "var(--z-color-dim)" }}>{a().week >= 0 ? `${Math.round(a().week)}%` : "—"}</span></span>
          <span class="nsd-reset">{a().reset}</span>
        </span>
      </Show>
      <Show when={!multi() && !meters() && lines().length}>
        <span class="nsd-lines"><For each={lines()}>{(line) => (
          <span style={{ color: line.warn ? "var(--z-color-warn)" : "var(--z-color-dim)" }} classList={{ danger: !!line.danger }}>{line.text}</span>
        )}</For></span>
      </Show>
    </button>
  );
}

/** The chosen account when an agent has several: its Accounts label, 5h and week use and the 5h reset, opening the list. */
function DenseChip(props: { agent: DenseAgent; open: boolean; onOpen(open: boolean): void }) {
  const chosen = () => props.agent.accounts[props.agent.chosen];
  return (
    <button type="button" class="nsd-chip" classList={{ on: props.open }} aria-haspopup="listbox" aria-expanded={props.open}
      aria-label={tr("Account: %1", chosen()?.label ?? "")}
      title={props.agent.accounts.map((account) => (account.connected
        ? tr("%1 — 5h %2%, wk %3%", account.label, Math.round(Math.max(0, account.fiveHour)), Math.round(Math.max(0, account.week)))
        : tr("%1 — not signed in", account.label))).join("\n")}
      onClick={() => props.onOpen(!props.open)}>
      <span class="nsd-count">{tr("%1 acc", props.agent.accounts.length)}</span>
      <span class="nsd-chip-email ell">{chosen()?.label}</span>
      <Show when={chosen()?.connected} fallback={<span class="nsd-chip-absent">{tr("not signed in")}</span>}>
        <span class="nsd-meter"><i class="nsd-bar"><b style={{ width: `${Math.max(1, Math.min(100, chosen()!.fiveHour))}%`, "background-color": denseUsageColor(Math.max(0, chosen()!.fiveHour)) }} /></i><span>5h</span></span>
        <span class="nsd-meter"><i class="nsd-bar"><b style={{ width: `${Math.max(1, Math.min(100, chosen()!.week))}%`, "background-color": denseUsageColor(Math.max(0, chosen()!.week)) }} /></i><span>wk</span></span>
      </Show>
      <span class="nsd-reset">{chosen()?.reset}</span>
      <Icon name={props.open ? "chevron-up" : "chevron-down"} size={11} class="nsd-chip-chevron" />
    </button>
  );
}

function NewSessionForm(props: { s: () => NewSessionState; popup: () => string | undefined; setPopup(popup: string | undefined): void; confirm: ReturnType<typeof createConfirm> }) {
  const s = props.s;
  const form = () => s().form;
  const automatic = () => s().worktrees.automatic;
  const canStart = () => automatic() !== undefined && !!form()?.start_enabled && !s().starting && !s().submitted;
  // Until the core's first answer: nothing is locked, nothing can start.
  const fields = () => form()?.fields_enabled ?? true;
  const project = () => s().projects[s().project];
  const computer = () => s().computers[s().computer];
  const folder = () => s().folders[s().folder];
  const online = () => computer()?.online ?? false;
  const computerName = () => s().host || (machineCards[""]?.host ?? "");
  const opened = (name: string) => props.popup() === name;
  const toggle = (name: string) => (open: boolean) => props.setPopup(open ? name : opened(name) ? undefined : props.popup());

  // Keys keep the list rows while labels change in place.
  const projectKeys = createMemo(() => s().projects.map(projectKey), [], { equals: sameKeys });
  const computerKeys = createMemo(() => s().computers.map(computerKey), [], { equals: sameKeys });
  const folderKeys = createMemo(() => s().folders.map(folderKey), [], { equals: sameKeys });
  const accountKeys = createMemo(() => s().accounts.map(accountKey), [], { equals: sameKeys });
  // Each choice's selected key, read by the lists' key and click handlers too: a memo, so no
  // read outside the view's tracking makes a computation nobody disposes (it would read the
  // closed dialog's state and stop Escape from closing it).
  const projectSelected = createMemo(() => { const row = project(); return row ? projectKey(row) : undefined; });
  const computerSelected = createMemo(() => { const row = computer(); return row ? computerKey(row) : undefined; });
  const folderSelected = createMemo(() => { const row = folder(); return row ? folderKey(row) : undefined; });
  const projectOf = (key: string) => s().projects.find((row) => projectKey(row) === key);
  const computerOf = (key: string) => s().computers.find((row) => computerKey(row) === key);
  const folderOf = (key: string) => s().folders.find((row) => folderKey(row) === key);

  const projectOption = (key: string) => {
    // The project's colour as the Sessions list's header shows it.
    const color = () => {
      const row = projectOf(key);
      return row && !row.newProject ? projectColor(row) : undefined;
    };
    return (
      <>
        <Show when={!projectOf(key)?.newProject}><Icon name="projects" size={15} class="ns-project-icon" style={{ color: color() }} /></Show>
        <span class="ell">{projectOf(key)?.name}</span>
      </>
    );
  };
  const tile = (target: string): MachineState => {
    if (computerOf(target)?.online) return "online";
    return connectionOf(catalog.computers[target]?.status).tile === "connecting" ? "connecting" : "offline";
  };
  const computerOption = (key: string) => (
    <>
      <MachineTile os={osOf(machineCards[key]?.platform)} state={tile(key)} size={20} />
      <span class="ell">{computerOf(key)?.label}</span>
    </>
  );
  const folderOption = (key: string) => <span class="ell">{folderOf(key)?.name}</span>;

  // Task #368: the skin's view (Qt's views.newSession): the fork's dense dialog,
  // or the classic one when a skin names classic. The dense rail reads the same
  // account rows the classic list shows.
  const dense = () => skin().layout["new-session"] === "dense";
  const denseMinute = createMemo(() => Math.floor(now() / 60) * 60);
  const denseRail = createMemo(() => denseAgentsOf(s().accounts, s().agent, s().account, denseMinute()));
  const denseCurrent = createMemo(() => denseRail().find((row) => row.selected));
  const denseMachine = () => computer()?.label ?? "";
  const denseMulti = () => !!online() && !!denseCurrent() && denseCurrent()!.accounts.length > 1;
  const [denseListOpen, setDenseListOpen] = createSignal(false);
  createEffect(() => { if (!denseMulti()) setDenseListOpen(false); });
  // The line under the rail: the chosen account, or why that agent cannot start here.
  const denseInfo = createMemo(() => {
    const machine = denseMachine();
    if (!online() && s().computers.length) return { title: tr("%1 is offline", machine), details: tr("pick another computer"), tone: "danger" };
    const current = denseCurrent();
    if (!current) return { title: s().accountStatus || tr("Loading accounts…"), details: "", tone: "" };
    if (current.absent) return { title: tr("%1 — no account on %2", current.name, machine), details: tr("add one in Agent accounts"), tone: "warn" };
    if (current.key === "ollama" && current.row < 0) return { title: tr("Ollama — not set up on %1", machine), details: tr("add a local model account to start"), tone: "warn" };
    if (!current.connected) {
      if (current.kind === "balance") return { title: tr("%1 — no API key", current.name), details: tr("add an API key to start"), tone: "warn" };
      return { title: tr("%1 — not connected", current.name), details: tr("sign in to use this account"), tone: "warn" };
    }
    if (current.kind === "subscription") {
      const accountLabel = current.accounts[current.chosen]?.label ?? "";
      const five = launchWindows(current.row >= 0 ? s().accounts[current.row]?.usage : undefined, denseMinute()).find((window) => window.minutes === 300);
      const parts: string[] = [];
      if (current.fiveHour >= 0) parts.push(tr("5h %1% used, resets in %2", Math.round(current.fiveHour), denseResetWords(five?.resetsIn)));
      if (current.week >= 0) parts.push(tr("week %1% used", Math.round(current.week)));
      return { title: accountLabel ? `${current.name} — ${accountLabel}` : current.name, details: parts.join(", "), tone: "" };
    }
    if (current.kind === "balance") return { title: current.name, details: current.balance ? tr("balance %1, pay-as-you-go", current.balance) : tr("API key configured"), tone: "" };
    if (current.key === "ollama") return { title: tr("Ollama"), details: tr("local on %1", s().host || machineCards[""]?.host || machine), tone: "" };
    if (current.key === "opencode") return { title: current.name, details: tr("native provider configuration"), tone: "" };
    return { title: current.name, details: tr("interactive terminal session"), tone: "" };
  });
  // The refresh icon says how old the chosen account's limits are (Qt's age()).
  const denseUpdated = createMemo(() => {
    const checked = denseCurrent()?.row !== undefined && denseCurrent()!.row >= 0 ? s().accounts[denseCurrent()!.row]?.usage?.checked_at : undefined;
    if (!checked) return "";
    const seconds = Math.max(0, denseMinute() - checked);
    return seconds < 60 ? tr("now") : seconds < 3600 ? tr("%1m", Math.floor(seconds / 60)) : tr("%1h", Math.floor(seconds / 3600));
  });

  // Browse… (581-588, 312-316): this computer's own dialog, else the folder browser over this one.
  const browse = () => {
    if (s().host === "") { void act.browseLocal(); return; }
    setNewSessionChild({ kind: "browse", target: s().host, computer: computerName(), project: project()?.id ?? "", path: folder()?.path ?? "" });
  };
  const browseRepository = () => {
    if (s().host === "") { void act.browseRepositoryLocal(); return; }
    setNewSessionChild({ kind: "browse-repository", target: s().host, computer: computerName(), parent: s().repositoryParent });
  };
  // A click on a disabled entry does nothing and keeps the menu open, as Qt's QMenu.
  const chooseExisting = () => {
    if (!form()?.choose_existing_enabled) return;
    props.setPopup(undefined);
    setNewSessionChild({ kind: "choose-worktree", target: s().host, computer: computerName(), project: project()?.id ?? "", path: folder()?.path ?? "", style: s().style });
  };
  const createManually = () => {
    if (!form()?.create_manually_enabled) return;
    props.setPopup(undefined);
    setNewSessionChild({ kind: "create-worktree", target: s().host, computer: computerName(), project: project()?.id ?? "", path: folder()?.path ?? "", catalog: s().worktrees.catalog, branch: s().name, style: s().style });
  };
  // WhiteWh fork: a Claude desktop app session continues as a new session;
  // its dialog reads the app's list for this computer and folder.
  const continueClaudeApp = () => {
    props.setPopup(undefined);
    setNewSessionChild({ kind: "claude-app", target: s().host, computer: computerName(), project: project()?.id ?? "", path: folder()?.path ?? "" });
  };

  // The worktree options menu: Escape is the dialog's (it closes the menu first), arrows move.
  // As Qt's QMenu, opened by the keyboard its first enabled entry is active; by the mouse none is.
  let menu: HTMLDivElement | undefined;
  let menuByKeys = false;
  const toggleMenu = (e: MouseEvent) => {
    // A click the keyboard made (Space) has no pointer detail.
    menuByKeys = e.detail === 0;
    props.setPopup(opened("worktree") ? undefined : "worktree");
  };
  createEffect(() => {
    if (!opened("worktree")) return;
    const byKeys = menuByKeys;
    queueMicrotask(() => {
      const first = byKeys ? menu?.querySelector<HTMLElement>(".menu-item:not([aria-disabled=true])") : undefined;
      (first ?? menu)?.focus();
    });
    const away = (e: PointerEvent) => { if (!menu?.parentElement?.contains(e.target as Node)) props.setPopup(undefined); };
    document.addEventListener("pointerdown", away, true);
    onCleanup(() => document.removeEventListener("pointerdown", away, true));
  });
  const menuKey = (e: KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const items = [...(menu?.querySelectorAll<HTMLElement>(".menu-item:not([aria-disabled=true])") ?? [])];
    const at = items.indexOf(document.activeElement as HTMLElement);
    const down = e.key === "ArrowDown";
    // With no active entry, Down takes the first and Up the last.
    items[at < 0 ? (down ? 0 : items.length - 1) : (at + (down ? 1 : -1) + items.length) % items.length]?.focus();
  };

  // The account list: arrows choose; taking the focus never does (947-949).
  // Enter goes to the default button, as in Qt's dialog. Enabled unless the form is locked (698).
  let accountList!: HTMLUListElement;
  let formEl!: HTMLFormElement;
  const accountsEnabled = () => fields();
  const accountKeyDown = (e: KeyboardEvent) => {
    if (!accountsEnabled()) return;
    const count = s().accounts.length;
    if (!count) return;
    const at = s().account;
    let next: number;
    if (e.key === "ArrowDown") next = at < 0 ? 0 : Math.min(count - 1, at + 1);
    else if (e.key === "ArrowUp") next = at < 0 ? 0 : Math.max(0, at - 1);
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = count - 1;
    else if (e.key === "PageDown") next = at < 0 ? 0 : Math.min(count - 1, at + 4);
    else if (e.key === "PageUp") next = at < 0 ? 0 : Math.max(0, at - 4);
    else if (e.key === "Enter") { e.preventDefault(); if (!composing(e)) formEl.requestSubmit(); return; }
    else return;
    e.preventDefault();
    act.chooseAccount(next);
    queueMicrotask(() => accountList.querySelector(".on")?.scrollIntoView({ block: "nearest" }));
  };

  // The Git repository source focuses its URL field (606).
  let url: HTMLInputElement | undefined;
  // Ordinary edits/form answers replace the whole state. Only a changed focus
  // request may move focus; `on` alone does not compare the selected value.
  const focusUrl = createMemo(() => s().focusUrl);
  createEffect(on(focusUrl, () => queueMicrotask(() => url?.focus()), { defer: true }));

  const start = (e: Event) => {
    e.preventDefault();
    if (!canStart()) return;
    void startNewSession((text) => props.confirm.ask(text));
  };

  // Enter on a line edit, the check box, the Effort list and the buttons Qt
  // made `setAutoDefault(false)` goes to Start (`dialogKey`); their
  // `data-keys` names the control. Cancel and Open Machines keep Enter.
  const formKey = (e: KeyboardEvent) => {
    const control = (e.target as HTMLElement | null)?.dataset?.keys as KeyControl | undefined;
    if (!control || control === "choice") return;
    const action = dialogKey(control, e);
    if (action === "start") { e.preventDefault(); formEl.requestSubmit(); }
    else if (action === "ignore") e.preventDefault();
  };

  // Task #368: the fork's dense view ('B - Dense, labels left'): one 50 | field |
  // 30 | 148 grid, captions on the left, the agent rail, the design's menus and
  // the mono path line. Same fields, same actions as the classic layout.
  const denseBody = () => (
    <div class="nsd">
      <h2 id="ns-title" class="nsd-title">{tr("New session")}</h2>
      <div class="nsd-header">
        <span class="nsd-cap">{tr("Name")}</span>
        <TextInput id="nsd-name" class="nsd-name" data-keys="text" data-initial maxLength={200} value={s().name} disabled={!fields()}
          aria-label={tr("Name")} aria-invalid={!!form()?.name_problem}
          onInput={(e) => act.setName(e.currentTarget.value)} />
        <button type="button" class="nsd-close" aria-label={tr("Close")} title={tr("Close")} disabled={!(form()?.cancel_enabled ?? true)}
          onClick={() => closeNewSession()}><Icon name="x" size={14} /></button>
      </div>
      <div class="nsd-rule" />
      <div class="nsd-grid">
        <span class="nsd-cap">{tr("Target")}</span>
        <Choice id="nsd-project" label={tr("Project")} dense check header={tr("Project")} keys={projectKeys()} selected={projectSelected()}
          disabled={!fields()} open={opened("project")} onOpen={toggle("project")} option={projectOption} class="nsd-project"
          actions={[{ label: tr("New project…"), icon: "add", disabled: !projectKeys().some((key) => projectOf(key)?.newProject),
            run: () => act.chooseProject(s().projects.findIndex((row) => row.newProject)) }]}
          onPick={(key) => act.chooseProject(s().projects.findIndex((row) => projectKey(row) === key))} />
        <button type="button" class="nsd-icon" data-keys="button" data-tip={tr("Manage this project's folders")} aria-label={tr("Manage this project's folders")}
          disabled={!fields()} onClick={() => act.manageFolders()}><Icon name="settings" size={16} /></button>
        <div class="nsd-right">
          <Choice id="nsd-computer" label={tr("Computer")} dense check header={tr("Computer")} keys={computerKeys()} selected={computerSelected()}
            disabled={!fields()} open={opened("computer")} onOpen={toggle("computer")} option={computerOption}
            tip={(key) => (computerOf(key)?.online ? undefined : tr("%1 is offline", computerOf(key)?.label ?? key))}
            onPick={(key) => act.chooseComputer(s().computers.findIndex((row) => computerKey(row) === key))} />
        </div>
        <Show when={s().newProjectRow}>
          <span class="nsd-cap" classList={{ "nsd-reserved": !s().newProjectVisible }}>{tr("New project")}</span>
          <Show when={s().newProjectVisible} fallback={<span class="nsd-reserved-field" />}>
            <TextInput id="nsd-new-project" class="field nsd-wide" data-keys="text" maxLength={80} placeholder={tr("Project name")} value={s().newProjectName}
              disabled={!fields()} onInput={(e) => act.setNewProjectName(e.currentTarget.value)} />
          </Show>
        </Show>
        <span class="nsd-cap">{tr("Source")}</span>
        <div class="nsd-seg nsd-wide" role="group" aria-label={tr("Source")}>
          <button type="button" class="nsd-segbtn" data-keys="button" data-source="folder" classList={{ on: !s().repository }} aria-pressed={!s().repository}
            disabled={!fields()} onClick={() => act.setSource(false)}><Icon name="folder" size={14} />{tr("Working folder")}</button>
          <button type="button" class="nsd-segbtn" data-keys="button" data-source="repository" classList={{ on: s().repository }} aria-pressed={s().repository}
            disabled={!fields() || !online()} onClick={() => act.setSource(true)}><Icon name="git-branch" size={14} />{tr("Git repository")}</button>
        </div>
        <Show when={!s().repository}>
          <span class="nsd-cap">{tr("Folder")}</span>
          <Choice id="nsd-folder" label={tr("Working folder")} dense check header={tr("Working folder on %1", denseMachine())} keys={folderKeys()}
            selected={folderSelected()} placeholder={tr("Choose a folder…")} disabled={!fields()} open={opened("folder")} onOpen={toggle("folder")}
            option={(key) => <span class="ell nsd-path" title={folderOf(key)?.tooltip}>{folderOf(key)?.tooltip ?? folderOf(key)?.name}</span>}
            actions={[
              { label: tr("Browse…"), icon: "add", disabled: !(form()?.browse_enabled ?? false), run: browse },
              { label: tr("Clone repository…"), icon: "git-branch", disabled: !fields() || !online(), run: () => act.setSource(true) },
            ]}
            onPick={(key) => act.chooseFolder(s().folders.findIndex((row) => folderKey(row) === key))} />
          <button type="button" id="nsd-browse" class="nsd-icon" data-keys="button" data-tip={tr("Browse for folder")} aria-label={tr("Browse for folder")}
            disabled={!(form()?.browse_enabled ?? false)} onClick={browse}><Icon name="folder" size={16} /></button>
          <div class="nsd-right">
            <div class="nsd-seg" role="group" aria-label={tr("Worktree")}>
              <button type="button" class="nsd-segbtn" data-keys="button" classList={{ on: automatic() === true }} aria-pressed={automatic() === true}
                disabled={!(form()?.auto_enabled ?? false)} onClick={() => act.setAuto(true)}>{tr("Auto")}</button>
              <button type="button" class="nsd-segbtn" data-keys="button" classList={{ on: automatic() === false }} aria-pressed={automatic() === false}
                disabled={!(form()?.none_enabled ?? true)} onClick={() => act.setAuto(false)}>{tr("None")}</button>
              <button type="button" id="nsd-worktree-options" class="nsd-seg-more" data-keys="button" data-tip={tr("More worktree options")}
                aria-label={tr("More worktree options")} aria-haspopup="menu" aria-expanded={opened("worktree")}
                onClick={() => props.setPopup(opened("worktree") ? undefined : "worktree")}><Icon name="ellipsis" size={13} /></button>
            </div>
            <Show when={opened("worktree")}>
              <div class="menu nsd-menu" role="menu" tabindex="-1" ref={menu} onKeyDown={menuKey}>
                <div class="nsd-menu-header" role="presentation">{tr("Worktree")}</div>
                <button type="button" class="menu-item" role="menuitem" aria-disabled={!form()?.create_manually_enabled} onClick={createManually}>{tr("New worktree…")}</button>
                <button type="button" class="menu-item" role="menuitem" aria-disabled={!form()?.choose_existing_enabled} onClick={chooseExisting}>{tr("Use an existing worktree…")}</button>
                <div class="menu-sep" role="separator" />
                <button type="button" class="menu-item" role="menuitem" onClick={() => { props.setPopup(undefined); act.setSource(!s().repository); }}>
                  {s().repository ? tr("Use a working folder") : tr("Clone a Git repository…")}
                </button>
                <button type="button" class="menu-item" role="menuitem" onClick={continueClaudeApp}>{tr("Continue a Claude app session…")}</button>
              </div>
            </Show>
          </div>
        </Show>
        <Show when={s().repository}>
          <span class="nsd-cap">{tr("Repository")}</span>
          <TextInput id="nsd-url" ref={url} class="field mono nsd-url" data-keys="text" maxLength={4096} placeholder="https://github.com/owner/repository.git"
            title={tr("An SSH or HTTPS repository URL, on GitHub or another Git server.")} value={s().url} disabled={!fields() || !online()}
            onInput={(e) => act.setUrl(e.currentTarget.value)} />
          <button type="button" id="nsd-browse-repository" class="nsd-icon" data-keys="button" data-tip={tr("Browse for folder")} aria-label={tr("Browse for folder")}
            disabled={!fields() || !online()} onClick={browseRepository}><Icon name="folder" size={16} /></button>
          <span class="nsd-cap">{tr("Clone to")}</span>
          <TextInput id="nsd-destination" class="field mono nsd-dest" data-keys="text" maxLength={8192} value={s().destination} disabled={!fields() || !online()}
            title={tr("The complete new checkout path. Edit its folder name or choose a parent folder with Browse.")}
            onInput={(e) => act.setDestination(e.currentTarget.value)} />
          <button type="button" class="nsd-icon" data-keys="button" data-tip={tr("Browse for folder")} aria-label={tr("Browse for folder")}
            disabled={!fields() || !online()} onClick={browseRepository}><Icon name="folder" size={16} /></button>
        </Show>
        <span class="nsd-cap nsd-top">{tr("Agent")}</span>
        <div class="nsd-agents">
          <div class="nsd-rail" role="radiogroup" aria-label={tr("Agent")}>
            <For each={denseRail()}>{(agent) => (
              <DenseTile agent={agent} machine={denseMachine()} disabled={!fields()} onPick={() => { if (agent.row >= 0) act.chooseAccount(agent.row); }} />
            )}</For>
          </div>
          <div class="nsd-info-row">
            <Show when={!denseMulti()} fallback={<DenseChip agent={denseCurrent()!} open={denseListOpen()} onOpen={setDenseListOpen} />}>
              <span class={`nsd-info ${denseInfo().tone}`} title={denseInfo().details}>
                <b class="ell">{denseInfo().title}</b>
                <Show when={denseInfo().details}><span class="ell nsd-info-details">{denseInfo().details}</span></Show>
              </span>
            </Show>
            <button type="button" class="nsd-refresh" data-keys="button" disabled={!s().refreshEnabled} onClick={() => act.refreshAccounts()}
              title={denseUpdated() ? tr("Updated %1 ago", denseUpdated()) : tr("Refresh accounts")} aria-label={tr("Refresh accounts")}>
              <Icon name="refresh" size={11} /><Show when={denseUpdated()}><span>{denseUpdated()}</span></Show>
            </button>
          </div>
          <Show when={denseMulti() && denseListOpen()}>
            <ul class="nsd-acc-list" role="listbox" aria-label={tr("Account and agent")}>
              <For each={denseCurrent()!.accounts}>{(account) => (
                <li role="option" aria-selected={account.index === denseCurrent()!.row} classList={{ on: account.index === denseCurrent()!.row }}
                  onClick={() => { act.chooseAccount(account.index); setDenseListOpen(false); }}>
                  <Show when={account.index === denseCurrent()!.row}><Icon name="check" size={12} class="nsd-check" /></Show>
                  <span class="ell nsd-acc-email">{account.label}</span>
                  <Show when={account.connected} fallback={<span class="nsd-chip-absent">{tr("not signed in")}</span>}>
                    <span class="nsd-meter"><i class="nsd-bar"><b style={{ width: `${Math.max(1, Math.min(100, account.fiveHour))}%`, "background-color": denseUsageColor(Math.max(0, account.fiveHour)) }} /></i><span>{Math.round(Math.max(0, account.fiveHour))}%</span></span>
                    <span class="nsd-meter"><i class="nsd-bar"><b style={{ width: `${Math.max(1, Math.min(100, account.week))}%`, "background-color": denseUsageColor(Math.max(0, account.week)) }} /></i><span>{Math.round(Math.max(0, account.week))}%</span></span>
                    <span class="nsd-reset">{account.reset}</span>
                  </Show>
                </li>
              )}</For>
            </ul>
          </Show>
        </div>
        <span class="nsd-cap">{tr("Model")}</span>
        <Show when={s().openCode} fallback={
          <Dropdown id="ns-model" class="field nsd-model" data-keys="select" aria-label={tr("Model")} value={s().model} disabled={!s().modelEnabled}
            onChange={act.chooseModel} options={s().models.map((model) => ({ value: model.value, label: model.text || model.label }))} />
        }>
          <button type="button" id="ns-model" class="field nsd-model" data-keys="button" aria-label={tr("Model")} aria-haspopup="dialog" aria-expanded={s().pickerOpen}
            disabled={!s().modelEnabled} title={s().modelButton}
            onClick={() => (s().pickerOpen ? act.closeModelPicker() : act.openModelPicker())}>
            <span class="ell">{s().modelButton}</span><Icon name="chevron-down" size={13} class="ns-choice-chevron" />
          </button>
        </Show>
        <span />
        <div class="nsd-right">
          <Show when={s().effortVisible}>
            <select id="ns-effort" class="field nsd-effort" data-keys="select" aria-label={tr("Effort")} value={s().effort} disabled={!fields()}
              title={tr("Variants of the selected model. Default uses the model's own setting.")} onChange={(e) => act.chooseEffort(e.currentTarget.value)}>
              <For each={s().efforts}>{(effort) => <option value={effort.value}>{effort.label}</option>}</For>
            </select>
          </Show>
        </div>
        <Show when={s().service.visible}>
          <span />
          <div class="nsd-service nsd-wide">
            <span class="ell nsd-info-details">{s().service.reason}</span>
            <Show when={s().service.offer}>
              <button type="button" class="btn small" data-keys="button" title={s().service.tooltip} disabled={!s().service.enabled}
                onClick={() => act.startOpenCode()}>{s().service.text}</button>
            </Show>
          </div>
        </Show>
        <Show when={form()?.name_problem}>
          <span /><span class="nsd-problem nsd-wide">{trText(form()!.name_problem!)}</span>
        </Show>
        <Show when={s().error}>
          <span /><span class="nsd-problem nsd-wide" role="status">{s().error}</span>
        </Show>
      </div>
      <Show when={s().accounts[s().account]?.profile?.permission_label.source ? s().accounts[s().account]?.profile : undefined}>{(p) => (
        <>
          <div class="nsd-rule" />
          <div class="ns-perm dim nsd-wide" data-perm title={[trText(p().permission_hint), tr("Change it on the Accounts screen.")].filter(Boolean).join("\n")}>
            {p().permission_default ? tr("Permissions: %1 (Zerus default)", trText(p().permission_label)) : tr("Permissions: %1", trText(p().permission_label))}
          </div>
        </>
      )}</Show>
      <div class="nsd-rule" />
      <div class="nsd-footer">
        <label class="check nsd-terminal" title={form()?.open_terminal_tooltip ? trText(form()!.open_terminal_tooltip) : undefined}>
          <input type="checkbox" data-keys="check" checked={s().openTerminal} disabled={!(form()?.open_terminal_enabled ?? false)}
            onChange={(e) => act.setOpenTerminal(e.currentTarget.checked)} />
          {tr("Open terminal window")}
        </label>
        <span class="grow" />
        <Show when={s().noMachine}><button type="button" class="btn" onClick={() => act.openMachines()}>{tr("Open Machines")}</button></Show>
        <button type="button" class="btn" disabled={!(form()?.cancel_enabled ?? true)} onClick={() => closeNewSession()}>{tr("Cancel")}</button>
        <button type="submit" class="btn primary nsd-start" disabled={!canStart()}>{form() ? trText(form()!.start_text) : tr("Start session")}</button>
      </div>
    </div>
  );

  return (
    <form class="ns-form" classList={{ "nsd-form": dense() }} ref={formEl} onSubmit={start} onKeyDown={formKey}>
      <Show when={!dense()} fallback={denseBody()}>
      <div class="dlg-head">
        <h2 id="ns-title">{tr("New session")}</h2>
        <span class="grow" />
        <button type="button" class="icon-btn" aria-label={tr("Close")} disabled={!(form()?.cancel_enabled ?? true)} onClick={() => closeNewSession()}><Icon name="x" size={16} /></button>
      </div>
      <div class="dlg-body ns-body">
        <div class="form-row">
          <label for="ns-project">{tr("Target")}</label>
          <div class="ns-target">
            <Choice id="ns-project" label={tr("Project")} keys={projectKeys()} selected={projectSelected()} disabled={!fields()}
              open={opened("project")} onOpen={toggle("project")} option={projectOption} class="ns-project" initial
              onPick={(key) => act.chooseProject(s().projects.findIndex((row) => projectKey(row) === key))} />
            <button type="button" class="icon-btn ns-manage" data-keys="button" data-tip={tr("Manage this project's folders")} aria-label={tr("Manage this project's folders")} disabled={!fields()}
              onClick={() => act.manageFolders()}><Icon name="settings" size={18} /></button>
            <Choice id="ns-computer" label={tr("Computer")} keys={computerKeys()} selected={computerSelected()} disabled={!fields()}
              open={opened("computer")} onOpen={toggle("computer")} option={computerOption} class="ns-computer"
              onPick={(key) => act.chooseComputer(s().computers.findIndex((row) => computerKey(row) === key))} />
          </div>
        </div>
        <Show when={s().newProjectRow}>
          <div class="form-row">
            <label for="ns-new-project" classList={{ "ns-reserved": !s().newProjectVisible }}>{tr("New project")}</label>
            <Show when={s().newProjectVisible} fallback={<span class="ns-reserved-field" />}>
              <TextInput id="ns-new-project" class="field" data-keys="text" maxLength={80} placeholder={tr("Project name")} value={s().newProjectName} disabled={!fields()}
                onInput={(e) => act.setNewProjectName(e.currentTarget.value)} />
            </Show>
          </div>
        </Show>
        <div class="form-row">
          <label>{tr("Source")}</label>
          <div class="ns-seg" role="group" aria-label={tr("Source")}>
            <button type="button" class="ns-segbtn" data-keys="button" classList={{ on: !s().repository }} aria-pressed={!s().repository} disabled={!fields()} onClick={() => act.setSource(false)}>
              <Icon name="folder" size={14} />{tr("Working folder")}
            </button>
            <button type="button" class="ns-segbtn" data-keys="button" classList={{ on: s().repository }} aria-pressed={s().repository} disabled={!fields()} onClick={() => act.setSource(true)}>
              <Icon name="git-branch" size={14} />{tr("Git repository")}
            </button>
          </div>
        </div>
        <Show when={!s().repository} fallback={
          <>
            <div class="form-row">
              <label for="ns-url">{tr("Repository")}</label>
              <TextInput id="ns-url" ref={url} class="field mono" data-keys="text" maxLength={4096} placeholder="https://github.com/owner/repository.git"
                title={tr("An SSH or HTTPS repository URL, on GitHub or another Git server.")} value={s().url} disabled={!fields() || !online()}
                onInput={(e) => act.setUrl(e.currentTarget.value)} />
            </div>
            <div class="form-row top">
              <label for="ns-destination">{tr("Clone to")}</label>
              <div class="ns-src-detail">
                <div class="row">
                  <TextInput id="ns-destination" class="field mono grow" data-keys="text" maxLength={8192} value={s().destination} disabled={!fields() || !online()}
                    title={tr("The complete new checkout path. Edit its folder name or choose a parent folder with Browse.")}
                    onInput={(e) => act.setDestination(e.currentTarget.value)} />
                  <button type="button" id="ns-browse-repository" class="btn" data-keys="button" disabled={!fields() || !online()} onClick={browseRepository}>{tr("Browse…")}</button>
                </div>
              </div>
            </div>
          </>
        }>
          <div class="form-row">
            <label for="ns-folder">{tr("Working folder")}</label>
            <div class="row">
              <Choice id="ns-folder" label={tr("Working folder")} keys={folderKeys()} selected={folderSelected()} placeholder={tr("Choose a folder…")}
                disabled={!fields()} open={opened("folder")} onOpen={toggle("folder")} option={folderOption} tip={(key) => folderOf(key)?.tooltip} class="grow"
                onPick={(key) => act.chooseFolder(s().folders.findIndex((row) => folderKey(row) === key))} />
              <button type="button" id="ns-browse" class="btn" data-keys="button" disabled={!(form()?.browse_enabled ?? false)} onClick={browse}>{tr("Browse…")}</button>
            </div>
          </div>
          <div class="form-row top">
            <span />
            <div class="ns-src-detail">
              <div class="mono ns-path ell" title={s().folderPath} style={{ visibility: s().folderPath ? "visible" : "hidden" }}>{s().folderPath || " "}</div>
              <div class="ns-preview" classList={{ warn: !!form()?.outside_project }} title={form()?.preview ? trText(form()!.preview!) : undefined}>{form()?.preview ? trText(form()!.preview!) : ""}</div>
            </div>
          </div>
        </Show>

        <div class="form-row top">
          <label>{tr("Worktree")}</label>
          <div class="col ns-wt">
            <div class="row">
              <div class="ns-seg" role="group" aria-label={tr("Worktree")}>
                <button type="button" class="ns-segbtn" data-keys="button" classList={{ on: automatic() === true }} aria-pressed={automatic() === true} disabled={!(form()?.auto_enabled ?? false)}
                  title={tr("Create a generated branch and worktree when you start the session.")} onClick={() => act.setAuto(true)}>
                  <Icon name="git-branch" size={14} />{tr("Auto")}
                </button>
                <button type="button" class="ns-segbtn" data-keys="button" classList={{ on: automatic() === false }} aria-pressed={automatic() === false} disabled={!(form()?.none_enabled ?? true)}
                  title={tr("Start directly in the working folder.")} onClick={() => act.setAuto(false)}>
                  <Icon name="folder" size={14} />{tr("None")}
                </button>
              </div>
              <div class="ns-menu-anchor">
                <button type="button" id="ns-worktree-options" class="icon-btn" data-keys="button" data-tip={tr("Worktree options")} aria-label={tr("Worktree options")} aria-haspopup="menu" aria-expanded={opened("worktree")}
                  disabled={!(form()?.worktree_options_enabled ?? false)} onClick={toggleMenu}>
                  <Icon name="more" size={16} />
                </button>
                <Show when={opened("worktree")}>
                  <div class="menu ns-menu" role="menu" tabindex="-1" ref={menu} onKeyDown={menuKey}>
                    <Show when={s().worktrees.chooseVisible}>
                      <button type="button" class="menu-item" role="menuitem" aria-disabled={!form()?.choose_existing_enabled} onClick={chooseExisting}>
                        <Icon name="git-branch" size={14} /><span>{tr("Choose existing…")}</span>
                      </button>
                    </Show>
                    <Show when={s().worktrees.createVisible}>
                      <button type="button" class="menu-item" role="menuitem" aria-disabled={!form()?.create_manually_enabled} onClick={createManually}>
                        <Icon name="add" size={14} /><span>{tr("Create manually…")}</span>
                      </button>
                    </Show>
                    <div class="menu-sep" role="separator" />
                    <button type="button" class="menu-item" role="menuitem" onClick={continueClaudeApp}>
                      <AgentGlyph agent="claude" size={14} /><span>{tr("Continue a Claude app session…")}</span>
                    </button>
                  </div>
                </Show>
              </div>
            </div>
            <div class="mono ns-wt-target" title={form()?.worktree_tooltip ? trText(form()!.worktree_tooltip!) : undefined}>{form()?.worktree_target ?? ""}</div>
          </div>
        </div>

        <div class="form-row top">
          <label>{tr("Account and agent")}</label>
          <div class="col">
            <ul class="ns-accounts" ref={accountList} role="listbox" aria-label={tr("Account and agent")} tabindex={accountsEnabled() ? 0 : -1}
              aria-disabled={!accountsEnabled()} aria-activedescendant={s().account >= 0 ? `ns-account-${s().account}` : undefined} onKeyDown={accountKeyDown}>
              <For each={accountKeys()}>{(key) => {
                const row = rowOf(() => s().accounts.find((r) => accountKey(r) === key));
                const index = () => s().accounts.findIndex((r) => accountKey(r) === key);
                const selected = () => index() >= 0 && index() === s().account;
                return (
                  <li id={`ns-account-${index()}`} role="option" aria-selected={selected()} classList={{ on: selected() }} title={row().tooltip || undefined}
                    aria-label={row().accessible} onClick={() => { if (accountsEnabled()) act.chooseAccount(index()); }}>
                    <AccountLine row={row} selected={selected()} />
                  </li>
                );
              }}</For>
            </ul>
            {/* The effective permission mode of the account (owner, 2026-11-07, T-6hcfnu). */}
            <Show when={s().accounts[s().account]?.profile?.permission_label.source ? s().accounts[s().account]?.profile : undefined}>{(p) => (
              <div class="ns-perm dim" title={[trText(p().permission_hint), tr("Change it on the Accounts screen.")].filter(Boolean).join("\n")}>
                {p().permission_default ? tr("Permissions: %1 (Zerus default)", trText(p().permission_label)) : tr("Permissions: %1", trText(p().permission_label))}
              </div>
            )}</Show>
            <div class="row ns-acc-status">
              <span class="grow ns-status">{s().accountStatus}</span>
              <button type="button" class="btn small" data-keys="button" aria-label={tr("Refresh accounts")} disabled={!s().refreshEnabled} onClick={() => act.refreshAccounts()}>
                <Icon name="refresh" size={12} />{tr("Refresh")}
              </button>
            </div>
          </div>
        </div>

        <Show when={s().modelVisible}>
          <div class="form-row">
            <label for="ns-model">{tr("Model")}</label>
            <Show when={s().openCode} fallback={
              <Dropdown id="ns-model" class="field ns-model-btn" data-keys="select" aria-label={tr("Model")} value={s().model} disabled={!s().modelEnabled}
                onChange={act.chooseModel} options={s().models.map((model) => ({ value: model.value, label: model.text || model.label }))} />
            }>
              <button type="button" id="ns-model" class="field ns-model-btn" data-keys="button" aria-label={tr("Model")} aria-haspopup="dialog" aria-expanded={s().pickerOpen}
                title={tr("Models OpenCode offers on this computer. Native default uses OpenCode's configuration.")} disabled={!s().modelEnabled}
                onClick={() => (s().pickerOpen ? act.closeModelPicker() : act.openModelPicker())}>
                <span class="ell">{s().modelButton}</span><Icon name="chevron-down" size={14} class="ns-choice-chevron" />
              </button>
            </Show>
          </div>
        </Show>
        <Show when={s().service.visible}>
          <div class="form-row">
            <span />
            <div class="row ns-service">
              <span class="grow ns-status">{s().service.reason}</span>
              <Show when={s().service.offer}>
                <button type="button" class="btn small" data-keys="button" title={s().service.tooltip} disabled={!s().service.enabled} onClick={() => act.startOpenCode()}>{s().service.text}</button>
              </Show>
            </div>
          </div>
        </Show>
        <Show when={s().effortVisible}>
          <div class="form-row">
            <label for="ns-effort">{tr("Effort")}</label>
            <Dropdown id="ns-effort" class="field ns-effort" data-keys="select" aria-label={tr("Effort")} title={tr("Variants of the selected model. Default uses the model's own setting.")}
              disabled={!fields()} value={s().effort} onChange={(value) => act.chooseEffort(value)} options={[...((s().efforts) ?? []).map((effort) => ({ value: effort.value, label: effort.label }))]} />
          </div>
        </Show>

        <div class="form-row">
          <label for="ns-name">{tr("Session name")}</label>
          <TextInput id="ns-name" class="field mono" data-keys="text" value={s().name} disabled={!fields()} aria-invalid={!!form()?.name_problem}
            aria-describedby={form()?.name_problem ? "ns-name-problem" : undefined} onInput={(e) => act.setName(e.currentTarget.value)} />
        </div>
        <Show when={form()?.name_problem}>
          <div class="form-row"><span /><span id="ns-name-problem" class="ns-status">{form()?.name_problem ? trText(form()!.name_problem!) : ""}</span></div>
        </Show>
        <div class="form-row">
          <span />
          <label class="check" title={form()?.open_terminal_tooltip ? trText(form()!.open_terminal_tooltip) : undefined}>
            <input type="checkbox" data-keys="check" checked={s().openTerminal} disabled={!(form()?.open_terminal_enabled ?? false)} onChange={(e) => act.setOpenTerminal(e.currentTarget.checked)} />
            {tr("Open terminal window")}
          </label>
        </div>
      </div>
      {/* The error line, on its own line between the form and the buttons (264-265). */}
      <div class="ns-error" role="status">{s().error}</div>
      <div class="dlg-foot">
        <span class="spacer" />
        <Show when={s().noMachine}>
          <button type="button" class="btn" onClick={() => act.openMachines()}>{tr("Open Machines")}</button>
        </Show>
        <button type="button" class="btn" disabled={!(form()?.cancel_enabled ?? true)} onClick={() => closeNewSession()}>{tr("Cancel")}</button>
        <button type="submit" class="btn primary ns-start" disabled={!canStart()}>{form() ? trText(form()!.start_text) : tr("Start session")}</button>
      </div>
      </Show>
    </form>
  );
}

/** The one New session dialog, opened by the store (`openNewSession`) and closed by Cancel, Escape or an admitted Start. */
export function NewSessionDialog() {
  let el!: HTMLDialogElement;
  const confirm = createConfirm();
  const [popup, setPopup] = createSignal<string>();
  const open = () => !!newSession();
  // Task #368: the skin's view picks the dense frame (Qt's views.newSession).
  const dense = () => skin().layout["new-session"] === "dense";
  createEffect(on(open, (shown) => {
    if (!shown) {
      setPopup(undefined);
      setNewSessionChild(undefined);
      confirm.cancel();
      if (el.open) el.close();
      return;
    }
    if (el.open) return;
    el.showModal();
    queueMicrotask(() => {
      const first = el.querySelector<HTMLElement>("[data-initial]:not(:disabled)")
        ?? el.querySelector<HTMLElement>(".dlg-body :is(input, select, button, [tabindex='0']):not(:disabled)");
      first?.focus();
    });
  }));
  const cancelEnabled = () => newSession()?.form?.cancel_enabled ?? true;
  return (
    <>
      <dialog class="dlg ns-dlg" classList={{ "nsd-dlg": dense() }} ref={el} aria-labelledby="ns-title"
        onCancel={(e) => {
          // Escape closes the model picker, then an open list or menu, back to its button; the dialog closes only through the store.
          e.preventDefault();
          if (newSession()?.pickerOpen) {
            act.closeModelPicker();
            document.getElementById("ns-model")?.focus();
            return;
          }
          if (popup()) {
            const owner = el.querySelector<HTMLElement>("[aria-expanded='true']");
            setPopup(undefined);
            owner?.focus();
            return;
          }
          if (cancelEnabled()) closeNewSession();
        }}
        onClose={() => { if (newSession()) closeNewSession(); }}>
        <Show when={newSession()}>{(state) => <NewSessionForm s={state} popup={popup} setPopup={setPopup} confirm={confirm} />}</Show>
        <Show when={newSession()?.pickerOpen}>
          <ModelPopover view={() => newSession()?.picker ?? EMPTY_PICKER} act={PICKER} anchor={() => document.getElementById("ns-model")} />
        </Show>
      </dialog>
      <NewSessionChildren />
      <LaunchWarning confirm={confirm} />
    </>
  );
}

const EMPTY_PICKER: NewSessionState["picker"] = {
  groups: [], notice: "", all: { checked: false, enabled: false, tooltip: "" }, selected: undefined, query: "", favoritesAvailable: false,
};

/** The model popover's clicks, sent to the open dialog's picker. */
const PICKER: ModelPickerActions = {
  click: (value, star) => act.pickerClick(value, star),
  select: (value) => act.pickerSelect(value),
  toggleAll: () => act.pickerToggleAll(),
  search: (text) => act.pickerSearch(text),
  useNative: () => act.useNativeModel(),
  close: () => act.closeModelPicker(),
};

/** A peer's folder listings and New folder, bound to it (`list_directories`, `create_directory`). */
const folderCalls = (target: string): FolderBrowserCalls => ({
  list: (path, hidden) => bridge.listDirectories(target, path, hidden),
  create: (path) => bridge.createDirectory(target, path),
});

/** The control a child dialog gives the focus back to. */
const OPENER: Record<NewSessionChild["kind"], string> = {
  browse: "ns-browse", "browse-repository": "ns-browse-repository", "choose-worktree": "ns-worktree-options", "create-worktree": "ns-worktree-options",
  "claude-app": "ns-worktree-options",
};

/**
 * The child dialog New session opened (`newSessionChild`), each in its own
 * modal layer over it. Its answer goes to the store with the computer and
 * project it opened with; the store applies it only while they are still
 * the dialog's (585, 1123, 1134). Opening and inspecting them only reads.
 */
function NewSessionChildren() {
  const done = (kind: NewSessionChild["kind"]) => {
    setNewSessionChild(undefined);
    queueMicrotask(() => document.getElementById(OPENER[kind])?.focus());
  };
  // The computer's listed sessions, for the worktree picker's counts (G1). The memo reads only
  // the rows' computer and paths, and an equal list keeps its identity, so the picker counts
  // again only when that computer's sessions or their paths change.
  const sessionsOn = (target: string) => createMemo((): PickerSession[] =>
    Object.values(catalog.rows).filter((row) => row.computer === target).map((row) => ({ gitRoot: row.gitRoot, cwdCanonical: row.cwdCanonical })),
  [], { equals: sameSessions });
  const onlineOf = (target: string) => newSession()?.computers.find((row) => row.target === target)?.online ?? false;
  return (
    <Switch>
      <Match when={(() => { const c = newSessionChild(); return c?.kind === "browse" ? c : undefined; })()} keyed>{(child) => (
        <FolderBrowserDialog computer={child.computer} start={child.path} calls={folderCalls(child.target)}
          onChoose={(path) => act.applyBrowsedFolder(child.target, child.project, path)} onClose={() => done(child.kind)} />
      )}</Match>
      <Match when={(() => { const c = newSessionChild(); return c?.kind === "browse-repository" ? c : undefined; })()} keyed>{(child) => (
        <FolderBrowserDialog computer={child.computer} start={child.parent} calls={folderCalls(child.target)}
          onChoose={(path) => { if (newSession()?.host === child.target) act.setRepositoryParent(path); }} onClose={() => done(child.kind)} />
      )}</Match>
      <Match when={(() => { const c = newSessionChild(); return c?.kind === "choose-worktree" ? c : undefined; })()} keyed>{(child) => {
        const sessions = sessionsOn(child.target);
        return (
          <ChooseWorktreeDialog machine={child.computer} path={child.path} online={() => onlineOf(child.target)} style={() => child.style} sessions={sessions}
            reads={{ snapshot: () => bridge.worktreeSnapshot(child.target, child.path), catalog: (refresh) => bridge.worktreeCatalog(child.target, child.path, refresh) }}
            onUse={(path) => act.chooseExistingWorktree(child.target, child.project, path)} onClose={() => done(child.kind)} />
        );
      }}</Match>
      <Match when={(() => { const c = newSessionChild(); return c?.kind === "create-worktree" ? c : undefined; })()} keyed>{(child) => (
        <NewWorktreeDialog target={child.target} machine={child.computer} source={child.path} catalog={child.catalog} branch={child.branch} style={child.style}
          create={(branch, base, destination) => bridge.createWorktree(child.target, child.path, child.catalog, branch, base, destination)}
          pickLocal={(start) => bridge.pickLocalFolder(start)} browse={folderCalls(child.target)}
          onCreated={(created) => act.worktreeCreated(child.target, child.project, created.path, created.catalog)} onClose={() => done(child.kind)} />
      )}</Match>
      <Match when={(() => { const c = newSessionChild(); return c?.kind === "claude-app" ? c : undefined; })()} keyed>{(child) => (
        <ClaudeAppDialog host={child.target} folder={child.path}
          onSent={() => { setNewSessionChild(undefined); closeNewSession(); }} onClose={() => done(child.kind)} />
      )}</Match>
    </Switch>
  );
}
