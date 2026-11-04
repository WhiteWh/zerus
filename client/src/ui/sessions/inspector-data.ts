import { sessionUsageView } from "./session-usage-ui";
import { object, text } from "../screens/tasks-ui";
import { tr } from "../../i18n";
import type { Account, AccountsView, Profile } from "../../types";
import { accountChoices, rowBinding, type BoundRow } from "./scope-filters";
import { profileTags } from "../screens/accounts-ui";
export function sessionPlan(value: unknown) {
  const lists = object(object(value).task_lists), main = object(lists.main);
  const items = Array.isArray(main.items) ? main.items : [];
  return items.slice(0, 100).map((v) => { const item = object(v); return { title: text(item.title), status: text(item.status) }; }).filter((v) => v.title);
}
export function sessionGoal(value: unknown) {
  const goal = object(object(value).goal);
  return { objective: text(goal.objective), completion: text(goal.completion_criterion), status: text(goal.status) };
}
export function usageRows(value: unknown): [string, string][] {
  const view = sessionUsageView(object(value).session_usage);
  return [...view.rows.map((row): [string, string] => [row.label, row.value]), ...(view.context ? [[tr("Latest context"), view.context] as [string, string]] : [])];
}

/** The row's recorded binding grouped as the account chooser groups it (exact
 * profile id, or an unambiguous older home); an unknown profile finds none. */
export function sessionAccount(row: BoundRow, view: AccountsView): { account?: Account; profile?: Profile; tag?: string } {
  const binding = rowBinding(row);
  const choice = accountChoices(view.accounts, view.binding_evidence_complete ? view.binding_evidence ?? [] : [], []).find((c) => c.bindings.includes(binding));
  const account = choice && view.accounts.find((a) => a.key === choice.id);
  if (!account) return {};
  const profile = account.members.find((p) => p.host === row.computer && (row.accountId ? p.id === row.accountId : p.home === row.accountHome));
  return { account, profile, tag: profile && profileTags(account).get(profile.key) };
}

/** A path under the usual user directories as ~; the caller keeps the full path as its tooltip. */
export function homeShort(path: string): string {
  const home = /^(?:\/home\/[^/]+|\/Users\/[^/]+|\/root)(?=\/|$)/.exec(path) ?? /^[A-Za-z]:\\Users\\[^\\]+(?=\\|$)/.exec(path);
  return home ? `~${path.slice(home[0].length)}` : path;
}
