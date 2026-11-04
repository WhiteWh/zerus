import { defaultRangeExtractor, type Range, type VirtualItem } from "@tanstack/solid-virtual";
import { markdownTree, type Inline, type MarkdownBlock } from "./markdown";
import { tr } from "../../i18n";
import type { TranscriptItem } from "./types";

export const activityTexts = new WeakMap<HTMLDivElement, () => string>();
export const activityAll = new WeakMap<HTMLDivElement, boolean>();

export function selectionCoversActivity(scroller: HTMLDivElement): boolean {
  const col = scroller.querySelector(".act-col"), selection = document.getSelection();
  if (!col || !selection || selection.isCollapsed || selection.rangeCount === 0) return false;
  const range = selection.getRangeAt(0);
  return range.startContainer === col && range.startOffset === 0
    && range.endContainer === col && range.endOffset === col.childNodes.length;
}

/** Keep the entire logical interval, including rows between distant endpoints. */
export function activitySelectionIndexes(scroller: HTMLDivElement, count: number): number[] {
  const selection = document.getSelection();
  if (!selection || selection.isCollapsed || !selection.rangeCount) return [];
  if (selectionCoversActivity(scroller)) return Array.from({ length: count }, (_, index) => index);
  const range = selection.getRangeAt(0);
  const indexOf = (node: Node): number | undefined => {
    const element = node.nodeType === Node.ELEMENT_NODE ? node as Element : node.parentElement;
    const row = element?.closest<HTMLElement>(".act-virtual-row");
    const index = Number(row?.dataset.index);
    return row && scroller.contains(row) && Number.isSafeInteger(index) && index >= 0 && index < count ? index : undefined;
  };
  const intersecting = [...scroller.querySelectorAll<HTMLElement>(".act-virtual-row")].filter((row) => range.intersectsNode(row));
  const start = indexOf(range.startContainer) ?? (intersecting[0] ? indexOf(intersecting[0]) : undefined);
  const end = indexOf(range.endContainer) ?? (intersecting.at(-1) ? indexOf(intersecting.at(-1)!) : undefined);
  return start === undefined || end === undefined ? [] : Array.from({ length: Math.abs(end - start) + 1 }, (_, index) => Math.min(start, end) + index);
}

export function selectActivityAll(scroller: HTMLDivElement) {
  const col = scroller.querySelector(".act-col");
  if (!col) return;
  document.getSelection()?.selectAllChildren(col);
  activityAll.set(scroller, true);
}

export function activitySelectionText(scroller: HTMLDivElement): string {
  const selected = document.getSelection()?.toString() ?? "";
  return activityAll.get(scroller) && selectionCoversActivity(scroller)
    ? activityTexts.get(scroller)?.() ?? selected : selected;
}

function inlineText(parts: Inline[]): string {
  return parts.map((part) => part.kind === "br" ? "\n" : "text" in part ? part.text : inlineText(part.children)).join("");
}

function blocksText(blocks: MarkdownBlock[]): string {
  return blocks.map((block): string => {
    switch (block.kind) {
      case "p": case "text": case "heading": return inlineText(block.inline);
      case "code": return block.text;
      case "quote": return blocksText(block.blocks);
      case "list": return block.items.map((item) => blocksText(item.blocks)).join("\n");
      case "table": return [block.header, ...block.rows].map((row) => row.map(inlineText).join("\t")).join("\n");
      case "hr": return "";
    }
  }).join("\n");
}

const markdownText = (text: string) => blocksText(markdownTree(text));

const lines = (values: (string | undefined)[]) => values.filter((value): value is string => !!value).join("\n");

export function activityText(items: TranscriptItem[], expanded: Record<string, boolean> = {}): string {
  return items.map((item) => {
    if (item.kind === "media") return lines([
      item.media === "video" ? tr("You (video)") : tr("You (voice)"), item.at,
      expanded[item.id] ? item.transcript : undefined, item.error,
    ]);
    if (item.kind === "context") return item.body ?? "";
    if (item.kind === "thinking") return lines([tr("Thinking"), item.body]);
    if (item.kind === "report") return lines([
      item.title, item.sender ? tr("from %1", item.sender) : undefined, item.at,
      expanded[item.id] ? item.body : undefined,
    ]);
    if (item.kind === "tool" || item.kind === "tools") return lines([
      item.title, item.kind === "tools" ? item.body : undefined, item.at,
      item.kind === "tool" ? item.body : undefined,
      expanded[item.id] ? item.lines?.join("\n") : undefined,
    ]);
    const body = item.replies?.length
      ? item.replies.map((reply) => lines([tr("Question"), reply.question, tr("Your answer"), reply.answer])).join("\n")
      : item.kind === "user" ? item.body : item.body ? markdownText(item.body) : undefined;
    return lines([
      item.label ?? (item.kind === "user" ? tr("You") : item.kind === "agent" ? tr("Agent") : item.sender ?? "Zerus"),
      item.local?.stamp ?? item.at, item.receipt, body, item.local?.error, item.code,
      item.attachments?.map((attachment, index) => item.local && !item.local.available[index]
        ? `Attachment: ${attachment.name}` : lines([attachment.caption, attachment.meta])).join("\n"),
    ]);
  }).filter(Boolean).join("\n\n");
}

export function activityRange(range: Range, playingIndex: number, measuring: readonly number[] = []): number[] {
  const indexes = defaultRangeExtractor(range);
  const retained = [...measuring, playingIndex].filter((index) => index >= 0 && index < range.count);
  return [...new Set([...indexes, ...retained])].sort((a, b) => a - b);
}

export function activityVirtualOffset(scroller: HTMLElement, virtual: HTMLElement, start: number): number {
  const edge = scroller.getBoundingClientRect().top + scroller.clientTop;
  return virtual.getBoundingClientRect().top - edge + start;
}

export function activityFlow(items: readonly Pick<VirtualItem, "key" | "start" | "end">[]) {
  const margins = new Map<string, number>(), starts = new Map<string, number>();
  items.forEach((item, index) => {
    starts.set(String(item.key), item.start);
    margins.set(String(item.key), index === 0 ? 0 : Math.max(0, item.start - items[index - 1].end));
  });
  return { padding: items[0]?.start ?? 0, margins, starts };
}

/** The explicit premeasure pass must complete even during initial placement.
 * Virtualizer.measureElement defers to ResizeObserver while scrolling, which
 * cannot measure rows removed immediately after this pass. */
export function measureActivityRows(rows: Iterable<HTMLElement>, resize: (index: number, height: number) => void) {
  for (const row of rows) {
    const index = Number(row.dataset.index), height = row.getBoundingClientRect().height;
    if (row.isConnected && Number.isInteger(index) && index >= 0 && Number.isFinite(height) && height > 0) resize(index, height);
  }
}

export interface ActivityGeometryRow {
  index: number;
  key: string;
  top: number;
  bottom: number;
  height: number;
  start: number;
  domStart: number;
}

export interface ActivityGeometryOverlap {
  above: ActivityGeometryRow;
  below: ActivityGeometryRow;
  pixels: number;
  virtualPixels: number;
}

export interface ActivityGeometryReport {
  rows: ActivityGeometryRow[];
  visible: ActivityGeometryRow[];
  overlaps: ActivityGeometryOverlap[];
  firstVisibleGap: number | null;
}

export function checkActivityGeometry(rows: readonly ActivityGeometryRow[], viewportTop: number, viewportHeight: number): ActivityGeometryReport {
  const ordered = [...rows].sort((a, b) => a.index - b.index);
  const visible = ordered.filter((row) => row.bottom > viewportTop && row.top < viewportTop + viewportHeight);
  const overlaps: ActivityGeometryOverlap[] = [];
  for (let index = 1; index < ordered.length; index++) {
    const above = ordered[index - 1], below = ordered[index];
    const pixels = above.bottom - below.top;
    if (pixels > 0) overlaps.push({ above, below, pixels, virtualPixels: above.start + above.height - below.start });
  }
  const first = visible[0];
  return { rows: ordered, visible, overlaps, firstVisibleGap: first ? Math.max(0, first.top - viewportTop) : null };
}

export function activityGeometry(scroller: HTMLElement): ActivityGeometryReport {
  const edge = scroller.getBoundingClientRect().top + scroller.clientTop;
  const virtualRows = [...scroller.querySelectorAll<HTMLElement>(".act-virtual-row")];
  const rows = virtualRows.length ? virtualRows : [...scroller.querySelectorAll<HTMLElement>(".act-col > [id^=item-]")];
  const container = scroller.querySelector<HTMLElement>(".act-virtual") ?? scroller.querySelector<HTMLElement>(".act-col");
  const containerTop = container?.getBoundingClientRect().top ?? edge;
  const measured = rows.map((row, index) => {
    const rect = row.getBoundingClientRect();
    const domStart = rect.top - containerTop;
    return {
      index: virtualRows.length ? Number(row.dataset.index) : index,
      key: virtualRows.length ? row.querySelector<HTMLElement>('[id^="item-"]')?.id ?? row.dataset.index ?? "" : row.id,
      top: rect.top,
      bottom: rect.bottom,
      height: rect.height,
      start: Number(row.dataset.start ?? domStart),
      domStart,
    };
  });
  return checkActivityGeometry(measured, edge, scroller.clientHeight);
}

export function activityGeometryValid(report: ActivityGeometryReport, estimateSize = 96): boolean {
  return report.rows.length > 0 && report.visible.length > 0 && report.overlaps.length === 0
    && (report.firstVisibleGap === null || report.firstVisibleGap <= estimateSize);
}
