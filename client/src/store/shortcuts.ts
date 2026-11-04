import { setSetting, settings } from "./settings";
import { tr, trNoop } from "../i18n";

// Keyboard commands (tray/src/Shortcuts.h, Shortcuts.cpp): stable ids, default
// chords per platform, the rules that decide when a chord may leave Terminal
// or a text field, and the user's overrides under shortcuts/<id> in the
// settings store (one list of portable chord strings; an empty list means no
// chord; no entry means the defaults). Chords use Qt's portable notation:
// "Ctrl" is Command and "Meta" the Control key on macOS.

export type Platform = "linux" | "windows" | "macos";

export function detectPlatform(userAgent: string): Platform {
  return /Mac/.test(userAgent) ? "macos" : /Win/.test(userAgent) ? "windows" : "linux";
}
export const platform: Platform = typeof navigator === "undefined" ? "linux" : detectPlatform(navigator.userAgent);

/** Window: anywhere in the main window. Composer: only in a message field, which matches it itself. */
export type Scope = "window" | "composer";
export interface Command { id: string; group: string; title: string; scope: Scope }

export const COMMANDS: readonly Command[] = [
  { id: "go.overview", group: trNoop("Go to"), title: trNoop("Go to Overview"), scope: "window" },
  { id: "go.sessions", group: trNoop("Go to"), title: trNoop("Go to Sessions"), scope: "window" },
  { id: "go.projects", group: trNoop("Go to"), title: trNoop("Go to Projects"), scope: "window" },
  { id: "go.machines", group: trNoop("Go to"), title: trNoop("Go to Machines"), scope: "window" },
  { id: "go.accounts", group: trNoop("Go to"), title: trNoop("Go to Accounts"), scope: "window" },
  { id: "go.settings", group: trNoop("Go to"), title: trNoop("Open Settings"), scope: "window" },
  { id: "go.anywhere", group: trNoop("Go to"), title: trNoop("Go to…"), scope: "window" },
  { id: "help.shortcuts", group: trNoop("Go to"), title: trNoop("Keyboard shortcuts"), scope: "window" },
  { id: "session.next", group: trNoop("Sessions"), title: trNoop("Next session"), scope: "window" },
  { id: "session.previous", group: trNoop("Sessions"), title: trNoop("Previous session"), scope: "window" },
  { id: "session.nextAttention", group: trNoop("Sessions"), title: trNoop("Next session that needs attention"), scope: "window" },
  { id: "session.previousAttention", group: trNoop("Sessions"), title: trNoop("Previous session that needs attention"), scope: "window" },
  { id: "session.new", group: trNoop("Sessions"), title: trNoop("New session"), scope: "window" },
  { id: "session.rename", group: trNoop("Sessions"), title: trNoop("Rename session"), scope: "window" },
  { id: "session.search", group: trNoop("Sessions"), title: trNoop("Search sessions"), scope: "window" },
  { id: "view.refresh", group: trNoop("Sessions"), title: trNoop("Refresh"), scope: "window" },
  { id: "tab.next", group: trNoop("Session view"), title: trNoop("Next tab"), scope: "window" },
  { id: "tab.previous", group: trNoop("Session view"), title: trNoop("Previous tab"), scope: "window" },
  { id: "tab.terminal", group: trNoop("Session view"), title: trNoop("Switch between Activity and Terminal"), scope: "window" },
  { id: "panel.toggle", group: trNoop("Session view"), title: trNoop("Show or hide the side panel"), scope: "window" },
  { id: "focus.next", group: trNoop("Session view"), title: trNoop("Next area"), scope: "window" },
  { id: "focus.previous", group: trNoop("Session view"), title: trNoop("Previous area"), scope: "window" },
  { id: "message.send", group: trNoop("Messages"), title: trNoop("Send message"), scope: "composer" },
  { id: "queue.sendNow", group: trNoop("Messages"), title: trNoop("Send queued messages now"), scope: "window" },
  { id: "message.push", group: trNoop("Messages"), title: trNoop("Send and push now"), scope: "composer" },
  { id: "zoom.in", group: trNoop("Content scale"), title: trNoop("Increase content scale"), scope: "window" },
  { id: "zoom.out", group: trNoop("Content scale"), title: trNoop("Decrease content scale"), scope: "window" },
  { id: "zoom.reset", group: trNoop("Content scale"), title: trNoop("Reset content scale"), scope: "window" },
];

export const command = (id: string) => COMMANDS.find((c) => c.id === id);

const COMMON: Record<string, string[]> = {
  "go.overview": ["Ctrl+1"], "go.sessions": ["Ctrl+2"], "go.projects": ["Ctrl+3"],
  "go.machines": ["Ctrl+4"], "go.accounts": ["Ctrl+5"], "go.settings": ["Ctrl+,"],
  "go.anywhere": ["Ctrl+K"], "help.shortcuts": ["F1", "Ctrl+/"],
  "session.nextAttention": ["Alt+Shift+Down"], "session.previousAttention": ["Alt+Shift+Up"],
  "session.new": ["Ctrl+N"], "session.rename": ["F2"], "session.search": ["Ctrl+F"],
  "view.refresh": ["Ctrl+R", "F5"], "panel.toggle": ["Ctrl+I"],
  "focus.next": ["F6"], "focus.previous": ["Shift+F6"],
  "message.send": ["Ctrl+Return"], "queue.sendNow": ["Ctrl+Shift+Return"],
  "zoom.in": ["Ctrl+=", "Ctrl++"], "zoom.out": ["Ctrl+-"], "zoom.reset": ["Ctrl+0"],
};

/**
 * The default chords of a command (Shortcuts.cpp portable()). macOS keeps
 * physical Control for tabs: Command-Tab and Command-` belong to the system.
 */
export function portableDefaults(id: string, on: Platform = platform): string[] {
  const mac = on === "macos";
  if (id in COMMON) return COMMON[id];
  if (id === "session.next") return ["Alt+Down", mac ? "Ctrl+Alt+Down" : "Ctrl+PgDown"];
  if (id === "session.previous") return ["Alt+Up", mac ? "Ctrl+Alt+Up" : "Ctrl+PgUp"];
  if (id === "tab.next") return [mac ? "Meta+Tab" : "Ctrl+Tab"];
  if (id === "tab.previous") return [mac ? "Meta+Shift+Tab" : "Ctrl+Shift+Tab"];
  if (id === "tab.terminal") return [mac ? "Meta+`" : "Ctrl+`"];
  if (id === "message.push") return [mac ? "Meta+X, Meta+S" : "Ctrl+X, Ctrl+S"];
  return [];
}

// ---------------------------------------------------------------------------
// Chords: modifiers in Qt's terms and a key in Qt's portable names.

export interface Combo { meta: boolean; ctrl: boolean; alt: boolean; shift: boolean; key: string }
export type Sequence = Combo[];

const MODIFIERS: Record<string, keyof Omit<Combo, "key">> = { meta: "meta", ctrl: "ctrl", alt: "alt", shift: "shift" };

/** One chord in portable text ("Ctrl+Shift+Return", "Ctrl++", "F5"). */
export function parseChord(text: string): Combo | undefined {
  const trimmed = text.trim();
  if (!trimmed) return undefined;
  let key: string, mods: string;
  if (trimmed === "+") { key = "+"; mods = ""; }
  else if (trimmed.endsWith("++")) { key = "+"; mods = trimmed.slice(0, -2); }
  else {
    const at = trimmed.lastIndexOf("+");
    key = trimmed.slice(at + 1);
    mods = at < 0 ? "" : trimmed.slice(0, at);
  }
  if (!key) return undefined;
  const combo: Combo = { meta: false, ctrl: false, alt: false, shift: false, key: key.length === 1 ? key.toUpperCase() : key };
  for (const part of mods ? mods.split("+") : []) {
    const name = MODIFIERS[part.toLowerCase()];
    if (!name) return undefined;
    combo[name] = true;
  }
  return combo;
}

/** A sequence of up to four chords, separated by ", " in portable text. */
export function parseSequence(text: string): Sequence | undefined {
  const chords = text.split(", ").map(parseChord);
  return chords.length && chords.length <= 4 && chords.every((c) => c) ? (chords as Sequence) : undefined;
}

/** Portable text, modifiers in Qt's order. */
export function formatSequence(sequence: Sequence): string {
  return sequence.map((c) => `${c.meta ? "Meta+" : ""}${c.ctrl ? "Ctrl+" : ""}${c.alt ? "Alt+" : ""}${c.shift ? "Shift+" : ""}${c.key}`).join(", ");
}

const MAC_KEYS: Record<string, string> = {
  Return: "↵", Enter: "⌤", Tab: "⇥", Backspace: "⌫", Del: "⌦", Esc: "⎋", Home: "↖", End: "↘",
  Left: "←", Right: "→", Up: "↑", Down: "↓", PgUp: "⇞", PgDown: "⇟",
};

/** QKeySequence::NativeText: portable text on Linux and Windows, symbols on macOS. */
export function nativeText(sequence: Sequence, on: Platform = platform): string {
  if (on !== "macos") return formatSequence(sequence);
  return sequence.map((c) => `${c.meta ? "⌃" : ""}${c.alt ? "⌥" : ""}${c.shift ? "⇧" : ""}${c.ctrl ? "⌘" : ""}${MAC_KEYS[c.key] ?? c.key}`).join(", ");
}

export const sameCombo = (a: Combo, b: Combo) =>
  a.key === b.key && a.meta === b.meta && a.ctrl === b.ctrl && a.alt === b.alt && a.shift === b.shift;

const NAMED: Record<string, string> = {
  Enter: "Return", Tab: "Tab", Backspace: "Backspace", Delete: "Del", Insert: "Ins", Escape: "Esc", " ": "Space",
  ArrowUp: "Up", ArrowDown: "Down", ArrowLeft: "Left", ArrowRight: "Right", PageUp: "PgUp", PageDown: "PgDown",
  Home: "Home", End: "End", ContextMenu: "Menu",
};
// The US layout's symbols by key position, unshifted and with Shift.
const CODE_SYMBOLS: Record<string, string> = {
  Minus: "-", Equal: "=", BracketLeft: "[", BracketRight: "]", Backslash: "\\", Semicolon: ";", Quote: "'",
  Comma: ",", Period: ".", Slash: "/", Backquote: "`",
};
const SHIFTED_SYMBOLS: Record<string, string> = {
  Minus: "_", Equal: "+", BracketLeft: "{", BracketRight: "}", Backslash: "|", Semicolon: ":", Quote: "\"",
  Comma: "<", Period: ">", Slash: "?", Backquote: "~",
};

const SHIFTED_DIGITS = ")!@#$%^&*(";
const printable = (key: string) => /^[\x21-\x7e]$/.test(key);

/**
 * The key of an event in Qt's names, or undefined for a modifier or an
 * unknown key. With Shift (or Option on macOS), a digit or symbol key that
 * does not type a letter is its base key, as Qt's first candidate is (a
 * layout's letter there, AZERTY's M or Dvorak's W, stays the letter);
 * otherwise a printable ASCII
 * character is the key the layout names (AZERTY, Dvorak), and another
 * script (Cyrillic) falls back to the Latin key at that position, as Qt
 * does, so Ctrl+б is Ctrl+, on a Russian layout.
 */
export function keyOf(e: Pick<KeyboardEvent, "key" | "code" | "altKey" | "shiftKey">, on: Platform = platform): string | undefined {
  if (e.code === "NumpadEnter") return "Enter";
  if (/^F([1-9]|[12][0-9]|3[0-5])$/.test(e.key)) return e.key;
  if (e.key in NAMED) return NAMED[e.key];
  if ((e.shiftKey || (on === "macos" && e.altKey)) && !/^[A-Za-z]$/.test(e.key)) {
    if (/^Digit[0-9]$/.test(e.code)) return e.code.slice(5);
    if (e.code in CODE_SYMBOLS) return CODE_SYMBOLS[e.code];
  }
  if (printable(e.key)) return e.key.toUpperCase();
  if (/^Key[A-Z]$/.test(e.code)) return e.code.slice(3);
  if (/^Digit[0-9]$/.test(e.code)) return e.code.slice(5);
  if (e.code in CODE_SYMBOLS) return CODE_SYMBOLS[e.code];
  if (e.key.length === 1) return e.key.toUpperCase();
  return undefined;
}

type KeyEvent = Pick<KeyboardEvent, "key" | "code" | "altKey" | "ctrlKey" | "metaKey" | "shiftKey">;

/** The chord of a key event (Shortcuts::combination), Qt's first candidate; macOS Command is Qt's Ctrl. */
export function comboFromEvent(e: KeyEvent, on: Platform = platform): Combo | undefined {
  const key = keyOf(e, on);
  if (!key) return undefined;
  const mac = on === "macos";
  return { meta: mac ? e.ctrlKey : e.metaKey, ctrl: mac ? e.metaKey : e.ctrlKey, alt: e.altKey, shift: e.shiftKey, key };
}

/**
 * The chords Qt tries for an event (QKeyMapper::possibleKeys): the base key
 * with every modifier, then a shifted digit's or symbol's character without
 * Shift, so Ctrl+Shift+= also matches Ctrl++.
 */
export function candidates(e: KeyEvent, on: Platform = platform): Combo[] {
  const combo = comboFromEvent(e, on);
  if (!combo) return [];
  if (!combo.shift) return [combo];
  const digit = /^Digit([0-9])$/.exec(e.code)?.[1];
  const shifted = printable(e.key) ? e.key
    : digit !== undefined ? SHIFTED_DIGITS[Number(digit)]
    : SHIFTED_SYMBOLS[e.code];
  const key = shifted?.toUpperCase();
  if (key && key !== combo.key && !/^[A-Z]$/.test(key)) return [combo, { ...combo, shift: false, key }];
  // A layout's own shifted symbol on another key: also without Shift.
  if (combo.key.length === 1 && !/[A-Z0-9]/.test(combo.key)) return [combo, { ...combo, shift: false }];
  return [combo];
}

/** QKeySequence::matches: typed so far against a bound sequence. */
export function matchSequence(bound: Sequence, typed: Sequence): "full" | "partial" | false {
  if (!typed.length || typed.length > bound.length) return false;
  for (let i = 0; i < typed.length; i++) if (!sameCombo(bound[i], typed[i])) return false;
  return typed.length === bound.length ? "full" : "partial";
}

const overlaps = (a: Sequence, b: Sequence) => matchSequence(a, b) !== false || matchSequence(b, a) !== false;

// ---------------------------------------------------------------------------
// Rules (Shortcuts.cpp)

const anyChord = (c: Combo) => c.ctrl || c.alt || c.meta;
const functionKey = (key: string) => /^F([1-9]|[12][0-9]|3[0-5])$/.test(key);
const only = (c: Combo, mods: Partial<Omit<Combo, "key">>) =>
  c.meta === !!mods.meta && c.ctrl === !!mods.ctrl && c.alt === !!mods.alt && c.shift === !!mods.shift;

/** Ctrl is Command on macOS, so the same combinations serve both families. */
function editingKey(c: Combo): boolean {
  if (only(c, { ctrl: true }) && ["C", "X", "V", "Z", "Y", "A", "Ins"].includes(c.key)) return true;
  if (only(c, { ctrl: true, shift: true }) && c.key === "Z") return true;
  return ["Left", "Right", "Home", "End", "Backspace", "Del"].includes(c.key) && anyChord(c);
}

/** Cut and Copy do nothing without a selection, so they may start a sequence. */
export const selectionKey = (c: Combo) => only(c, { ctrl: true }) && (c.key === "X" || c.key === "C");

/** Why a sequence cannot be assigned (Shortcuts::assignable), or undefined. */
export function assignable(sequence: Sequence, on: Platform = platform): string | undefined {
  if (!sequence.length) return tr("Press a key combination.");
  if (sequence.length > 2) return tr("Use one key combination or a sequence of two.");
  const mac = on === "macos";
  for (let i = 0; i < sequence.length; i++) {
    const c = sequence[i];
    if (["Shift", "Ctrl", "Alt", "Meta", "AltGr"].includes(c.key)) return tr("Press a key together with the modifiers.");
    if ((c.key === "F10" && only(c, { shift: true })) || c.key === "Menu") return tr("This key opens context menus.");
    if (!anyChord(c) && !functionKey(c.key))
      return mac ? tr("Keys without ⌘, ⌃ or ⌥ are used for typing and navigation.") : tr("Keys without Ctrl or Alt are used for typing and navigation.");
    if (!mac && only(c, { ctrl: true, shift: true }) && (c.key === "C" || c.key === "V")) return tr("Terminal uses this key to copy and paste.");
    if (editingKey(c) && !(i === 0 && sequence.length > 1 && selectionKey(c))) return tr("Text fields use this key for editing.");
  }
  return undefined;
}

/** While Terminal has focus, only these may reach a window command. */
export function terminalSafe(c: Combo, on: Platform = platform): boolean {
  if (on === "macos") {
    if (c.ctrl) return !(only(c, { ctrl: true }) && (c.key === "C" || c.key === "V"));
    return (only(c, { meta: true }) || only(c, { meta: true, shift: true })) && (c.key === "Tab" || c.key === "`");
  }
  if (!only(c, { ctrl: true }) && !only(c, { ctrl: true, shift: true })) return false;
  if (/^[0-9]$/.test(c.key)) return true;
  if (["Tab", "PgUp", "PgDown", "`", ","].includes(c.key)) return true;
  return c.shift && (c.key === "Return" || c.key === "Enter");
}

/** Editing keys a text field keeps even when a command uses them. */
export function protectedInTextField(c: Combo): boolean {
  if (!anyChord(c)) return !functionKey(c.key);
  return editingKey(c);
}

/** The Terminal rule in one sentence, for Settings. */
export function terminalRule(on: Platform = platform): string {
  return on === "macos"
    ? tr("In Terminal, a command's chord reaches Zerus only when it uses ⌘ (except ⌘C and ⌘V) or is ⌃Tab, ⌃⇧Tab, ⌃` or ⌃⇧`. Every other ⌃ chord and every ⌥ chord goes to the agent, bound or not.")
    : tr("In Terminal, a command's chord reaches Zerus only when it is Ctrl or Ctrl+Shift with a digit, Tab, Page Up, Page Down, backtick or comma, or Ctrl+Shift+Enter. Everything else goes to the agent, bound or not.");
}

// ---------------------------------------------------------------------------
// The registry: defaults overridden by the settings store (ShortcutRegistry).

const settingKey = (id: string) => `shortcuts/${id}`;
const defaults = (id: string, on: Platform) => portableDefaults(id, on).map(parseSequence).filter((s): s is Sequence => !!s);

/** The current chords of a command; reactive through the settings store. */
export function keys(id: string, on: Platform = platform): Sequence[] {
  if (!command(id)) return [];
  const stored = settings[settingKey(id)];
  if (!Array.isArray(stored)) return defaults(id, on);
  return stored.map(parseSequence).filter((s): s is Sequence => !!s && s.length > 0);
}

export function shortcutTooltip(label: string, command: string): string {
  const first = keys(command)[0];
  return first ? `${label} (${nativeText(first)})` : label;
}

export const customized = (id: string) => Array.isArray(settings[settingKey(id)]);

/** Stores a command's chords; its defaults are stored as an absence, so they follow future default changes. */
export function setKeys(id: string, sequences: Sequence[], on: Platform = platform) {
  if (!command(id)) return;
  const chords = sequences.filter((s) => s.length).map(formatSequence);
  const usual = defaults(id, on).map(formatSequence);
  void setSetting(settingKey(id), chords.length === usual.length && chords.every((c, i) => c === usual[i]) ? null : chords);
}

export const reset = (id: string) => void setSetting(settingKey(id), null);

/** Removes every shortcuts/ override, as Qt removes the group (ShortcutRegistry::resetAll). */
export function resetAll() {
  for (const key of Object.keys(settings)) if (key.startsWith("shortcuts/") && settings[key] != null) void setSetting(key, null);
}

/** Every other command whose chord equals the sequence or is a prefix either way, in command order. */
export function conflicts(id: string, sequence: Sequence, on: Platform = platform): string[] {
  if (!sequence.length) return [];
  return COMMANDS.filter((c) => c.id !== id && keys(c.id, on).some((key) => overlaps(key, sequence))).map((c) => c.id);
}
export { overlaps };

/** The first chord in native text, for keyboard settings and command hints. */
export function chord(id: string, on: Platform = platform): string {
  const current = keys(id, on);
  return current.length ? nativeText(current[0], on) : "";
}

// ---------------------------------------------------------------------------
// The router (ShortcutRouter, QShortcutMap): one capture listener and one
// map for window and composer commands. A partial match is held until the
// next key (Qt has no timeout); Escape and a focus change start over. Bound
// commands win over Terminal and text fields under their rules.

export interface SequenceState { typed: Sequence }
export const sequenceState = (): SequenceState => ({ typed: [] });

export type Context = "terminal" | "composer" | "text-field" | "window" | "none";

/** What the router reads of a focus target; a DOM element has all of it. */
interface Focused {
  closest(selector: string): { getAttribute(name: string): string | null } | null;
  matches(selector: string): boolean;
  isContentEditable?: boolean;
  tagName?: string;
}

const focused = (target: EventTarget | null): Focused | null => {
  const el = target as Partial<Focused> | null;
  return el && typeof el.closest === "function" && typeof el.matches === "function" ? (el as Focused) : null;
};

/**
 * Whether a modal dialog or an open popup (a menu, a panel marked
 * data-shortcut-context="none") is up: they take every key, wherever the
 * focus has fallen.
 */
export function popupOpen(doc: Pick<Document, "querySelector"> | undefined = typeof document === "undefined" ? undefined : document): boolean {
  if (!doc) return false;
  try {
    return !!doc.querySelector('dialog:modal, [data-shortcut-context="none"]');
  } catch {
    // An engine without :modal.
    return !!doc.querySelector('dialog[open], [data-shortcut-context="none"]');
  }
}

export function contextOf(target: EventTarget | null, popup: boolean = popupOpen()): Context {
  if (popup) return "none";
  const el = focused(target);
  // A modal dialog is its own window in Qt, and the Keyboard page's capture
  // field records keys: neither runs commands.
  if (el?.closest("dialog[open]")) return "none";
  const marked = el?.closest("[data-shortcut-context]")?.getAttribute("data-shortcut-context");
  if (marked === "capture" || marked === "none") return "none";
  if (marked === "terminal" || marked === "composer") return marked;
  // xterm's hidden text area is Terminal, marked or not.
  if (el?.closest(".xterm")) return "terminal";
  if (el && (el.matches("input, textarea, select") || el.isContentEditable)) return "text-field";
  return "window";
}

function hasSelection(target: EventTarget | null): boolean {
  const field = target as { selectionStart?: unknown; selectionEnd?: unknown } | null;
  if (field && typeof field.selectionStart === "number") return field.selectionStart !== field.selectionEnd;
  const selection = typeof getSelection === "undefined" ? null : getSelection();
  return !!selection && !selection.isCollapsed && typeof Node !== "undefined" && target instanceof Node
    && selection.anchorNode !== null && target.contains(selection.anchorNode);
}

/** Whether the focus lets a bound sequence starting with `first` through. */
export function allowed(context: Context, first: Combo, bound: Sequence, selection: boolean, on: Platform = platform): boolean {
  if (context === "terminal") return terminalSafe(first, on);
  if (context === "text-field" || context === "composer")
    return !protectedInTextField(first) || (bound.length > 1 && selectionKey(first) && !selection);
  return true;
}

/** A drop-down list opens with Alt+Up, Alt+Down and F4 (Shortcuts.cpp comboPopupKey). */
const listPopupKey = (target: EventTarget | null, c: Combo) =>
  focused(target)?.tagName === "SELECT"
  && ((only(c, { alt: true }) && (c.key === "Up" || c.key === "Down")) || (only(c, {}) && c.key === "F4"));

/** What a handler is limited to: the focus must be inside it (a message field). */
export interface Root { contains(target: Node | null): boolean }
interface Handler { run: () => void; root?: Root }

// The handlers per command: the newest one whose root holds the focus runs,
// and removing it restores the one before.
const handlers = new Map<string, Handler[]>();

/**
 * The handler that runs `id` for a key aimed at `target`: of those whose
 * root holds the target, the innermost root's, then the newest.
 */
export function handlerFor(id: string, target: EventTarget | null): (() => void) | undefined {
  const stack = handlers.get(id) ?? [];
  let best: Handler | undefined;
  for (let i = stack.length - 1; i >= 0; i--) {
    const entry = stack[i];
    if (entry.root && !entry.root.contains(target as Node | null)) continue;
    const inner = !best || (entry.root && (!best.root || (best.root !== entry.root && best.root.contains(entry.root as unknown as Node))));
    if (inner) best = entry;
  }
  return best?.run;
}

/**
 * Runs `handler` for a command: a window command anywhere in the window, a
 * composer command while the focus is inside `root`, its message field.
 * Returns the unregister function.
 */
export function onCommand(id: string, handler: () => void, root?: Root): () => void {
  const entry: Handler = { run: handler, root };
  const stack = handlers.get(id) ?? [];
  stack.push(entry);
  handlers.set(id, stack);
  return () => {
    const current = handlers.get(id);
    const at = current?.lastIndexOf(entry) ?? -1;
    if (!current || at < 0) return;
    current.splice(at, 1);
    if (!current.length) handlers.delete(id);
  };
}

// In a message field its own commands come first, as Qt's field keeps the
// keys its actions claim (ClaimedKeys).
const COMPOSER_FIRST = [...COMMANDS.filter((c) => c.scope === "composer"), ...COMMANDS.filter((c) => c.scope !== "composer")];

const windowState = sequenceState();

type RouterEvent = Pick<KeyboardEvent, "key" | "code" | "altKey" | "ctrlKey" | "metaKey" | "shiftKey" | "target" | "isComposing" | "keyCode">
  & { getModifierState?: (key: string) => boolean };

/** The router's decision for one key event: the command to run, a partial match to hold, or nothing. */
export function route(event: RouterEvent, state: SequenceState = windowState, on: Platform = platform): { run?: string; consume: boolean } {
  // An input method composing text owns its keys.
  if (event.isComposing || event.keyCode === 229 || event.key === "Process") return { consume: false };
  const tried = candidates(event, on);
  const combo = tried[0];
  if (!combo) return { consume: false };
  const pending = state.typed.length > 0;
  if (pending && only(combo, {}) && combo.key === "Esc") { state.typed = []; return { consume: true }; }
  const context = contextOf(event.target);
  const typing = context === "text-field" || context === "composer";
  // AltGr types characters; on Windows it arrives as Ctrl+Alt.
  if (context === "none" || (typing && event.getModifierState?.("AltGraph")) || listPopupKey(event.target, combo)) {
    state.typed = [];
    return { consume: false };
  }
  const selection = hasSelection(event.target);
  let full: string | undefined;
  let partial: Combo | undefined;
  for (const c of context === "composer" ? COMPOSER_FIRST : COMMANDS) {
    if ((c.scope === "composer" && context !== "composer") || !handlerFor(c.id, event.target)) continue;
    for (const candidate of tried) {
      const typed = [...state.typed, candidate];
      for (const key of keys(c.id, on)) {
        const result = matchSequence(key, typed);
        if (!result || !allowed(context, typed[0], key, selection, on)) continue;
        if (result === "full" && !full) full = c.id;
        else if (result === "partial" && !partial) partial = candidate;
      }
    }
  }
  if (full) { state.typed = []; return { run: full, consume: true }; }
  if (partial) { state.typed = [...state.typed, partial]; return { consume: true }; }
  state.typed = [];
  // Qt's map handled the key that ended a partial match.
  return { consume: pending };
}

/** Installs the router on `target` in the capture phase; returns the uninstall function. */
export function installShortcutRouter(target: Window = window): () => void {
  const keydown = (event: KeyboardEvent) => {
    if (event.defaultPrevented) return;
    const decision = route(event);
    if (!decision.consume) return;
    event.preventDefault();
    event.stopPropagation();
    if (decision.run) handlerFor(decision.run, event.target)?.();
  };
  const restart = () => { windowState.typed = []; };
  target.addEventListener("keydown", keydown, true);
  target.addEventListener("focusin", restart, true);
  return () => {
    target.removeEventListener("keydown", keydown, true);
    target.removeEventListener("focusin", restart, true);
  };
}
