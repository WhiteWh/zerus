import { For, createSignal, onCleanup, onMount } from "solid-js";
import { Icon } from "../lib/icons";
import { machineAppearance } from "../../store/machineAppearance";
import { setSetting, settings } from "../../store/settings";
import { toast } from "../../store/notices";
import { paletteIndex } from "../sessions/SessionCard";
import { tr } from "../../i18n";

const SWATCHES = ["#ffcf33", "#ff9234", "#f85c63", "#e45dbf", "#a96bf5", "#6677ff", "#2b97ff", "#23c5dd", "#00aa7f", "#41c86b", "#a6cc38", "#8796aa"];
const sixDigit = (color: string) => /^#[\da-f]{3}$/iu.test(color)
  ? `#${[...color.slice(1)].map((digit) => digit + digit).join("")}`
  : color;

export default function MachineColorDialog(props: { alias: string; origin?: HTMLElement; close: () => void }) {
  let dialog!: HTMLDialogElement;
  const origin = props.origin ?? document.activeElement;
  const fallback = () => `var(--z-entity-palette-${paletteIndex(props.alias)})`;
  const current = () => machineAppearance(settings, props.alias, fallback());
  const initial = machineAppearance(settings, props.alias, SWATCHES[6]);
  const [hex, setHex] = createSignal(sixDigit(initial.color));
  const [vivid, setVivid] = createSignal(initial.vivid);
  const [busy, setBusy] = createSignal(false);
  const valid = () => /^#[\da-f]{6}$/iu.test(hex());
  const apply = async () => {
    if (busy() || !valid()) return;
    setBusy(true);
    try {
      const saved = await Promise.all([
        setSetting(`machines/colors/${props.alias}`, hex().toLowerCase()),
        setSetting(`machines/vivid/${props.alias}`, vivid()),
      ]);
      if (saved.every(Boolean)) props.close();
      else toast(tr("Label color"), tr("The machine appearance could not be saved. Try again."), "warn", true);
    } finally { setBusy(false); }
  };
  onMount(() => { dialog.showModal(); queueMicrotask(() => dialog.querySelector<HTMLInputElement>("input")?.focus()); });
  onCleanup(() => { dialog.close(); queueMicrotask(() => { if (origin instanceof HTMLElement && origin.isConnected) origin.focus(); }); });
  return <dialog class="dlg" ref={dialog} aria-labelledby="machine-color-title" style={{ width: "380px" }} onCancel={(event) => { if (busy()) event.preventDefault(); else props.close(); }} onClose={props.close}>
    <div class="dlg-head"><Icon name="palette" size={18} class="accent" /><h2 id="machine-color-title">{tr("Label color for %1", props.alias)}</h2><span class="grow" /><button class="icon-btn" aria-label={tr("Close")} disabled={busy()} onClick={props.close}><Icon name="x" size={16} /></button></div>
    <div class="dlg-body">
      <div class="cd-preview"><span class="entity mach-label" classList={{ vivid: vivid() }} style={{ "--entity": valid() ? hex() : current().color }}>{props.alias}</span></div>
      <div class="cd-swatches" role="radiogroup" aria-label={tr("Color")}>
        <For each={SWATCHES}>{(color) => <button class="cd-swatch" role="radio" aria-checked={hex().toLowerCase() === color} aria-label={color} classList={{ on: hex().toLowerCase() === color }} style={{ "--sw": color }} disabled={busy()} onClick={() => setHex(color)} />}</For>
      </div>
      <div class="form-row"><label for="cd-hex">{tr("Hex")}</label><input id="cd-hex" class="field mono" maxLength={7} value={hex()} aria-invalid={!valid()} disabled={busy()} onInput={(event) => setHex(event.currentTarget.value)} /></div>
      <div class="form-row"><label for="cd-style">{tr("Style")}</label><select id="cd-style" class="field" value={vivid() ? "bright" : "soft"} disabled={busy()} onChange={(event) => setVivid(event.currentTarget.value === "bright")}><option value="soft">{tr("Soft fill")}</option><option value="bright">{tr("Bright fill")}</option></select></div>
      <label class="btn ghost cd-more">{tr("More colors…")}<input type="color" value={valid() ? hex() : "#2b97ff"} disabled={busy()} onInput={(event) => setHex(event.currentTarget.value)} /></label>
    </div>
    <div class="dlg-foot"><span class="spacer" /><button class="btn" disabled={busy()} onClick={props.close}>{tr("Cancel")}</button><button class="btn primary" disabled={busy() || !valid()} onClick={() => void apply()}>{tr("Apply")}</button></div>
  </dialog>;
}
