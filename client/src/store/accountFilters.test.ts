import { beforeEach, describe, expect, it, vi } from "vitest";
import { createComputed, createRoot } from "solid-js";
vi.hoisted(() => { vi.stubGlobal("location", new URL("http://localhost/")); });
vi.mock("./accounts", async (original) => ({ ...(await original<typeof import("./accounts")>()), loadAccountFilters: vi.fn() }));
import { loadAccountFilters, setAccountsPage } from "./accounts";
import { accountFilter } from "./ui";
import { cancelFilterAccountReads, chooseFilterAccounts, filterAccounts, openFilterAccounts, toggleFilterAccount } from "./accountFilters";
import { accountBinding } from "../ui/sessions/scope-filters";
import { emptyAccounts, emptyUsage, type Account, type Profile } from "../types";

const account = (id: string, bindings: string[]): Account => ({ key: id, label: id, provider: "claude", installed: true, machines: [],
  members: bindings.map((id) => ({ id, host: "", home: "" } as Profile)), usage: emptyUsage(), permission: { source: "" }, identity_lines: [], identity_note: null });
beforeEach(() => { cancelFilterAccountReads(); chooseFilterAccounts([]); setAccountsPage(emptyAccounts()); vi.clearAllMocks(); });

describe("account selection keeps exact presented bindings", () => {
  it("never expands another selection when a chip is removed after a catalog refresh", () => createRoot((dispose) => {
    setAccountsPage({ ...emptyAccounts(), accounts: [account("Work", ["a"]), account("Personal", ["p"])] });
    let publications = 0;
    createComputed(() => { filterAccounts(); accountFilter(); publications++; });
    chooseFilterAccounts(["Work", "Personal"]);
    const personal = filterAccounts().find((a) => a.id === "Personal")!;
    setAccountsPage({ ...emptyAccounts(), accounts: [account("Work", ["a", "new-profile"]), account("Personal", ["p"])] });
    toggleFilterAccount(personal, false);
    expect(accountFilter()).toEqual([accountBinding("", "claude", "a")]);
    expect(publications).toBeLessThan(12);
    dispose();
  }));
  it("a menu action keeps its snapshot when the catalog splits an identity", () => {
    setAccountsPage({ ...emptyAccounts(), accounts: [account("Combined", ["a", "b"])] });
    const shown = filterAccounts()[0];
    setAccountsPage({ ...emptyAccounts(), accounts: [account("Split", ["a", "new-profile"])] });
    toggleFilterAccount(shown, true);
    expect(new Set(accountFilter())).toEqual(new Set([accountBinding("", "claude", "a"), accountBinding("", "claude", "b")]));
  });
});

describe("bounded on-open account view pulls", () => {
  it("coalesces opens and forces only the first read, until the catalog settles", async () => {
    vi.useFakeTimers();
    vi.mocked(loadAccountFilters).mockImplementation(async (force) => { setAccountsPage({ ...emptyAccounts(), catalog_loading: !!force }); });
    const first = openFilterAccounts();
    expect(openFilterAccounts()).toBe(first);
    await vi.advanceTimersByTimeAsync(250); await first;
    expect(vi.mocked(loadAccountFilters).mock.calls).toEqual([[true], []]);
    vi.useRealTimers();
  });
  it("stops at the view-read cap or when the Sessions component goes away", async () => {
    vi.useFakeTimers();
    vi.mocked(loadAccountFilters).mockImplementation(async () => { setAccountsPage({ ...emptyAccounts(), catalog_loading: true }); });
    const first = openFilterAccounts(); await vi.advanceTimersByTimeAsync(16_000); await first;
    expect(loadAccountFilters).toHaveBeenCalledTimes(65);
    vi.clearAllMocks();
    const second = openFilterAccounts(); await Promise.resolve(); cancelFilterAccountReads();
    await vi.advanceTimersByTimeAsync(250); await second;
    expect(vi.mocked(loadAccountFilters).mock.calls).toEqual([[true]]);
    vi.useRealTimers();
  });
});
