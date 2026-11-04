import { beforeEach, describe, expect, it, vi } from "vitest";
import * as bridge from "../bridge";
import { captureUntrackedShell, untrackedShell, UNVERIFIED_SHELL } from "./untrackedShell";
import { toSession } from "./catalog";
import type { UntrackedShellIdentity } from "../types";
vi.mock("../bridge", () => ({ untrackedShellIdentity: vi.fn() }));
const identity: UntrackedShellIdentity = { version: 1, socket: "/tmp/example-tmux.sock", socket_dev: "1", socket_ino: "9007199254740993", server_pid: 100, server_start: "server-birth", session_id: "$1", session_created: 1700000000, pane_id: "%2", pane_pid: 101, pane_start: "shell-birth", shell: "bash" };
const row = () => toSession("example-peer", "session:example-shell", { name: "example-shell", state: "running", tracked: false, agent: "" });
beforeEach(() => { vi.mocked(bridge.untrackedShellIdentity).mockReset(); });
describe("untracked shell confirmation", () => {
  it("captures the original terminal at dialog preparation, preserving full inode precision", async () => {
    vi.mocked(bridge.untrackedShellIdentity).mockResolvedValue(identity);
    const captured = await captureUntrackedShell(row());
    expect(captured).toEqual({ kind: "terminate_shell", identity });
    expect(bridge.untrackedShellIdentity).toHaveBeenCalledExactlyOnceWith({ computer: "example-peer", name: "example-shell", run_id: "", conversation_id: "" });
    expect(captured.identity.socket_ino).toBe("9007199254740993");
  });
  it.each([{ tracked: true }, { runId: "run" }, { conversationId: "conversation" }, { state: "archived" }, { cmd: "codex" }, { cmd: "claude" }, { cmd: "kimi" }])("excludes agent/saved rows before any terminal lookup: %j", async (changed) => {
    const source = { ...row(), ...changed };
    expect(untrackedShell(source)).toBe(false);
    await expect(captureUntrackedShell(source)).rejects.toBe(UNVERIFIED_SHELL);
    expect(bridge.untrackedShellIdentity).not.toHaveBeenCalled();
  });
  it("never replaces a failed proof lookup with a name-only kill", async () => {
    vi.mocked(bridge.untrackedShellIdentity).mockRejectedValue(UNVERIFIED_SHELL);
    await expect(captureUntrackedShell(row())).rejects.toBe(UNVERIFIED_SHELL);
    expect(bridge.untrackedShellIdentity).toHaveBeenCalledTimes(1);
  });
});
