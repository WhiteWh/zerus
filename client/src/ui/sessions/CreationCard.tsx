import { AgentBadge, MachineBadge, StatusBadge } from "./SessionCard";
import { tr } from "../../i18n";
import { clearMulti } from "./organize";
import { agentKind } from "../../store/catalog";
import { HIDE_LAUNCH_STATUS, accessibleText, computerOf, creationRowId, rowDetail } from "./creations-ui";
import { hideCreation, selectCreation } from "../../store/creations";
import { peek, selectedKey, setMenu, setPeek } from "../../store/ui";
import type { CreationCard as Card } from "../../types";
import "../styles/creations.css";

// A creation row in Sessions (SessionsWindowRows.cpp 376-391), painted as Qt
// paints it with the session card's delegate: the tag as the title, the
// caption in a neutral status badge, the agent's badge, the folder and the
// computer, and the error or "Preparing the session in the background". A
// click selects the card; its menu hides the status (502-511). Nothing here
// opens a composer or touches drafts.

/** A click selects the card, as a session row's plain click does. */
function choose(id: string) {
  clearMulti();
  selectCreation(id);
  if (peek()) setPeek(false);
}

/** "Hide launch status" (505): the menu of the row under the pointer, which it does not select. */
function menu(e: MouseEvent, id: string) {
  e.preventDefault();
  e.stopPropagation();
  setMenu({ x: e.clientX, y: e.clientY, items: [{ label: tr(HIDE_LAUNCH_STATUS), action: () => { void hideCreation(id); } }] });
}

export function CreationCard(props: { card: Card }) {
  const c = () => props.card;
  return (
    <div class="rowhit" onClick={() => choose(c().id)} onContextMenu={(e) => menu(e, c().id)}>
      <span class="cr-a11y">{accessibleText(c())}</span>
      <div class="scard" classList={{ selected: selectedKey() === creationRowId(c().id) }} aria-hidden="true">
        <div class="sc-l1">
          <span class="sc-title ell">{c().tag}</span>
          <StatusBadge kind="neutral" caption={c().caption} />
          <span class="grow" />
          <AgentBadge agent={agentKind(c().cmd)} />
        </div>
        <div class="sc-l2">
          <span class="sc-meta ell">{c().target}</span>
          <span class="grow" />
          <MachineBadge alias={computerOf(c())} computer={c().host} />
        </div>
        <div class="sc-l3"><span class="ell">{rowDetail(c())}</span></div>
      </div>
    </div>
  );
}

/**
 * A creation row in the collapsed strip, a tile like a session's
 * (SessionCardDelegate.h paintStripCard): the neutral status mark and the
 * agent's badge, the tag on up to two lines, the caption. The tooltip names
 * them as StripTile does for a session (SessionList.cpp 267-286). A click
 * selects the card, as on the full list.
 */
export function CreationTile(props: { card: Card }) {
  const c = () => props.card;
  const tip = () => [`${c().tag}: ${c().caption}`, rowDetail(c()), `${c().target} on ${computerOf(c())}`].filter(Boolean).join("\n");
  return (
    <button class="strip-tile" classList={{ selected: selectedKey() === creationRowId(c().id) }} data-frame="neutral" title={tip()} aria-label={accessibleText(c())}
      onClick={() => choose(c().id)} onContextMenu={(e) => menu(e, c().id)}>
      <span class="st-block">
        <span class="st-top">
          <span class="st-status"><StatusBadge kind="neutral" caption="" iconOnly /></span>
          <span class="st-provider"><AgentBadge agent={agentKind(c().cmd)} narrow /></span>
        </span>
        <span class="st-name">{c().tag}</span>
        <span class="st-state" data-tone="neutral">{c().caption}</span>
      </span>
    </button>
  );
}
