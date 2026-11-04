import { batch } from "solid-js";
import { catalog, computerOffline } from "../../store/catalog";
import { nativeUi } from "../../store/native-ui";
import { setDetailTab, setPage, setSelectedKey, setSubagentOpen, type MenuEntry } from "../../store/ui";
import type { Session } from "./types";
import type { NativeBinding } from "../../store/native-ui-model";
import { tr } from "../../i18n";

export function nativeBinding(s: Session | undefined): NativeBinding | undefined {
  return s?.agent === "deepseek" && s.runId && s.conversationId ? {
    catalog_key: s.catalogKey,
    target: { computer: s.computer, name: s.name, run_id: s.runId, conversation_id: s.conversationId },
  } : undefined;
}

/** Capture the original row before a menu opens or a local inspect awaits. */
export function nativeRoute(s: Session | undefined, browserFallback = false): () => void {
  const binding = nativeBinding(s), key = s?.key;
  return () => {
    const live = key ? catalog.rows[key] : undefined;
    if (!live || !binding || JSON.stringify(nativeBinding(live)) !== JSON.stringify(binding)) return;
    batch(() => { setSelectedKey(key); setSubagentOpen(undefined); setPage("sessions"); setDetailTab("native"); });
    nativeUi.select(binding, !computerOffline(live.computer));
    if (browserFallback) nativeUi.fromMenu();
  };
}

export function nativeEntry(s: Session): MenuEntry | undefined {
  if (s.agent !== "deepseek" || s.state === "archived") return undefined;
  return { label: tr("Open native DeepSeek UI"), icon: "external-link", disabled: computerOffline(s.computer) || !nativeBinding(s),
    hint: computerOffline(s.computer) ? "Native UI is unavailable while this machine is offline." : undefined,
    action: nativeRoute(s, true) };
}
