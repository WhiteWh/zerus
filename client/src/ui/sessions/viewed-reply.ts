import { createEffect, on, onCleanup } from "solid-js";
import { windowVisibility } from "../../bridge";
import { followedFor } from "../../store/actions";
import { computerFresh } from "../../store/catalog";
import { composer, draftKey } from "../../store/composer";
import { markReplyRead } from "../../store/marks";
import { activity } from "../../store/session";
import { popupOpen } from "../../store/shortcuts";
import { terminal } from "../../store/terminal";
import { detailTab, page, selected, selectedKey, setReadSelectionKey, subagentOpen } from "../../store/ui";
import { CHECK_MS, createViewedReply, type ViewedFacts } from "../../store/viewedReply";
import { documentFocused, pageHidden } from "../../store/visibility";
import { activityShowsReply } from "./Activity";

// Qt's m_readTimer (SessionsWindow.cpp:2028, started in showEvent 5834 and
// stopped in hideEvent 5869) drives checkViewedReply: while the window shows,
// the selected session's reply is checked every 400 ms and read after a
// second in view (store/viewedReply.ts). Nothing else calls it in Qt.

/** One check's facts, read when it runs; the window and the view are read only for an unread reply. */
function facts(): ViewedFacts {
  const s = selected();
  const v = composer.view();
  return {
    subagent: !!subagentOpen(),
    row: s,
    online: !!s && computerFresh(s.computer),
    // Qt's m_details: the followed session's own inspection, never another row's.
    inspected: s ? followedFor(s.key).value : undefined,
    // Qt's m_inspectError: the follow failed, or the open composer's last read did.
    inspectError: !!s && ((activity.target()?.id === s.key && !!activity.error())
      || (!!v?.inspect_error && v.key === draftKey({ computer: s.computer, key: s.catalogKey, name: s.name, archive_id: null }))),
    tab: detailTab(),
    active: () => page() === "sessions" && !pageHidden() && !windowVisibility.hidden() && documentFocused() && !popupOpen(),
    activityShows: (reply) => !!s && activityShowsReply(s.key, reply),
    terminalShowsLatest: () => terminal.showsLatest(),
  };
}

/** Checks the viewed reply while the session detail is mounted (the Sessions page) and the window shows. */
export function followViewedReplies() {
  const viewed = createViewedReply({ now: () => performance.now(), markReplyRead, keep: setReadSelectionKey });
  createEffect(() => {
    // A hidden or minimised window stops the timer and forgets the candidate (hideEvent).
    if (pageHidden()) { viewed.reset(); return; }
    const timer = setInterval(() => { viewed.check(facts()); }, CHECK_MS);
    onCleanup(() => clearInterval(timer));
  });
  // Another selection starts over (selectSession and selectCreation clear m_readCandidate).
  createEffect(on(selectedKey, () => { viewed.reset(); }, { defer: true }));
  onCleanup(() => viewed.reset());
}
