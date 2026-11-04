import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { inTauri } from "../../../bridge";
import { flag, setSetting, text } from "../../../store/settings";
import { Icon } from "../../lib/icons";
import { Check, Group, Lines, SectionHead, Select, Status, type Option } from "./ui";
import { formValid, retype, type AdapterView, type EditorForm } from "./voice-form";
import { tr, trNoop } from "../../../i18n";
import { trText, type Text } from "../../../text";

// Settings > Voice input (tray/src/speech/SpeechSettings.cpp) as the approved
// mockup lays it out (design/webui/src/screens/settings/Voice.tsx). The
// providers live on a processing computer: the core's speech_settings runs
// every call, keeps the change under way and says what the page shows; this
// device keeps Preview before sending and the microphone.

interface ProfileRow { id: string; name: string; kind: string; model: string; readiness: string; reason: string }
interface EditorStart {
  name: string; id: string; adapter: string; model: string; language: string; device: string; base_url: string; allow_insecure_http: boolean;
}
interface EditorView {
  ticket: number; title: string; create: boolean; computer: string; readiness: string;
  adapters: AdapterView[]; unsupported: string | null; start: EditorStart; extra_model: string | null; extra_device: string | null;
  key_stored: boolean; status: string; busy: boolean; conflict: boolean; check_visible: boolean; check_enabled: boolean;
}
interface View {
  /** Grows with every view: an older one arriving late is dropped. */
  seq: number;
  hosts: Option[]; host: string; loaded: boolean; profiles: ProfileRow[]; selected: number | null; details: string;
  can_add: boolean; can_edit: boolean; can_remove: boolean; can_test: boolean; can_use_default: boolean;
  default_text: string; status: string; result: string; result_failed: boolean; can_refresh: boolean;
  pending: string; can_check: boolean; statistics: Text | null; editor: EditorView | null;
}
interface RemoveAsk { ticket: number; text: string }
interface Microphone { id: string; name: string; default: boolean }

const [view, setShown] = createSignal<View | undefined>();
/** Shows `next` unless a newer view already arrived (answers and events race). */
const setView = (next: View) => { if (next.seq > (view()?.seq ?? 0)) setShown(next); };
const [media, setMedia] = createSignal(false);
const [microphones, setMicrophones] = createSignal<Microphone[]>([]);

/** One action on the page; the answer is the page as the core now has it. */
async function input(kind: string, fields: Record<string, unknown> = {}): Promise<RemoveAsk | undefined> {
  if (!inTauri()) return undefined;
  try {
    const answer = await invoke<{ view: View; ask: RemoveAsk | null; media: boolean }>("speech_input", { input: { kind, ...fields } });
    setView(answer.view);
    setMedia(answer.media);
    return answer.ask ?? undefined;
  } catch (error) {
    console.warn(`voice input settings: ${String(error)}`);
    return undefined;
  }
}

function loadMicrophones() {
  if (!inTauri() || !media()) return;
  invoke<Microphone[]>("voice_microphones").then(setMicrophones, () => setMicrophones([]));
}

/** SpeechSettings::refresh: the providers, the statistics and the microphones, when the section opens. */
export function refreshVoice() {
  void input("refresh").then(loadMicrophones);
}

/** The documentation link; the text to show when no browser opened. */
async function openDocumentation(adapter: string): Promise<string> {
  if (!inTauri()) return "";
  try {
    await invoke("open_speech_documentation", { adapter });
    return "";
  } catch (error) {
    return String(error);
  }
}

const DOCUMENTATION = "https://github.com/ufna/zerus/blob/main/docs/speech-setup.md";

export function VoiceSection() {
  const [ask, setAsk] = createSignal<RemoveAsk | undefined>();
  const [documentationFailure, setDocumentationFailure] = createSignal("");
  onMount(() => {
    if (!inTauri()) return;
    let stop: (() => void) | undefined;
    let gone = false;
    // Shown again once listening: a change in between would otherwise wait
    // for the next one.
    listen<View>("speech-settings", (e) => setView(e.payload)).then((off) => { if (gone) off(); else { stop = off; void input("show"); } }, () => {});
    onCleanup(() => { gone = true; stop?.(); });
    void input("show");
  });
  const v = () => view();
  const microphone = () => text("speech/microphone");
  const microphoneOptions = createMemo<Option[]>(() => {
    const options = [{ value: "", label: tr("System default microphone") }, ...microphones().map((m) => ({ value: m.id, label: m.name }))];
    const selected = microphone();
    if (selected && !options.some((o) => o.value === selected)) options.push({ value: selected, label: tr("Selected microphone (unavailable)") });
    return options;
  });
  const removeAnswer = (confirmed: boolean) => {
    const asked = ask();
    setAsk(undefined);
    if (asked) void input("remove", { ticket: asked.ticket, confirmed });
  };
  return (
    <>
      <SectionHead title={trNoop("Voice input")} />
      <Group>
        <Check label={trNoop("Preview before sending")} checked={flag("speech/preview_before_send")} onChange={(on) => void setSetting("speech/preview_before_send", on)} />
        <p class="set-group-hint">{tr("Show Stop instead of Send while recording audio or video. Stop retains a preview for review and a separate Send. This preference applies on this device.")}</p>
      </Group>
      <Group title={trNoop("Recognition providers")}>
        <p class="set-group-hint">{tr("Manage recognition providers on a processing computer. Recording starts on this device; Send voice uses the provider selected beside your recording.")}</p>
        <div class="set-row">
          <div class="set-row-text"><label class="set-label" for="voice-host">{tr("Configure providers on")}</label></div>
          <div class="set-row-control">
            <Select id="voice-host" value={v()?.host ?? ""} options={v()?.hosts ?? []} onChange={(host) => void input("choose_host", { host })} />
          </div>
        </div>
        <div class="voice-list" role="listbox" aria-label={tr("Speech providers")} aria-disabled={!v()?.loaded}>
          <For each={v()?.profiles ?? []}>
            {(p, index) => (
              <button type="button" role="option" class="voice-item" classList={{ on: v()?.selected === index() }} aria-selected={v()?.selected === index()}
                title={p.reason || undefined} disabled={!v()?.loaded}
                onClick={() => void input("select", { index: index() })}
                onDblClick={() => { if (v()?.can_edit) void input("open_editor", { create: false }); }}>
                <Icon name={p.kind === "HTTP server" ? "network" : "cpu"} size={15} class="dim" />
                <span class="col grow voice-item-text">
                  <b class="ell">{p.name}</b>
                  <span class="muted parts"><span>{p.kind}</span><span>{p.model}</span></span>
                </span>
                <span class="voice-ready" classList={{ ok: p.readiness === "Ready" }}>{p.readiness}</span>
              </button>
            )}
          </For>
        </div>
        <Show when={v()?.details}>{(details) => <Lines class="muted" text={details()} />}</Show>
        <div class="set-actions">
          <button type="button" class="btn" disabled={!v()?.can_add} onClick={() => void input("open_editor", { create: true })}><Icon name="plus" size={13} />{tr("Add")}</button>
          <button type="button" class="btn" disabled={!v()?.can_edit} onClick={() => void input("open_editor", { create: false })}><Icon name="pencil" size={13} />{tr("Edit")}</button>
          <button type="button" class="btn" disabled={!v()?.can_test} onClick={() => void input("test")}><Icon name="play" size={13} />{tr("Test")}</button>
          <button type="button" class="btn danger" disabled={!v()?.can_remove} onClick={() => void input("ask_remove").then(setAsk)}><Icon name="trash" size={13} />{tr("Remove")}</button>
          <span class="grow" />
          <button type="button" class="btn" disabled={!v()?.can_use_default} onClick={() => void input("use_default")}><Icon name="star" size={13} />{tr("Use as default")}</button>
        </div>
        <p class="set-group-hint">{tr("Test sends a short synthetic recording from the selected computer. It may load a prepared model or use the remote API; it does not use your microphone or send anything to an agent.")}</p>
        <Show when={v()?.default_text}>{(text) => <Status>{text()}</Status>}</Show>
        <Show when={v()?.status}>{(text) => <Status kind={v()?.can_refresh ? "muted" : "busy"}>{text()}</Status>}</Show>
        <Show when={v()?.result}>{(text) => <Status kind={v()?.result_failed ? "error" : "ok"}>{text()}</Status>}</Show>
        <Show when={v()?.pending}>{(text) => <Status kind="warn">{text()}</Status>}</Show>
        <div class="set-actions">
          <button type="button" class="btn" title={DOCUMENTATION} onClick={() => void openDocumentation("").then(setDocumentationFailure)}>
            {tr("Speech provider documentation")}<Icon name="external-link" size={12} />
          </button>
          <button type="button" class="btn" disabled={v() !== undefined && !v()!.can_refresh} onClick={refreshVoice}>
            <Icon name="refresh-cw" size={13} />{tr("Refresh providers and microphones")}
          </button>
          <Show when={v()?.pending}>
            <button type="button" class="btn" disabled={!v()?.can_check} onClick={() => void input("check")}>{tr("Check operation status")}</button>
          </Show>
        </div>
        <Show when={documentationFailure()}>{(failure) => <Lines class="selectable dlg-error" text={failure()} />}</Show>
      </Group>
      <Group title={trNoop("This device")}>
        <Show when={media()}>
          <div class="set-row">
            <div class="set-row-text"><label class="set-label" for="voice-mic">{tr("Microphone on this device")}</label></div>
            <div class="set-row-control">
              <Select id="voice-mic" value={microphone()} options={microphoneOptions()} onChange={(id) => void setSetting("speech/microphone", id)} />
            </div>
          </div>
        </Show>
        <p class="set-group-hint">{tr("Provider settings and credentials stay on their processing computer. Recordings are limited to two minutes. Original audio and recognized text remain in conversation history; your typed draft stays separate.")}</p>
        <Show when={v()?.statistics}>{(text) => <Lines class="muted voice-stats" text={trText(text())} />}</Show>
      </Group>
      <Show when={v()?.editor}>{(editor) => <ProviderEditor editor={editor()} />}</Show>
      <Show when={ask()}>{(asked) => <RemoveQuestion text={asked().text} answer={removeAnswer} />}</Show>
    </>
  );
}

/** The remove confirmation: Remove provider, or Cancel (the default). */
function RemoveQuestion(props: { text: string; answer: (confirmed: boolean) => void }) {
  let dialog: HTMLDialogElement | undefined;
  const answer = (confirmed: boolean) => { if (dialog?.open) dialog.close(); props.answer(confirmed); };
  onCleanup(() => { if (dialog?.open) dialog.close(); });
  return (
    <dialog class="dlg" ref={(el) => { dialog = el; queueMicrotask(() => el.showModal()); }} aria-labelledby="voice-remove-title"
      onCancel={(e) => { e.preventDefault(); answer(false); }}>
      <div class="dlg-head"><h2 id="voice-remove-title">{tr("Remove speech provider?")}</h2></div>
      <div class="dlg-body">{props.text}</div>
      <div class="dlg-foot">
        <span class="spacer" />
        <button type="button" class="btn danger" onClick={() => answer(true)}>{tr("Remove provider")}</button>
        <button type="button" class="btn primary" autofocus onClick={() => answer(false)}>{tr("Cancel")}</button>
      </div>
    </dialog>
  );
}

const HELP: Record<string, string> = {
  openai_http: trNoop("The processing computer sends audio to the server's /audio/transcriptions endpoint. Use the server's model ID and a two-letter language code, or leave language empty.\n\nThe optional API key is saved on the processing computer. Prefer HTTPS; private HTTP requires explicit consent. Saving sends no audio."),
  gigaam: trNoop("Local inference runs on the processing computer named above. Saving creates a profile; it does not install the service or download model weights.\n\nv3_e2e_rnnt is the default for Russian with punctuation; use ru for Russian.\n\nCPU uses the processor. CUDA requires a supported NVIDIA GPU and CUDA-enabled PyTorch. MPS requires a supported Apple GPU/runtime. Use Test to verify the chosen model and device."),
};
const OTHER_HELP = trNoop("This provider type cannot be edited by this version of Zerus. See the provider documentation before changing it.");

/**
 * Add or Edit speech provider (SpeechSettings.cpp ProfileEditor). The fields
 * stay here until Save; the API key goes out with Save and is cleared once
 * the core takes the save (a refused one, such as a duplicate ID, keeps it).
 */
function ProviderEditor(props: { editor: EditorView }) {
  const start = props.editor.start;
  const [name, setName] = createSignal(start.name);
  const [id, setId] = createSignal(start.id);
  const [adapter, setAdapter] = createSignal(start.adapter);
  const [model, setModel] = createSignal(start.model);
  const [language, setLanguage] = createSignal(start.language);
  const [device, setDevice] = createSignal(start.device);
  const [url, setUrl] = createSignal(start.base_url);
  const [insecure, setInsecure] = createSignal(start.allow_insecure_http);
  const [credential, setCredential] = createSignal("keep");
  const [key, setKey] = createSignal("");
  const [documentationFailure, setDocumentationFailure] = createSignal("");
  // A saved model or device the computer no longer offers stays listed
  // until another type is chosen, as Qt's combo boxes keep it.
  const [retyped, setRetyped] = createSignal(false);
  const editor = () => props.editor;
  const remote = () => adapter() === "openai_http";
  const offered = () => editor().adapters.find((a) => a.id === adapter());
  const models = () => [...(offered()?.models ?? []), ...(!retyped() && editor().extra_model ? [editor().extra_model!] : [])];
  const devices = () => [...(offered()?.devices ?? []), ...(!retyped() && editor().extra_device ? [editor().extra_device!] : [])];
  const types = (): Option[] => [
    ...editor().adapters.map((a) => ({ value: a.id, label: a.label })),
    ...(editor().unsupported !== null ? [{ value: editor().unsupported!, label: tr("Unsupported provider type") }] : []),
  ];
  // ProfileEditor::configureType: the chosen type's models and devices.
  createEffect(on(adapter, (chosen) => {
    setDocumentationFailure("");
    if (!retyped()) return;
    const next = retype(chosen, editor().adapters, start);
    setModel(next.model);
    setDevice(next.device);
  }, { defer: true }));
  const form = (): EditorForm => ({
    name: name(), id: id(), adapter: adapter(), model: model(), language: language(), device: device(),
    base_url: url(), allow_insecure_http: insecure(), credential: credential(), key: key(),
  });
  const busy = () => editor().busy;
  const canSave = () => !busy() && !editor().conflict && formValid(form(), editor().adapters);
  const save = () => {
    if (!canSave()) return;
    // The key stays typed until the core takes the save (a duplicate ID
    // is refused first, as Qt checks it before clearing the key).
    void input("save", { ticket: editor().ticket, form: form() }).then(() => {
      if (view()?.editor?.busy) setKey("");
    });
  };
  const close = () => void input("close_editor", { ticket: editor().ticket });
  const documentationLabel = () => remote() ? "HTTP transcription documentation" : adapter() === "gigaam" ? tr("Local GigaAM documentation") : tr("Speech provider documentation");
  const documentationUrl = () => remote() ? `${DOCUMENTATION}#openai-compatible-server` : adapter() === "gigaam" ? `${DOCUMENTATION}#local-gigaam` : DOCUMENTATION;
  let dialog: HTMLDialogElement | undefined;
  onCleanup(() => { if (dialog?.open) dialog.close(); });
  return (
    <dialog class="dlg voice-editor" ref={(el) => { dialog = el; queueMicrotask(() => el.showModal()); }} aria-labelledby="voice-editor-title"
      onCancel={(e) => { e.preventDefault(); close(); }}>
      <div class="dlg-head"><h2 id="voice-editor-title">{editor().title}</h2></div>
      <div class="dlg-body">
        <p class="muted">{editor().computer}</p>
        <p class="muted">{editor().readiness}</p>
        <div class="form-row"><label for="voice-name">{tr("Name")}</label>
          <input id="voice-name" class="field" maxLength={128} disabled={busy()} value={name()} onInput={(e) => setName(e.currentTarget.value)}
            placeholder={remote() ? "e.g. Team transcription server" : "e.g. Russian speech"} /></div>
        <div class="form-row"><label for="voice-id">{tr("Profile ID")}</label>
          <input id="voice-id" class="field mono" maxLength={64} readOnly={!editor().create} disabled={busy()} value={id()} onInput={(e) => setId(e.currentTarget.value)} /></div>
        <div class="form-row"><label for="voice-type">{tr("Type")}</label>
          <Select id="voice-type" value={adapter()} options={types()} disabled={busy()} onChange={(chosen) => { setRetyped(true); setAdapter(chosen); }} /></div>
        <div class="form-row"><label for="voice-model">{tr("Model")}</label>
          <Show when={remote()} fallback={
            <Select id="voice-model" value={model()} options={models().map((m) => ({ value: m, label: m }))} disabled={busy()} onChange={setModel} />
          }>
            <input id="voice-model" class="field mono" maxLength={128} list="voice-models" disabled={busy()} value={model()} onInput={(e) => setModel(e.currentTarget.value)} />
            <datalist id="voice-models"><For each={models()}>{(m) => <option value={m} />}</For></datalist>
          </Show>
        </div>
        <div class="form-row"><label for="voice-language">{tr("Language")}</label>
          <input id="voice-language" class="field mono voice-lang" maxLength={2} placeholder={tr("Auto-detect")} disabled={busy()} value={language()} onInput={(e) => setLanguage(e.currentTarget.value)} /></div>
        <Show when={remote()} fallback={
          <div class="form-row"><label for="voice-device">{tr("Device")}</label>
            <Select id="voice-device" value={device()} options={devices().map((d) => ({ value: d, label: d }))} disabled={busy()} onChange={setDevice} /></div>
        }>
          <div class="form-row"><label for="voice-url">{tr("Base URL")}</label>
            <input id="voice-url" class="field mono" maxLength={2048} placeholder="https://speech.example.net/v1" disabled={busy()} value={url()} onInput={(e) => setUrl(e.currentTarget.value)} /></div>
          <div class="form-row"><span />
            <Check label={trNoop("Allow unencrypted HTTP for a local or private server")} checked={insecure()} disabled={busy()} onChange={setInsecure} /></div>
          <div class="form-row"><label for="voice-credential">{tr("API key")}</label>
            <Select id="voice-credential" value={credential()} disabled={busy()} onChange={(mode) => { setKey(""); setCredential(mode); }} options={[
              { value: "keep", label: editor().key_stored ? trNoop("Keep stored API key") : trNoop("No API key") },
              { value: "replace", label: trNoop("Replace API key") },
              { value: "remove", label: trNoop("Remove API key") },
            ]} /></div>
          <div class="form-row"><label for="voice-key">{tr("New API key")}</label>
            <input id="voice-key" class="field" type="password" maxLength={8192} autocomplete="off" disabled={busy() || credential() !== "replace"}
              value={key()} onInput={(e) => setKey(e.currentTarget.value)} /></div>
        </Show>
        <Lines class="hint voice-help" text={tr(HELP[adapter()] ?? OTHER_HELP)} />
        <div>
          <button type="button" class="btn ghost" title={documentationUrl()} onClick={() => void openDocumentation(adapter()).then(setDocumentationFailure)}>
            {documentationLabel()}<Icon name="external-link" size={12} />
          </button>
        </div>
        <Show when={documentationFailure()}>{(failure) => <Lines class="selectable dlg-error" text={failure()} />}</Show>
        <Show when={editor().status}>{(status) => <p class="voice-editor-status">{status()}</p>}</Show>
        <Show when={editor().check_visible}>
          <button type="button" class="btn" disabled={!editor().check_enabled} onClick={() => void input("check")}>{tr("Check operation status")}</button>
        </Show>
      </div>
      <div class="dlg-foot">
        <span class="spacer" />
        <button type="button" class="btn" onClick={close}>{tr("Cancel")}</button>
        <button type="button" class="btn primary" disabled={!canSave()} onClick={save}>{tr("Save")}</button>
      </div>
    </dialog>
  );
}
