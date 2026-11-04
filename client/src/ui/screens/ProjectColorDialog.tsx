import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { Icon } from "../lib/icons";
import { ProjectLabel } from "../components/ProjectLabel";
import { settings, setSetting } from "../../store/settings";
import { setProjectColor } from "../../store/organization";
import { projectsPage } from "../../store/projects";
import { toast } from "../../store/notices";
import type { Project } from "../../types";
import { tr } from "../../i18n";
const SWATCHES = ["#ffcf33", "#ff9234", "#f85c63", "#e45dbf", "#a96bf5", "#6677ff", "#2b97ff", "#23c5dd", "#00aa7f", "#41c86b", "#a6cc38", "#8796aa"];
const toHex = (h: number, s: number, l: number) => {
  const a = (s / 100) * Math.min(l / 100, 1 - l / 100);
  const f = (n: number) => { const k = (n + h / 30) % 12; return Math.round((l / 100 - a * Math.max(-1, Math.min(k - 3, 9 - k, 1))) * 255).toString(16).padStart(2, "0"); };
  return `#${f(0)}${f(8)}${f(4)}`;
};
/** Exact setup-project-color mockup popup. Cancel/Escape never store a preview. */
export default function ProjectColorDialog(props: { project: Project; origin?: HTMLElement; close: () => void }) {
  let dialog!: HTMLDialogElement;
  const p = props.project, origin = props.origin ?? document.activeElement;
  const slot = getComputedStyle(document.documentElement).getPropertyValue(`--z-entity-palette-${p.palette}`).trim();
  const initial = (p.color ?? (/^#[0-9a-f]{6}$/iu.test(slot) ? slot : SWATCHES[0])).toLowerCase();
  const [hex, setHex] = createSignal(initial);
  const [vivid, setVivid] = createSignal(settings[`projects/vivid/${p.id}`] === true);
  const [more, setMore] = createSignal(false), [hsl, setHsl] = createSignal({ h: 160, s: 60, l: 55 });
  const [busy, setBusy] = createSignal(false);
  const valid = () => /^#[0-9a-fA-F]{6}$/u.test(hex());
  const applyHsl = (patch: Partial<{ h: number; s: number; l: number }>) => { const value = { ...hsl(), ...patch }; setHsl(value); setHex(toHex(value.h, value.s, value.l)); };
  const apply = async () => {
    if (busy() || !valid()) return;
    if (!projectsPage().projects.some((current) => current.id === p.id && current.accessible)) { toast(tr("Project color"), tr("This project is no longer listed. Open the menu again."), "warn"); return; }
    setBusy(true);
    try {
      if (hex().toLowerCase() !== initial) {
        const result = await setProjectColor(p.id, hex().toLowerCase());
        if (result.outcome === "refused") return;
      }
      if (await setSetting(`projects/vivid/${p.id}`, vivid())) props.close();
      else toast(tr("Project color"), tr("The project style could not be saved. Try again."), "warn", true);
    } catch (error) { toast(tr("Project color"), String(error), "warn", true); }
    finally { setBusy(false); }
  };
  onMount(() => { dialog.showModal(); queueMicrotask(() => dialog.querySelector<HTMLInputElement>("input")?.focus()); });
  onCleanup(() => { dialog.close(); queueMicrotask(() => { if (origin instanceof HTMLElement && origin.isConnected) origin.focus(); }); });
  return <dialog class="dlg su-dlg project-color" ref={dialog} aria-labelledby="project-color-title" style={{ width: "420px" }} onCancel={(e) => { if (busy()) e.preventDefault(); else props.close(); }} onClose={props.close} onClick={(e) => { if (!busy() && e.target === dialog) { const r = dialog.getBoundingClientRect(); if (e.clientX < r.left || e.clientX > r.right || e.clientY < r.top || e.clientY > r.bottom) props.close(); } }}>
    <div class="dlg-head"><Icon name="palette" size={18} class="accent" /><h2 id="project-color-title">{tr("Project color")}</h2><span class="grow" /><button class="icon-btn" aria-label={tr("Close")} disabled={busy()} onClick={props.close}><Icon name="x" size={16} /></button></div>
    <div class="dlg-body pc-body">
      <ProjectLabel project={{ ...p, color: valid() && hex() !== initial ? hex() : p.color }} vivid={vivid()} class="pc-preview" />
      <div class="pc-swatches" role="group" aria-label={tr("Preset colors")}><For each={SWATCHES}>{(color) => <button class="pc-sw" classList={{ on: hex().toLowerCase() === color }} style={{ background: color }} aria-label={color} title={color} disabled={busy()} onClick={() => setHex(color)} />}</For></div>
      <div class="form-row"><label for="pc-hex">{tr("Color")}</label><input id="pc-hex" class="field mono pc-hex" maxLength={7} value={hex()} aria-invalid={!valid()} disabled={busy()} onInput={(e) => setHex(e.currentTarget.value)} /></div>
      <div class="form-row"><label for="pc-style">{tr("Style")}</label><select id="pc-style" class="field" value={vivid() ? "bright" : "soft"} disabled={busy()} onChange={(e) => setVivid(e.currentTarget.value === "bright")}><option value="soft">{tr("Soft fill")}</option><option value="bright">{tr("Bright fill")}</option></select></div>
      <button class="btn small pc-more" aria-expanded={more()} disabled={busy()} onClick={() => setMore(!more())}><Icon name="palette" size={12} />{tr("More colors…")}</button>
      <Show when={more()}><div class="pc-picker" role="group" aria-label={tr("Color picker")}><label>{tr("Hue")}<input type="range" min="0" max="359" value={hsl().h} disabled={busy()} onInput={(e) => applyHsl({ h: +e.currentTarget.value })} /></label><label>{tr("Saturation")}<input type="range" min="0" max="100" value={hsl().s} disabled={busy()} onInput={(e) => applyHsl({ s: +e.currentTarget.value })} /></label><label>{tr("Lightness")}<input type="range" min="10" max="90" value={hsl().l} disabled={busy()} onInput={(e) => applyHsl({ l: +e.currentTarget.value })} /></label></div></Show>
    </div><div class="dlg-foot"><span class="grow" /><button class="btn" disabled={busy()} onClick={props.close}>{tr("Cancel")}</button><button class="btn primary" disabled={busy() || !valid()} onClick={() => void apply()}>{tr("Apply")}</button></div>
  </dialog>;
}
