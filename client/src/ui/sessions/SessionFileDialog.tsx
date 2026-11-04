import { createSignal, onMount, onCleanup, Show } from "solid-js";
import { copyVerbatim } from "../../bridge";
import { actOnSessionFile, closeSessionFile, downloadSessionFile, saveSessionFile, sessionFile } from "./sessionFiles";
import { Icon } from "../lib/icons";
import { tr } from "../../i18n";

export function SessionFileDialog() {
  return (
    <Show when={sessionFile()}>
      {(file) => {
        let dialog: HTMLDialogElement | undefined;
        onMount(() => { dialog?.showModal(); queueMicrotask(() => dialog?.querySelector<HTMLElement>("[data-initial]:not(:disabled)")?.focus()); });
        const id = file().id;
        const [copying, setCopying] = createSignal(false);
        const [copied, setCopied] = createSignal(false);
        const [copyError, setCopyError] = createSignal(false);
        let live = true, reset: ReturnType<typeof setTimeout> | undefined;
        const copyPath = async () => {
          if (copying() || !file().path) return;
          const path = file().path;
          clearTimeout(reset); setCopied(false); setCopying(true); setCopyError(false);
          try {
            await copyVerbatim(path);
            if (live && sessionFile()?.id === id) {
              setCopied(true); clearTimeout(reset);
              reset = setTimeout(() => setCopied(false), 2000);
            }
          } catch { if (live && sessionFile()?.id === id) setCopyError(true); }
          finally { if (live) setCopying(false); }
        };
        const previous = document.activeElement;
        onCleanup(() => { live = false; clearTimeout(reset); dialog?.close(); if (previous instanceof HTMLElement && previous.isConnected) previous.focus(); });
        return (
        <dialog class="dlg session-file" ref={dialog} aria-labelledby="session-file-title"
          onCancel={(event) => { event.preventDefault(); closeSessionFile(); }} onClose={() => { if (sessionFile()?.id === id) closeSessionFile(); }}>
          <div class="dlg-head"><Icon name="file-text" size={18} class="accent" /><h2 id="session-file-title">{tr("File reference")}</h2><span class="grow" /><button class="icon-btn" aria-label={tr("Close")} onClick={closeSessionFile}><Icon name="x" size={16} /></button></div>
          <div class="dlg-body">
            <div class="dlg-detail">{file().title}</div>
            <div class="session-file-path-row">
              <div class="session-file-path">{file().path}</div>
              <button class="btn small" disabled={copying() || !file().path} onClick={() => void copyPath()}>
                <Icon name={copied() ? "check" : "copy"} size={14} /><span aria-live="polite">{copied() ? tr("Path copied") : tr("Copy path")}</span>
              </button>
            </div>
            <Show when={copyError()}><div class="dlg-error" role="alert">{tr("The clipboard is unavailable")}</div></Show>
            <Show when={file().location}><div class="dlg-detail">{tr("Source position: %1", file().location ?? "")}</div></Show>
            <Show when={file().checking || file().hint}><div class="dlg-detail session-file-hint">{file().checking ? tr("Checking the downloaded copy…") : file().hint}</div></Show>
            <Show when={file().checking || file().downloading}><progress aria-label={file().downloading ? tr("Downloading file from %1", file().computer) : tr("Checking the downloaded copy…")} /></Show>
            <Show when={file().error}><div class="dlg-error">{file().error}</div></Show>
          </div>
          <div class="dlg-foot session-file-actions">
              <Show when={!file().remote && file().local_available}>
                <button class="btn primary" data-initial onClick={() => actOnSessionFile("open_file")}>{tr("Open file")}</button>
                <button class="btn" onClick={() => actOnSessionFile("open_folder")}>{tr("Open folder")}</button>
              </Show>
              <Show when={file().remote}>
                <button class="btn" disabled={!file().can_download || file().downloading || file().picking} onClick={saveSessionFile}>{tr("Download…")}</button>
                <button class={file().local_available ? "btn" : "btn primary"} data-initial={!file().local_available || undefined}
                  disabled={!file().can_download || file().checking || file().downloading || file().picking} onClick={() => downloadSessionFile(true)}>
                  {file().local_available ? tr("Download again") : tr("Download and open")}
                </button>
                <Show when={file().local_available && !file().downloading}>
                  <button class="btn primary" data-initial disabled={file().picking} onClick={() => actOnSessionFile("open_copy")}>{tr("Open local copy")}</button>
                  <button class="btn" disabled={file().picking} onClick={() => actOnSessionFile("show_file")}>{tr("Show file")}</button>
                </Show>
              </Show>
              <Show when={file().remote}><button class="btn" onClick={() => actOnSessionFile("open_ssh")}><Icon name="square-terminal" size={13} />{tr("Open SSH terminal")}</button></Show>
            <button class="btn" onClick={closeSessionFile}>{tr("Close")}</button>
          </div>
        </dialog>
      ); }}
    </Show>
  );
}
