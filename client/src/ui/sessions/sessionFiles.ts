import { createSignal } from "solid-js";
import {
  onSessionFileChanged,
  sessionFileAct,
  sessionFileClose,
  sessionFileCheck,
  onSessionFileNotice,
  sessionFileDialog,
  sessionFileDownload,
  sessionFileExternal,
  sessionFileSave,
  type SessionFileView,
} from "../../bridge";
import { toast } from "../../store/ui";
import type { Session } from "./types";
import { tr } from "../../i18n";

const [sessionFile, setSessionFile] = createSignal<SessionFileView>();
export { sessionFile };

let generation = 0;
function apply(id: number, view: SessionFileView | null) {
  const current = sessionFile();
  if (!view || current?.id !== id || view.id !== id) return;
  if (view.closed) { setSessionFile(undefined); return; }
  if (view.revision >= current.revision) setSessionFile(view);
}
onSessionFileChanged((view) => apply(view.id, view));
onSessionFileNotice((notice) => toast(tr("File reference"), notice.text, notice.warn ? "warn" : "info", notice.warn));

function report(error: unknown) { toast(tr("File reference"), String(error), "warn", true); }
export function openExternal(url: string) { void sessionFileExternal(url).catch(report); }

export function openSessionFile(session: Session, href: string) {
  const request = ++generation;
  const old = sessionFile();
  if (old) { setSessionFile(undefined); void sessionFileClose(old.id).catch(report); }
  void sessionFileDialog(session.computer, session.catalogKey, href, session.cwdPath || session.cwdCanonical).then((view) => {
    if (request !== generation) { void sessionFileClose(view.id).catch(report); return; }
    setSessionFile(view);
    if (view.remote && view.path) void sessionFileCheck(view.id).then((result) => apply(view.id, result)).catch(report);
  }).catch(report);
}
export function closeSessionFile() {
  ++generation;
  const current = sessionFile();
  setSessionFile(undefined);
  if (current) void sessionFileClose(current.id).catch(report);
}
export function actOnSessionFile(action: "open_file" | "open_folder" | "open_copy" | "show_file" | "open_ssh") {
  const current = sessionFile();
  if (current) void sessionFileAct(current.id, action).then((view) => apply(current.id, view)).catch(report);
}
export function downloadSessionFile(open: boolean) {
  const current = sessionFile();
  if (current) void sessionFileDownload(current.id, open).then((view) => apply(current.id, view)).catch(report);
}
export function saveSessionFile() {
  const current = sessionFile();
  if (current) void sessionFileSave(current.id).then((view) => apply(current.id, view)).catch(report);
}
