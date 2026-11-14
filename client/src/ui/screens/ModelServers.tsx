import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount, type JSX } from "solid-js";
import { tr } from "../../i18n";
import { trText } from "../../text";
import { Icon } from "../lib/icons";
import * as bridge from "../../bridge";
import type { ModelServer, ModelServerLogs, ModelServerModel } from "../../bridge";
import { modelServers as store, words, type StatusEntry } from "../../store/modelServers";
import { machineCards } from "../../store/machines";
import { projectsPage, loadProjects } from "../../store/projects";
import { selected } from "../../store/ui";
import { FolderBrowserDialog } from "../components/FolderBrowser";
import { openSettings } from "./settings/sections";
import { openModelServer } from "./settings/model-server-section";
import {
  agentChoices, ago, canStart, catalogLine, downUse, footText, hostText, machineNames, pill, projectOf, sessionSummary, sinceText, stateTone, summary, tagFor,
  usedBy, variantText,
} from "./model-servers";
import "../styles/model-servers.css";
import { TextInput } from "../lib/TextField";

// Task 67: Qt's ModelServersPage. Every server of this computer with its API
// address (copied in one click), SSH host, free memory and models: their
// state, New session…, Logs, Up and Down… when the server has control
// commands. A server may be shared, so every Down and Down all asks first and
// says so (Cancel focused). Nothing retries on its own.

function editServers(server: string) {
  openModelServer(server);
  openSettings("modelservers");
}

function Modal(props: { id: string; title: string; wide?: boolean; onClose(): void; children: JSX.Element; foot: JSX.Element }) {
  let dialog!: HTMLDialogElement;
  const origin = document.activeElement instanceof HTMLElement ? document.activeElement : undefined;
  onMount(() => { dialog.showModal(); queueMicrotask(() => dialog.querySelector<HTMLElement>("[data-initial]")?.focus()); });
  onCleanup(() => { if (dialog.open) dialog.close(); if (origin?.isConnected) origin.focus(); });
  return <dialog ref={dialog} class="dlg ms-dlg" classList={{ wide: props.wide }} aria-labelledby={`${props.id}-title`} onCancel={(e) => { e.preventDefault(); props.onClose(); }}>
    <div class="dlg-head"><h2 id={`${props.id}-title`}>{props.title}</h2><span class="grow" />
      <button type="button" class="icon-btn" aria-label={tr("Close")} onClick={props.onClose}><Icon name="x" size={16} /></button></div>
    <div class="dlg-body">{props.children}</div>
    <div class="dlg-foot"><span class="spacer" />{props.foot}</div>
  </dialog>;
}

/** Down / Down all: which of your sessions use each model, then Cancel (focused) or Down. */
function DownDialog(props: { server: ModelServer; models: string[]; onClose(): void }) {
  const entry = () => store.entry(props.server.name);
  const state = (model: string) => entry().status?.models.find((m) => m.name === model)?.state ?? "";
  const label = (model: string) => entry().status?.models.find((m) => m.name === model)?.label;
  const [by, setBy] = createSignal<Record<string, string[]>>();
  const [why, setWhy] = createSignal<string>();
  void store.sessions(props.server.name).then(setBy, (error) => setWhy(words(error)));
  const one = props.models.length === 1;
  const confirm = () => { props.onClose(); void store.down(props.server.name, props.models); };
  return <Modal id="ms-down" title={one ? tr("Down %1", props.models[0]) : tr("Down all on %1", props.server.name)} onClose={props.onClose}
    foot={<>
      <button type="button" class="btn" data-initial onClick={props.onClose}>{tr("Cancel")}</button>
      <button type="button" class="btn danger" onClick={confirm}><Icon name="power" size={13} />{one ? tr("Down") : tr("Down %1 models", props.models.length)}</button>
    </>}>
    <h3 class="ms-down-title">{one ? tr("Down %1 on %2?", props.models[0], props.server.name) : tr("Down all on %1?", props.server.name)}</h3>
    <p class="ms-shared">{tr("%1 may be shared. Other people's sessions on %2 stop getting answers too, and Zerus cannot see them.", props.server.name, one ? tr("this model") : tr("these models"))}</p>
    <For each={props.models}>{(model) => (
      <div class="ms-down-row">
        <span class="mono ms-down-model">{model}</span>
        <span class={`ms-state tone-${stateTone(state(model))}`}>{label(model) ? trText(label(model)!) : ""}</span>
        <span class="muted grow" title={(by()?.[model] ?? []).join("\n")}>{downUse(model, state(model), by(), why())}</span>
      </div>
    )}</For>
    <p class="hint">{props.server.ssh
      ? tr("Runs your down command over SSH to %1 once for each model. Nothing starts again on its own: Up them here when you need them.", props.server.ssh)
      : tr("Runs your down command on this computer once for each model. Nothing starts again on its own: Up them here when you need them.")}</p>
  </Modal>;
}

/** Logs: read once, Read again. */
function LogsDialog(props: { server: string; model: string; onClose(): void }) {
  const [logs, setLogs] = createSignal<ModelServerLogs>();
  const [state, setState] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  let text!: HTMLPreElement;
  const read = async () => {
    setBusy(true);
    try {
      const read = await store.logs(props.server, props.model);
      setLogs(read);
      setState(read.truncated ? tr("The last %1 lines", read.lines.length) : tr("%1 lines", read.lines.length));
      queueMicrotask(() => { text.scrollTop = text.scrollHeight; });
    } catch (error) { setState(tr("Not read: %1", words(error))); }
    finally { setBusy(false); }
  };
  void read();
  const title = props.model ? tr("Logs of %1 on %2", props.model, props.server) : tr("Logs of %1", props.server);
  return <Modal id="ms-logs" title={title} wide onClose={props.onClose}
    foot={<>
      <button type="button" class="btn" disabled={busy()} onClick={() => void read()}>{tr("Read again")}</button>
      <button type="button" class="btn" data-initial onClick={props.onClose}>{tr("Close")}</button>
    </>}>
    <pre ref={text} class="ms-logs mono selectable">{logs() ? logs()!.lines.join("\n") : tr("Reading the logs once…")}</pre>
    <p class="muted">{state()}</p>
  </Modal>;
}

/** New session on a model server: model, agent, folder and name; nothing else changes. */
function SessionDialog(props: { server: ModelServer; model: string; onClose(): void }) {
  const status = () => store.entry(props.server.name).status;
  const models = () => {
    const rows = status()?.models ?? [];
    return rows.some((m) => m.name === props.model) ? rows : [...rows, { name: props.model, state: "", label: { source: "" }, since: 0, usable: true, can_down: false, can_up: false }];
  };
  const [model, setModel] = createSignal(props.model);
  const row = () => models().find((m) => m.name === model());
  const agents = () => agentChoices(props.server, row());
  const [agent, setAgent] = createSignal<string>(agents().find((a) => a.ok)?.id ?? "");
  const local = () => { const s = selected(); return s && s.computer === "" && s.cwdPath ? [s.cwdPath] : []; };
  const host = () => machineCards[""]?.host ?? "";
  const folders = createMemo(() => {
    const out = [...local()];
    for (const project of projectsPage().projects) for (const folder of project.folders) if ((folder.computer === "" || folder.machine === host()) && !out.includes(folder.path)) out.push(folder.path);
    return out;
  });
  void loadProjects();
  const [folder, setFolder] = createSignal(folders()[0] ?? "");
  const [tag, setTag] = createSignal(tagFor(props.model));
  const [tagEdited, setTagEdited] = createSignal(false);
  const [browsing, setBrowsing] = createSignal(false);
  const state = () => models().find((m) => m.name === model())?.state;
  const choose = (name: string) => {
    setModel(name);
    if (!tagEdited()) setTag(tagFor(name));
    // A service without the chosen agent's wire picks the other agent.
    if (!agents().find((a) => a.id === agent())?.ok) setAgent(agents().find((a) => a.ok)?.id ?? "");
  };
  const name = () => (agent() === "codex" ? tr("Codex") : tr("Claude Code"));
  const start = () => {
    if (!canStart(agent(), model(), state(), folder(), tag())) return;
    props.onClose();
    void store.startSession(agent(), folder().trim(), tag().trim(), props.server.name, model());
  };
  return <>
    <Modal id="ms-session" title={tr("New session on %1", props.server.name)} wide onClose={props.onClose}
      foot={<>
        <button type="button" class="btn" onClick={props.onClose}>{tr("Cancel")}</button>
        <button type="button" class="btn primary" data-initial disabled={!canStart(agent(), model(), state(), folder(), tag())} onClick={start}>{tr("Start session")}</button>
      </>}>
      <div class="ms-grid">
        <label for="ms-model">{tr("Model")}</label>
        <select id="ms-model" class="field mono" value={model()} onChange={(e) => choose(e.currentTarget.value)}>
          <For each={models()}>{(m) => <option value={m.name} disabled={!!m.state && !m.usable}>{m.label.source ? `${m.name}   ${trText(m.label)}` : m.name}</option>}</For>
        </select>
        <span>{tr("Agent")}</span>
        <div class="ms-agents" role="radiogroup" aria-label={tr("Agent")}>
          <For each={agents()}>{(choice) => (
            <label class="ms-agent" classList={{ off: !choice.ok, on: agent() === choice.id }}>
              <input type="radio" name="ms-agent" value={choice.id} checked={agent() === choice.id} disabled={!choice.ok} onChange={() => setAgent(choice.id)} />
              <b>{choice.name}</b><span class="muted">{choice.how}</span>
            </label>
          )}</For>
        </div>
        <label for="ms-folder">{tr("Folder")}</label>
        <div class="row">
          <TextInput id="ms-folder" class="field mono grow" list="ms-folders" value={folder()} onInput={(e) => setFolder(e.currentTarget.value)} />
          <datalist id="ms-folders"><For each={folders()}>{(f) => <option value={f} />}</For></datalist>
          <button type="button" class="btn" onClick={() => setBrowsing(true)}>{tr("Browse…")}</button>
        </div>
        <label for="ms-tag">{tr("Name")}</label>
        <div class="row"><span class="mono muted">{`${agent()}/${projectOf(folder())}/`}</span>
          <TextInput id="ms-tag" class="field mono grow" maxLength={60} value={tag()} onInput={(e) => { setTag(e.currentTarget.value); setTagEdited(true); }} /></div>
        <span>{tr("For this session")}</span>
        <pre class="ms-summary mono">{sessionSummary(agent(), props.server, model(), row())}</pre>
      </div>
      <p class="muted">{tr("Your %1 configuration is not changed: the provider is set for this session only. If %2 stops, the session waits; Zerus does not start the model again on its own.", name(), model())}</p>
    </Modal>
    <Show when={browsing()}>
      <FolderBrowserDialog computer="" start={folder()} calls={{ list: (p, hidden) => bridge.listDirectories("", p, hidden), create: (p) => bridge.createDirectory("", p) }}
        onChoose={setFolder} onClose={() => setBrowsing(false)} />
    </Show>
  </>;
}

/** model-catalog/2: a running service restarts in its new variant; its sessions lose answers meanwhile. */
function VariantDialog(props: { server: ModelServer; model: string; variant: string; onClose(): void }) {
  const [by, setBy] = createSignal<Record<string, string[]>>();
  const [why, setWhy] = createSignal<string>();
  void store.sessions(props.server.name).then(setBy, (error) => setWhy(words(error)));
  const confirm = () => { props.onClose(); void store.variant(props.server.name, props.model, props.variant, true); };
  return <Modal id="ms-variant" title={tr("Restart %1 as %2", props.model, props.variant)} onClose={props.onClose}
    foot={<>
      <button type="button" class="btn" data-initial onClick={props.onClose}>{tr("Cancel")}</button>
      <button type="button" class="btn danger" onClick={confirm}><Icon name="rotate-ccw" size={13} />{tr("Restart")}</button>
    </>}>
    <h3 class="ms-down-title">{tr("Restart %1 on %2 as %3?", props.model, props.server.name, props.variant)}</h3>
    <p class="ms-shared">{tr("%1 is running: a new variant restarts it, which can take many minutes. Its sessions get no answers until it is ready, and %2 may be shared.", props.model, props.server.name)}</p>
    <p class="muted">{why() ? tr("Your sessions were not read: %1", why()!) : by() ? usedBy(by()![props.model] ?? []) : tr("Reading your sessions…")}</p>
  </Modal>;
}

function ServerCard(props: { server: ModelServer; entry: StatusEntry; ask(dialog: Dialog): void }) {
  const s = () => props.server;
  const st = () => props.entry.status;
  const failed = () => props.entry.known && !!st()?.error && !st()?.stale;
  const catalog = () => s().protocol === "model-catalog/2";
  const canLogs = () => s().logs.length > 0 || (catalog() && !!s().api_url);
  const canDown = () => s().down.length > 0 || (catalog() && !!s().api_url);
  // model-catalog/2: who uses each service, read with every status read.
  const [users, setUsers] = createSignal<Record<string, string[]>>({});
  createEffect(on(() => (catalog() ? props.entry.readAt : 0), (at) => {
    if (at) void store.sessions(s().name).then(setUsers, () => setUsers({}));
  }));
  const variantChange = (m: ModelServerModel, variant: string) => {
    if (!variant || variant === m.catalog?.variant) return;
    if (m.state === "running" || m.state === "loading") props.ask({ kind: "variant", server: s().name, model: m.name, variant });
    else void store.variant(s().name, m.name, variant, false);
  };
  const p = () => pill(s(), props.entry);
  const [copied, setCopied] = createSignal(false);
  const copy = () => { void bridge.copyVerbatim(s().agent_url).then(() => { setCopied(true); setTimeout(() => setCopied(false), 2000); }); };
  // Owner, 2026-11-09: a server card folds to its header; the choice is kept per server.
  const foldKey = () => `zerus.modelServers.folded.${s().name}`;
  const readFolded = () => { try { return localStorage.getItem(foldKey()) === "1"; } catch { return false; } };
  const [folded, setFolded] = createSignal(readFolded());
  const toggleFold = () => { const next = !folded(); setFolded(next); try { localStorage.setItem(foldKey(), next ? "1" : "0"); } catch { /* per-viewer convenience only */ } };
  return <section class="ms-card" classList={{ unreachable: failed(), folded: folded() }} data-server={s().name}>
    <header class="ms-head">
      <button type="button" class="icon-btn small ms-fold" aria-expanded={!folded()} title={folded() ? tr("Unfold %1", s().name) : tr("Fold %1", s().name)} onClick={toggleFold}>
        <Icon name={folded() ? "chevron-right" : "chevron-down"} size={14} /></button>
      <span class="ms-tile"><Icon name="hard-drive" size={14} /></span>
      <b class="ms-name">{s().name}</b>
      <span class={`ms-pill tone-${p().tone}`}>{p().text}</span>
      <span class="ms-host"><Icon name={s().ssh ? "terminal" : "monitor"} size={12} />{hostText(s(), machineNames())}</span>
      <span class="ms-api"><span class="ms-kind mono">{s().kind.toUpperCase()}</span><span class="mono selectable">{s().agent_url}</span>
        <button type="button" class="icon-btn small" title={tr("Copy the API address")} aria-label={tr("Copy the API address of %1", s().name)} onClick={copy}>
          <Icon name={copied() ? "check" : "copy"} size={13} />{copied() ? tr("Copied") : ""}</button></span>
      <Show when={!failed() && props.entry.known}>
        <Show when={st()?.memory.source} fallback={<span class="muted">{tr("memory not reported")}</span>}>
          <span class="ms-memory" classList={{ low: st()!.low }}><i style={{ width: `${Math.round(st()!.used * 100)}%` }} /></span>
          <span class="mono ms-memtext" classList={{ low: st()!.low }}>{trText(st()!.memory)}</span>
        </Show>
      </Show>
      <span class="grow" />
      <Show when={failed()} fallback={<>
        <Show when={s().logs.length}><button type="button" class="btn small ghost" onClick={() => props.ask({ kind: "logs", server: s().name, model: "" })}><Icon name="file-text" size={13} />{tr("Logs")}</button></Show>
        <Show when={canDown() && st()?.active.length}><button type="button" class="btn small danger" onClick={() => props.ask({ kind: "down", server: s().name, models: [...st()!.active] })}><Icon name="power" size={13} />{tr("Down all…")}</button></Show>
        <Show when={s().read_only}><button type="button" class="btn small ghost" onClick={() => store.refreshStatus(s().name)}><Icon name="refresh-cw" size={13} />{tr("Refresh")}</button></Show>
      </>}>
        <button type="button" class="btn small" onClick={() => store.refreshStatus(s().name)}><Icon name="refresh-cw" size={13} />{tr("Try again")}</button>
        <button type="button" class="btn small ghost" onClick={() => editServers(s().name)}><Icon name="settings" size={13} />{tr("Edit…")}</button>
      </Show>
    </header>
    <Show when={!folded()}>
    <Show when={!failed()} fallback={<p class="ms-problem selectable"><Icon name="triangle-alert" size={14} />{st() ? trText(st()!.problem) : ""}</p>}>
      <Show when={st()?.stale}>
        <p class="ms-problem selectable"><Icon name="triangle-alert" size={14} />{tr("%1: %2. This is the catalog read %3.", trText(st()!.error_label), st()!.detail, ago((st()!.fetched_at ?? 0) * 1000, Date.now()))}</p>
      </Show>
      <For each={st()?.models ?? []}>{(m) => (
        <div class="ms-model" data-model={m.name} data-state={m.state} classList={{ unmanaged: m.catalog?.managed === false }}>
          <Icon name="box" size={12} />
          <b class="mono ms-model-name">{m.name}</b>
          <span class={`ms-state tone-${stateTone(m.state)}`}><span class="dot" aria-hidden="true" />{trText(m.label)}</span>
          <Show when={st()!.source === "api"} fallback={<Show when={m.since > 0}><span class="muted">{sinceText(m.since)}</span></Show>}><span class="muted">{tr("from GET /v1/models")}</span></Show>
          <Show when={m.catalog}>{(facts) => <>
            <Show when={facts().managed} fallback={<span class="muted">{tr("runs outside the server's control")}</span>}>
              <Show when={facts().can_variant && facts().variants.length > 1} fallback={<Show when={facts().variant}><span class="mono muted">{facts().variant}</span></Show>}>
                <select class="field small mono ms-variant" aria-label={tr("Quality of %1", m.name)} title={tr("Quality: the engine and context the server runs %1 with", m.name)}
                  value={facts().variant} onChange={(e) => { const next = e.currentTarget.value; e.currentTarget.value = facts().variant; variantChange(m, next); }}>
                  <For each={facts().variants}>{(v) => <option value={v.id}>{variantText(v)}</option>}</For>
                  <option value="auto">{tr("auto: the server chooses")}</option>
                </select>
              </Show>
              <span class="muted ms-facts">{catalogLine(facts())}</span>
              <Show when={(users()[m.name] ?? []).length}><span class="muted" title={(users()[m.name] ?? []).join("\n")}>{usedBy(users()[m.name] ?? [])}</span></Show>
            </Show>
          </>}</Show>
          <span class="grow" />
          <Show when={m.usable}><button type="button" class="btn small" onClick={() => props.ask({ kind: "session", server: s().name, model: m.name })}><Icon name="plus" size={13} />{tr("New session…")}</button></Show>
          <Show when={canLogs() && m.catalog?.managed !== false}><button type="button" class="btn small ghost" onClick={() => props.ask({ kind: "logs", server: s().name, model: m.name })}><Icon name="file-text" size={13} />{tr("Logs")}</button></Show>
          <Show when={m.can_down}><button type="button" class="btn small danger" onClick={() => props.ask({ kind: "down", server: s().name, models: [m.name] })}><Icon name="power" size={13} />{tr("Down…")}</button></Show>
          <Show when={m.can_up}><button type="button" class="btn small" disabled={store.upRunning(s().name, m.name)} onClick={() => store.up(s().name, m.name)}><Icon name="power" size={13} />{store.upRunning(s().name, m.name) ? tr("Up running") : tr("Up")}</button></Show>
        </div>
      )}</For>
      <Show when={st()?.raw.length}><pre class="ms-raw mono selectable">{st()!.raw.join("\n")}</pre></Show>
      <footer class="ms-foot">
        <Icon name={s().read_only ? "eye" : "terminal"} size={12} />
        <span class="muted grow">{footText(s())}</span>
        <Show when={props.entry.known}><span class="muted">{props.entry.reading ? tr("reading") : ago(props.entry.readAt, Date.now())}</span></Show>
        <Show when={!s().read_only}><button type="button" class="btn small ghost" disabled={props.entry.reading} onClick={() => store.refreshStatus(s().name)}><Icon name="refresh-cw" size={13} />{tr("Refresh")}</button></Show>
      </footer>
    </Show>
    </Show>
  </section>;
}

type Dialog = { kind: "down"; server: string; models: string[] } | { kind: "logs"; server: string; model: string } | { kind: "session"; server: string; model: string }
  | { kind: "variant"; server: string; model: string; variant: string };

export function ModelServersScreen() {
  const [dialog, setDialog] = createSignal<Dialog>();
  onMount(() => { store.setActive(true); void store.refreshList(); });
  onCleanup(() => store.setActive(false));
  const count = () => store.servers().length;
  const server = (name: string) => store.server(name);
  return <div class="ms-page">
    <header class="ms-header">
      <span class="ms-tile"><Icon name="hard-drive" size={16} /></span>
      <h1>{tr("Model servers")}</h1>
      <span class="muted grow">{summary(store.servers(), store.statuses())}</span>
      <Show when={count() > 0}>
        <button type="button" class="btn small ghost" title={tr("Read every server once")} onClick={() => store.refreshAll()}><Icon name="refresh-cw" size={13} />{tr("Refresh all")}</button>
        <button type="button" class="btn small ghost" onClick={() => editServers("")}><Icon name="settings" size={13} />{tr("Edit servers")}</button>
      </Show>
    </header>
    <Show when={store.notice()}>{(n) => <p class="ms-notice" classList={{ error: n().error }} role="status">{n().text}</p>}</Show>
    <div class="ms-list">
      <Show when={count() > 0} fallback={
        <div class="ms-empty">
          <Icon name="hard-drive" size={28} />
          <b>{store.listed() ? tr("No model servers on this computer") : tr("Reading the model servers")}</b>
          <p class="muted">{tr("Add a computer on your network, or this one, that serves models through an OpenAI- or Anthropic-compatible API.")}</p>
          <button type="button" class="btn" onClick={() => editServers("")}><Icon name="plus" size={13} />{tr("Add server")}</button>
        </div>}>
        <For each={store.servers()}>{(s) => <ServerCard server={s} entry={store.entry(s.name)} ask={setDialog} />}</For>
      </Show>
    </div>
    <Show when={dialog()}>{(d) => {
      const target = server(d().server);
      if (!target) return null;
      const close = () => setDialog(undefined);
      const shown = d();
      return shown.kind === "down" ? <DownDialog server={target} models={shown.models} onClose={close} />
        : shown.kind === "logs" ? <LogsDialog server={shown.server} model={shown.model} onClose={close} />
        : shown.kind === "variant" ? <VariantDialog server={target} model={shown.model} variant={shown.variant} onClose={close} />
        : <SessionDialog server={target} model={shown.model} onClose={close} />;
    }}</Show>
  </div>;
}
