import { For, Show, createEffect, createSignal, on as observe, onCleanup, onMount } from "solid-js";
import { createStore, reconcile, unwrap } from "solid-js/store";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { inTauri } from "../../../bridge";
import { setPage } from "../../../store/ui";
import { Check, Group, Lines, SectionHead, Select, Status } from "./ui";
import { saveOutcome } from "./recovery-wait";
import { tr, trNoop } from "../../../i18n";
import { desktopService } from "../../../store/desktopService";
import { maintenanceIntent, type RecoveryForm } from "../../../store/maintenance";
import { MaintenanceStatus } from "./MaintenanceStatus";

// Settings > Automatic recovery (tray/src/RecoverySettings.h) as the
// approved mockup lays it out (design/webui/src/screens/settings/Recovery.tsx).
// The policy lives in hgs and is shared by every connected machine; the
// core's recovery_settings keeps it in step in the background, and Save is
// checked against the policy this page loaded its form from.

/** `known`: this computer's machine card has arrived; until then the
 *  service reads nothing and the page offers nothing. */
interface RecoveryView { status: string; error: string; busy: boolean; can_save: boolean; known: boolean; has_machines: boolean; has_policy: boolean; saves: number }
type Form = RecoveryForm;

const RULES = [
  { id: "service", label: trNoop("Overload / server unavailable") },
  { id: "rate_limit", label: trNoop("Temporary rate limit") },
  { id: "session_limit", label: trNoop("Session usage limit") },
  { id: "network", label: trNoop("Connection / timeout") },
];
const HELP = trNoop("Each positive number is a delay before the next attempt. End with −1 to stop, or 0 to repeat the previous delay forever.\nExample: 300, 0 retries every 5 minutes until stopped.\n\nNew messages and Stop cancel recovery. Sign-in, balance, other quota and context errors require your action.");

/** Opening the section reads every machine again (Qt's openRecovery). */
export function refreshRecovery() {
  if (inTauri()) void invoke("recovery_reload").catch(() => {});
}

export function RecoverySection() {
  const [view, setView] = createSignal<RecoveryView | undefined>();
  const [form, setForm] = createStore<Form>({ enabled: false, rules: {}, session_limit_mode: "reset", delays: {} });
  const [loaded, setLoaded] = createSignal(false);
  const [saving, setSaving] = createSignal(false);
  const [message, setMessage] = createSignal("");
  const [serviceSaveId, setServiceSaveId] = createSignal<string>();
  const servicePending = () => desktopService.enabled() && (maintenanceIntent.busy() || maintenanceIntent.blocked());
  // The form holds edits the policy has not taken yet; the policy it was
  // loaded from (Qt's base), which Save is checked against; and the answered
  // saves when ours was sent, so its answer is told from a read's.
  let dirty = false;
  let base: Record<string, unknown> | null = null;
  let savesBefore = 0;

  const load = async () => {
    const answer = await invoke<{ form: Form | null; base: Record<string, unknown> | null }>("recovery_load")
      .catch(() => ({ form: null, base: null }));
    // An edit made while the policy was read keeps the form and its base.
    if (dirty || saving() || servicePending() || !answer.form || !answer.base) return;
    setForm(reconcile(answer.form));
    base = answer.base;
    setLoaded(true);
  };
  // RecoverySettings' changed(): the status, a save's outcome, and the
  // newest policy while nothing is being edited.
  const apply = (next: RecoveryView) => {
    setView(next);
    const outcome = saving() ? saveOutcome(savesBefore, next) : { done: false as const };
    if (outcome.done) {
      setSaving(false);
      if (outcome.saved) dirty = false;
      setMessage(outcome.message);
    }
    if (!dirty && !saving()) void load();
  };
  const edit = (change: () => void) => { dirty = true; if (desktopService.enabled()) setMessage(""); change(); };
  const save = async () => {
    if (!base) return;
    if (desktopService.enabled()) {
      setMessage("");
      const previous = maintenanceIntent.pending()?.id;
      const sent = maintenanceIntent.submit({ policy: unwrap(form), base });
      const current = maintenanceIntent.pending()?.id;
      if (current && current !== previous) setServiceSaveId(current);
      await sent.catch(() => { if (!maintenanceIntent.blocked()) setServiceSaveId(undefined); });
      return;
    }
    savesBefore = view()?.saves ?? 0;
    const answer = await invoke<{ view: RecoveryView; message: string | null; sent: boolean }>("recovery_save", { form: unwrap(form), base })
      .catch((error) => { setMessage(String(error)); return undefined; });
    if (!answer) return;
    if (answer.message) { setMessage(answer.message); return; }
    if (!answer.sent) { setView(answer.view); setMessage(answer.view.error); return; }
    setSaving(true);
    // The answer may already have arrived as an event: read it again.
    apply(await invoke<RecoveryView>("recovery_view"));
  };
  createEffect(observe(maintenanceIntent.result, (result) => {
    // Aggregate save counters or another view's intent cannot acknowledge ours.
    if (desktopService.enabled() && result?.id === serviceSaveId() && result?.recoveryEdit && result.state === "applied") {
      setServiceSaveId(undefined);
      dirty = false;
      setMessage("Saved.");
      void load();
    }
  }));
  // Reload keeps the message line, as Qt's does.
  const reload = async () => {
    dirty = false;
    const next = await invoke<RecoveryView>("recovery_reload").catch((error) => { setMessage(String(error)); return undefined; });
    if (next) apply(next); else void load();
  };

  onMount(() => {
    if (!inTauri()) return;
    let stop: (() => void) | undefined;
    let gone = false;
    const read = () => void invoke<RecoveryView>("recovery_view").then(apply, () => {});
    // Read again once listening: a change in between would otherwise wait
    // for the next one.
    listen<RecoveryView>("recovery-changed", (e) => apply(e.payload)).then((off) => { if (gone) off(); else { stop = off; read(); } }, () => {});
    onCleanup(() => { gone = true; stop?.(); });
    read();
  });

  const usable = () => loaded() && !!view()?.has_machines && !servicePending();
  const on = () => usable() && form.enabled;
  const statusKind = () => {
    const v = view();
    if (!v || !v.has_machines || v.status.startsWith("Waiting for")) return v ? "warn" : "muted";
    return v.busy ? "busy" : "muted";
  };
  const rule = (r: { id: string; label: string }) => {
    const session = r.id === "session_limit";
    const delaysOn = () => on() && !!form.rules[r.id] && (!session || form.session_limit_mode === "interval");
    return (
      <Group>
        <div class="rec-rule">
          <Check label={r.label} checked={!!form.rules[r.id]} disabled={!on()} onChange={(v) => edit(() => setForm("rules", r.id, v))} />
          <Show when={session}>
            <div class="rec-mode">
              <Select value={form.session_limit_mode} label={r.label} disabled={!on() || !form.rules[r.id]}
                onChange={(v) => edit(() => setForm("session_limit_mode", v))}
                options={[{ value: "reset", label: trNoop("Wait for reset") }, { value: "interval", label: trNoop("Retry on a schedule") }]} />
              <p class="set-hint">{tr("Wait until the provider's reset time by default. If no reset time is available, recovery needs your attention. Choose a schedule to retry at your own intervals.")}</p>
            </div>
          </Show>
          <label class="rec-delays" classList={{ disabled: !delaysOn() }}>
            <span class="set-label">{tr("Retry after (seconds)")}</span>
            <input class="field mono grow" placeholder="15, 30, 60, 300, -1" value={form.delays[r.id] ?? ""} disabled={!delaysOn()}
              onInput={(e) => { const text = e.currentTarget.value; edit(() => setForm("delays", r.id, text)); }} />
          </label>
        </div>
      </Group>
    );
  };
  return (
    <>
      <SectionHead title={trNoop("Automatic recovery")} intro={tr("Shared by all connected machines. Agents finish their own retries first; recovery continues the same task.")} />
      <Group>
        <Check label={trNoop("Recover automatically after temporary failures")} checked={form.enabled} disabled={!usable()} onChange={(v) => edit(() => setForm("enabled", v))} />
      </Group>
      <div class="rec-rules"><For each={RULES}>{rule}</For></div>
      <div class="set-note"><Lines text={tr(HELP)} /></div>
      <Show when={view()?.status}>{(status) => <Status kind={statusKind()}>{status()}</Status>}</Show>
      <Show when={view()?.known && !view()!.has_machines}>
        <div class="set-actions"><button type="button" class="btn" onClick={() => setPage("machines")}>{tr("Open Machines")}</button></div>
      </Show>
      <Show when={!message() && view()?.error}>{(error) => <Status kind="error">{tr(error())}</Status>}</Show>
      <Show when={message()}>{(text) => <Status kind={text() === "Saved." ? "ok" : "error"}>{tr(text())}</Status>}</Show>
      <Show when={desktopService.enabled()}><MaintenanceStatus apply={!view()?.has_machines} /></Show>
      <div class="set-actions">
        <button type="button" class="btn primary" disabled={!usable() || !view()?.can_save || saving()} onClick={() => void save()}>{tr("Save recovery settings")}</button>
        <button type="button" class="btn" disabled={servicePending()} onClick={() => void reload()}>{tr("Reload")}</button>
      </div>
    </>
  );
}
