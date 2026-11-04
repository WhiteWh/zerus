import type { ActivityEvent } from "../types";
import type { ActivityStore } from "./activity";

// What the Activity view (ui/sessions/Activity.tsx) reads: the live follow of
// a session (store/activity.ts) or the recorded Activity of an archived row
// (store/archiveActivity.ts). A source that lacks something renders nothing
// for it.

export interface ActivitySource {
  /** The session shown; `id` is its row id (computer + "\n" + key). */
  target(): { id: string } | undefined;
  /** Layout continuity only; target() remains the current run's routing identity. */
  displayKey?(): string | undefined;
  /** Retained transcript content during resume, never live action/read evidence. */
  transcriptSummary?(): Record<string, unknown> | undefined;
  /** Oldest first. */
  events(): readonly ActivityEvent[];
  /** The fields the transcript reads besides the events (a live `state` value, an inspection's other fields). */
  summary(): Record<string, unknown> | undefined;
  /** The first answer arrived. */
  loaded(): boolean;
  loadingOlder(): boolean;
  /** No older page to read: the view never asks for one. */
  olderDone(): boolean;
  /** The session left the list and its Activity stopped. */
  ended(): boolean;
  /** Reads the page before the oldest event. */
  loadOlder(): void;
  /** Why the last read failed; a read-only source shows it as a notice while set. */
  error(): string | undefined;
  /**
   * Recorded Activity that is only read: no live states, send or interrupt.
   * Its separate composer may still edit the archived draft. The live follow leaves it out.
   */
  readonly readOnly?: boolean;
  /** Where its attachments are read from; undefined while nothing is shown. A source without it opens no attachment. */
  attachmentScope?(): AttachmentScope | undefined;
}

/**
 * The conversation an Activity's attachments belong to (Qt's
 * AttachmentLoader::fetch arguments): its computer, session, conversation and
 * archive. The main Activity's messages are the session's own (no subagent).
 */
export interface AttachmentScope {
  computer: string;
  name: string;
  conversation_id: string;
  archive_id: string | null;
  agent_id?: string | null;
}

type Holds<T extends true> = T;
/** The live follow is a source as it is: this fails to compile otherwise. */
export type LiveIsSource = Holds<ActivityStore extends ActivitySource ? true : false>;

/**
 * True for an archived row (`archive:<archive id>`, as a catalog key or a row
 * id): its Activity is recorded and read-only, and it is never streamed. The
 * separate composer keeps the archived draft editable, with Send blocked.
 */
export function activityReadOnly(key: string): boolean {
  return key.slice(key.lastIndexOf("\n") + 1).startsWith("archive:");
}
