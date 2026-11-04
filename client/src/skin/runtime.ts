import { createSignal, createMemo } from "solid-js";
import { resolveSkins, compileSkin, type ResolvedSkin, type SkinFile } from "./skin";
import { DEFAULTS, flag, setSetting, settingsReady, text } from "../store/settings";
import zerusDark from "./skins/zerus-dark.json";
import zerusLight from "./skins/zerus-light.json";
import fjord from "./skins/fjord.json";
import paper from "./skins/paper.json";

// The base skin must come first: it defines the token set others may override.
export const skins: ResolvedSkin[] = resolveSkins([zerusDark, zerusLight, fjord, paper] as SkinFile[]);

const sheet = document.createElement("style");
sheet.id = "zerus-skins";
sheet.textContent = skins.map(compileSkin).join("\n");
document.head.prepend(sheet);

// Settings > Appearance: the Qt theme (workspace/theme: follow the system,
// dark or light) picks one of two skins, one per appearance
// (client/darkSkin, client/lightSkin). `?skin=` overrides both for a run.
const fromUrl = new URLSearchParams(location.search).get("skin");
const urlSkin = fromUrl && skins.some((s) => s.id === fromUrl) ? fromUrl : undefined;

export type Theme = "system" | "dark" | "light";
export type Appearance = "dark" | "light";

const media = typeof matchMedia === "undefined" ? undefined : matchMedia("(prefers-color-scheme: light)");
const [systemLight, setSystemLight] = createSignal(media?.matches ?? false);
media?.addEventListener("change", (e) => setSystemLight(e.matches));
export { systemLight };

export const theme = (): Theme => text("workspace/theme") as Theme;
const slot = (appearance: Appearance) => (appearance === "dark" ? "client/darkSkin" : "client/lightSkin");

/** Reactive list of built-in plus imported skins. */
const [skinListSignal, setSkinList] = createSignal<ResolvedSkin[]>([...skins]);
export const skinList = skinListSignal;

/** The skin chosen for `appearance`; an unknown or mismatched id falls back to the slot's default. */
export function chosenSkin(appearance: Appearance): string {
  const id = text(slot(appearance));
  return skinListSignal().some((s) => s.id === id && s.appearance === appearance) ? id : String(DEFAULTS[slot(appearance)]);
}

/** The appearance a theme shows now. */
export const appearanceFor = (choice: Theme, light: boolean): Appearance =>
  choice === "dark" ? "dark" : choice === "light" ? "light" : light ? "light" : "dark";

/** The skin in effect: the theme's appearance picks one of the pair. */
export const skinId = createMemo(() => urlSkin ?? chosenSkin(appearanceFor(theme(), systemLight())));
export const skin = createMemo(() => skinListSignal().find((s) => s.id === skinId()) ?? skins[0]);

/** Settings > Appearance > Theme. */
export function chooseTheme(choice: Theme) {
  void setSetting("workspace/theme", choice);
}

/**
 * Chooses a skin for its appearance. A fixed theme of the other appearance
 * moves to this one, as the mockup does; whether the skin is in use now.
 */
export function chooseSkin(id: string): boolean {
  const chosen = skinListSignal().find((s) => s.id === id);
  if (!chosen) return false;
  void setSetting(slot(chosen.appearance), id);
  if (theme() !== "system" && theme() !== chosen.appearance) void setSetting("workspace/theme", chosen.appearance);
  const shown = appearanceFor(theme(), systemLight()) === chosen.appearance;
  // Choosing a skin that shows applies its activity-column variant (a user
  // action; a skin waiting for the other appearance, or one the system
  // appearance switches to, changes no setting).
  const full = chosen.layout["activity-column"] === "full";
  if (shown && full !== flag("workspace/activityFullWidth")) void setSetting("workspace/activityFullWidth", full);
  return shown;
}

/**
 * The skin picked before the settings store (local storage), moved once into
 * its slot. A skin of the other appearance than the system's also sets the
 * theme, so it stays in use (the mockup's start). The entry goes only after
 * the store took the move.
 */
const LEGACY_SKIN = "zerus-client-skin";
void settingsReady.then(async () => {
  let id: string | null = null;
  try { id = localStorage.getItem(LEGACY_SKIN); } catch { return; /* storage may be unavailable */ }
  if (id === null) return;
  const legacy = skins.find((s) => s.id === id);
  let stored = true;
  if (legacy) {
    stored = await setSetting(slot(legacy.appearance), legacy.id);
    if (stored && (legacy.appearance === "light") !== systemLight()) stored = await setSetting("workspace/theme", legacy.appearance);
  }
  if (stored) try { localStorage.removeItem(LEGACY_SKIN); } catch { /* storage may be unavailable */ }
});

/** Replace the bounded user library and its rules; reimports never grow the style sheet. */
export function replaceUserSkins(resolved: ResolvedSkin[]) {
  let imported = document.getElementById("zerus-imported-skins");
  if (!imported) {
    imported = document.createElement("style"); imported.id = "zerus-imported-skins"; document.head.append(imported);
  }
  imported.textContent = resolved.map(compileSkin).join("\n");
  setSkinList([...skins, ...resolved]);
}

/** Reflect the active skin's tokens and L1 variants onto the root element. */
export function applySkin(s: ResolvedSkin) {
  const root = document.documentElement;
  root.dataset.skin = s.id;
  root.dataset.appearance = s.appearance;
  for (const [name, value] of Object.entries(s.components)) root.dataset[camel(name)] = value;
}

const camel = (name: string) => name.replace(/-([a-z])/g, (_, c: string) => c.toUpperCase());
