import { For, Index, Show, Switch, Match, createEffect, createMemo, createSignal, lazy, Suspense, onCleanup } from "solid-js";
import { Icon } from "../lib/icons";
import { MachineTile } from "../components/MachineTile";
import { MachineLabel } from "../components/MachineLabel";
import { Actions, Spinner, Status } from "../components/Status";
import { AgentBadge, ProviderGlyph } from "../sessions/SessionCard";
import { ResizeHandle } from "../sessions/SessionList";
import type { Account } from "../../types";
import { agentKind, catalog } from "../../store/catalog";
import { machineCards } from "../../store/machines";
import { accountMachine, accountsPage, loadAccounts, requestedProfile, setAccountMachine, setRequestedProfile } from "../../store/accounts";
import { connectionOf, osOf } from "../../store/overview";
import { useNow } from "../../store/latest";
import { sessionsWidth, setPage } from "../../store/ui";
import { clock, providerName, stateText, windowsAt } from "../lib/usage";
import { UsageMeters } from "../components/UsageMeter";
import { accountManagement } from "../../store/accountManagement";
import { accountSetup } from "../../store/accountSetup";
import {accountDistribution} from "../../store/accountDistribution";
import { creditsText, loginLabel, memberStateLabel, memberStateText, memberOf, panelWindow, profileTags, summaryLine, tone, visibleAccounts, visibleMembers } from "./accounts-ui";
import { tr } from "../../i18n";
import { trText } from "../../text";

// Agent accounts (AccountsPage.cpp), ported from design/webui/src/screens/Accounts.tsx:
// profiles grouped by provider identity across machines; choosing a machine
// tag selects that machine's profile. Catalog edits use native snapshots;
// setup and distribution share that writer and the existing terminal backend.

const AccountDistributionDialog=lazy(()=>import("./AccountDistributionDialog"));
const AccountSetupDialog = lazy(() => import("./AccountSetupDialog"));
const managementBusy=()=>accountManagement.busy()||accountSetup.busy()||accountDistribution.busy();
const closeManagement=()=>{accountManagement.close();accountSetup.close();accountDistribution.close();};
const clearManagementNotice=()=>{accountManagement.clearNotice();accountSetup.clearNotice();accountDistribution.clearNotice();};
const openCatalog=(...args:Parameters<typeof accountManagement.open>)=>{accountSetup.close();accountDistribution.close();accountSetup.clearNotice();accountDistribution.clearNotice();return accountManagement.open(...args);};
const AccountDefaultsDialog = lazy(() => import("./AccountDefaultsDialog"));
const AccountCatalogDialog = lazy(() => import("./AccountCatalogDialog"));

const [selectedAccount, setSelectedAccount] = createSignal<string | undefined>();
const [selectedProfile, setSelectedProfile] = createSignal<string | undefined>();

/** The id tag that tells apart two profiles of one account on the same machine. */
function ProfileTag(props: { tag?: string; id: string }) {
  return <Show when={props.tag}><span class="acct-pid tabular" title={tr("Profile %1", props.id)}>{props.tag}</span></Show>;
}

function AccountRow(props: { a: Account; now: number }) {
  const on = () => selectedAccount() === props.a.key;
  const tags = createMemo(() => profileTags(props.a));
  const pick = () => {
    closeManagement();clearManagementNotice();
    setSelectedAccount(props.a.key);
    setSelectedProfile(memberOf(props.a, undefined, accountMachine())?.key);
  };
  return (
    <button class="acct-row" classList={{ on: on() }} aria-current={on()} title={props.a.members.map((m) => `${m.machine}${tags().has(m.key) ? ` ${tags().get(m.key)}` : ""}: ${trText(m.permission_label)}`).join("\n")} onClick={pick}>
      {/* The session card's 34 px leading column, holding the provider tile. */}
      <span class="acct-av"><span class="acct-ptile" style={{ "--agent": `var(--z-agent-${agentKind(props.a.provider)})` }}><ProviderGlyph agent={agentKind(props.a.provider)} size={18} /></span></span>
      <span class="acct-l1">
        <b class="ell grow">{props.a.label}</b>
        <AgentBadge agent={agentKind(props.a.provider)} />
      </span>
      <span class="row acct-row-tags">
        <span class="row acct-machines">
          <For each={props.a.members.slice(0, 3)}>{(m) => <span class="acct-mchip"><MachineLabel alias={m.machine} dimmed={!m.online} /><ProfileTag tag={tags().get(m.key)} id={m.id} /></span>}</For>
          <Show when={props.a.members.length > 3}><span class="dim">+{props.a.members.length - 3}</span></Show>
        </span>
        <span class="grow" />
        <span class="acct-perm" classList={{ bypass: props.a.members.length > 0 && props.a.members.every((m) => m.permission === "bypass") }}>{trText(props.a.permission)}</span>
      </span>
      {/* The detail's usage windows in the Overview card's compact meters; the reset shows from 90%. */}
      <Show when={props.a.usage.windows.length}>
        <span class="acct-row-usage"><UsageMeters windows={windowsAt(props.a.usage.windows, props.now)} compact resetFrom={90} /></span>
      </Show>
    </button>
  );
}

function UsagePanel(props: { a: Account }) {
  // Resets and pace re-render every 15 s while shown; nothing else runs on a timer.
  const now = useNow(15_000);
  const u = () => props.a.usage;
  return (
    <section class="acct-usage box">
      <div class="row">
        <h3>{tr("Account usage")}</h3>
        <span class="grow" />
        <Show when={u().checked_at !== null}><span class="dim">{u().seeded ? tr("Checked %1", clock(u().checked_at!)) : tr("Updated %1", clock(u().checked_at!))}</span></Show>
      </div>
      <p class="muted">{tr("Shared by sessions using this account.")}</p>
      <Show when={u().offline}><Status>{tr("Machine is offline. Showing the last available account data.")}</Status></Show>
      <Show when={u().refreshing}><Status kind="busy">{tr("Updating… Previous values remain visible.")}</Status></Show>
      <Show when={!u().refreshing && u().refresh_error}><Status kind="warn">{tr("Update failed. Showing the last available values.")}</Status></Show>
      <Show when={!u().windows.length && !u().balances.length && !u().offline}><p>{stateText(u())}</p></Show>
      <For each={u().windows}>{(p) => {
        const w = () => panelWindow(p, now());
        return (
          <div class="acct-window">
            <div class="row"><b class="tabular">{w().period}</b><span class="tabular">{w().usedText}</span></div>
            <div class="acct-meter rails">
              <div class="acct-bar" classList={{ warn: w().tone === "warning", danger: w().tone === "critical" }}><i style={{ "--w": `${w().used}%` }} /></div>
              <Show when={w().elapsed !== undefined}>
                <div class="acct-bar elapsed" title={tr("%1% of the window elapsed", Math.round(w().elapsed!))}><i style={{ "--w": `${w().elapsed}%` }} /></div>
              </Show>
            </div>
            <div class="row dim">
              <Show when={w().resets}><span>{w().resets}</span></Show>
              <Show when={u().seeded && u().checked_at !== null}><span>{tr("Checked %1", clock(u().checked_at!))}</span></Show>
              <span class="grow" />
              <span class="acct-pace">{w().forecast}</span>
            </div>
          </div>
        );
      }}</For>
      <For each={u().balances}>{(b) => <p>{`${b.kind}: ${b.balance} ${b.currency}`}</p>}</For>
      <Show when={creditsText(u().credits)}>{(text) => <p>{text()}</p>}</Show>
    </section>
  );
}

function Detail(props: { a: Account }) {
  const m = () => memberOf(props.a, selectedProfile(), accountMachine())!;
  const [moreOpen, setMoreOpen] = createSignal(false);
  const [moreQuery, setMoreQuery] = createSignal("");
  const shown = () => visibleMembers(props.a, m().key);
  const nativeConfig = () => props.a.provider === "opencode";
  const defaultable = () => props.a.provider !== "dsh" && !nativeConfig();
  const matches = () => props.a.members.filter((x) => x.machine.toLowerCase().includes(moreQuery().trim().toLowerCase()));
  const tags = createMemo(() => profileTags(props.a));
  // The selected machine, with its profile tag when another profile shares it.
  const where = () => (tags().has(m().key) ? `${m().machine} (${tags().get(m().key)})` : m().machine);
  const choose = (key: string) => { setSelectedProfile(key); closeManagement(); clearManagementNotice(); };
  return (
    <div class="acct-detail">
      <div class="acct-title">
        <h2 class="ell">{props.a.label}</h2>
        <span class="grow" />
        <button class="btn" disabled={managementBusy()} onClick={event=>void openCatalog("rename",props.a.key,m().key,event.currentTarget)}><Icon name="pencil" size={13} />{tr("Rename…")}</button>
      </div>
      <div class="row acct-identity">
        <AgentBadge agent={agentKind(props.a.provider)} />
        <div class="col selectable">
          <For each={props.a.identity_lines}>{(line) => <span>{trText(line)}</span>}</For>
          <Show when={props.a.identity_note}>{(note) => <span class="muted">{trText(note())}</span>}</Show>
        </div>
      </div>

      <section class="acct-card box">
        <h3>{tr("Machines")}</h3>
        {/* One row per profile: the row chooses it; the default is a badge or a separate button beside it. */}
        <div class="acct-mrows" classList={{ defaults: defaultable() }} role="radiogroup" aria-label={tr("Machine profile")}>
          <For each={shown()}>{(x) => (
            <div class="acct-mrow" classList={{ on: x.key === m().key }}>
              <button class="acct-mrow-pick" role="radio" aria-checked={x.key === m().key} onClick={() => choose(x.key)}>
                <MachineTile os={osOf(machineCards[x.host]?.platform)} state={x.online ? connectionOf(catalog.computers[x.host]?.status).tile : "offline"} size={22} />
                <MachineLabel alias={x.machine} dimmed={!x.online} />
                <ProfileTag tag={tags().get(x.key)} id={x.id} />
                <span class="grow" />
                <span class="acct-mstate" data-tone={tone(x.state)}>{memberStateLabel(x.state)}</span>
              </button>
              <span class="acct-mrow-default">
                <Show when={x.is_default} fallback={
                  <Show when={x.online && x.state === "ready" && defaultable()}>
                    <button class="btn small" disabled={managementBusy()} title={tr("Use this account for new %1 sessions on %2 when no account is specified. Existing sessions keep their account.", providerName(props.a.provider), x.machine)} onClick={event=>void openCatalog("default",props.a.key,x.key,event.currentTarget)}>{tr("Make default")}</button>
                  </Show>
                }>
                  <span class="acct-default" title={tr("Default for new %1 sessions on %2 when no account is chosen", providerName(props.a.provider), x.machine)}>{tr("Default")}</span>
                </Show>
              </span>
            </div>
          )}</For>
        </div>
        <div class="row acct-mfoot">
          <Show when={props.a.members.length > shown().length}>
            <span class="acct-more-anchor">
              <button class="btn small" aria-haspopup="dialog" aria-expanded={moreOpen()} onClick={() => { setMoreQuery(""); setMoreOpen(!moreOpen()); }}>
                {tr("+%1 more", props.a.members.length - shown().length)}
              </button>
              <Show when={moreOpen()}>
                <div class="acct-more" role="dialog" aria-label={tr("Machines with %1", props.a.label)}
                  onKeyDown={(e) => { if (e.key === "Escape") { e.stopPropagation(); setMoreOpen(false); } }}>
                  <input class="field" type="search" placeholder={tr("Search %1 machines", props.a.members.length)} value={moreQuery()}
                    onInput={(e) => setMoreQuery(e.currentTarget.value)} autofocus aria-label={tr("Search machines")} />
                  <div class="acct-more-list">
                    <For each={matches()} fallback={<div class="dim acct-more-empty">{tr("No machine matches.")}</div>}>
                      {(x) => (
                        <button class="acct-more-row" classList={{ on: x.key === m().key }} onClick={() => { choose(x.key); setMoreOpen(false); }}>
                          <MachineTile os={osOf(machineCards[x.host]?.platform)} state={x.online ? connectionOf(catalog.computers[x.host]?.status).tile : "offline"} size={22} />
                          <MachineLabel alias={x.machine} />
                          <ProfileTag tag={tags().get(x.key)} id={x.id} />
                          <span class="grow" />
                          <Show when={x.is_default}><span class="acct-default">{tr("Default")}</span></Show>
                          <span class="dim">{memberStateText(x.state)}</span>
                        </button>
                      )}
                    </For>
                  </div>
                </div>
              </Show>
            </span>
          </Show>
          <button class="btn small acct-add" disabled={managementBusy()} onClick={event=>{accountManagement.close();accountSetup.close();accountManagement.clearNotice();accountSetup.clearNotice();void accountDistribution.open(props.a.key,m().key,event.currentTarget);}}><Icon name="plus" size={12} />{tr("Add to machine…")}</button>
        </div>
        <div class="acct-status">
          <Status kind={tone(m().state)}>{trText(m().detail)}</Status>
          <Actions>
            <Show when={loginLabel(m())}>{(label) => <button class="btn" disabled={!m().online||!m().installed||managementBusy()} title={m().provider==="dsh"?tr("Save a DeepSeek API key on %1.", m().machine):undefined} onClick={event=>{accountManagement.close();accountManagement.clearNotice();void accountSetup.open(m().provider==="dsh"?"key":"login",event.currentTarget,props.a.key,m().key);}}><Icon name="key-round" size={13} />{label()}</button>}</Show>
            <Show when={!m().installed}><button class="btn" disabled={!m().online||m().provider==="opencode"||managementBusy()} onClick={event=>{accountManagement.close();accountManagement.clearNotice();void accountSetup.open("install",event.currentTarget,props.a.key,m().key);}}><Icon name="download" size={13} />{tr("Install agent…")}</button></Show>
            <Show when={m().opencode?.stopped}>
              <button class="btn" disabled={!m().online||managementBusy()} onClick={event=>{accountManagement.close();accountManagement.clearNotice();void accountSetup.open("start",event.currentTarget,props.a.key,m().key);}}><Icon name="play" size={13} />{tr("Start OpenCode on %1", m().machine)}</button>
            </Show>
          </Actions>
        </div>
        <Show when={m().opencode}>{(oc) => (
          <>
            <Show when={oc().providers.length}>
              <div class="acct-oc" aria-label={tr("OpenCode providers")}>
                <For each={oc().providers}>{(p) => (
                  <div class="acct-oc-row"><span class="grow">{p.label}</span><span class="muted tabular">{p.models}</span><span class="dim">{p.connection}</span></div>
                )}</For>
              </div>
            </Show>
            <Show when={oc().truncated}><p class="dim">{tr("Showing the first %1 providers.", oc().providers.length)}</p></Show>
            <Show when={oc().stale_catalog_at !== null}>
              <p class="dim">{tr("Last known catalog from %1.", new Date(oc().stale_catalog_at! * 1000).toLocaleString(undefined, { dateStyle: "short", timeStyle: "short" }))}</p>
            </Show>
          </>
        )}</Show>
        <div class="acct-setting">
          <span title={trText(m().permission_hint)}>
            {tr("Permissions on %1:", m().machine)} <b classList={{ danger: m().permission === "bypass" }}>{trText(m().permission_label)}</b>
            <span class="dim acct-src">{(m().permission_source ?? (m().permission_mode === "bypass" ? "zerus" : "provider")) === "zerus" ? tr("set in Zerus") : tr("from provider settings")}</span>
          </span>
          <button class="btn small" disabled={!m().online || nativeConfig() || managementBusy()} title={tr("Change permission defaults on %1", m().machine)} onClick={event=>void openCatalog("permissions",props.a.key,m().key,event.currentTarget)}>{tr("Change…")}</button>
        </div>
        <Show when={m().model_supported}>
          <div class="acct-setting">
            <span>{trText(m().model_text)}</span>
            <button class="btn small" disabled={!m().online || managementBusy()} onClick={event=>void openCatalog("model",props.a.key,m().key,event.currentTarget)}>{tr("Model and effort…")}</button>
          </div>
        </Show>
        <Actions>
          <Show when={defaultable()}>
            <button class="btn" disabled={m().is_default || !m().online || managementBusy()} title={tr("Use this account for new %1 sessions on %2 when no account is specified. Existing sessions keep their account.", providerName(props.a.provider), m().machine)} onClick={event=>void openCatalog("default",props.a.key,m().key,event.currentTarget)}>{m().is_default ? tr("Default on %1", where()) : tr("Make default on %1", where())}</button>
          </Show>
          <button class="btn danger" disabled={!m().online || managementBusy()} title={tr("Remove this profile from the selected machine's Zerus catalog. You will confirm the machine and consequences before anything changes.")} onClick={event=>void openCatalog("remove",props.a.key,m().key,event.currentTarget)}><Icon name="trash" size={13} />{props.a.members.length > 1 ? tr("Remove from %1…", where()) : tr("Remove…")}</button>
        </Actions>
        <Show when={accountManagement.notice()||accountSetup.notice()||accountDistribution.notice()}>{text=><Status kind="ok">{text()}</Status>}</Show>
      </section>
      <UsagePanel a={props.a} />
    </div>
  );
}

export function AccountsScreen() {
  onCleanup(closeManagement);
  const view = accountsPage;
  const visible = createMemo(() => visibleAccounts(view(), accountMachine()));
  // The rows' reset times re-render every 15 s; one timer serves every row.
  const now = useNow(15_000);
  // An Overview card asks for a profile: select its account once it is listed.
  createEffect(() => {
    const key = requestedProfile();
    const account = key ? view().accounts.find((a) => a.members.some((m) => m.key === key)) : undefined;
    if (!account) return;
    closeManagement();accountManagement.clearNotice();
    setSelectedAccount(account.key);
    setSelectedProfile(key);
    setRequestedProfile(undefined);
  });
  const current = () => visible().find((a) => a.key === selectedAccount()) ?? visible()[0];
  const filterName = () => view().hosts.find((h) => h.computer === accountMachine())?.name ?? accountMachine();
  // Selection stays in view as the list scrolls or the filter changes; arrow keys move it.
  let list: HTMLDivElement | undefined;
  createEffect(() => {
    const key = current()?.key;
    if (!key || !list) return;
    queueMicrotask(() => list?.querySelector<HTMLElement>(".acct-row.on")?.scrollIntoView({ block: "nearest" }));
  });
  const step = (e: KeyboardEvent) => {
    const rows = [...(list?.querySelectorAll<HTMLButtonElement>(".acct-row") ?? [])];
    const at = rows.findIndex((r) => r === document.activeElement);
    const next = e.key === "ArrowDown" ? at + 1 : e.key === "ArrowUp" ? at - 1 : e.key === "Home" ? 0 : e.key === "End" ? rows.length - 1 : undefined;
    if (next === undefined || !rows.length) return;
    e.preventDefault();
    const row = rows[Math.max(0, Math.min(rows.length - 1, next))];
    row.focus();
    row.click();
  };
  return (
    <div class="acct-page">
      {/* The Sessions list's pane: its width, border and handle; only the list scrolls under a fixed header. */}
      <section class="acct-pane" style={{ "--pane-w": `${sessionsWidth()}px` }} aria-label={tr("Agent accounts")}>
        <div class="sp-panel acct-panel">
          <header class="sp-head acct-head">
            <h1>{tr("Agent accounts")}</h1>
            <button class="icon-btn hd" aria-label={tr("Refresh accounts")} disabled={view().busy || managementBusy()} onClick={() => void loadAccounts(true)}
              title={view().busy ? tr("Updating accounts… %1 remaining", view().pending) : tr("Refresh account usage (automatically every 5 minutes)")}>
              <Show when={!view().busy} fallback={<Spinner />}><Icon name="refresh-cw" size={15} /></Show>
            </button>
            <span class="grow" />
            {/* The Sessions pane's "+" button: a text button cannot fit the pane in every language. */}
            <button class="icon-btn hd" aria-label={tr("Add account")} data-tip={view().no_hosts ?? tr("Add account")} disabled={!!view().no_hosts||managementBusy()} onClick={event=>{accountManagement.close();accountManagement.clearNotice();void accountSetup.open("add",event.currentTarget,"","",accountMachine());}}><Icon name="add" size={18} /></button>
          </header>
          <p class="muted acct-summary">{tr("Your accounts across all machines. Choose an account to manage where it is available.")}</p>
          <For each={view().notices}>{(notice) => <Status kind="warn">{notice}</Status>}</For>
          <Show when={!view().no_hosts}>
            <select class="field acct-filter" aria-label={tr("Machine")} value={accountMachine()} onChange={(e) => {closeManagement();clearManagementNotice();setAccountMachine(e.currentTarget.value);}}>
              <option value="*">{tr("All machines")}</option>
              <For each={view().hosts}>{(h) => <option value={h.computer}>{h.label}</option>}</For>
            </select>
            <Show when={view().snapshot}>
              <div class="row acct-count"><Show when={view().pending}><Spinner size={12} /></Show><span class="muted ell">{summaryLine(view(), visible().length)}</span></div>
            </Show>
          </Show>
          <div class="acct-scroll" ref={list} role="list" aria-label={tr("Agent accounts")} onKeyDown={step}>
            <Show when={!view().no_hosts}>
              <Show when={view().snapshot} fallback={<div class="acct-loading"><div class="row"><Spinner /><span>{tr("Loading accounts…")}</span></div><p class="muted">{tr("Checking accounts across your machines.")}</p></div>}>
                <Show when={visible().length} fallback={<p class="muted acct-none">{accountMachine() === "*" ? tr("No accounts.") : tr("No accounts on %1.", filterName())}</p>}>
                  <Index each={visible()}>{(a) => <AccountRow a={a()} now={now()} />}</Index>
                </Show>
                <Show when={view().truncated > 0}><p class="dim acct-none">{tr("%1 more accounts are not shown.", view().truncated)}</p></Show>
              </Show>
            </Show>
          </div>
        </div>
        <ResizeHandle />
      </section>
      <main class="acct-main">
        <div class="acct-main-scroll">
          <Show when={!view().no_hosts} fallback={
            <div class="empty acct-empty"><Icon name="user-round-cog" size={28} /><h3>{tr("No machine keeps agent accounts")}</h3>
              <p>{view().no_hosts}</p>
              <button class="btn" onClick={() => setPage("machines")}>{tr("Open Machines")}</button></div>
          }>
            <Show when={view().snapshot && current()} fallback={<div class="acct-detail"><h2 class="muted">{tr("Choose an account")}</h2></div>}>
              {(a) => <Show when={a().key} keyed><Detail a={a()} /></Show>}
            </Show>
          </Show>
        </div>
      </main>
      <Show when={accountDistribution.prompt()}>{prompt=><Suspense><AccountDistributionDialog prompt={prompt()}/></Suspense>}</Show>
      <Show when={accountSetup.prompt()}>{prompt=><Suspense><AccountSetupDialog prompt={prompt()} busy={accountSetup.busy()} close={accountSetup.close} confirm={(values,handle)=>void accountSetup.confirm(values,handle)}/></Suspense>}</Show>
      <Show when={accountManagement.prompt()}>{prompt=><Suspense><Switch><Match when={prompt().kind==="permissions" || prompt().kind==="model"}><AccountDefaultsDialog prompt={prompt()} close={accountManagement.close} confirm={(values,handle)=>void accountManagement.confirm(values,handle)}/></Match><Match when={true}><AccountCatalogDialog prompt={prompt()} close={accountManagement.close} confirm={(label,handle)=>void accountManagement.confirm(label,handle)}/></Match></Switch></Suspense>}</Show>
    </div>
  );
}
