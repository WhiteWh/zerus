import type { Account, BindingEvidence } from "../../types";
import type { Session } from "./types";
import { same, type Style } from "./paths";
import { tr } from "../../i18n";

/** Qt SessionAccounts::binding: lengths are UTF-16 code units in both clients. */
export const accountBinding = (host: string, provider: string, id: string, home = "") =>
  [host, provider, id ? "id" : "home", id || home].map((v) => `${v.length}:${v}`).join("");
export type BoundRow = Pick<Session, "computer" | "cmd"> & Partial<Pick<Session, "accountProvider" | "accountId" | "accountHome" | "accountReported">>;
export const rowBinding = (s: BoundRow) => accountBinding(s.computer, s.accountProvider ?? (s.cmd === "deepseek" ? "dsh" : s.cmd), s.accountId ?? "", s.accountHome ?? "");
export const acceptsAccount = (s: BoundRow, bindings: ReadonlySet<string>) => !bindings.size || (s.accountReported !== false && bindings.has(rowBinding(s)));

export interface FolderScope { computer: string; path: string; checkout: boolean; style: Style }
export const acceptsFolder = (s: Pick<Session, "computer" | "cwdCanonical" | "gitRoot">, folder?: FolderScope) =>
  !folder || (s.computer === folder.computer && same(folder.checkout ? s.gitRoot : s.cwdCanonical, folder.path, folder.style));
export const sameFolderForCreation = (s: { host: string; target: string }, folder?: FolderScope) =>
  !folder || (s.host === folder.computer && same(s.target, folder.path, folder.style));

export interface AccountChoice { id: string; label: string; provider: string; bindings: string[] }
/** An open Qt chooser only appends new disjoint choices; existing actions keep their snapshot. */
export function appendAccountChoices(shown: readonly AccountChoice[], next: readonly AccountChoice[]): AccountChoice[] {
  const out = [...shown], ids = new Set(out.map((c) => c.id)), bindings = new Set(out.flatMap((c) => c.bindings));
  for (const c of next) {
    if (ids.has(c.id) || c.bindings.some((b) => bindings.has(b))) continue;
    out.push(c); ids.add(c.id); c.bindings.forEach((b) => bindings.add(b));
  }
  return out;
}
export const bindingState = (choice: AccountChoice, selected: ReadonlySet<string>): boolean | "mixed" => {
  const any = choice.bindings.some((b) => selected.has(b));
  return any && !choice.bindings.every((b) => selected.has(b)) ? "mixed" : any;
};

function bindingParts(key: string): string[] {
  const parts: string[] = []; let at = 0;
  while (parts.length < 4 && at < key.length) {
    const colon = key.indexOf(":", at), length = key.slice(at, colon);
    if (colon <= at || !/^[0-9]{1,9}$/.test(length)) return [];
    const size = Number(length); at = colon + 1;
    if (size > key.length - at) return [];
    parts.push(key.slice(at, at + size)); at += size;
  }
  return parts.length === 4 && at === key.length ? parts : [];
}
/** Ids, never labels, carry selection. A verified identity may group profiles on several hosts. */
export function accountChoices(accounts: readonly Account[], evidence: readonly BindingEvidence[], selected: readonly string[], retained: readonly AccountChoice[] = []): AccountChoice[] {
  const homes = new Map<string, Set<string>>();
  for (const e of evidence) {
    if (!e.home) continue;
    const home = accountBinding(e.host, e.provider, "", e.home);
    const groups = homes.get(home) ?? new Set<string>(); groups.add(e.group); homes.set(home, groups);
  }
  const choices = accounts.map((a) => {
    let label = a.label || tr("Unnamed account");
    const { email, organization } = a.usage.identity;
    if (email && !label.toLowerCase().includes(email.toLowerCase())) label += ` (${email})`;
    if (organization) label += ` (${organization})`;
    const bindings = new Set<string>();
    for (const p of a.members) {
      bindings.add(accountBinding(p.host, a.provider, p.id, p.home));
      const home = accountBinding(p.host, a.provider, "", p.home);
      // Missing evidence on an older peer is not proof of an unambiguous home.
      if (p.home && homes.get(home)?.size === 1 && homes.get(home)!.has(a.key)) bindings.add(home);
    }
    return { id: a.key, label, provider: a.provider, bindings: [...bindings] };
  });
  const known = new Set(choices.flatMap((c) => c.bindings));
  const oldBindings = new Map(retained.flatMap((c) => c.bindings.map((b) => [b, c] as const)));
  const byId = new Map(choices.map((c) => [c.id, c]));
  for (const binding of selected) {
    if (known.has(binding)) continue;
    const previous = oldBindings.get(binding);
    const id = previous?.id ?? `binding:${binding}`;
    const choice = byId.get(id);
    if (choice) choice.bindings.push(binding);
    else { const added = { id, label: previous?.label ?? tr("Unavailable account"), provider: previous?.provider ?? "", bindings: [binding] }; choices.push(added); byId.set(id, added); }
    known.add(binding);
  }
  const captions = new Map<string, number>();
  for (const c of choices) captions.set(`${c.provider}\n${c.label}`, (captions.get(`${c.provider}\n${c.label}`) ?? 0) + 1);
  for (const c of choices) if ((captions.get(`${c.provider}\n${c.label}`) ?? 0) > 1) {
    const profiles = c.bindings.map(bindingParts).filter((p) => p.length === 4).map((p) => `${p[0] || tr("This computer")} / ${p[3] || tr("Not recorded")}`).sort();
    if (profiles.length) c.label += ` (${profiles.join(", ")})`;
  }
  return choices.sort((a, b) => `${a.provider}\n${a.label}`.localeCompare(`${b.provider}\n${b.label}`) || a.id.localeCompare(b.id));
}

export interface SessionScopes { machines: readonly string[]; projects: readonly string[]; accounts: readonly string[]; folder?: FolderScope }
/** Build selection indexes once per scope, not once per listed session. */
export function compileScopes(scope: SessionScopes) {
  const machines = new Set(scope.machines), projects = new Set(scope.projects), accounts = new Set(scope.accounts);
  return (s: Session, group: string) => (!machines.size || machines.has(s.machine)) && (!projects.size || projects.has(group))
    && acceptsAccount(s, accounts) && acceptsFolder(s, scope.folder);
}

/** Qt counts each chooser before its own selection and text search, after the other scopes. */
export function scopeCounts(rows: readonly Session[], groupOf: (s: Session) => string, scope: SessionScopes, visible: (s: Session) => boolean) {
  const machine = new Map<string, number>(), project = new Map<string, number>(), account = new Map<string, number>();
  const add = (map: Map<string, number>, key: string) => map.set(key, (map.get(key) ?? 0) + 1);
  const machines = new Set(scope.machines), projects = new Set(scope.projects), accounts = new Set(scope.accounts);
  for (const s of rows) {
    if (!visible(s) || !acceptsFolder(s, scope.folder)) continue;
    const group = groupOf(s), binding = rowBinding(s);
    const m = !machines.size || machines.has(s.machine), p = !projects.size || projects.has(group), a = !accounts.size || (s.accountReported !== false && accounts.has(binding));
    if (m && a) add(project, group);
    if (p && a) add(machine, s.computer);
    if (m && p) add(account, binding);
  }
  return { machine, project, account };
}
