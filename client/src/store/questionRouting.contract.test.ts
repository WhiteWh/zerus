import { describe, expect, it } from "vitest";
import { questionRoutingOf } from "./catalog";
import contract from "../../../mobile/android/app/src/test/resources/question-routing-contract.json";

// The shared question routing contract (zerus-sa9qn.33): the desktop reads a
// row's switch exactly as the phones and the core do.
describe("a row's question routing switch", () => {
  it("parses only the exact shape and anything else is unknown", () => {
    for (const c of contract.parse) {
      const routing = c.routing ? { sendToParent: c.routing.send_to_parent, revision: c.routing.revision,
        personOff: (c.routing as { person_off?: boolean }).person_off === true } : undefined;
      expect(questionRoutingOf(c.question_routing), c.name).toEqual(routing);
    }
    expect(questionRoutingOf(undefined)).toBeUndefined();
  });
});
