import { For, Show, createEffect, createSignal, on, onMount, type JSX } from "solid-js";
import { tr } from "../../../i18n";
import { trText } from "../../../text";
import { Icon } from "../../lib/icons";
import type { ModelServer, ModelServerForm, ModelServerTest } from "../../../bridge";
import { modelServers as store, words } from "../../../store/modelServers";
import { formOf, keyMode, listNote, machineNames, sshMode, urlHint, type SshMode } from "../model-servers";
import { NEW_SERVER, requestedServer } from "./model-server-section";
import "../../styles/model-servers.css";
import { TextInput } from "../../lib/TextField";

// Qt's ModelServerSettings. Adds, edits, tests and removes this
// computer's model servers through hgs (save and test --json). A saved key is
// never shown again: Replace and Remove only. Test reads once and saves
// nothing; Up and Down never run from here (they belong to the tab).

type Result = { tone: string; text: string } | undefined;

export function ModelServersSection(): JSX.Element {
  // Task a section opened again shows the first server at once, never an
  // empty form; the read in onMount replaces it with the requested one.
  const initial = store.servers()[0];
  const [form, setForm] = createSignal<ModelServerForm>(formOf(initial));
  const [ssh, setSsh] = createSignal<SshMode>(sshMode(initial?.ssh ?? "", machineNames()));
  const [hasKey, setHasKey] = createSignal(!!initial?.has_key);
  const [busy, setBusy] = createSignal(false);
  const [saveError, setSaveError] = createSignal("");
  const [api, setApi] = createSignal<Result>();
  const [status, setStatus] = createSignal<Result>();
  const [raw, setRaw] = createSignal<string[]>([]);
  const [logs, setLogs] = createSignal<Result>();
  let nameField: HTMLInputElement | undefined;

  const set = (patch: Partial<ModelServerForm>) => setForm({ ...form(), ...patch });
  const load = (server: ModelServer | undefined) => {
    setForm(formOf(server)); setHasKey(!!server?.has_key);
    setSsh(sshMode(server?.ssh ?? "", machineNames()));
    setSaveError(""); setApi(undefined); setStatus(undefined); setRaw([]); setLogs(undefined);
  };
  const add = () => { load(undefined); queueMicrotask(() => nameField?.focus()); };
  const show = (name: string) => {
    const server = name === NEW_SERVER ? undefined : name ? store.server(name) : store.servers()[0];
    if (server) load(server); else add();
  };
  onMount(() => { void store.refreshList().then(() => show(requestedServer().name)); });
  createEffect(on(requestedServer, (r) => show(r.name), { defer: true }));

  const shown = (result: ModelServerTest): Result => ({ tone: result.tone, text: trText(result.text) });
  const sent = (): ModelServerForm => ({ ...form(), ssh: ssh() === "none" ? "" : form().ssh });
  const test = async (what: "api" | "status" | "logs") => {
    const target = what === "api" ? setApi : what === "status" ? setStatus : setLogs;
    target({ tone: "", text: tr("Testing once…") });
    try {
      if (what === "logs") {
        const model = store.entry(form().was).status?.models[0]?.name ?? "";
        target(shown(await store.testLogs(sent(), model)));
        return;
      }
      const result = await store.test(sent(), what);
      target(shown(result));
      if (what === "status") setRaw(result.raw);
    } catch (error) { target({ tone: "error", text: words(error) }); }
  };
  const save = async () => {
    setBusy(true); setSaveError("");
    const name = form().name.trim();
    try {
      await store.save(sent());
      load(store.server(name));
      store.refreshStatus(name);
    } catch (error) { setSaveError(tr("Not saved: %1", words(error))); }
    finally { setBusy(false); }
  };
  /** Delete asks first in its own dialog, Cancel focused. */
  const [deleting, setDeleting] = createSignal("");
  let deleteDialog: HTMLDialogElement | undefined;
  createEffect(() => { if (deleting() && deleteDialog && !deleteDialog.open) { deleteDialog.showModal(); queueMicrotask(() => deleteDialog?.querySelector<HTMLElement>("[data-initial]")?.focus()); } });
  const remove = () => { if (form().was) setDeleting(form().was); };
  const confirmRemove = async () => {
    const was = deleting();
    setDeleting(""); deleteDialog?.close();
    if (!was) return;
    setBusy(true);
    try { await store.remove(was); show(""); }
    catch (error) { setSaveError(tr("Not deleted: %1", words(error))); }
    finally { setBusy(false); }
  };
  const key = () => keyMode(hasKey(), form().key_change);

  return (
    <div class="set-section ms-settings">
      <Show when={deleting()}>
        <dialog ref={deleteDialog} class="dlg ms-dlg" aria-labelledby="ms-delete-title" onCancel={(e) => { e.preventDefault(); setDeleting(""); deleteDialog?.close(); }}>
          <div class="dlg-head"><h2 id="ms-delete-title">{tr("Delete %1?", deleting())}</h2></div>
          <div class="dlg-body"><p>{tr("Delete %1 from this computer's model servers? Its key is removed with it. Nothing on the server changes and no model stops.", deleting())}</p></div>
          <div class="dlg-foot"><span class="spacer" />
            <button type="button" class="btn" data-initial onClick={() => { setDeleting(""); deleteDialog?.close(); }}>{tr("Cancel")}</button>
            <button type="button" class="btn danger" onClick={() => void confirmRemove()}>{tr("Delete server")}</button>
          </div>
        </dialog>
      </Show>
      <div class="row"><h2 class="grow">{tr("Model servers")}</h2><button type="button" class="btn primary small" onClick={add}><Icon name="plus" size={13} />{tr("Add server")}</button></div>
      <p class="set-note">{tr("Computers on your network, or this one, that serve models through an OpenAI- or Anthropic-compatible API. Kept on this computer only.")}</p>
      <div class="ms-settings-body">
        <ul class="ms-server-list" role="listbox" aria-label={tr("Model servers")}>
          <For each={store.servers()}>{(server) => (
            <li role="option" aria-selected={form().was === server.name} classList={{ on: form().was === server.name }} onClick={() => load(server)}>
              <b>{server.name}</b><span class="muted"> {listNote(server, store.entry(server.name))}</span><br /><span class="mono muted">{server.base_url}</span>
            </li>
          )}</For>
        </ul>
        <form class="ms-form" onSubmit={(e) => { e.preventDefault(); void save(); }}>
          <div class="ms-field"><label for="msf-name">{tr("Name")}</label>
            <div class="ms-control"><TextInput id="msf-name" ref={nameField} class="field" placeholder={tr("lab")} value={form().name} onInput={(e) => set({ name: e.currentTarget.value })} /></div>
          </div>
          <div class="ms-field"><label for="msf-url">{tr("Server root")}</label>
            <div class="ms-control">
              <TextInput id="msf-url" class="field mono" placeholder="http://host.example:8000" value={form().base_url} onInput={(e) => set({ base_url: e.currentTarget.value })} />
              <div class="ms-seg" role="radiogroup" aria-label={tr("API kind")}>
                <button type="button" role="radio" aria-checked={form().kind !== "anthropic"} classList={{ on: form().kind !== "anthropic" }} onClick={() => set({ kind: "openai" })}>{tr("OpenAI")}</button>
                <button type="button" role="radio" aria-checked={form().kind === "anthropic"} classList={{ on: form().kind === "anthropic" }} onClick={() => set({ kind: "anthropic" })}>{tr("Anthropic")}</button>
              </div>
              <button type="button" class="btn small" onClick={() => void test("api")}>{tr("Test API")}</button>
            </div>
          </div>
          <p class="ms-note">{urlHint(form().base_url)}</p>
          <Show when={api()}>{(r) => <p class={`ms-result ${r().tone}`} role="status">{r().text}</p>}</Show>
          <div class="ms-field"><label for="msf-key">{tr("API key")}</label>
            <div class="ms-control">
              <Show when={key().stored}><span class="muted"><Icon name="key-round" size={12} /> {key().text}</span></Show>
              <Show when={key().field}><TextInput id="msf-key" type="password" class="field" placeholder={tr("none")} value={form().key} onInput={(e) => set({ key: e.currentTarget.value })} /></Show>
              <Show when={key().actions}>
                <button type="button" class="btn small" onClick={() => set({ key_change: "replace" })}>{tr("Replace")}</button>
                <button type="button" class="btn small danger" onClick={() => set({ key_change: "remove" })}>{tr("Remove")}</button>
              </Show>
            </div>
          </div>
          <div class="ms-field"><span>{tr("SSH host")}</span>
            <div class="ms-control">
              <div class="ms-seg" role="radiogroup" aria-label={tr("SSH host")}>
                <button type="button" role="radio" aria-checked={ssh() === "none"} classList={{ on: ssh() === "none" }} onClick={() => setSsh("none")}>{tr("None")}</button>
                <button type="button" role="radio" aria-checked={ssh() === "machine"} classList={{ on: ssh() === "machine" }} onClick={() => { setSsh("machine"); set({ ssh: machineNames().includes(form().ssh) ? form().ssh : machineNames()[0] ?? "" }); }}>{tr("Machine")}</button>
                <button type="button" role="radio" aria-checked={ssh() === "alias"} classList={{ on: ssh() === "alias" }} onClick={() => { setSsh("alias"); set({ ssh: machineNames().includes(form().ssh) ? "" : form().ssh }); }}>{tr("SSH alias")}</button>
              </div>
              <Show when={ssh() === "machine"}>
                <select class="field" aria-label={tr("Machine")} value={form().ssh} onChange={(e) => set({ ssh: e.currentTarget.value })}>
                  <For each={machineNames()}>{(alias) => <option value={alias}>{alias}</option>}</For>
                </select>
              </Show>
              <Show when={ssh() === "alias"}>
                <TextInput class="field" aria-label={tr("SSH alias")} placeholder={tr("an alias from your SSH configuration")} value={form().ssh} onInput={(e) => set({ ssh: e.currentTarget.value })} />
              </Show>
            </div>
          </div>
          <p class="ms-note">{tr("Reached through hgs's own SSH transport (BatchMode); hgs is not needed on the server.")}</p>
          <p class="ms-control-title"><b>{tr("CONTROL COMMANDS")}</b> {tr("optional, argv templates; {model} is the model, {server} the server")}</p>
          <div class="ms-field"><label for="msf-status">{tr("Status")}</label>
            <div class="ms-control">
              <TextInput id="msf-status" class="field mono" placeholder={tr("not set")} value={form().status} onInput={(e) => set({ status: e.currentTarget.value })} />
              <button type="button" class="btn small" onClick={() => void test("status")}><Icon name="play" size={12} />{tr("Test")}</button>
            </div>
          </div>
          <Show when={status()}>{(r) => <p class={`ms-result ${r().tone}`} role="status">{r().text}</p>}</Show>
          <Show when={raw().length}><pre class="ms-raw mono">{raw().join("\n")}</pre></Show>
          <div class="ms-field"><label for="msf-up">{tr("Up NAME")}</label>
            <div class="ms-control"><TextInput id="msf-up" class="field mono" placeholder={tr("not set")} value={form().up} onInput={(e) => set({ up: e.currentTarget.value })} /></div>
          </div>
          <div class="ms-field"><label for="msf-down">{tr("Down NAME")}</label>
            <div class="ms-control"><TextInput id="msf-down" class="field mono" placeholder={tr("not set")} value={form().down} onInput={(e) => set({ down: e.currentTarget.value })} /></div>
          </div>
          <p class="ms-note">{tr("Up and Down run from the Model servers tab, after its confirmations: the server may be shared.")}</p>
          <div class="ms-field"><label for="msf-logs">{tr("Logs NAME")}</label>
            <div class="ms-control">
              <TextInput id="msf-logs" class="field mono" placeholder={tr("not set")} value={form().logs} onInput={(e) => set({ logs: e.currentTarget.value })} />
              <button type="button" class="btn small" onClick={() => void test("logs")}><Icon name="play" size={12} />{tr("Test")}</button>
            </div>
          </div>
          <Show when={logs()}>{(r) => <p class={`ms-result ${r().tone}`} role="status">{r().text}</p>}</Show>
          <pre class="ms-formats mono">{`{"models":[{"name":"small","state":"running"}],"memory":{"free":12.5,"total":64}}\n${tr("or plain lines:  small: running   large: stopped   memory: 12.5/64 GB")}`}</pre>
          <Show when={saveError()}><p class="ms-result error" role="alert">{saveError()}</p></Show>
          <div class="ms-buttons">
            <Show when={form().was}><button type="button" class="btn danger small" disabled={busy()} onClick={() => void remove()}><Icon name="trash" size={13} />{tr("Delete server…")}</button></Show>
            <span class="grow" />
            <button type="button" class="btn small" disabled={busy()} onClick={() => show(form().was)}>{tr("Cancel")}</button>
            <button type="submit" class="btn primary small" disabled={busy()}>{tr("Save")}</button>
          </div>
        </form>
      </div>
    </div>
  );
}
