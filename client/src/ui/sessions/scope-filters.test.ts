import { describe, expect, it, vi } from "vitest";
vi.hoisted(() => { vi.stubGlobal("location", new URL("http://localhost/")); });
import { toSession } from "../../store/catalog";
import { emptyUsage, type Account, type BindingEvidence, type Profile, type ViewRow } from "../../types";
import { acceptsAccount, acceptsFolder, accountBinding, accountChoices, appendAccountChoices, bindingState, compileScopes, rowBinding, sameFolderForCreation, scopeCounts, type FolderScope, type SessionScopes } from "./scope-filters";
import { archiveListRows, archivedSession } from "../../store/archive";

const row = (host: string, id: string, extra: Partial<ViewRow> = {}) => toSession(host, id, {
  name: id, agent: "claude", machine: host || "workstation", account_id: id, cwd_canonical: "/srv/example/app", git_root: "/srv/example/app", ...extra,
});
const account = (key: string, members: { host: string; id: string; home: string }[], label = "Work"): Account => ({
  key, label, provider: "claude", installed: true, machines: [], members: members as Profile[], usage: emptyUsage(), permission: { source: "" }, identity_lines: [], identity_note: null,
});
const evidence = (accounts: Account[]): BindingEvidence[] => accounts.flatMap((a) => a.members.map((p) => ({ host: p.host, provider: a.provider, id: p.id, home: p.home, group: a.key })));
const empty: SessionScopes = { machines: [], projects: [], accounts: [] };

describe("recorded account bindings (Qt SessionAccounts)", () => {
  it("keeps host, provider and id exact, with collision-free UTF-16 fields", () => {
    expect(accountBinding("a", "bc", "d")).not.toBe(accountBinding("ab", "c", "d"));
    expect(accountBinding("😀", "dsh", "p")).toBe("2:😀3:dsh2:id1:p");
    expect(rowBinding(row("studio", "p", { agent: "deepseek", account_provider: "dsh" }))).toBe(accountBinding("studio", "dsh", "p"));
    expect(rowBinding(row("", "p", { account_id: "", account_home: "/old/home" }))).toBe(accountBinding("", "claude", "", "/old/home"));
  });
  it("groups verified identities for display while selecting only recorded bindings", () => {
    const a = account("verified", [{ host: "", id: "local", home: "/a" }, { host: "studio", id: "remote", home: "/b" }]);
    const choice = accountChoices([a], evidence([a]), [])[0], selected = new Set(choice.bindings);
    expect(acceptsAccount(row("", "local"), selected)).toBe(true);
    expect(acceptsAccount(row("studio", "remote"), selected)).toBe(true);
    expect(acceptsAccount(row("studio", "local"), selected)).toBe(false);
    expect(acceptsAccount(row("", "local", { account_provider: "codex" }), selected)).toBe(false);
    expect(acceptsAccount(row("", "", { account_id: "" }), selected)).toBe(false);
    // New catalog members do not silently widen an existing selection.
    a.members.push({ host: "new-host", id: "later", home: "/c" } as Profile);
    expect(acceptsAccount(row("new-host", "later"), selected)).toBe(false);
  });
  it("aliases a home only when all current and removed evidence names one identity", () => {
    const a = account("current", [{ host: "", id: "p", home: "/shared" }]);
    const old = row("", "old", { account_id: "", account_home: "/shared" });
    const selected = (e: BindingEvidence[]) => new Set(accountChoices([a], e, [])[0].bindings);
    expect(acceptsAccount(old, selected(evidence([a])))).toBe(true);
    expect(acceptsAccount(old, selected([]))).toBe(false);
    const retired = { host: "", provider: "claude", id: "retired", home: "/shared", group: "former" };
    expect(acceptsAccount(old, selected([...evidence([a]), retired]))).toBe(false);
    expect(acceptsAccount(old, selected([...evidence([a]), { ...retired, group: "current" }]))).toBe(true);
  });
  it("retains selected unavailable accounts without treating their captions as identity", () => {
    const a = account("a", [{ host: "", id: "gone", home: "/a" }]);
    const saved = accountChoices([a], evidence([a]), []), selected = saved[0].bindings;
    expect(accountChoices([], [], selected, saved)).toEqual(saved);
    expect(accountChoices([], [], ["unknown"])[0].label).toBe("Unavailable account");
    expect(accountChoices([], [], [], saved)).toEqual([]);
  });
  it("keeps an open chooser's exact labels and bindings while identities regroup", () => {
    const shown = [{ id: "combined", label: "Work", provider: "claude", bindings: ["a", "b"] }];
    const next = [{ id: "split-a", label: "Personal", provider: "claude", bindings: ["a"] }, { id: "new", label: "New", provider: "codex", bindings: ["c"] }];
    const menu = appendAccountChoices(shown, next);
    expect(menu).toEqual([shown[0], next[1]]);
    expect(bindingState(shown[0], new Set(["a"]))).toBe("mixed");
    expect(bindingState(shown[0], new Set(["a", "b"]))).toBe(true);
    expect(bindingState(shown[0], new Set())).toBe(false);
  });
  it("disambiguates equal captions by recorded computer and profile, as Qt does", () => {
    const accounts = [account("one", [{ host: "", id: "first", home: "" }]), account("two", [{ host: "studio", id: "second", home: "" }])];
    expect(accountChoices(accounts, evidence(accounts), []).map((c) => c.label)).toEqual(["Work (studio / second)", "Work (This computer / first)"]);
  });
});

describe("exact folder and combined scopes", () => {
  const folder: FolderScope = { computer: "", path: "/srv/example/app", checkout: false, style: "posix" };
  it("uses canonical folder or checkout on exactly the recorded computer", () => {
    expect(acceptsFolder(row("", "a"), folder)).toBe(true);
    expect(acceptsFolder(row("studio", "a"), folder)).toBe(false);
    expect(acceptsFolder(row("", "a", { cwd_canonical: "/srv/example/app/child" }), folder)).toBe(false);
    expect(acceptsFolder(row("", "a", { cwd_canonical: "/srv/example/app/child" }), { ...folder, checkout: true })).toBe(true);
    expect(acceptsFolder(row("", "a", { cwd: folder.path, cwd_canonical: "" }), folder)).toBe(false);
    expect(acceptsFolder(row("", "a", { cwd_canonical: "C:\\Work\\App" }), { ...folder, path: "c:/work/app", style: "windows" })).toBe(true);
    expect(sameFolderForCreation({ host: "", target: folder.path }, folder)).toBe(true);
    expect(sameFolderForCreation({ host: "studio", target: folder.path }, folder)).toBe(false);
  });
  it("applies all scopes to metadata and content rows alike", () => {
    const s = row("", "a"), scope = { machines: [s.machine], projects: ["alpha"], accounts: [rowBinding(s)], folder };
    const accepts = compileScopes(scope);
    expect(accepts(s, "alpha")).toBe(true);
    expect(accepts(s, "beta")).toBe(false);
    expect(accepts(row("", "b"), "alpha")).toBe(false);
    expect(accepts(row("studio", "a"), "alpha")).toBe(false);
  });
  it("counts before each picker's own selection, after state and other scopes, without reading search", () => {
    const rows = [row("", "a"), row("", "b"), row("studio", "a"), row("", "other", { cwd_canonical: "/elsewhere" })];
    const scope = { ...empty, machines: ["workstation"], projects: ["alpha"], accounts: [rowBinding(rows[0])], folder };
    const counts = scopeCounts(rows, () => "alpha", scope, () => true);
    expect([...counts.account]).toEqual([[rowBinding(rows[0]), 1], [rowBinding(rows[1]), 1]]);
    expect([...counts.machine]).toEqual([["", 1]]);
    expect([...counts.project]).toEqual([["alpha", 1]]);
    expect(scopeCounts(rows, () => "alpha", scope, () => false).account.size).toBe(0);
  });
  it("applies the same predicate to a bounded loaded archive page", () => {
    const rows = [archivedSession("", "workstation", { name: "old-a", agent: "claude", archive_id: "a", account_id: "work", archived_at: 42, cwd_canonical: folder.path, git_root: folder.path }),
      archivedSession("", "workstation", { name: "old-b", agent: "claude", archive_id: "b", account_id: "personal", archived_at: 42, cwd_canonical: folder.path, git_root: folder.path })];
    const accepts = compileScopes({ ...empty, accounts: [rowBinding(rows[0])], folder });
    const visible = archiveListRows(rows, { machines: [], includes: () => true, compact: () => false, accepts: (s) => accepts(s, "ungrouped") });
    expect(visible.map((r) => r.id)).toEqual([rows[0].key]);
  });
});
