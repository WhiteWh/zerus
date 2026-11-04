import { batch } from "solid-js";
import { setFolderFilter, setProjectFilter, setAccountFilter, setStateFilter, setMachineFilter, setSearch, setPage, toast } from "./ui";
import type { FolderScope } from "../ui/sessions/scope-filters";
import { tr } from "../i18n";

/** SessionsWindow::filterFolder: a folder action replaces project/account scopes. */
export function filterFolder(folder: FolderScope) {
  batch(() => { setProjectFilter([]); setAccountFilter([]); setFolderFilter(folder); setPage("sessions"); });
  toast(tr("Folder filter"), tr("The list shows the sessions in %1", folder.path));
}
/** The mockup's Worktrees All sessions action. */
export function clearFolderFilter() {
  batch(() => { setFolderFilter(undefined); setProjectFilter([]); setAccountFilter([]); setStateFilter("all"); setMachineFilter([]); setSearch(""); setPage("sessions"); });
  toast(tr("All sessions"), tr("The folder filter was cleared"));
}

/** Qt's selected-folder chip clears only that scope. */
export function clearFolderScope() {
  setFolderFilter(undefined);
  toast(tr("All sessions"), tr("The folder filter was cleared"));
}
