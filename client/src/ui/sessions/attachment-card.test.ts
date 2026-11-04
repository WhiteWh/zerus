import { describe, expect, it } from "vitest";
import { attachments } from "./transcript";
import { cardTarget, createOpen, OPENING } from "./attachment-card";
import type { AttachmentScope } from "../../store/activitySource";
import type { ActivityEvent } from "../../types";

const message = (files: ActivityEvent["attachments"]): ActivityEvent => ({ seq: 1, type: "UserPromptSubmit", detail: "See the shot", attachments: files });
const scope: AttachmentScope = { computer: "studio", name: "claude/sample-project/review", conversation_id: "conv-1", archive_id: null };

describe("Activity attachment cards (ActivityView.cpp:759-782)", () => {
  it("caption, size and thumbnail as Qt shows them", () => {
    const [shot, notes, drawing, unsaved] = attachments(message([
      { name: "shot.png", mime: "image/png", bytes: 1500, reference: "[Image #1]", request_id: "req-1", index: 0 },
      { name: "notes.txt", mime: "text/plain", bytes: 1, request_id: "req-1", index: 1 },
      { name: "plan.svg", mime: "image/svg+xml", bytes: 0, request_id: "req-1", index: 2 },
      { name: "old.bin", mime: "application/octet-stream", bytes: 2048 },
    ]))!;
    expect(shot).toEqual({ name: "shot.png", caption: "[Image #1] shot.png", mime: "image/png", meta: "2 KiB", request_id: "req-1", index: 0, image: true });
    expect([notes.caption, notes.meta, notes.image]).toEqual(["notes.txt", "1 KiB", false]);
    expect([drawing.meta, drawing.image]).toEqual(["File", false]);
    expect([unsaved.request_id, unsaved.index]).toEqual(["", -1]);
    expect(attachments(message([]))).toBeUndefined();
  });

  it("reads a file by its message, in the conversation shown", () => {
    const [shot, unsaved] = attachments(message([{ name: "shot.png", mime: "image/png", request_id: "req-1", index: 0 }, { name: "old.bin" }]))!;
    expect(cardTarget(shot, scope)).toEqual({
      computer: "studio",
      ref: { name: "claude/sample-project/review", request_id: "req-1", index: 0, conversation_id: "conv-1", archive_id: null, agent_id: null },
    });
    expect(cardTarget(shot, { ...scope, archive_id: "a-1" })?.ref.archive_id).toBe("a-1");
    expect(cardTarget(unsaved, scope), "no saved reference: the card only names the file").toBeUndefined();
    expect(cardTarget(shot, undefined)).toBeUndefined();
  });
});

describe("Open on an Activity attachment (SessionsWindow.cpp:1093-1102)", () => {
  const ref = cardTarget(attachments(message([{ name: "shot.png", request_id: "req-1", index: 0 }]))![0], scope)!.ref;

  function harness(answer: () => Promise<string | null>) {
    const said: [string, string, boolean, string][] = [];
    const open = createOpen({ open: answer, notify: (tone, text, sticky, key) => { said.push([tone, text, sticky, key]); } });
    return { open, said };
  }

  it("says it is opening, and leaves that line to expire when it opened", async () => {
    const h = harness(async () => null);
    await h.open("studio", ref);
    expect(h.said).toEqual([["info", "Opening attachment…", false, OPENING]]);
  });

  it("replaces the line with the file manager's notice, or with the error as a warning", async () => {
    const shown = harness(async () => "setup.exe can run programs, so it is shown in the file manager instead of being opened.");
    await shown.open("studio", ref);
    expect(shown.said[1]).toEqual(["info", "setup.exe can run programs, so it is shown in the file manager instead of being opened.", false, OPENING]);
    const failed = harness(async () => { throw "No application could open this file."; });
    await failed.open("studio", ref);
    expect(failed.said[1]).toEqual(["error", "No application could open this file.", true, OPENING]);
  });
});

// Every text of Task 9 is Qt's: it appears in the Qt source it comes from and
// in the client source that shows it. Read from disk as the Settings text test does.
const fs = (await import(/* @vite-ignore */ ["node", "fs"].join(":"))) as { readFileSync(path: URL, encoding: "utf8"): string };
const repo = new URL("../../../../", import.meta.url);
const read = (file: string) => fs.readFileSync(new URL(file, repo), "utf8");

describe("Task 9's texts are Qt's", () => {
  const cases: [qt: string, client: string, text: string][] = [
    ["tray/src/AttachmentViewer.h", "client/src-tauri/src/composer/fetch.rs", "No application could open this file."],
    ["tray/src/AttachmentViewer.h", "client/src-tauri/src/composer/fetch.rs", "This attachment has no saved file reference."],
    ["tray/src/MessageComposer.cpp", "client/src-tauri/src/composer/fetch.rs", "Could not open the attachment."],
    ["tray/src/MessageComposer.cpp", "client/src-tauri/src/composer/fetch.rs", "Could not open the attachment with its default application."],
    ["tray/src/SessionsWindow.cpp", "client/src/ui/sessions/attachment-card.ts", "Opening attachment…"],
    ["tray/src/ActivityView.cpp", "client/src/ui/sessions/Activity.tsx", "Open →"],
  ];
  it.each(cases)("%s and %s say %s", (qt, client, text) => {
    expect(read(qt)).toContain(`tr("${text}")`);
    expect(read(client)).toContain(text);
  });

  it("fills in Qt's formats", () => {
    expect(read("tray/src/ActivityView.cpp")).toContain('tr("Attachment: %1")');
    expect(read("client/src/ui/sessions/Activity.tsx")).toContain('tr("Attachment: %1", props.file.name)');
    expect(read("tray/src/ActivityView.cpp")).toContain('tr("%1 KiB")');
    expect(read("client/src/ui/sessions/transcript.ts")).toContain("`${Math.max(1, Math.ceil(bytes / 1024))} KiB` : \"File\"");
    expect(read("tray/src/MessageComposer.cpp")).toContain('tr("Open %1")');
    expect(read("client/src/ui/sessions/Attachments.tsx")).toContain('title={tr("Open %1", a().name)}');
  });
});
