import { describe, expect, it } from "vitest";
import { connectionOf, fleetCards, overviewStats } from "../../store/overview";
import { toSession } from "../../store/catalog";
import { NOW, machineMeta, projected, rowOut, usageOf } from "../../store/testing";
import { accountIdentity, accountTone, accountsNote, bytes, cpuFigure, level, memoryFigure, sampleNote, sessionsLabel } from "./overview-ui";
import { emptyAccounts, type Account } from "../../types";

const account = (extra: Partial<Account> = {}): Account => ({
  key: "k", label: "Work", provider: "claude", installed: true, machines: ["workstation"], members: [], usage: usageOf(),
  permission: { source: "Default" }, identity_lines: [], identity_note: null, ...extra,
});

describe("overview cards", () => {
  it("a reconnecting peer keeps its last values in the unknown colour, leaves Machines online and counts as offline", () => {
    const computers = { "": { id: "", status: "live", attempt: 0 }, studio: { id: "studio", status: "reconnecting", attempt: 1 } };
    const cards = fleetCards(computers, { "": machineMeta("workstation"), studio: machineMeta("studio", { cpu_percent: 42, sampled_at: NOW - 30 }) });
    const studio = cards.find((c) => c.computer === "studio")!;
    expect(studio.connection).toEqual({ tile: "connecting", text: "Reconnecting", fresh: false });
    const cpu = cpuFigure(studio.meta);
    expect(cpu.value).toBe("42%");
    expect(level(cpu.percent, studio.connection.fresh)).toBe("unknown");
    expect(cards.filter((c) => c.connection.fresh)).toHaveLength(1);
    expect(sampleNote(studio.meta, studio.connection, NOW)).toMatch(/^Last sample /);
    const rows = [rowOut("", 1), rowOut("studio", 2), rowOut("studio", 3)].map((r) => toSession(r.computer, r.key, r.row));
    expect(overviewStats(rows, computers).offline).toBe(2);
  });

  it("show percentages as the main figures with Qt's details", () => {
    const live = connectionOf("live");
    const meta = machineMeta("workstation", { cpu_percent: 71.6, cpu_count: 16, load_1: 2.5, memory_used_bytes: 7.5 * 2 ** 30, memory_total_bytes: 31.3 * 2 ** 30 });
    expect(cpuFigure(meta)).toMatchObject({ value: "72%", detail: "16 cores, load 2.50" });
    expect(level(71.6, true)).toBe("warning");
    expect(level(95, true)).toBe("critical");
    expect(level(undefined, true)).toBe("unknown");
    expect(memoryFigure(meta)).toMatchObject({ value: "24%", detail: "7.5 GiB of 31.3 GiB" });
    expect(bytes(512 * 2 ** 20)).toBe("512 MiB");
    expect(cpuFigure(undefined).detail).toBe("Update hgs for metrics");
    expect(cpuFigure(machineMeta("w", { cpu_percent: null, cpu_state: "warming" })).detail).toBe("Waiting for second sample");
    expect(cpuFigure(machineMeta("w", { cpu_percent: null, cpu_state: "unavailable" })).detail).toBe("CPU metrics unavailable");
    expect(memoryFigure(machineMeta("w", { memory_total_bytes: null })).detail).toBe("Memory unavailable");
    expect(sampleNote(meta, live, NOW + 59)).toBe("");
    expect(sampleNote(meta, live, NOW + 60)).toMatch(/^Last sample /);
    expect(sampleNote(undefined, connectionOf("unsupported"), NOW)).toBe("No sample yet");
    expect(sessionsLabel(1, 0)).toBe("1 session");
    expect(sessionsLabel(3, 2)).toBe("3 sessions, 2 need attention");
  });

  it("word the account cards as Qt does", () => {
    const identity = { ...usageOf().identity, email: "person@example.com", plan: "max" };
    expect(accountIdentity(account({ usage: usageOf({ identity }) }))).toBe("person@example.com / max");
    expect(accountIdentity(account({ installed: false }))).toBe("Agent not installed");
    expect(accountTone(account({ usage: usageOf({ windows: [projected({ used: 95, resets_at: null })] }) }), NOW)).toBe("critical");
    expect(accountTone(account(), NOW)).toBe("none");
    expect(accountsNote(emptyAccounts())).toBe("Loading accounts…");
    expect(accountsNote({ ...emptyAccounts(), snapshot: true, loading: true })).toBe("Loading account details…");
    expect(accountsNote({ ...emptyAccounts(), snapshot: true })).toBe("Open Manage accounts to view or set up accounts.");
    expect(accountsNote({ ...emptyAccounts(), snapshot: true, overview: [account()] })).toBeUndefined();
  });
});
