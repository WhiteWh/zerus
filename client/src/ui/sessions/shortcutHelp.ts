import { COMMANDS, keys, nativeText, parseSequence, platform, type Platform } from "../../store/shortcuts";
import { tr, trNoop } from "../../i18n";

export interface ShortcutHelpRow { group: string; title: string; chord: string }

const native = (text: string) => nativeText(parseSequence(text)!);

export function fixedShortcutRows(on: Platform = platform): ShortcutHelpRow[] {
  const fixed = tr("Fixed keys");
  const terminalCopy = on === "macos" ? "⌘C, ⌘V" : "Ctrl+Shift+C, Ctrl+Shift+V";
  return [
    { group: fixed, title: tr("Send the message"), chord: native("Return") },
    { group: fixed, title: tr("New line in the message field"), chord: native("Shift+Return") },
    { group: fixed, title: tr("Clear the search, then stop the selected turn; close dialogs"), chord: native("Escape") },
    { group: fixed, title: tr("Move focus"), chord: `${native("Tab")}, ${native("Shift+Tab")}` },
    { group: fixed, title: tr("Take the suggestion in an empty message field"), chord: native("Tab") },
    { group: fixed, title: tr("Move and open in lists"), chord: `${native("Up")} ${native("Down")} ${native("Home")} ${native("End")} ${native("Return")}` },
    { group: fixed, title: tr("Context menu"), chord: `${native("Menu")}, ${native("Shift+F10")}` },
    { group: fixed, title: tr("Select several items"), chord: tr("%1-click", on === "macos" ? "⌘" : "Ctrl") },
    { group: fixed, title: tr("Stop voice capture"), chord: `${native("Space")}, ${native("Escape")}` },
    { group: fixed, title: tr("Copy and paste in Terminal"), chord: terminalCopy },
    { group: fixed, title: tr("Scroll Terminal"), chord: `${native("Shift+PageUp")}, ${native("Shift+PageDown")}` },
    { group: fixed, title: tr("Reorder projects in the Projects dialog"), chord: `${native("Alt+Up")}, ${native("Alt+Down")}` },
  ];
}

const GROUPS: Record<string, string> = { "Go to": trNoop("Navigation"), Sessions: trNoop("Sessions"), "Session view": trNoop("Sessions"), Messages: trNoop("Composer"), "Content scale": trNoop("Composer") };

export function shortcutHelpRows(on: Platform = platform): ShortcutHelpRow[] {
  return [
    ...COMMANDS.map((command) => ({ group: tr(GROUPS[command.group] ?? command.group), title: tr(command.title), chord: keys(command.id, on).map((key) => nativeText(key, on)).join(", ") })),
    ...fixedShortcutRows(on),
  ];
}

export function filterShortcutRows(rows: readonly ShortcutHelpRow[], needle: string): ShortcutHelpRow[] {
  const query = needle.trim().toLowerCase();
  if (!query) return [...rows];
  return rows.filter((row) => row.title.toLowerCase().includes(query) || row.chord.toLowerCase().includes(query));
}
