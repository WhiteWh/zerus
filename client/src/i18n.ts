import { createSignal } from "solid-js";
import { settingsReady, text } from "./store/settings";

// Generated from Qt's reviewed, unambiguous finished entries by
// client/scripts/generate-i18n-catalog.py. Explicit tr() calls keep agent
// messages, command output, identities and filenames out of translation.
let RU: Record<string, string | string[]> = {};
export type UiLanguage = "en" | "ru";
export type TranslationArgument = string | number;

export function preferredLanguage(value: unknown): UiLanguage {
  return value === "ru" ? "ru" : "en";
}

function russianForm(count: number): number {
  const n = Math.abs(Math.trunc(count));
  if (n % 10 === 1 && n % 100 !== 11) return 0;
  if (n % 10 >= 2 && n % 10 <= 4 && (n % 100 < 12 || n % 100 > 14)) return 1;
  return 2;
}

export function translateWithCatalog(catalog: Record<string, string | string[]>, language: UiLanguage, source: string, ...args: TranslationArgument[]): string {
  const translated = language === "ru" ? catalog[source] : undefined;
  const template = Array.isArray(translated)
    ? translated[russianForm(Number(args[0] ?? 0))]
    : translated ?? source;
  return template.replace(/%L?(\d+)|%n/g, (token, index: string | undefined) => {
    const argument = token === "%n" ? args[0] : args[Number(index) - 1];
    return argument === undefined ? token : String(argument);
  });
}

export function translate(language: UiLanguage, source: string, ...args: TranslationArgument[]): string {
  return translateWithCatalog(RU, language, source, ...args);
}

const [language, setLanguage] = createSignal<UiLanguage>("en");
let installed = false;

/** Applies the stored choice once. A later edit still needs the promised restart. */
export function installUiLanguage() {
  if (installed) return;
  installed = true;
  void settingsReady.then(async () => {
    const preferred = preferredLanguage(text("workspace/language"));
    if (preferred === "ru") {
      try {
        RU = (await import("./i18n.ru.json")).default;
      } catch (error) {
        console.warn(`interface language: Russian catalog could not be loaded: ${String(error)}`);
        setLanguage("en");
        return;
      }
    }
    setLanguage(preferred);
  });
}

export const uiLanguage = language;
/** Qt's QT_TR_NOOP: marks a source for the coverage test; translate it with tr() where shown. */
export const trNoop = <T extends string>(source: T): T => source;
export const tr = (source: string, ...args: TranslationArgument[]) => translate(language(), source, ...args);
/** Qt's tr(source, disambiguation) for a source that reads differently in one use ("Pause" in media playback). */
export const trc = (disambiguation: string, source: string, ...args: TranslationArgument[]) => {
  const key = `${disambiguation}\u0004${source}`;
  return language() === "ru" && key in RU ? translate("ru", key, ...args) : translate("en", source, ...args);
};
