import { createTaskEditor } from "./taskEditor";
import { taskFormOpen, taskFormEdit, taskFormSubmit, taskFormCancel } from "../bridge";
import { exitFlush } from "./exitFlush";
import { notify } from "./actions";
import { groupOf } from "./organization";
import { sessionIdentity } from "../ui/screens/tasks-ui";
import type { Session } from "../ui/sessions/types";
import { tr } from "../i18n";
export type FormKind = "title" | "new_card";
export interface FormSubmit {
  kind: FormKind; handle: number; rev: number; recover: boolean; backlog: boolean;
  target: { computer: string; name: string; run_id: string; conversation_id: string } | null;
  catalog_key: string | null; session_id: string | null;
}
let creation: Pick<FormSubmit, "backlog" | "target" | "catalog_key" | "session_id"> | undefined;
function makeForm(kind: FormKind) {
  const form = createTaskEditor<FormKind>({
    open: taskFormOpen, edit: (input) => taskFormEdit(kind, input),
    pick: () => Promise.reject(new Error("Task titles do not have attachments.")),
    remove: () => Promise.reject(new Error("Task titles do not have attachments.")),
    submit: (handle, rev, recover) => taskFormSubmit({ kind, handle, rev, recover, backlog: false, target: null, catalog_key: null, session_id: null, ...(kind === "new_card" ? creation : {}) }),
    discard: (handle, rev) => taskFormCancel(kind, handle, rev),
  }, (message) => notify("error", message));
  exitFlush.register(form.flushForExit);
  return form;
}
export const taskTitle = makeForm("title"), newCard = makeForm("new_card");
export async function createCard(project: string, backlog: boolean, session?: Session) {
  if (newCard.locked() || newCard.view()?.project !== project) return false;
  if (session && groupOf(session.key) !== project) { notify("error", tr("The selected project changed. The new task draft was kept.")); return false; }
  const id = session && session.tracked && session.state !== "archived" ? sessionIdentity(session) : "";
  creation = { backlog, target: id && session ? { computer: session.computer, name: session.name, run_id: session.runId ?? "", conversation_id: session.conversationId ?? "" } : null,
    catalog_key: id ? session!.catalogKey : null, session_id: id || null };
  try { return await newCard.submit(!!newCard.view()?.recovery); }
  finally { creation = undefined; }
}
