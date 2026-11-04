import { For, Index, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";
import { Icon } from "../lib/icons";
import { composing } from "../lib/keys";
import { platform } from "../../store/shortcuts";
import { FolderBrowser, browserEnter, listMenu, promptEnter, type BrowserPlace, type FolderBrowserCalls, type FolderPrompt, type PromptPlace } from "./folder-browser";
import "../styles/folder-browser.css";
import { tr } from "../../i18n";

// The folder browser of another computer (tray/src/DirectoryDialog.cpp, the
// peer branch), from the prototype's layout (design/webui/src/dialogs/setup/
// Clone.tsx, DirectoryBrowser) where Qt does not differ. Its rules and calls
// are folder-browser.ts's. It opens as its own modal over the dialog that
// asked for it. Its keys are Qt's (`browserEnter`, `promptEnter`): Use this
// folder and Create folder are the default buttons, so Enter on any other
// control presses them when they are enabled; the path field, the list, the
// inline row and the context menu keep their own Enter.

/** New folder — {computer}: the folder's name, Create folder and Cancel (286-318). */
function PromptDialog(props: { prompt: () => FolderPrompt; onName(name: string): void; onCreate(): void; onCancel(): void }) {
  let el!: HTMLDialogElement;
  let name!: HTMLInputElement;
  let create!: HTMLButtonElement;
  let cancel!: HTMLButtonElement;
  let gone = false;
  onMount(() => {
    el.showModal();
    queueMicrotask(() => { name.focus(); name.select(); });
  });
  onCleanup(() => { gone = true; if (el.open) el.close(); });
  // Create folder is the default button; Enter on Cancel cancels (`promptEnter`).
  const keys = (e: KeyboardEvent) => {
    const place: PromptPlace | undefined = e.target === name ? "name" : e.target === create ? "create" : e.target === cancel ? "cancel" : undefined;
    if (!place) return;
    switch (promptEnter(place, e)) {
      case "create": e.preventDefault(); props.onCreate(); return;
      case "cancel": e.preventDefault(); props.onCancel(); return;
      case "ignore": e.preventDefault(); return;
    }
  };
  return (
    <dialog class="dlg fb-prompt" ref={el} aria-labelledby="fb-prompt-title" onKeyDown={keys}
      onCancel={(e) => { e.preventDefault(); props.onCancel(); }} onClose={() => { if (!gone) props.onCancel(); }}>
      <div class="dlg-head"><h2 id="fb-prompt-title">{props.prompt().title}</h2></div>
      <div class="dlg-body">
        <p class="fb-text">{props.prompt().text}</p>
        <div class="form-row">
          <label for="fb-prompt-name">{tr("Folder name")}</label>
          <input id="fb-prompt-name" ref={name} class="field" aria-label={tr("New folder name")} value={props.prompt().name} onInput={(e) => props.onName(e.currentTarget.value)} />
        </div>
      </div>
      <div class="dlg-foot">
        <span class="spacer" />
        <button type="button" ref={cancel} class="btn" onClick={() => props.onCancel()}>{tr("Cancel")}</button>
        <button type="button" ref={create} class="btn primary" disabled={!props.prompt().createEnabled} onClick={() => props.onCreate()}>{tr("Create folder")}</button>
      </div>
    </dialog>
  );
}

/**
 * Choose folder — {computer}. `calls` are bound to that computer; `start` is
 * where it opens ("" is home). Use this folder answers `onChoose` with the
 * folder; Cancel and Escape close it, except while a folder is being created.
 */
export function FolderBrowserDialog(props: { computer: string; start: string; calls: FolderBrowserCalls; onChoose(path: string): void; onClose(): void }) {
  let el!: HTMLDialogElement;
  let list!: HTMLUListElement;
  let inline: HTMLInputElement | undefined;
  const model = new FolderBrowser(props.calls, props.computer, props.start);
  const [version, setVersion] = createSignal(0);
  model.onChange = () => setVersion((v) => v + 1);
  const v = createMemo(() => { version(); return model.view(); });
  const [menu, setMenu] = createSignal<{ left: number; top: number }>();
  let gone = false;

  onMount(() => {
    el.showModal();
    queueMicrotask(() => el.querySelector<HTMLElement>(".fb-nav button:not(:disabled)")?.focus());
  });
  onCleanup(() => {
    gone = true;
    model.dispose();
    if (el.open) el.close();
  });
  // A new inline row takes the focus (241).
  createEffect(on(() => v().inline !== undefined, (shown) => { if (shown) queueMicrotask(() => inline?.focus()); }, { defer: true }));
  createEffect(() => {
    const at = v().selected;
    if (at === undefined) return;
    list.querySelector(`[data-path="${CSS.escape(at)}"]`)?.scrollIntoView({ block: "nearest" });
  });

  const close = () => { if (model.canClose()) props.onClose(); };
  const choose = () => {
    if (!v().chooseEnabled) return;
    props.onChoose(model.chosen());
    props.onClose();
  };
  const newFolder = () => {
    model.newFolder();
    if (v().inline) queueMicrotask(() => inline?.focus());
  };
  const cancelInline = () => { model.cancelInline(); list.focus(); };

  const listKey = (e: KeyboardEvent) => {
    if (e.target !== list || !v().listEnabled) return;
    const entries = v().entries;
    const at = entries.findIndex((entry) => entry.path === v().selected);
    const move = (to: number) => { e.preventDefault(); if (entries.length) model.select(entries[Math.max(0, Math.min(entries.length - 1, to))].path); };
    switch (e.key) {
      case "ArrowDown": move(at < 0 ? 0 : at + 1); return;
      case "ArrowUp": move(at < 0 ? 0 : at - 1); return;
      case "PageDown": move(at < 0 ? 0 : at + 10); return;
      case "PageUp": move(at < 0 ? 0 : at - 10); return;
      case "Home": move(0); return;
      case "End": move(entries.length - 1); return;
      case "Enter": {
        if (composing(e)) return;
        e.preventDefault();
        const selected = v().selected;
        if (selected !== undefined) model.open(selected);
      }
    }
  };

  const place = (target: HTMLElement): BrowserPlace =>
    target.dataset.role === "path" ? "path"
      : target === list ? "list"
        : target === inline ? "inline-name"
          : target.closest(".fb-inline") ? "inline"
            : target.closest(".fb-menu") ? "menu"
              : "other";
  const keys = (e: KeyboardEvent) => {
    // Ctrl+Shift+N presses New folder (92-93), Command on macOS as Qt maps Ctrl.
    const command = platform === "macos" ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
    if (command && e.shiftKey && !e.altKey && e.code === "KeyN") {
      e.preventDefault();
      if (v().newFolderEnabled) newFolder();
      return;
    }
    switch (browserEnter(place(e.target as HTMLElement), e, v())) {
      case "go": e.preventDefault(); model.go(); return;
      case "create": e.preventDefault(); model.createInline(); return;
      case "choose": e.preventDefault(); choose(); return;
      case "ignore": e.preventDefault(); return;
    }
  };

  const entries = () => listMenu(v());
  const contextMenu = (e: MouseEvent) => {
    e.preventDefault();
    if (!entries()) return;
    const box = el.getBoundingClientRect();
    setMenu({ left: e.clientX - box.left, top: e.clientY - box.top });
  };
  // A list that turns disabled has no menu.
  createEffect(() => { if (!entries()) setMenu(undefined); });
  createEffect(() => {
    if (!menu()) return;
    const away = (e: PointerEvent) => { if (!(e.target as HTMLElement).closest?.(".fb-menu")) setMenu(undefined); };
    document.addEventListener("pointerdown", away, true);
    onCleanup(() => document.removeEventListener("pointerdown", away, true));
    queueMicrotask(() => el.querySelector<HTMLElement>(".fb-menu .menu-item:not([aria-disabled=true])")?.focus());
  });

  return (
    <>
      <dialog class="dlg fb-dlg" ref={el} aria-labelledby="fb-title" onKeyDown={keys}
        onCancel={(e) => {
          e.preventDefault();
          if (menu()) { setMenu(undefined); list.focus(); return; }
          // Escape in the new folder's name cancels it, not the browser (171-174).
          if (inline && document.activeElement === inline) { cancelInline(); return; }
          close();
        }}
        onClose={() => {
          if (gone) return;
          // A close that `cancel` could not refuse (WebView2's close watcher closes a modal on a
          // second Escape without user activation): while a folder is being created the browser
          // shows again and keeps its place, so the creation's answer still arrives.
          if (!model.canClose()) { el.showModal(); return; }
          props.onClose();
        }}>
        <div class="dlg-head"><h2 id="fb-title">{v().title}</h2></div>
        <div class="dlg-body fb-body">
          <h3 class="section-title">{tr("Choose a folder")}</h3>
          <p class="muted fb-machine">{v().machine}</p>
          <div class="row fb-nav">
            <button type="button" class="btn" disabled={!v().controlsEnabled} onClick={() => model.up()}>{tr("Up")}</button>
            <button type="button" class="btn" disabled={!v().controlsEnabled} onClick={() => model.home()}>{tr("Home")}</button>
            <input class="field mono grow" data-role="path" aria-label={tr("Directory path")} value={v().path} disabled={!v().controlsEnabled}
              onInput={(e) => model.editPath(e.currentTarget.value)} />
            <button type="button" class="btn" disabled={!v().controlsEnabled} onClick={() => model.go()}>{tr("Go")}</button>
          </div>
          <input class="field fb-filter" type="search" placeholder={tr("Filter folders in this directory…")} value={v().filter} disabled={!v().controlsEnabled}
            onInput={(e) => model.setFilter(e.currentTarget.value)}
            onKeyDown={(e) => {
              // Escape is the dialog's, as in Qt: WebKit's search field would only clear itself.
              if (e.key === "Escape") { e.preventDefault(); close(); }
            }} />
          <ul class="fb-list" ref={list} role="listbox" aria-label={tr("Directories")} tabindex={v().listEnabled ? 0 : -1} aria-disabled={!v().listEnabled}
            aria-activedescendant={v().selected !== undefined ? `fb-o-${v().entries.findIndex((e) => e.path === v().selected)}` : undefined}
            onKeyDown={listKey} onContextMenu={contextMenu}>
            <Show when={v().inline}>{(row) => (
              <li class="fb-inline">
                <Icon name="folder" size={14} class="fb-icon" />
                <input ref={inline} class="field grow" placeholder={tr("Folder name")} aria-label={tr("New folder name")} value={row().name}
                  onInput={(e) => model.setInlineName(e.currentTarget.value)} />
                <button type="button" class="btn small" disabled={!row().createEnabled} onClick={() => model.createInline()}>{tr("Create")}</button>
                <button type="button" class="btn small" aria-label={tr("Cancel new folder")} data-tip={tr("Cancel new folder")} onClick={cancelInline}>{tr("Cancel")}</button>
              </li>
            )}</Show>
            <For each={v().entries}>{(entry, index) => (
              <li id={`fb-o-${index()}`} role="option" data-path={entry.path} aria-selected={entry.path === v().selected} classList={{ on: entry.path === v().selected }}
                title={entry.path} onClick={() => { if (v().listEnabled) model.select(entry.path); }} onDblClick={() => model.open(entry.path)}>
                <Icon name="folder" size={14} class="fb-icon" /><span class="ell">{entry.name}</span>
              </li>
            )}</For>
          </ul>
          <div class="row fb-actions">
            <label class="check">
              <input type="checkbox" checked={v().hidden} disabled={!v().controlsEnabled} onChange={(e) => model.setHidden(e.currentTarget.checked)} />
              Show hidden folders
            </label>
            <span class="grow" />
            <button type="button" class="btn small" disabled={!v().newFolderEnabled} onClick={newFolder}>{tr("New folder")}</button>
            <button type="button" class="btn small" aria-label={tr("New folder dialog…")} data-tip={tr("New folder dialog…")} disabled={!v().dialogEnabled}
              onClick={() => model.newFolderDialog()}>{tr("Dialog…")}</button>
          </div>
          <p class="fb-status" role="status">{v().status}</p>
        </div>
        <div class="dlg-foot">
          <span class="spacer" />
          <button type="button" class="btn" disabled={!v().controlsEnabled} onClick={close}>{tr("Cancel")}</button>
          <button type="button" class="btn primary" disabled={!v().chooseEnabled} onClick={choose}>{v().chooseText}</button>
        </div>
        <Show when={menu()}>{(at) => (
          <div class="menu fb-menu" role="menu" style={{ left: `${at().left}px`, top: `${at().top}px` }}
            onKeyDown={(e) => {
              if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
              e.preventDefault();
              const items = [...el.querySelectorAll<HTMLElement>(".fb-menu .menu-item:not([aria-disabled=true])")];
              const i = items.indexOf(document.activeElement as HTMLElement);
              items[(i + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length]?.focus();
            }}>
            <Index each={entries() ?? []}>{(entry) => (
              <button type="button" class="menu-item" role="menuitem" aria-disabled={!entry().enabled}
                onClick={() => {
                  // A click on a disabled entry does nothing and keeps the menu open, as Qt's QMenu.
                  if (!entry().enabled) return;
                  setMenu(undefined);
                  if (entry().action === "new") newFolder();
                  else model.newFolderDialog();
                }}>{entry().label}</button>
            )}</Index>
          </div>
        )}</Show>
      </dialog>
      <Show when={v().prompt}>
        <PromptDialog prompt={() => v().prompt!} onName={(name) => model.setPromptName(name)} onCreate={() => model.confirmPrompt()} onCancel={() => model.cancelPrompt()} />
      </Show>
    </>
  );
}
