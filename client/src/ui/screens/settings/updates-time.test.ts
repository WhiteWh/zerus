import { describe, expect, it } from "vitest";
import { lastCheckedText } from "./updates-time";
import { clockOf } from "../../lib/dates";

describe("Updates last successful check", () => {
  it("shows Never as the interface's word", () => {
    expect(lastCheckedText("Never")).toBe("Never");
  });

  it("shows the timestamp at local time, with the year only when it differs", () => {
    const raw = "1970-01-01T00:16:40Z";
    const at = new Date(raw);
    expect(lastCheckedText(raw, new Date(at.getFullYear(), 6, 1))).toBe(`${at.getDate()} ${["Jan", "Dec"][at.getMonth() === 0 ? 0 : 1]} ${clockOf(at)}`);
    expect(lastCheckedText(raw, new Date(2026, 0, 1))).toContain(String(at.getFullYear()));
    expect(lastCheckedText(raw)).not.toContain("T00:16:40Z");
    expect(lastCheckedText("not a time")).toBe("not a time");
  });
});
