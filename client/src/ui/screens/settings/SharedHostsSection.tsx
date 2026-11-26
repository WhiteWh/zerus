import { For, Show, createSignal, type JSX } from "solid-js";
import { hostAccess, type HostAccessOutcome } from "../../../bridge";
import { tr, trNoop } from "../../../i18n";
import { TextInput } from "../../lib/TextField";
import { Actions, Group, Row } from "./ui";

/**
 * Settings > Shared hosts (zerus-un7n phase 5/6): who this desktop is on a
 * multi-person host, which provider modes that host supports, and the
 * project history people there published. The host decides everything: a
 * refusal shows its code, an older host shows that it lacks the capability,
 * and nothing here hides data as a security measure. Continue with my
 * account starts from a private handoff under your own account; it never
 * resumes or sends to the author's session.
 */

interface Item { person: string; label?: string; id: string; generation: number; title: string; at: number }

export function outcomeText(outcome: HostAccessOutcome): string {
  switch (outcome.outcome) {
    case "done": return "";
    case "unsupported": return tr("This computer's hgs has no multi-person host access; update it there.");
    case "refused": return `${tr("The host refused:")} ${outcome.code}: ${outcome.detail}`;
    case "uncertain": return tr("The host may have applied the change. Load again before retrying.");
    default: return outcome.detail;
  }
}

export function SharedHostsSection(): JSX.Element {
  const [host, setHost] = createSignal("");
  const [status, setStatus] = createSignal<any>(null);
  const [support, setSupport] = createSignal<any[]>([]);
  const [project, setProject] = createSignal("");
  const [items, setItems] = createSignal<Item[]>([]);
  const [cursor, setCursor] = createSignal<string | null>(null);
  const [notice, setNotice] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const call = async (op: any): Promise<any | null> => {
    setBusy(true);
    try {
      const outcome = await hostAccess({ host: host().trim(), op });
      if (outcome.outcome === "done") return outcome.value;
      setNotice(outcomeText(outcome));
      return null;
    } catch (error) {
      setNotice(String(error));
      return null;
    } finally {
      setBusy(false);
    }
  };

  const load = async () => {
    setNotice("");
    setItems([]);
    setCursor(null);
    const view = await call({ op: "status" });
    setStatus(view);
    const matrix = await call({ op: "support" });
    setSupport(matrix?.rows ?? []);
    const first = view?.projects?.[0]?.project ?? "";
    setProject(first);
  };

  const more = async (fresh: boolean) => {
    if (!project()) return;
    const page = await call({ op: "list", project: project(), kind: "history", cursor: fresh ? null : cursor(), limit: 50 });
    if (!page) return;
    setItems(fresh ? page.items : [...items(), ...page.items]);
    setCursor(page.cursor);
  };

  const continueWith = async (item: Item) => {
    const result = await call({ op: "continue", project: project(), person: item.person, id: item.id, generation: item.generation });
    if (result) setNotice(tr("A private handoff was made for a new session under your own account."));
  };

  return (
    <div class="set-section">
      <h2>{tr("Shared hosts")}</h2>
      <p class="set-note">{tr("A multi-person host shares chosen projects, never provider sign-ins. Each person keeps their own accounts; the host enforces who may read or continue.")}</p>
      <Row label={tr("Computer")} hint={tr("A configured computer alias; leave empty for this computer.")}>
        <TextInput class="field" aria-label={tr("Computer")} value={host()} onInput={(e) => setHost(e.currentTarget.value)} />
      </Row>
      <Actions>
        <button type="button" class="btn primary" disabled={busy()} onClick={load}>{tr("Load")}</button>
      </Actions>
      <Show when={notice()}><p class="set-note" role="status">{notice()}</p></Show>
      <Show when={status()}>
        <Group title={trNoop("Access")}>
          <Row label={tr("Mode")}><span>{status().mode}</span></Row>
          <Show when={status().label}><Row label={tr("Person")}><span>{status().label}</span></Row></Show>
          <Show when={status().context}><Row label={tr("Execution context")}><span>{status().context}</span></Row></Show>
          <Show when={status().mode === "bound" && !status().bound}>
            <p class="set-note">{tr("Bind your state folder on that computer (hgs host-access bind) before publishing or continuing.")}</p>
          </Show>
        </Group>
        <Show when={(status().projects ?? []).length}>
          <Group title={trNoop("Shared history")}>
            <Row label={tr("Project")}>
              <select class="field" aria-label={tr("Project")} value={project()} onChange={(e) => setProject(e.currentTarget.value)}>
                <For each={status().projects}>{(row: any) => <option value={row.project}>{row.project} ({row.role})</option>}</For>
              </select>
            </Row>
            <Actions>
              <button type="button" class="btn" disabled={busy()} onClick={() => more(true)}>{tr("Show history")}</button>
              <Show when={items().length}><button type="button" class="btn" disabled={busy()} onClick={() => more(false)}>{tr("Show more")}</button></Show>
            </Actions>
            <ul class="set-list">
              <For each={items()}>{(item) => (
                <li>
                  <span>{item.title}</span> <span class="muted">{item.label ?? item.person.slice(0, 12)}</span>
                  <button type="button" class="btn small" disabled={busy()} onClick={() => continueWith(item)}>{tr("Continue with my account")}</button>
                </li>
              )}</For>
            </ul>
          </Group>
        </Show>
      </Show>
      <Show when={support().length}>
        <Group title={trNoop("Provider support")}>
          <table class="set-table" aria-label={tr("Provider support")}>
            <thead><tr><th scope="col">{tr("Provider")}</th><th scope="col">{tr("Mode")}</th><th scope="col">{tr("Status")}</th><th scope="col">{tr("Reason")}</th></tr></thead>
            <tbody>
              <For each={support()}>{(row: any) => (
                <tr classList={{ muted: !row.usable }}><td>{row.provider}</td><td>{row.mode}</td><td>{row.status}</td><td>{row.reason}</td></tr>
              )}</For>
            </tbody>
          </table>
        </Group>
      </Show>
    </div>
  );
}
