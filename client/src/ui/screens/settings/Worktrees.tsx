import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { inTauri } from "../../../bridge";
import { Icon } from "../../lib/icons";
import { Actions, Check, Group, NumberField, Row, SectionHead, Select, Status } from "./ui";
import { DEFAULT_RULES, controls, createWorktrees, type Rules, type View, type WorktreesState } from "./worktrees-page";
import { tr, trNoop } from "../../../i18n";

// Settings > Worktrees (tray/src/WorktreeSettings.h) as the approved mockup
// lays it out: each owning computer's cleanup rules, read from and applied
// to that computer's hgs. Opening it never removes a checkout. A policy
// that could not be read leaves nothing to apply (it fails closed). The
// computer, a request in flight and its answer live in the app, so they
// outlast the section.

// The open page, for opening the section again while it shows.
let reopen: (() => void) | undefined;

/** Opening the section reads its computer's rules again (SettingsPage.h:157). */
export function refreshWorktrees() {
  reopen?.();
}

export function WorktreesSection() {
  const [state, setState] = createSignal<WorktreesState>({ rules: DEFAULT_RULES, sending: false });
  const page = createWorktrees({
    view: () => invoke<View>("worktree_policy_view"),
    read: () => invoke<View>("worktree_policy_read"),
    choose: (route) => invoke<View>("worktree_policy_choose", { route }),
    apply: (rules) => invoke<View>("worktree_policy_apply", { rules }),
  }, setState);

  onMount(() => {
    if (!inTauri()) return;
    let gone = false;
    let stop: (() => void) | undefined;
    // Once listening, the view is read again: a request that answered
    // before would otherwise leave the page busy.
    listen("worktree-policy-changed", () => page.changed())
      .then((off) => { if (gone) off(); else { stop = off; page.changed(); } }, () => {});
    const mine = () => page.open();
    reopen = mine;
    // The section reads when it opens, as SettingsPage does, unless a
    // request still runs: then its outcome shows when it comes.
    page.open();
    onCleanup(() => {
      gone = true;
      stop?.();
      if (reopen === mine) reopen = undefined;
    });
  });

  const view = () => state().view;
  const rules = () => state().rules;
  const can = () => controls(view(), rules(), state().sending);
  const edit = (change: Partial<Rules>) => page.edit(change);
  const status = () => state().error ?? view()?.status ?? tr("Refresh to read this computer's cleanup rules.");
  const kind = () => (view()?.busy ? "busy" : state().error || view()?.failed ? "error" : "muted");
  return (
    <>
      <SectionHead title={trNoop("Worktrees")} />
      <Group>
        <Actions>
          <Select value={view()?.route ?? ""} label={trNoop("Computer")} disabled={!can().computer} onChange={(route) => page.choose(route)}
            options={[{ value: "", label: tr("This computer") }, ...(view()?.peers ?? []).map((peer) => ({ value: peer, label: peer }))]} />
          <button type="button" class="btn" disabled={!can().refresh} onClick={() => page.refresh()}><Icon name="refresh-cw" size={13} />{tr("Refresh")}</button>
        </Actions>
        <p class="set-group-hint">{tr("Cleanup runs on the computer that owns each worktree, even when this window is closed. Rules are enabled by default and apply across its projects. Other computers keep their own settings.")}</p>
        <Check label={trNoop("Enable automatic worktree cleanup")} checked={rules().enabled} disabled={!can().enabled} onChange={(on) => edit({ enabled: on })} />
        <For each={[
          ["after_session", tr("Clean integrated worktrees after the last session is archived or terminated")],
          ["unused_integrated", tr("Periodically clean unused integrated worktrees")],
          ["missing", tr("Forget verified managed worktrees whose folders no longer exist")],
        ] as const}>
          {([key, label]) => (
            <Check label={label} checked={rules()[key]} disabled={!can().others} onChange={(on) => edit({ [key]: on })} />
          )}
        </For>
        <Row label={trNoop("Minimum inactivity for periodic cleanup")} disabled={!can().grace}>
          <NumberField value={rules().grace_hours} min={1} max={8760} suffix="hours" label={trNoop("Minimum inactivity for periodic cleanup")}
            disabled={!can().grace} onChange={(hours) => edit({ grace_hours: hours })} />
        </Row>
        <p class="set-group-hint">{tr("Only verified Zerus-created worktrees qualify. Cleanup keeps branches and conversation history. Changed, untracked and ignored local files, sessions, running programs, locked folders and incomplete checks protect a worktree. Build folders and local configuration require review in Projects > Worktrees.")}</p>
        <Show when={status()}>{(text) => <Status kind={kind()}>{text()}</Status>}</Show>
        <Actions><button type="button" class="btn primary" disabled={!can().apply} onClick={() => page.apply()}>{tr("Apply to this computer")}</button></Actions>
      </Group>
    </>
  );
}
