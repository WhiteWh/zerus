// @vitest-environment happy-dom
import { describe, expect, it, vi } from "vitest";

vi.mock("../../../bridge", () => ({ hostAccess: vi.fn() }));
import { outcomeText } from "./SharedHostsSection";

describe("shared hosts outcomes (zerus-un7n)", () => {
  it("shows a host refusal with its contract code, never as success", () => {
    const text = outcomeText({ outcome: "refused", code: "not-a-member", detail: "you are not a member of this project" });
    expect(text).toContain("not-a-member");
    expect(text).toContain("you are not a member of this project");
  });
  it("says an older host lacks the capability instead of falling back", () => {
    expect(outcomeText({ outcome: "unsupported", detail: "x" })).toMatch(/multi-person host access/);
  });
  it("asks to load again after an uncertain write and stays silent on success", () => {
    expect(outcomeText({ outcome: "uncertain", detail: "x", request_id: "r" })).toMatch(/Load again/);
    expect(outcomeText({ outcome: "done", value: {} })).toBe("");
  });
});
