import { catalog, computerOffline, orderedRows } from "./catalog";
import { updating } from "./actions";
import { applies } from "./batch";
import { parentKeyOf } from "../ui/sessions/model";
import { childOffer, liveDescendants, type ChildAction, type ChildOffer } from "../ui/sessions/end-children";
import { selectionSnapshot } from "../ui/sessions/selection";
import type { ActionRow } from "../ui/sessions/actions-ui";

/**
 * zerus-sa9qn.17: the live child sessions a Terminate or Archive confirmation
 * of `row` names, as the list nests them now, split by whether the same
 * action applies to each one now (`applies`, as a selection's batch decides).
 * Snapshots: the batch acts only on these exact instances.
 */
export function childOfferFor(row: ActionRow, a: ChildAction): ChildOffer {
  const listed = catalog.rows[row.key];
  if (!listed) return { ending: [], staying: [] };
  const found = liveDescendants(listed, orderedRows(), parentKeyOf());
  return childOffer(found.map(selectionSnapshot), (s) => applies(s, a, !computerOffline(s.computer), updating(s.computer, s.name)));
}
