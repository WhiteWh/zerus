import { For, Show, createEffect, on } from "solid-js";
import { Spinner } from "../components/Status";
import { copyVerbatim } from "../../bridge";
import { notify } from "../../store/actions";
import { createOutputFollow, groupItem, processes } from "../../store/processes";
import { setMenu } from "../../store/ui";
import type { ProcessesView } from "../../types";
import { tr } from "../../i18n";

// The Processes tab (tray/src/ProcessView.cpp:36-79) over the Rust hub's view
// (src-tauri processes.rs): the summary with the busy indicator and Stop
// selected, the source and stop notices, the tree of Command, Agent, State
// and Duration, and the current process's details and output. The tree is
// one focusable element (role tree) whose focused item is its active
// descendant, as the session list is; store/processes.ts owns its keys.

/** Within this distance of the end, the output stays at its end when it grows. */
const OUTPUT_END = 8;

/** The DOM id of a tree item. */
const domId = (item: string) => `proc-${item.replace(/[^A-Za-z0-9_-]/g, (c) => `_${c.charCodeAt(0).toString(16)}`)}`;

/**
 * The row menu (ProcessView.cpp:63-75): Terminate, or Terminate selected (N),
 * enabled and titled as Stop selected. It acts only while the view and its
 * selection are still the ones it opened on.
 */
function rowMenu(v: ProcessesView, at: { x: number; y: number }) {
  const key = v.key, ids = [...v.selected];
  setMenu({ x: at.x, y: at.y, items: [{
    label: ids.length === 1 ? tr("Terminate") : tr("Terminate selected (%1)", ids.length),
    disabled: !v.stop.enabled, hint: v.stop.tooltip,
    action: () => processes.stopIfStill(key, ids),
  }] });
}

/** Copy command: the command as the agent ran it (Qt copies it verbatim). */
async function copyCommand(command: string) {
  try { await copyVerbatim(command); } catch (error) { notify("error", typeof error === "string" ? error : String(error), true, undefined, tr("Not copied")); }
}

export function ProcessesTab() {
  const view = () => processes.view();
  let list!: HTMLDivElement;
  let output: HTMLPreElement | undefined;
  // Another session's view starts at the top.
  createEffect(on(() => view()?.key, () => { if (list) list.scrollTop = 0; }, { defer: true }));
  // The output keeps its end in view when it was there before it changed; a
  // new current process starts at its end, as Qt's cleared output does.
  const follow = createOutputFollow();
  createEffect(on(() => [view()?.detail?.id, view()?.detail?.output] as const, ([id]) => {
    follow.process(id);
    if (output && follow.stick()) output.scrollTop = output.scrollHeight;
  }));
  // The focused item stays in view.
  createEffect(on(() => processes.focus()?.id, (id) => {
    if (id) document.getElementById(domId(id))?.scrollIntoView({ block: "nearest" });
  }, { defer: true }));

  /** Opens the row menu at a point, or below the row the keyboard asks for. */
  const menuAt = (id: string, at?: { x: number; y: number }) => processes.requestMenu(id, (v) => {
    const r = document.getElementById(domId(id))?.getBoundingClientRect();
    rowMenu(v, at ?? { x: (r?.left ?? 0) + 24, y: r?.bottom ?? 0 });
  });
  const onKey = (e: KeyboardEvent) => {
    if (processes.key({ key: e.key, shift: e.shiftKey, toggle: e.ctrlKey || e.metaKey, alt: e.altKey }, (id) => menuAt(id))) e.preventDefault();
  };

  return (
    <div class="proc-tab">
      <Show when={view()}>
        {(v) => (
          <>
            <div class="proc-summary">
              <span class="proc-summary-text" title={v().summary_tooltip}>{v().summary}</span>
              <Show when={v().busy}><Spinner size={12} /></Show>
              <span class="grow" />
              <button class="btn small danger" disabled={!v().stop.enabled} title={v().stop.tooltip} onClick={() => processes.stop()}>{v().stop.label}</button>
            </div>
            <Show when={v().notes}><div class="proc-note hint">{v().notes}</div></Show>
            <Show when={v().stop_notice}><div class="proc-note hint">{v().stop_notice}</div></Show>
            <Show when={v().failed}>{(text) => <div class="proc-note warn">{text()}</div>}</Show>
            <div class="proc-list" ref={list} tabindex="0" role="tree" aria-label={tr("Processes")} aria-multiselectable="true"
              aria-activedescendant={processes.focus() ? domId(processes.focus()!.id) : undefined} onKeyDown={onKey}>
              <table class="proc-table" role="presentation">
                <thead role="presentation"><tr role="presentation"><th>{tr("Command")}</th><th>{tr("Agent")}</th><th>{tr("State")}</th><th>{tr("Duration")}</th></tr></thead>
                <tbody role="presentation">
                  <For each={v().groups}>{(g) => (
                    <>
                      <tr class="grp" id={domId(groupItem(g.group))} role="treeitem" aria-level="1" aria-expanded={!processes.collapsed()[g.group]}
                        classList={{ focused: processes.focus()?.id === groupItem(g.group) }} onClick={() => processes.toggleGroup(g.group)}>
                        <td colspan="4"><span class="proc-chevron">{processes.collapsed()[g.group] ? "▸" : "▾"}</span>{g.title}</td>
                      </tr>
                      <Show when={!processes.collapsed()[g.group]}>
                        <For each={g.rows}>{(r) => (
                          <tr id={domId(r.id)} role="treeitem" aria-level="2" aria-selected={v().selected.includes(r.id)}
                            classList={{ sel: v().selected.includes(r.id), current: v().current === r.id, focused: processes.focus()?.id === r.id }}
                            onClick={(e) => { list.focus({ preventScroll: true }); processes.click(r.id, { toggle: e.ctrlKey || e.metaKey, range: e.shiftKey }); }}
                            onContextMenu={(e) => { e.preventDefault(); list.focus({ preventScroll: true }); menuAt(r.id, { x: e.clientX, y: e.clientY }); }}>
                            <td class="mono" title={r.command_full}>{r.command}</td>
                            <td>{r.owner}</td>
                            <td class={`proc-state ${r.tone}`} title={r.tooltip}>{r.status}</td>
                            <td class="tabular">{r.duration}</td>
                          </tr>
                        )}</For>
                      </Show>
                    </>
                  )}</For>
                </tbody>
              </table>
            </div>
            <div class="proc-detail">
              <Show when={v().detail} fallback={<div class="hint">{tr("Select a command to view its output.")}</div>}>
                {(d) => (
                  <>
                    <b class="mono proc-command">{d().command}</b>
                    <Show when={d().details}><div class="hint proc-details">{d().details}</div></Show>
                    <div class="row">
                      <span class="hint grow">{d().notice}</span>
                      <button class="btn small" onClick={() => void copyCommand(d().command)}>{tr("Copy command")}</button>
                    </div>
                    <pre class="act-code mono proc-output" ref={output}
                      onScroll={(e) => { const el = e.currentTarget; follow.scrolled(el.scrollHeight - el.scrollTop - el.clientHeight < OUTPUT_END); }}>{d().output ?? ""}</pre>
                  </>
                )}
              </Show>
            </div>
          </>
        )}
      </Show>
    </div>
  );
}
