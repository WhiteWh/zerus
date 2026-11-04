import type { Details, Target } from "../types";
import type { Session } from "../ui/sessions/types";
import type { ChildIdentity } from "../ui/sessions/actions-ui";
import { snapshotOf } from "../ui/sessions/actions-ui";
import type { MenuEntry } from "./ui";
import { computerOffline } from "./catalog";
import { stillListed } from "./actions";
import { toast } from "./notices";
import * as bridge from "../bridge";
import { tr } from "../i18n";

export interface FolderRequest {
  target: Target;
  archive_id: string | null;
  agent_id: string | null;
  directory: string;
}
export interface FolderStatus { folder: boolean; shell: boolean; folder_hint: string }

/** A child with no reported folder has no folder action; never borrow its parent's. */
export function folderRequest(s: Session, details?: Details, child?: ChildIdentity): FolderRequest {
  return {
    target: { computer: s.computer, name: s.name, run_id: details?.run_id || s.runId || "", conversation_id: details?.conversation_id || s.conversationId || "" },
    archive_id: s.archiveId || null,
    agent_id: child?.id ?? null,
    directory: child ? child.cwd ?? "" : details?.cwd ?? s.cwdPath ?? "",
  };
}

/** The existing header buttons and menus share exactly the same click path. */
export function folderEntries(s: Session, request: FolderRequest, status?: FolderStatus): { folder: Extract<MenuEntry, { label: string }>; shell: Extract<MenuEntry, { label: string }> } {
  const shown = snapshotOf(s);
  const run = async (kind: "folder" | "shell") => {
    if (!stillListed(shown)) return;
    try {
      const advisory = await bridge.sessionFolderOpen(request, kind);
      if (advisory) toast("Zerus: session opened, but not the way you asked", advisory, "warn");
    } catch (error) {
      toast(kind === "shell" ? tr("Could not open folder shell") : tr("Open folder"), String(error), "error", true);
    }
  };
  const identified = !!request.target.run_id && !!request.target.conversation_id;
  return {
    folder: { label: tr("Open folder"), icon: "folder-open", disabled: !identified || !status?.folder,
      hint: status?.folder_hint ?? (s.computer ? tr("Available only for sessions on this computer") : tr("Session folder is unavailable")), action: () => { void run("folder"); } },
    shell: { label: tr("Open shell in session folder"), icon: "square-terminal", disabled: !identified || !status?.shell || computerOffline(s.computer),
      hint: tr("Open shell in session folder"), action: () => { void run("shell"); } },
  };
}
