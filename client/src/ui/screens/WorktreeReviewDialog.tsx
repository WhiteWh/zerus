import { For, Show, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { Icon } from "../lib/icons";
import { worktreeReviewDialog, worktreeReview, worktreeReviewLoading, worktreeReviewIssuing, worktreeReviewError, closeWorktreeReview, checkWorktreesAgain, prepareWorktreeRemoval, confirmWorktreeRemoval, cancelWorktreeConfirmation, type WorktreeConfirmation } from "../../store/worktreeCleanup";
import { tr } from "../../i18n";
const leaf = (path: string) => path.split(/[\\/]/u).filter(Boolean).at(-1) ?? path;
/** The mockup's worktree-cleanup and setup-cleanup-confirm, backed by the
 * existing guarded CLI review. Opening, selecting and inspecting never remove. */
export default function WorktreeReviewDialog() {
  let dialog!: HTMLDialogElement;
  const origin = worktreeReviewDialog()?.origin;
  const [checked, setChecked] = createSignal<Record<number, boolean>>({});
  const [branches, setBranches] = createSignal(false), [asking, setAsking] = createSignal(false);
  const [confirmation, setConfirmation] = createSignal<{ value: WorktreeConfirmation; origin: HTMLElement }>();
  const view = worktreeReview, busy = () => !!view()?.busy || worktreeReviewIssuing(), reading = worktreeReviewLoading;
  const chosen = () => view()?.rows.filter((r) => r.selectable && checked()[r.id]) ?? [];
  const title = () => worktreeReviewDialog()?.focus ? tr("Remove worktree") : tr("Clean up worktrees");
  createEffect(() => { const v = view(); if (!v) return; setChecked(Object.fromEntries(v.rows.map((r) => [r.id, r.checked]))); setBranches(false); });
  const ask = async (origin: HTMLElement) => { if (asking()) return; setAsking(true); try { const value = await prepareWorktreeRemoval(chosen().map((r) => r.id), branches()); if (value) setConfirmation({ value, origin }); } finally { setAsking(false); } };
  onMount(() => { dialog.showModal(); queueMicrotask(() => dialog.querySelector<HTMLButtonElement>("[autofocus]")?.focus()); });
  onCleanup(() => { dialog.close(); queueMicrotask(() => { if (origin?.isConnected && !origin.matches(":disabled")) origin.focus(); else document.querySelector<HTMLElement>(".inspector .insp-tabs [aria-selected=true]")?.focus(); }); });
  return <>
    <dialog ref={dialog} class="dlg su-dlg worktree-review" style={{ width: "820px" }} aria-labelledby="worktree-review-title" onCancel={(e) => { e.preventDefault(); if (!busy()) closeWorktreeReview(); }} onClose={closeWorktreeReview}>
      <div class="dlg-head"><Icon name="folder-git-2" size={18} class="accent" /><h2 id="worktree-review-title">{title()}</h2><span class="grow" /><button class="icon-btn" aria-label={tr("Close")} disabled={busy()} onClick={closeWorktreeReview}><Icon name="x" size={16} /></button></div>
      <div class="dlg-body"><p class="muted su-pre">{view()?.focus ? tr("Computer: %1\nWorktree: %2", view()?.machine ?? worktreeReviewDialog()?.machine ?? "", view()?.focus ?? "") : tr("Computer: %1\nRepository: %2", view()?.machine ?? worktreeReviewDialog()?.machine ?? "", view()?.repository ?? "")}</p>
        <p class="hint">{tr("Removal uses git worktree remove without force. Ignored files in a removed folder, such as build output, are deleted with it. Blocked worktrees explain what to finish first.")}</p>
        <table class="su-table wc-table"><thead><tr><th /><th>{tr("Worktree")}</th><th>{tr("State")}</th><th>{tr("Details")}</th></tr></thead><tbody>
          <Show when={!reading()} fallback={<tr><td colspan="4" class="hint">{tr("Reviewing worktrees on %1…", view()?.machine ?? worktreeReviewDialog()?.machine ?? "")}</td></tr>}><For each={view()?.rows}>{(row) =>
            <tr classList={{ blocked: !row.selectable }} title={row.path}><td><input type="checkbox" aria-label={`Select ${row.path}`} checked={!!checked()[row.id]} disabled={!row.selectable || busy() || reading()} onChange={(e) => setChecked({ ...checked(), [row.id]: e.currentTarget.checked })} /></td><td><div class="ell"><b>{leaf(row.path)}</b></div><div class="mono dim ell">{row.branch || "—"}</div></td><td><span class="wc-verdict" data-v={row.state}>{row.state}</span></td><td class="hint" title={row.details}>{row.brief}</td></tr>
          }</For></Show>
        </tbody></table>
        <label class="check" title={tr("Uses git branch -d after the worktree is removed. Branches with commits that are not merged are always kept.")}><input type="checkbox" checked={branches()} disabled={reading() || busy()} onChange={(e) => setBranches(e.currentTarget.checked)} />{tr("Also delete merged branches")}</label>
        <Show when={busy()}><progress aria-label={tr("Removing worktrees")} /></Show>
      </div>
      <div class="dlg-foot"><span class="hint grow" role="status">{reading() ? tr("Reviewing worktrees on %1…", view()?.machine ?? worktreeReviewDialog()?.machine ?? "") : worktreeReviewError() || view()?.summary}<Show when={view()?.unknown}> {tr("An unknown result will not be retried.")}</Show></span>
        <button class="btn" disabled={reading() || busy() || !view() || !!view()?.rows.length && view()!.rows.every((r) => ["Removed", "Forgotten", "Unknown"].includes(r.state))} onClick={() => void checkWorktreesAgain()}>{tr("Review again")}</button>
        <button type="button" class="btn danger" disabled={reading() || busy() || asking() || !chosen().length} onClick={(e) => void ask(e.currentTarget)}>{chosen().length > 1 ? tr("Remove %1 worktrees…", chosen().length) : tr("Remove selected…")}</button>
        <button type="button" class="btn primary" autofocus disabled={busy()} onClick={closeWorktreeReview}>{tr("Close")}</button>
      </div>
    </dialog>
    <Show when={confirmation()}>{(c) => <WorktreeConfirm value={c().value} title={title()} origin={c().origin} cancel={() => { setConfirmation(undefined); void cancelWorktreeConfirmation().catch(() => {}); }} confirm={async (consent) => { const id = c().value.id; setConfirmation(undefined); await confirmWorktreeRemoval(id, consent); }} />}</Show>
  </>;
}
function WorktreeConfirm(props: { value: WorktreeConfirmation; title: string; origin: HTMLElement; cancel: () => void; confirm: (consent: boolean) => Promise<void> }) {
  let dialog!: HTMLDialogElement;
  const [consent, setConsent] = createSignal(false);
  const c = props.value;
  onMount(() => { dialog.showModal(); queueMicrotask(() => dialog.querySelector<HTMLButtonElement>("[autofocus]")?.focus()); });
  onCleanup(() => { dialog.close(); queueMicrotask(() => { if (props.origin.isConnected) props.origin.focus(); }); });
  return <dialog ref={dialog} class="dlg su-dlg" style={{ width: "520px" }} aria-labelledby="worktree-confirm-title" onCancel={(e) => { e.preventDefault(); props.cancel(); }}>
    <div class="dlg-head"><Icon name="triangle-alert" size={18} class="accent" /><h2 id="worktree-confirm-title">{props.title}</h2><span class="grow" /><button class="icon-btn" aria-label={tr("Close")} onClick={props.cancel}><Icon name="x" size={16} /></button></div>
    <div class="dlg-body"><div class="notice warn"><Icon name="triangle-alert" size={16} /><b>{c.paths.length === 1 ? tr("Remove 1 worktree on %1?", c.machine) : tr("Remove %1 worktrees on %2?", c.paths.length, c.machine)}</b></div>
      <ul class="cc-list mono"><For each={c.paths}>{(path) => <li>{path}</li>}</For></ul><ul class="cc-facts">
        <Show when={c.ignored.length}><li style={{ "white-space": "pre-line" }}>{tr("Ignored files deleted with them:")} {c.ignored.join("\n") }.</li></Show><Show when={c.missing}><li>{tr("Missing folders: only Git's record is removed.")}</li></Show>
        <li>{c.branches.length ? tr("Merged branches deleted: %1.", c.branches.join(", ")) : tr("All branches are kept.")}</li><Show when={c.process_warning}><li>{c.process_warning}</li></Show><li>{tr("Each worktree is checked again before removal. This cannot be undone.")}</li>
      </ul><Show when={c.consent}><label class="check"><input type="checkbox" checked={consent()} onChange={(e) => setConsent(e.currentTarget.checked)} />{c.consent}</label></Show>
    </div><div class="dlg-foot"><span class="grow" /><button type="button" class="btn solid-danger" disabled={!!c.consent && !consent()} onClick={() => void props.confirm(consent())}>{c.button}</button><button type="button" class="btn primary" autofocus onClick={props.cancel}>{tr("Cancel")}</button></div>
  </dialog>;
}
