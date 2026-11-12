import { Show, For, createSignal, createResource, onMount, onCleanup } from "solid-js";
import { terminationCleanupManualOffer, terminationCleanupManualConfirm, terminationCleanupManualCancel } from "../../bridge";
import { applyCleanup, emptyConsents } from "../../store/terminationCleanup";
import { tr } from "../../i18n";

/** Native-selected folder and one-use confirmation; no renderer path authority. */
export function RemoveAnywayDialog(props: { id: string; close: () => void }) {
  let dialog!: HTMLDialogElement;
  const id = props.id;
  const pending = terminationCleanupManualOffer(id);
  const [offer] = createResource(() => pending);
  const [consents, setConsents] = createSignal(emptyConsents());
  const [sending, setSending] = createSignal(false);
  const [error, setError] = createSignal("");
  let consumed = false;
  const cancel = () => { if (!sending()) props.close(); };
  // Loading first: reading a pending offer() here would suspend the review's
  // Suspense boundary and detach the whole review until the offer answers.
  const allowed = () => {
    if (offer.loading || offer.error) return false;
    const v = offer(), c = consents();
    return v?.ready && !sending() && (!v.required.local_files || c.local_files)
      && (!v.required.discard_changes || c.discard_changes) && (!v.required.unchecked_processes || c.unchecked_processes);
  };
  const confirm = async () => {
    if (!allowed()) return; const v = offer(); if (!v) return;
    setSending(true); consumed = true;
    try { applyCleanup(await terminationCleanupManualConfirm(id, v.confirmation, consents())); props.close(); }
    catch (e) { setError(String(e)); }
    finally { setSending(false); }
  };
  onMount(() => { dialog.showModal(); queueMicrotask(() => dialog.querySelector<HTMLButtonElement>("button")?.focus()); });
  onCleanup(() => {
    dialog.close();
    if (!consumed) void pending.then((v) => v && terminationCleanupManualCancel(v.confirmation)).catch(() => {});
  });
  return <dialog ref={dialog} class="dlg su-dlg" style={{width: "680px"}} aria-labelledby="manual-remove-title" onCancel={(event) => { event.preventDefault(); cancel(); }}>
    <div class="dlg-head"><h2 id="manual-remove-title">{tr("Remove this working folder anyway?")}</h2></div>
    <div class="dlg-body">
      <p class="warn">{tr("Zerus did not create this folder. Removing it permanently deletes its local files. The branch and conversation are kept.")}</p>
      <Show when={!offer.loading} fallback={<p role="status">{tr("Checking the folder…")}</p>}>
        <Show when={!offer.error} fallback={<p role="alert">{String(offer.error)}</p>}>
          <p class="mono cleanup-source">{offer()?.path}</p>
          <p>{tr("Computer: %1", offer()?.computer ?? "")}<br />{tr("Branch kept: %1", offer()?.branch ?? "")}<br />{tr("Size: %1", offer()?.size ?? "")}<br />{tr("Changed files: %1; untracked files: %2", offer()?.changes ?? 0, offer()?.untracked ?? 0)}</p>
          <p style={{"white-space": "pre-wrap", "overflow-wrap": "anywhere"}}>{offer()?.reason}</p>
          <For each={offer()?.sessions}>{(name) => <p>{name}</p>}</For>
          <For each={offer()?.processes}>{(program) => <p>{program.label} (PID {program.pid})</p>}</For>
          <Show when={!offer()?.ready}><p class="warn">{tr("Stop the listed programs and sessions first, then review this folder again. No folder will be removed from this review.")}</p></Show>
          <Show when={offer()?.required.local_files}><p>{tr("Local files outside Git: %1 entries, %2", offer()?.ignored_count ?? 0, offer()?.ignored_size ?? "")}</p><ul><For each={offer()?.ignored}>{(path) => <li class="mono cleanup-source">{path}</li>}</For></ul><label class="check"><input type="checkbox" disabled={sending()} checked={consents().local_files} onChange={(e) => setConsents({...consents(), local_files: e.currentTarget.checked})} />{tr("Delete the reviewed local files outside Git too")}</label></Show>
          <Show when={offer()?.required.discard_changes}><label class="check"><input type="checkbox" disabled={sending()} checked={consents().discard_changes} onChange={(e) => setConsents({...consents(), discard_changes: e.currentTarget.checked})} />{tr("Permanently discard uncommitted and untracked files")}</label></Show>
          <Show when={offer()?.required.unchecked_processes}><label class="check"><input type="checkbox" disabled={sending()} checked={consents().unchecked_processes} onChange={(e) => setConsents({...consents(), unchecked_processes: e.currentTarget.checked})} />{tr("I checked this folder is not in use; accept hidden program paths for five minutes")}</label></Show>
        </Show>
      </Show>
      <Show when={error()}><p role="alert">{error()} {tr("Close and review the recorded outcome. This confirmation will not be replayed.")}</p></Show>
    </div>
    <div class="dlg-foot"><span class="grow" /><button class="btn" disabled={sending()} onClick={cancel}>{tr("Cancel")}</button><button class="btn solid-danger" disabled={!allowed() || consumed} onClick={() => void confirm()}>{tr("Remove working folder")}</button></div>
  </dialog>;
}
