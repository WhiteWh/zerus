import { Show } from "solid-js";
import { cleanup, openCleanup } from "../../store/terminationCleanup";

/** The existing statusbar entry; the review dialog loads only when opened. */
export function CleanupChip() {
  return <Show when={cleanup().rows.length || cleanup().kept || cleanup().unknown}><button type="button" class="chip-accent" style={{ "white-space": "pre" }} classList={{ "warn-chip": cleanup().kept > 0 || cleanup().unknown > 0 }} title={cleanup().title} aria-label={cleanup().title} onClick={() => openCleanup()}>{cleanup().text}</button></Show>;
}
