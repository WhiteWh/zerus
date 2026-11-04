import { Show, createEffect } from "solid-js";
import { tr } from "../../i18n";
import { BUTTON, PROGRESS, createDetailsView, panelButtons, panelStatus } from "./creations-ui";
import { cardVersion, creationDetailsText, editChoices, hideCreation, returnToFilters, runCreationAction, selectedCard } from "../../store/creations";
import { now } from "../../store/ui";
import "../styles/creations.css";

// The creation progress view in the detail pane while a card is selected
// (SessionsWindow.cpp: the panel 1509-1537, renderCreation 3729-3756). The
// title is the tag, with "{caption} ({m}:{ss})" under it (D6); the details
// come from the core (`creation_details`), read again when the card
// changes and every second (the controller's clock, 65), keeping their
// scroll; the buttons follow Qt's grid (1522-1524). Each button acts on the
// operation shown. Nothing here opens a composer or touches drafts.

export function CreationPanel() {
  let text: HTMLTextAreaElement | undefined;
  let details: { element: HTMLTextAreaElement; view: ReturnType<typeof createDetailsView> } | undefined;
  const card = selectedCard;
  const id = () => card()?.id;

  createEffect(() => {
    if (!text) return;
    if (details?.element !== text) details = { element: text, view: createDetailsView(text) };
    const current = id();
    if (current !== undefined) {
      cardVersion(current);
      now();
    }
    const answer = details.view.read(current);
    if (current === undefined) return;
    creationDetailsText(current).then(answer, () => {});
  });

  return (
    <section class="detail cr-detail">
      <Show when={card()}>{(c) => {
        const buttons = () => panelButtons(c());
        return (
          <>
            <header class="cr-head">
              <h1 class="cr-title">{c().tag}</h1>
              <p class="cr-status tabular">{panelStatus(c(), now())}</p>
            </header>
            <textarea class="field cr-text" readonly aria-label={tr(PROGRESS)} ref={text} />
            <div class="cr-actions">
              <Show when={buttons().resume}><button class="btn c1" onClick={() => void runCreationAction(c().id, "resume")}>{tr(BUTTON.resume)}</button></Show>
              <Show when={buttons().check}><button class="btn c2" onClick={() => void runCreationAction(c().id, "check")}>{tr(BUTTON.check)}</button></Show>
              <Show when={buttons().stop}>
                <button class="btn wide" disabled={!buttons().stopEnabled} onClick={() => void runCreationAction(c().id, "stop")}>{buttons().stopText}</button>
              </Show>
              <Show when={buttons().edit}><button class="btn c1" onClick={() => void editChoices(c().id)}>{tr(BUTTON.edit)}</button></Show>
              <button class="btn c2" onClick={() => void hideCreation(c().id)}>{tr(BUTTON.hide)}</button>
              <button class="btn wide" onClick={returnToFilters}>{tr(BUTTON.back)}</button>
            </div>
          </>
        );
      }}</Show>
    </section>
  );
}
