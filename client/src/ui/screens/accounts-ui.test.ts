import { describe, expect, it } from "vitest";
import { creditsText, loginLabel, memberStateLabel, memberStateText, memberOf, panelWindow, profileTags, summaryLine, visibleAccounts, visibleMembers } from "./accounts-ui";
import { NOW, projected, usageOf } from "../../store/testing";
import { dayTime } from "../lib/usage";
import { emptyAccounts, type Account, type Profile } from "../../types";

const member = (extra: Partial<Profile> = {}): Profile => ({
  key: "\u001fclaude-work", host: "", machine: "workstation", id: "claude-work", label: "Work", provider: "claude", native: false,
  installed: true, is_default: false, home: "", auth_revision: "", online: true, state: "ready", detail: { source: "Ready on workstation." },
  signed_in: true, permission: "auto", permission_label: { source: "Auto" }, permission_hint: { source: "" }, model_supported: true, model_text: { source: "" },
  usage: usageOf(), opencode: null, ...extra,
});
const account = (members: Profile[], key = "a"): Account => ({
  key, label: "Work", provider: members[0].provider, installed: true, machines: members.map((m) => m.machine), members,
  usage: usageOf(), permission: { source: "Auto" }, identity_lines: [], identity_note: null,
});

describe("accounts page", () => {
  it("words every member state for the more list", () => {
    const states = ["ready", "offline", "not-installed", "apikey", "signed-out", "keychain", "desktop", "unavailable", "expired", "checking", "unknown"] as const;
    expect(states.map(memberStateText)).toEqual(["", "Offline", "Not installed", "API key", "Sign-in needed", "Keychain locked", "Desktop sign-in unavailable", "Sign-in unusable", "Sign-in expired", "Checking…", "Unknown"]);
    expect(memberStateText("bogus" as never)).toBe("Unknown");
  });

  it("labels the sign-in button as Qt does", () => {
    expect(loginLabel(member())).toBe("Sign in again on workstation…");
    expect(loginLabel(member({ signed_in: false }))).toBe("Sign in on workstation…");
    expect(loginLabel(member({ provider: "dsh", usage: usageOf({ status: "configured" }) }))).toBe("Change API key on workstation…");
    expect(loginLabel(member({ provider: "dsh", usage: usageOf({ status: "signed_out" }) }))).toBe("Set API key on workstation…");
    expect(loginLabel(member({ provider: "opencode" }))).toBeUndefined();
    expect(loginLabel(member({ installed: false }))).toBeUndefined();
  });

  it("selects the requested profile, else the filtered machine, else the first", () => {
    const studio = member({ key: "studio\u001fclaude-studio", host: "studio", machine: "studio" });
    const a = account([member(), studio]);
    expect(memberOf(a, "studio\u001fclaude-studio", "*")).toBe(studio);
    expect(memberOf(a, undefined, "studio")).toBe(studio);
    expect(memberOf(a, undefined, "*")?.machine).toBe("workstation");
    const other = account([member({ key: "mac\u001fx", host: "mac", machine: "mac" })], "b");
    const view = { ...emptyAccounts(), accounts: [a, other] };
    expect(visibleAccounts(view, "*")).toHaveLength(2);
    expect(visibleAccounts(view, "mac")).toEqual([other]);
  });

  it("shows at most four machine chips, keeping the selected one", () => {
    const many = account(Array.from({ length: 7 }, (_, i) => member({ key: `m${i}\u001fx`, host: `m${i}`, machine: `m${i}` })));
    expect(visibleMembers(many, "m6\u001fx").map((m) => m.machine)).toEqual(["m0", "m1", "m2", "m6"]);
    expect(visibleMembers(many, "m1\u001fx").map((m) => m.machine)).toEqual(["m0", "m1", "m2", "m3"]);
    expect(visibleMembers(account(many.members.slice(0, 4)), undefined)).toHaveLength(4);
    expect(visibleMembers(account(many.members.slice(0, 5)), undefined)).toHaveLength(4);
  });

  it("tags only profiles that share a machine, by native or a unique id prefix", () => {
    const at = (host: string, id: string, extra: Partial<Profile> = {}) => member({ key: `${host}\u001f${id}`, host, machine: host, id, ...extra });
    const tags = profileTags(account([
      at("arch", "claude-6875dbf59f"), at("arch", "claude-d036076906"), at("arch", "native-claude", { native: true }), at("mac", "claude-6875dbf59f"),
    ]));
    expect([...tags]).toEqual([["arch\u001fclaude-6875dbf59f", "6875db"], ["arch\u001fclaude-d036076906", "d03607"], ["arch\u001fnative-claude", "native"]]);
    const codex = { provider: "codex" };
    const close = profileTags(account([at("arch", "codex-94d8b8359f", codex), at("arch", "codex-94d8b8aaaa", codex), at("arch", "short", codex)]));
    expect([...close.values()]).toEqual(["94d8b83", "94d8b8a", "short"]);
    expect(profileTags(account([at("arch", "claude-a"), at("mac", "claude-b")])).size).toBe(0);
    expect(memberStateLabel("ready")).toBe("Ready");
    expect(memberStateLabel("signed-out")).toBe("Sign-in needed");
  });

  it("words the summary as Qt does", () => {
    const view = { ...emptyAccounts(), snapshot: true, loaded_machines: 2, pending: 3 };
    expect(summaryLine(view, 1)).toBe("1 account, 2 machines loaded (Updating… 3 remaining)");
    expect(summaryLine({ ...view, pending: 0, loaded_machines: 1 }, 4)).toBe("4 accounts, 1 machine loaded");
  });

  it("draws a panel window with Qt's threshold colour, its reset and the forecast", () => {
    const w = panelWindow(projected({ used: 75, resets_at: NOW + 150 * 60 }), NOW);
    expect(w).toMatchObject({ period: "5h", usedText: "75% used", used: 75, tone: "warning", resets: `Resets ${dayTime(NOW + 9000)}` });
    expect(w.elapsed).toBeCloseTo(50);
    expect(w.forecast).toBe("At the current pace, the limit runs out in about 50m");
    expect(panelWindow(projected({ resets_at: NOW - 60 }), NOW)).toMatchObject({ usedText: "Window ended; refresh for current usage", used: 0, forecast: "" });
    expect(panelWindow(projected({ label: "Monthly", minutes: null, resets_at: null, used: 50 }), NOW))
      .toMatchObject({ period: "Monthly", usedText: "50% used", elapsed: undefined, resets: undefined, forecast: "" });
    expect(creditsText({ balance: "", unlimited: true })).toBe("Credits: unlimited");
    expect(creditsText({ balance: "lots", unlimited: false })).toBe("Credits available: —");
    expect(creditsText({ balance: "12.5", unlimited: false })).toMatch(/^Credits available: 12[.,]50$/);
    expect(creditsText(null)).toBeUndefined();
  });
});
