/**
 * The kept folder's blockers as structured groups, each with its existing
 * explicit next step (zerus-l1ryn). No new authority: Archive is the session
 * menu's own action after a confirmation that lists every session, Check again
 * and the consents stay in the review, and an unknown result offers nothing.
 */
import { For, Show, createResource, createSignal, onCleanup, onMount } from "solid-js";
import { Icon } from "../lib/icons";
import { tr } from "../../i18n";
import type { Blockers } from "./cleanup-blockers";
import type { catalog } from "../../store/catalog";

type Session = (typeof catalog.rows)[string];

export function CleanupBlockersPanel(props: {
  blockers: Blockers;
  /** The fresh read of this folder's blockers; the recorded groups show meanwhile. */
  live?: "loading" | "ready" | "failed";
  /** The live session uniquely matched on this folder's computer, if any. */
  session: (name: string) => Session | undefined;
  open: (name: string) => void;
  archive: (names: string[]) => void;
  viewFiles: () => void;
  notes: string[];
  archived?: number;
  disabled: boolean;
}) {
  const b = () => props.blockers;
  const unknown = () => b().next.kind === "unknown";
  const rest = () => b().programCount - b().programs.length;
  const next = () => {
    const step = b().next;
    switch (step.kind) {
      case "unknown": return tr("The removal result is unknown and will not be retried. Inspect the folder before deciding what to do next.");
      case "archive": return tr("Removal waits until these sessions stop using the folder. Archive them, then choose Check again.");
      case "force": return tr("Some programs did not stop. Review them and choose Force stop only if you are sure.");
      case "close_programs": return tr("Close these programs, then choose Check again. Zerus never stops them on its own.");
      case "accept_unchecked": return tr("To remove anyway, select Remove even if some running programs cannot be checked below. It applies to this folder for five minutes.");
      case "remove_anyway": return tr("Choose Remove anyway to review this folder yourself. Programs Zerus cannot check are accepted for five minutes only after a separate confirmation.");
      case "check_hidden": return tr("Choose Check again. If only these programs remain, you can accept them for five minutes before removal.");
      case "discard": return tr("Removing the folder deletes these files. Select Permanently discard uncommitted and untracked files below to continue.");
      case "review_files": return tr("Review these files, then choose Check again.");
      case "none": return "";
    }
  };
  const checking = () => !unknown() && (props.live === "loading" || props.live === "failed");
  const any = () => unknown() || checking() || b().sessions.length || b().programCount || b().hidden || b().untracked || b().changes || b().other.length || props.notes.length;
  return <Show when={any()}>
    <section class="bc-blockers" aria-label={tr("Why this folder was kept")}>
      <Show when={next()}>
        <div class="notice" classList={{ warn: unknown() }} role="status"><Icon name={unknown() ? "triangle-alert" : "info"} size={14} /><span class="bc-next">{next()}</span>
          <Show when={b().next.kind === "archive" && b().sessions.length > 1}><button class="btn small" disabled={props.disabled} onClick={() => props.archive(b().sessions)}>{tr("Archive these %1 sessions…", b().sessions.length)}</button></Show>
        </div>
      </Show>
      <Show when={checking()}>
        <p class="hint bc-live" role="status"><Show when={props.live === "loading"} fallback={tr("Could not check what uses this folder now. The recorded result is shown.")}><Icon name="loader-circle" size={12} class="su-spin" />{tr("Checking what uses this folder…")}</Show></p>
      </Show>
      <Show when={props.archived}><p class="hint" role="status">{tr("Archive requested for %1 sessions. Choose Check again when they are archived.", props.archived ?? 0)}</p></Show>
      <Show when={!unknown()}>
        <Show when={b().sessions.length}>
          <div class="bc-group"><h4 class="bc-group-title">{tr("Other Zerus sessions use this folder")}</h4>
            <ul class="bc-list"><For each={b().sessions}>{(name) => <li><span class="ell" title={name}>{name}</span>
              <button class="btn small" disabled={!props.session(name)} onClick={() => props.open(name)}>{tr("Open")}</button>
              <button class="btn small" disabled={props.disabled || !props.session(name)} onClick={() => props.archive([name])}>{tr("Archive…")}</button></li>}</For></ul>
          </div>
        </Show>
        <Show when={b().programCount || b().hidden}>
          <div class="bc-group"><h4 class="bc-group-title">{tr("Running programs")}</h4>
            <ul class="bc-list"><For each={b().programs}>{(program) => <li><span class="ell" title={program.label}>{tr("%1 (PID %2)", program.label, program.pid)}</span></li>}</For></ul>
            <Show when={rest() > 0}><p class="hint">{tr("And %1 more running programs.", rest())}</p></Show>
            <Show when={b().hidden}><p class="hint">{tr("Programs Zerus cannot check: %1. They belong to other users or the system, so Zerus cannot see which folders they use.", b().hidden)}</p></Show>
          </div>
        </Show>
        <Show when={b().untracked || b().changes}>
          <div class="bc-group"><h4 class="bc-group-title">{b().changes ? tr("Uncommitted files") : tr("Untracked files")}</h4>
            <div class="row bc-files"><span>{b().changes ? tr("Changed files: %1; untracked files: %2", b().changes, b().untracked) : tr("Untracked files: %1", b().untracked)}</span>
              <button class="btn small" disabled={props.disabled} onClick={props.viewFiles}>{tr("View files")}</button></div>
          </div>
        </Show>
        <For each={b().other}>{(line) => <p class="hint bc-other">{line}</p>}</For>
      </Show>
      <For each={props.notes}>{(line) => <p class="hint bc-other">{line}</p>}</For>
    </section>
  </Show>;
}

/**
 * Lists every session before anything is archived; archiving reuses the
 * session menu's own Move to archive with its own checks, one session at a time.
 */
export function ArchiveSessionsDialog(props: { names: string[]; session: (name: string) => Session | undefined; close: () => void; archived: (count: number) => void }) {
  let dialog!: HTMLDialogElement;
  const origin = document.activeElement;
  const [busy, setBusy] = createSignal(false);
  const [entries] = createResource(async () => {
    const { actionEntries } = await import("../sessions/ActionDialogs");
    return props.names.map((name) => {
      const row = props.session(name);
      const item = row ? actionEntries(row).archive : undefined;
      return { name, machine: row?.machine ?? "", item, reason: !row ? tr("Open the session list to find this session.") : !item ? tr("This session cannot be archived.") : item.disabled ? item.hint ?? tr("This session cannot be archived now.") : "" };
    });
  });
  // Loading and error first: a pending entries() read would suspend the review,
  // and a failed import would throw from the read.
  const archivable = () => entries.loading || entries.error ? [] : (entries() ?? []).filter((e) => e.item && !e.item.disabled);
  // Until the list is known the disabled button names the requested count.
  const count = () => entries.loading || entries.error ? props.names.length : archivable().length;
  const confirm = () => {
    const ready = archivable(); if (!ready.length || busy()) return;
    setBusy(true);
    for (const entry of ready) entry.item!.action();
    props.archived(ready.length); props.close();
  };
  onMount(() => { dialog.showModal(); queueMicrotask(() => dialog.querySelector<HTMLButtonElement>(".dlg-foot button")?.focus()); });
  onCleanup(() => { dialog.close(); if (origin instanceof HTMLElement && origin.isConnected) origin.focus(); });
  return <dialog ref={dialog} class="dlg su-dlg bc-archive" style={{ width: "min(560px, calc(100vw - 32px))", position: "fixed", inset: "0", margin: "auto" }} aria-labelledby="bc-archive-title" onCancel={(event) => { event.preventDefault(); if (!busy()) props.close(); }}>
    <div class="dlg-head"><Icon name="archive" size={18} class="accent" /><h2 id="bc-archive-title">{props.names.length === 1 ? tr("Archive this session?") : tr("Archive these %1 sessions?", props.names.length)}</h2></div>
    <div class="dlg-body">
      <p>{tr("Archiving stops each session and moves it to the archive. Conversations are kept and can be restored. The folder is removed only after you check it again and confirm removal.")}</p>
      <Show when={!entries.loading} fallback={<p role="status">{tr("Loading…")}</p>}><Show when={!entries.error} fallback={<div role="alert" class="bc-load-failed"><p>{tr("Zerus could not load the session actions. Close this dialog and choose Archive again. If it keeps failing, restart Zerus.")}</p><p class="hint">{String(entries.error)}</p></div>}>
        <ul class="bc-list"><For each={entries()}>{(entry) => <li><span class="ell" title={entry.name}>{entry.name}</span><span class="hint">{entry.machine}</span><Show when={entry.reason}><span class="hint warn">{entry.reason}</span></Show></li>}</For></ul>
      </Show></Show>
    </div>
    <div class="dlg-foot"><span class="grow" />
      <button class="btn" disabled={busy()} onClick={props.close}>{tr("Cancel")}</button>
      <button class="btn solid-danger" disabled={busy() || !archivable().length} onClick={confirm}>{count() === 1 ? tr("Archive session") : tr("Archive %1 sessions", count())}</button>
    </div>
  </dialog>;
}
