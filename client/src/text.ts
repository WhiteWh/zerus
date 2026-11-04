import { tr, type TranslationArgument } from "./i18n";

// Interface prose from the Rust core (`zerus_client_core::text::Text`): the
// English catalog source and typed arguments. The core holds no language; the
// webview translates here. Nested prose is translated too; verbatim values
// (identities, paths, branch names, agent output) and numbers never are.

export type TextArg =
  | { kind: "text"; value: Text }
  | { kind: "verbatim"; value: string }
  | { kind: "number"; value: number };
export interface Text { source: string; args?: TextArg[] }

/** A core Text through `translate`: nested prose is translated, other arguments are not. */
export function renderText(text: Text, translate: (source: string, ...args: TranslationArgument[]) => string): string {
  const args = (text.args ?? []).map((arg): TranslationArgument => (arg.kind === "text" ? renderText(arg.value, translate) : arg.value));
  return translate(text.source, ...args);
}

/** A core Text in the interface language. */
export const trText = (text: Text): string => renderText(text, tr);
