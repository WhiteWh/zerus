import { batch, createEffect, createMemo, createRoot, createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { inTauri } from "../bridge";
import { catalog, computerFresh, orderedRows } from "./catalog";
import { archive, archivedSession, fleetComputers } from "./archive";
import { machineCards } from "./machines";
import { defaultProject, groups } from "./organization";
import { bindSearchRow, machineFilter, matchesFilter, page, projectFilter, accountFilter, folderFilter, search, selectedKey, setDetailTab, setSelectedKey, setSubagentOpen, stateFilter } from "./ui";
import { compileScopes } from "../ui/sessions/scope-filters";
import { pageHidden } from "./visibility";
import { notify, bindSearchActionRow } from "./actions";
import { createContentSearch, identityMatches, metadataMatches, mergeSessionMatch, searchCount, SEARCH_ROWS, type FoundHit, type SearchReply } from "./content-search-model";
import { clearSearchPreview, setSearchPreview } from "./searchSelection";
import type { Session } from "../ui/sessions/types";
import { trNoop, tr } from "../i18n";

export interface SearchItem { id: string; session: Session; found?: FoundHit; run: string; conversation: string }
export const STALE_SEARCH = trNoop("This session changed. Search again to see its current history.");
export { identityMatches, metadataMatches } from "./content-search-model";

export const { contentSearch, searchItems, searchStatus, activateSearchItem } = createRoot(() => {
  const contentSearch = createContentSearch({
    read: (computer, query, generation) => inTauri() ? invoke<SearchReply>("content_search", { computer, query, generation }) : Promise.reject(tr("Contents search needs the desktop app.")),
    cancel: (through) => { if (inTauri()) void invoke("cancel_content_search", { through }).catch(() => {}); },
  });
  const [held, setHeld] = createSignal<Session>();
  const heldRow = (key: string) => held()?.key === key ? held() : undefined;
  bindSearchRow(heldRow); bindSearchActionRow(heldRow);
  const liveRow = (computer: string, hit: FoundHit["hit"]) =>
    (hit.run_id ? catalog.rows[`${computer}\nrun:${hit.run_id}`] : undefined) ?? catalog.rows[`${computer}\nname:${hit.name}`];
  const hosts = createMemo(() => fleetComputers(catalog.computers, orderedRows(), machineCards)
    .filter((host) => !machineFilter().length || machineFilter().includes(host.machine))
    .filter((host) => !folderFilter() || folderFilter()!.computer === host.computer)
    .map((host) => ({ ...host, online: computerFresh(host.computer) })));
  createEffect(() => contentSearch.configure(search(), hosts(), page() === "sessions" && !pageHidden()));
  createEffect(() => { if (!search().trim()) clearSearchPreview(); if (held()?.key !== selectedKey()) setHeld(undefined); });
  const scoped = createMemo(() => compileScopes({ machines: machineFilter(), projects: projectFilter(), accounts: accountFilter(), folder: folderFilter() }));
  const accepts = (row: Session, group: string) => matchesFilter(row, stateFilter()) && scoped()(row, group)
    && (stateFilter() !== "archived" || archive.includes((row as { archivedAt?: number }).archivedAt ?? 0));
  const built = createMemo(() => {
    const items: SearchItem[] = [], positions = new Map<string, number>(); let limited = false;
    const append = (item: Omit<SearchItem, "run" | "conversation">) => {
      if (!mergeSessionMatch(items, positions, { ...item, id: `session:${item.session.key}`, run: item.session.runId ?? "", conversation: item.session.conversationId ?? "" }, SEARCH_ROWS)) limited = true;
    };
    const query = search().trim(); if (!query) return { items, limited };
    const metadata = stateFilter() === "archived" ? archive.rows() : orderedRows();
    for (const row of metadata) {
      const group = (row as { group?: string }).group ?? groups[row.key]?.group ?? defaultProject();
      if (accepts(row, group) && metadataMatches(row, query)) { if (items.length >= 300) { limited = true; continue; } append({ id: `metadata:${row.key}`, session: row }); }
    }
    for (const found of contentSearch.hits()) {
      const hit = found.hit;
      const row = hit.archive_id && hit.session ? archivedSession(found.computer, found.machine, { ...hit.session, group: hit.group }) : liveRow(found.computer, hit);
      const group = hit.archive_id ? hit.group : row ? groups[row.key]?.group ?? defaultProject() : "";
      if (identityMatches(row, hit) && accepts(row, group)) append({ id: `${row.key}\n${hit.message_id}`, session: row, found });
    }
    return { items, limited };
  });
  const searchItems = () => built().items;
  const searchStatus = createMemo(() => {
    const items = searchItems();
    let text = [...search().trim()].length < 2 ? "Type 2+ characters to search message contents" : searchCount(items.filter((item) => item.found).length, items.length);
    if (contentSearch.busy()) text += " — Searching…";
    if (contentSearch.limited() || built().limited) text += " — Limited history searched";
    const errors = new Set(contentSearch.errors());
    if (accountFilter().length) for (const found of contentSearch.hits()) {
      if (found.hit.archive_id && found.hit.session?.account_binding_reported === false) errors.add(`${found.machine}: contents unavailable`);
    }
    if (errors.size) text += ` — ${[...errors].join("; ")}`;
    return text;
  });
  function activateSearchItem(item: SearchItem) {
    const current = searchItems().find((candidate) => candidate.id === item.id);
    const row = item.session.archiveId ? current?.session : catalog.rows[item.session.key];
    if (!current || !row || row.name !== item.session.name || (row.runId ?? "") !== item.run || (row.conversationId ?? "") !== item.conversation) { notify("error", tr(STALE_SEARCH), true); return; }
    batch(() => {
      if (row.archiveId) setHeld(row);
      setSelectedKey(row.key); setSubagentOpen(undefined); setDetailTab("activity");
      if (item.found) setSearchPreview({ key: row.key, run: row.runId ?? "", conversation: row.conversationId ?? "", query: search().trim(), hit: item.found.hit });
      else clearSearchPreview();
    });
  }
  return { contentSearch, searchItems, searchStatus, activateSearchItem };
});
