import { Index, Show, createEffect, createMemo, createSignal, lazy, on, type JSX } from "solid-js";
import { Icon } from "../lib/icons";
import { OS_NAME } from "../lib/os-icons";
import { MachineTile, type MachineState } from "../components/MachineTile";
import { MachineLabel } from "../components/MachineLabel";
import { Actions, Lines, Spinner, Status } from "../components/Status";
import { paletteIndex } from "../sessions/SessionCard";
const MachineColorDialog = lazy(() => import("./MachineColorDialog"));
const MachineSetupDialog = lazy(() => import("./MachineSetupDialog"));
const MachineConnectionDialog = lazy(() => import("./MachineConnectionDialog"));
import { machineEditor as editor, NEW_MACHINE } from "../../store/machineEditor";
import { tr, trNoop } from "../../i18n";
import type { MachineMeta, MachineRow } from "../../types";
import { catalog } from "../../store/catalog";
import { machineCards, machinesPage, requestedMachine, resolveMachine, setRequestedMachine, testConnection } from "../../store/machines";
import { setAccountMachine } from "../../store/accounts";
import { connectionOf, osOf } from "../../store/overview";
import { machineAppearance } from "../../store/machineAppearance";
import { setSetting, settings, settingsLoaded } from "../../store/settings";
import { setMachineFilter, setPage } from "../../store/ui";
import { machineSetupEditor as setup } from "../../store/machineSetup";
import { LOCAL, localState, peerState, placeholders, summary, testEnabled, type LocalState, type PeerState } from "./machines-ui";

// Machines (MachinesPage.cpp), ported from design/webui/src/screens/Machines.tsx:
// this computer and the saved peers of `hgs machine ls`, each with its stream
// state, and an editor with the Zerus overrides and the effective SSH
// connection. Connection writes and terminal launches keep the CLI revision
// guard; setup and local enablement require their own explicit confirmation.

const selected = editor.selected;
const busy = () => editor.busy() || setup.busy();
const setSelected: typeof editor.choose = (...args) => { if (!setup.busy()) editor.choose(...args); };

function MetaLine(props: { meta: MachineMeta }) {
  return (
    <span class="mach-meta tabular parts">
      <Show when={osOf(props.meta.platform)}>{(os) => <span>{OS_NAME[os()]}</span>}</Show>
      <Show when={props.meta.hgs_version}><span>hgs {props.meta.hgs_version}</span></Show>
    </span>
  );
}

function MachineCard(props: { selection: string; alias: string; line: string; tile: MachineState; meta: MachineMeta | undefined; showMeta: boolean; offline: boolean }) {
  const on = () => selected() === props.selection;
  return (
    <button class="mach-card" classList={{ on: on(), offline: props.offline }} aria-current={on()} disabled={busy()} onClick={() => setSelected(props.selection)}>
      <MachineTile os={osOf(props.meta?.platform)} state={props.tile} />
      <span class="mach-col">
        <span class="row"><MachineLabel alias={props.alias} /></span>
        <span class="mach-state">{props.line}</span>
        <Show when={props.showMeta && props.meta}>{(m) => <MetaLine meta={m()} />}</Show>
      </span>
    </button>
  );
}

function Field(props: { label: string; id: string; children: JSX.Element }) {
  return (
    <div class="form-row mach-form-row">
      <label for={props.id}>{tr(props.label)}</label>
      <div class="col" style={{ gap: "4px" }}>{props.children}</div>
    </div>
  );
}

function Editor(props: { local: LocalState; peers: { row: MachineRow; state: PeerState }[] }) {
  const view = machinesPage;
  const isLocal = () => selected() === LOCAL;
  const peer = () => props.peers.find((p) => p.row.alias === selected());
  const row = () => peer()?.row;
  const name = () => (isLocal() ? props.local.name : editor.isNew() ? tr("New machine") : selected());
  const draft = editor.draft;
  const fieldsDisabled = () => isLocal() || busy();
  const effective = () => (view().effective?.alias === selected() ? view().effective : null);
  const check = () => (view().check?.alias === selected() ? view().check : null);
  const setupResult = () => setup.result()?.target === selected() ? setup.result() : undefined;
  const setupCheck = () => setupResult()?.followup ? check() : null;
  const hint = () => placeholders(draft().dirty ? null : effective(), selected());
  const formHidden = () => isLocal() && props.local.formHidden;
  const [colorEditor, setColorEditor] = createSignal<{ alias: string; origin: HTMLElement }>();
  const appearance = () => editor.isNew() ? { color: "var(--z-entity-palette-7)", vivid: false } : machineAppearance(settings, name(), `var(--z-entity-palette-${paletteIndex(name())})`);
  const appearanceAvailable = () => settingsLoaded() && !!name() && !editor.isNew();
  return (
    <div class="mach-editor">
      <Show when={setup.prompt()} keyed>{(prompt) => <MachineSetupDialog prompt={prompt} />}</Show>
      <Show when={editor.prompt()}>{(prompt) => <MachineConnectionDialog prompt={prompt()} close={editor.cancel} confirm={editor.confirm} />}</Show>
      <div class="mach-title">
        <h2>{editor.isNew() ? tr("New machine") : name()}</h2>
        <span class="grow" />
        <Show when={peer()}>{(p) => <span class="chip" classList={{ "mach-chip-ok": p().state.live }}>{p().state.line}</span>}</Show>
      </div>
      <p class="muted mach-origin">{isLocal() ? props.local.origin : row()?.origin ?? "SSH supplies inherited values from your OpenSSH configuration and defaults. Fields below are Zerus overrides; blank fields keep the inherited value."}</p>
      <Show when={isLocal() && props.local.sessionHost}><p class="mach-host selectable">{props.local.sessionHost}</p></Show>

      <section class="mach-section">
        <h3>{tr("Appearance")}</h3>
        <div class="row mach-appearance">
          <button class="btn" disabled={!appearanceAvailable()} onClick={(event) => setColorEditor({ alias: name(), origin: event.currentTarget })}>
            <span class="mach-swatch" style={{ "--entity": appearance().color }} />{tr("Label color…")}
          </button>
          <select class="field" aria-label={tr("Label style")} disabled={!appearanceAvailable()} value={appearance().vivid ? "bright" : "soft"}
            onChange={(event) => void setSetting(`machines/vivid/${name()}`, event.currentTarget.value === "bright")}>
            <option value="soft">{tr("Soft")}</option><option value="bright">{tr("Bright")}</option>
          </select>
          <button class="btn ghost" disabled={!appearanceAvailable()} title={tr("Use the default color for this machine")} onClick={() => void setSetting(`machines/colors/${name()}`, null)}>{tr("Automatic")}</button>
        </div>
        <div class="row mach-preview">
          <span class="dim">{tr("Preview")}</span>
          <Show when={!editor.isNew()} fallback={<span class="entity mach-label" style={{ "--entity": "var(--z-entity-palette-7)" }}>"new-machine"</span>}><MachineLabel alias={name()} /></Show>
          <span class="dim">{tr(editor.isNew() ? "Save the machine to choose its color" : "Saved on this device")}</span>
        </div>
        <Show when={colorEditor()}>{(editor) => <MachineColorDialog alias={editor().alias} origin={editor().origin} close={() => setColorEditor(undefined)} />}</Show>
      </section>

      <Show when={!formHidden()}>
        <section class="mach-section">
          <h3>{tr("Connection")}</h3>
          <Show when={!isLocal() && effective()}>{(e) => (
            <Show when={!e().pending} fallback={<Status kind="busy">{e().text}</Status>}>
              <Show when={!e().error} fallback={<Status kind="error"><Lines text={e().text} /></Status>}>
                <div class="mach-effective selectable"><Lines text={draft().dirty ? e().text.replace("Effective connection:", "Saved connection:") + "\nUnsaved edits are not included. Save to refresh these values." : e().text} /></div>
              </Show>
            </Show>
          )}</Show>
          <Field label={trNoop("Name / SSH alias")} id="mach-alias">
            <input id="mach-alias" class="field" value={isLocal() ? props.local.name : draft().alias} placeholder={tr("e.g. studio-mac")} readOnly={!editor.isNew()} disabled={fieldsDisabled()} aria-invalid={editor.isNew() && !!draft().alias && (!editor.valid() || editor.duplicate())} onInput={(e) => editor.edit("alias", e.currentTarget.value)} />
            <Show when={editor.duplicate()}><span class="danger hint">{tr("This name already exists. Select the machine in the list to edit it.")}</span></Show>
          </Field>
          <Field label={trNoop("Hostname / IP")} id="mach-host">
            <input id="mach-host" class="field" placeholder={hint().hostname} value={draft().hostname} disabled={fieldsDisabled()} onInput={(e) => editor.edit("hostname", e.currentTarget.value)} />
          </Field>
          <Field label={trNoop("SSH user")} id="mach-user">
            <input id="mach-user" class="field" placeholder={hint().user} value={draft().user} disabled={fieldsDisabled()} onInput={(e) => editor.edit("user", e.currentTarget.value)} />
          </Field>
          <Field label={trNoop("Port")} id="mach-port">
            <input id="mach-port" class="field tabular mach-port" placeholder={hint().port} type="number" min={0} max={65535} value={draft().port} disabled={fieldsDisabled()} onInput={(e) => editor.edit("port", e.currentTarget.value)} />
          </Field>
          <Field label={trNoop("Private key path")} id="mach-key">
            <div class="row">
              <input id="mach-key" class="field mono grow" placeholder={hint().key} value={draft().identity_file} disabled={fieldsDisabled()} onInput={(e) => editor.edit("identity_file", e.currentTarget.value)} />
              <button class="btn" disabled={fieldsDisabled()} title={tr("Choose SSH private key")} onClick={() => void editor.pickKey()}>{tr("Browse…")}</button>
            </div>
          </Field>
          <div class="form-row mach-form-row">
            <span />
            <label class="check">
              <input type="checkbox" checked={isLocal() || draft().enabled} disabled={fieldsDisabled()} onChange={(e) => editor.edit("enabled", e.currentTarget.checked)} />{tr("Show sessions and include in background polling")}
            </label>
          </div>
          <Actions>
            <button class="btn primary" disabled={fieldsDisabled() || !view().loaded || !draft().revision || !editor.valid() || editor.duplicate() || !(draft().dirty || editor.isNew())} onClick={() => void editor.save()}>{tr("Save connection")}</button>
            <Show when={editor.isNew()}><button class="btn" disabled={busy()} onClick={() => editor.choose(LOCAL)}>{tr("Discard draft")}</button></Show>
            <button class="btn" disabled={busy() || draft().dirty || editor.isNew() || !testEnabled(selected(), view())} onClick={() => void testConnection(selected())}><Icon name="plug" size={13} />{tr("Test connection")}</button>
            <button class="btn" disabled={fieldsDisabled() || editor.isNew() || draft().dirty || !draft().revision} onClick={() => void editor.openSsh()}><Icon name="square-terminal" size={13} />{tr("Open SSH terminal")}</button>
          </Actions>
        </section>
      </Show>

      <section class="mach-section">
        <h3>{tr("Setup")}</h3>
        <Actions>
          <Show when={isLocal() && props.local.canEnable}>
            <button class="btn primary" disabled={busy() || !setup.canEnable()} title={tr("Run agents on this computer too. Zerus stages the HGS CLI and a session host in your user profile.")} onClick={() => setup.open("enable")}>{tr("Enable local sessions")}</button>
          </Show>
          <Show when={!formHidden()}>
            <button class="btn" disabled={busy() || isLocal() || editor.isNew() || draft().dirty} onClick={() => setup.open("install")}><Icon name="download" size={13} />{tr("Install / update HGS…")}</button>
          </Show>
          <Show when={!(isLocal() && props.local.console)}>
            <button class="btn" disabled={busy() || editor.isNew()} onClick={() => { setMachineFilter([name()]); setPage("sessions"); }}>
              <Icon name="rows-3" size={13} />{tr("View sessions")}
            </button>
            <button class="btn" disabled={busy() || editor.isNew()} onClick={() => { setAccountMachine(isLocal() ? "" : selected()); setPage("accounts"); }}>
              <Icon name="user-round-cog" size={13} />{tr("Agent accounts…")}
            </button>
          </Show>
        </Actions>
        <Show when={!editor.notice() && setupResult()}>{(r) => <Status kind={(setupCheck()?.error ?? r().error) ? "error" : setup.busy() || setupCheck()?.pending ? "busy" : "muted"}><Lines text={setupCheck()?.text ?? r().text} /></Status>}</Show>
        <Show when={editor.notice()}>{(n) => <Status kind={n().error ? "error" : "muted"}><Lines text={n().text} /></Status>}</Show>
        <Show when={!editor.notice() && !setupResult() && check()}>{(c) => <Status kind={c().pending ? "busy" : c().error ? "error" : "muted"}><Lines text={c().text} /></Status>}</Show>
        <Show when={setupResult()?.output}><pre class="mach-log" aria-label={tr("Setup output")}>{setupResult()!.output}</pre></Show>
      </section>

      <Show when={!formHidden()}>
        <div class="mach-foot">
          <button class="btn" disabled={fieldsDisabled() || editor.isNew() || draft().dirty || !(row()?.inherited && row()?.source === "local")} title={tr("Restore the SSH settings of a machine from the shared configuration.")} onClick={() => void editor.reset()}>{tr("Reset SSH overrides")}</button>
          <button class="btn danger" disabled={fieldsDisabled() || editor.isNew() || draft().dirty} onClick={editor.remove}><Icon name="trash" size={13} />{tr("Remove from Zerus")}</button>
        </div>
      </Show>
    </div>
  );
}

export function MachinesScreen() {
  const view = machinesPage;
  // Rows per computer from the row ids alone (computer + "\n" + key): one pass per batch.
  const sessions = createMemo(() => {
    const per: Record<string, number> = {};
    for (const id of catalog.order) {
      const computer = id.slice(0, id.indexOf("\n"));
      per[computer] = (per[computer] ?? 0) + 1;
    }
    return per;
  });
  const local = () => localState(machineCards[""], view().local);
  const peers = createMemo(() => view().machines.map((row) => ({ row, state: peerState(row, catalog.computers[row.alias], sessions()[row.alias] ?? 0) })));
  // Another screen asked for a machine (Overview's Machine settings).
  createEffect(() => {
    const alias = requestedMachine();
    if (alias === undefined) return;
    setSelected(alias);
    setRequestedMachine(undefined);
  });
  // Selection or a newly saved list revision refreshes inherited SSH values.
  // The primitive key stays stable while resolve replies update the view.
  const connectionSelection = createMemo(() => `${selected()}\n${view().connection_revision ?? ""}\n${view().loaded}`);
  createEffect(on(connectionSelection, () => {
    const key = selected();
    if (view().loaded && key !== LOCAL && key !== NEW_MACHINE) void resolveMachine(key);
  }));
  return (
    <div class="mach-page">
      <div class="mach-shell">
        <header class="mach-head">
          <h1>{tr("Machines")}</h1>
          <button class="icon-btn" title={tr("Refresh machines")} aria-label={tr("Refresh machines")} disabled={busy()} onClick={() => editor.choose(selected(), true)}>
            <Show when={!view().refreshing} fallback={<Spinner />}><Icon name="refresh-cw" size={15} /></Show>
          </button>
          <span class="grow" />
          <button class="btn primary" disabled={busy()} onClick={() => { editor.choose(NEW_MACHINE); if (editor.isNew() && !editor.prompt()) queueMicrotask(() => document.getElementById("mach-alias")?.focus()); }}><Icon name="plus" size={14} />{tr("Add machine")}</button>
        </header>
        <p class="muted mach-summary">{summary({ console: local().console, live: ["live", "polled"].includes(catalog.computers[""]?.status ?? "") }, peers().map((p) => p.state))}</p>
        <Show when={view().error}>{(error) => <Status kind="error"><Lines text={error()} /></Status>}</Show>
        <div class="mach-body">
          <div class="mach-list" role="list" aria-label={tr("Machines")}>
            <MachineCard selection={LOCAL} alias={local().name} line={local().line} tile={connectionOf(catalog.computers[""]?.status).tile} meta={machineCards[""]} showMeta={!local().console} offline={false} />
            <Index each={peers()}>{(p) => <MachineCard selection={p().row.alias} alias={p().row.alias} line={p().state.line} tile={p().state.tile} meta={machineCards[p().row.alias]} showMeta offline={!p().state.live} />}</Index>
            <Show when={editor.isNew()}><button class="mach-card draft on" aria-current title={tr("This connection is a draft. Save it to add the machine to Zerus.")}><span class="row"><span class="entity mach-label" style={{ "--entity": "var(--z-entity-palette-7)" }}>{tr("New machine")}</span></span><span class="mach-state">{tr("Unsaved connection")}</span></button></Show>
          </div>
          <Editor local={local()} peers={peers()} />
        </div>
      </div>
    </div>
  );
}
