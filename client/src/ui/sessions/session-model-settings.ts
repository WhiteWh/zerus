import type { Session } from "./types";
import { effortValid } from "./opencode-models";
import { tr } from "../../i18n";

export interface ModelChoice { id: string; label: string; effort_options: string[]; effort_labels: string[] }
export interface ModelContext {
  row: Session;
  details?: Record<string, unknown>;
  online: boolean;
  busy: boolean;
  inspectError?: boolean;
  answering?: boolean;
}
export const text = (value: unknown) => typeof value === "string" ? value : "";
const short = (value: unknown, limit: number) => { const s = text(value); return s.length <= limit && !/\p{Cc}/u.test(s) ? s : ""; };

/** The exact account's inspected catalog, in native order, with bounded choices. */
export function modelChoices(value: unknown): ModelChoice[] {
  if (!Array.isArray(value)) return [];
  const seen = new Set<string>();
  return value.slice(0, 256).flatMap((entry) => {
    if (!entry || typeof entry !== "object") return [];
    const id = short(entry.value ?? entry.id, 256);
    if (!id || seen.has(id)) return [];
    seen.add(id);
    const variants: unknown[] = Array.isArray(entry.effort_options) ? entry.effort_options.slice(0, 64) : [];
    const labels = Array.isArray(entry.effort_labels) ? entry.effort_labels : [];
    const efforts = variants.flatMap((v, i) => short(v, 40) ? [{ value: text(v), label: short(labels[i], 80) || text(v) }] : []);
    return [{ id, label: short(entry.label, 160) || id, effort_options: efforts.map((e) => e.value), effort_labels: efforts.map((e) => e.label) }];
  });
}

/** MessageComposer::setModelSettings/updateSettingsButton/updateSettingsApply. */
export function settingsView(input: ModelContext, changing = false) {
  const { row, details: d, online, busy } = input;
  const model = typeof d?.settings_model === "string" ? d.settings_model : text(d?.model) || row.model;
  const effort = typeof d?.effort === "string" ? d.effort : row.effort ?? "";
  const pendingModel = text(d?.pending_model), pendingEffort = text(d?.pending_effort);
  const pending = !!pendingModel || !!pendingEffort;
  const applyWhen = text(d?.settings_apply_when);
  const reason = !online ? tr("Machine unavailable. Reconnect to change model settings.")
    : changing ? tr("Updating model settings…")
    : busy ? tr("Wait for the session update to finish.")
    : input.answering ? tr("Wait for the answer to be delivered.")
    : input.inspectError || !d ? tr("Refresh the session before changing model settings.")
    : d.settings_change_supported !== true ? text(d.settings_change_reason) || tr("Model settings are not available for this session.") : "";
  const shownModel = model || "Model", suffix = (effort ? `  ${effort}` : "") + (pending ? "  ◷" : "");
  let hint = reason || (applyWhen === "resume" ? tr("Saved for the next resume. The session stays stopped.")
    : applyWhen === "ready" ? tr("Applies when the agent is ready. Messages sent while it is working use the current model.") : tr("Applies to this session only."));
  const changeNotice = text(d?.settings_change_notice) || (row.cmd === "claude" ? tr("Claude re-reads the conversation with the new model on its next turn.") : "");
  if (pending) hint = `Pending: ${pendingModel || model}${pendingEffort ? `  ${pendingEffort}` : ""}\nCurrent: ${model}${effort ? `  ${effort}` : ""}\n${hint}`;
  let tooltip = `Current: ${shownModel}${effort ? `  ${effort}` : ""}`;
  if (pending) tooltip += `\nPending: ${pendingModel || model}${pendingEffort ? `  ${pendingEffort}` : ""}`;
  if (reason) tooltip += `\n${reason}`;
  return { model, effort, shownModel, suffix, tooltip, accessible: tr("Model and reasoning effort: %1%2", shownModel, suffix),
    pending, pendingModel, pendingEffort, applyWhen, reason, hint, changeNotice, enabled: !reason,
    intendedModel: pendingModel || model, intendedEffort: pending ? pendingEffort : effort,
    applyLabel: applyWhen === "resume" ? tr("Save for resume") : applyWhen === "ready" ? tr("Save change") : "Apply",
    openCode: row.cmd === "opencode", terminal: row.cmd === "claude" && online && row.state === "running" && !text(d?.expected_id),
    options: modelChoices(d?.model_options) };
}

/** Preserve the previous effort when offered; OpenCode's empty Default is explicit. */
export function effortChoices(options: ModelChoice[], model: string, preferred: string, openCode: boolean, currentModel: string, currentEffort: string) {
  const option = options.find((m) => m.id === model);
  const choices: { value: string; label: string }[] = openCode ? [{ value: "", label: "Default" }] : [];
  option?.effort_options.forEach((effort, i) => {
    if ((!openCode || effortValid(effort)) && !choices.some((c) => c.value === effort)) choices.push({ value: effort, label: openCode ? option.effort_labels[i] || effort : effort });
  });
  if (openCode && model === currentModel && effortValid(currentEffort) && !choices.some((c) => c.value === currentEffort)) choices.push({ value: currentEffort, label: currentEffort });
  if (!choices.length) choices.push({ value: "", label: tr("Provider default") });
  return { choices, value: choices.some((c) => c.value === preferred) ? preferred : choices[0].value };
}
