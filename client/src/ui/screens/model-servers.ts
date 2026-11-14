import { tr } from "../../i18n";
import { trText } from "../../text";
import type { ModelServer, ModelServerForm, ModelServerModel, ModelServerCatalogModel, ModelServerVariant } from "../../bridge";
import type { StatusEntry } from "../../store/modelServers";
import { machineCards } from "../../store/machines";

/** This computer's Machines (Qt m_fleet.peerNames()), to name an SSH host as one of them. */
export const machineNames = () => Object.keys(machineCards).filter((alias) => alias !== "");

// Task 67: what the Model servers tab and Settings > Model servers say and
// allow (Qt ModelServersPage.cpp and ModelServerSettings.cpp). Pure: the
// store's servers and statuses in, words and rules out.

/** The header: servers, models, running, without dots between them. */
export function summary(servers: readonly ModelServer[], statuses: Record<string, StatusEntry>): string {
  if (!servers.length) return "";
  let models = 0, running = 0;
  for (const s of servers) { const st = statuses[s.name]?.status; models += st?.models.length ?? 0; running += st?.running ?? 0; }
  return tr("%1, %2, %3 running", servers.length === 1 ? tr("1 server") : tr("%1 servers", servers.length), models === 1 ? tr("1 model") : tr("%1 models", models), running);
}

export type Tone = "accent" | "warn" | "folder" | "danger" | "muted";

/** The server's pill: why it was not read, reading, read only, online. */
export function pill(server: ModelServer, entry: StatusEntry): { text: string; tone: Tone } {
  const st = entry.status;
  if (entry.known && st?.error) return { text: trText(st.error_label), tone: "danger" };
  if (!entry.known) return { text: entry.reading ? tr("Reading") : tr("Not read yet"), tone: "muted" };
  if (server.read_only) return { text: tr("Read only"), tone: "folder" };
  return { text: tr("Online"), tone: "accent" };
}

export function stateTone(state: string): Tone {
  if (state === "running") return "accent";
  if (state === "loading" || state === "starting" || state === "stopping") return "warn";
  if (state === "listed") return "folder";
  if (state === "failed" || state === "error") return "danger";
  return "muted";
}

/** This computer, one of Machines, or an SSH alias. */
export function hostText(server: ModelServer, machines: readonly string[]): string {
  if (!server.ssh) return tr("This computer");
  return machines.includes(server.ssh) ? tr("%1 in Machines", server.ssh) : tr("ssh alias %1", server.ssh);
}

export function ago(readAt: number, now: number): string {
  const seconds = Math.max(0, Math.floor((now - readAt) / 1000));
  if (seconds < 60) return tr("read %1 s ago", seconds);
  if (seconds < 3600) return tr("read %1 min ago", Math.floor(seconds / 60));
  return tr("read %1 h ago", Math.floor(seconds / 3600));
}

const two = (n: number) => String(n).padStart(2, "0");
/** "since 09:12" today, "since 28 Oct 09:12" before. */
export function sinceText(seconds: number, now: Date = new Date()): string {
  const at = new Date(seconds * 1000);
  const time = `${two(at.getHours())}:${two(at.getMinutes())}`;
  if (at.toDateString() === now.toDateString()) return tr("since %1", time);
  const month = at.toLocaleString(undefined, { month: "short" });
  return tr("since %1", `${at.getDate()} ${month} ${time}`);
}

/** How this server is read, in its footer. */
export function footText(server: ModelServer): string {
  if (server.read_only) return tr("No control commands: the models come from the API's model list. No Up, Down or Logs here.");
  return server.ssh
    ? tr("Status, Up, Down and Logs run your commands over hgs's SSH to %1, once each; nothing retries on its own.", server.ssh)
    : tr("Status, Up, Down and Logs run your commands on this computer, once each; nothing retries on its own.");
}

/** Which of your sessions use a model, by name. */
export function usedBy(names: readonly string[]): string {
  if (!names.length) return tr("none of your sessions");
  const shown = names.slice(0, 3);
  if (names.length > 3) shown.push(tr("%1 more", names.length - 3));
  return names.length === 1 ? tr("1 of your sessions uses it: %1", shown.join(", ")) : tr("%1 of your sessions use it: %2", names.length, shown.join(", "));
}

/** What a Down row says about a model once your sessions are read. */
export function downUse(model: string, state: string, by: Record<string, string[]> | undefined, why: string | undefined): string {
  if (why !== undefined) return tr("your sessions not read: %1", why);
  if (!by) return tr("reading your sessions…");
  const names = by[model] ?? [];
  // A model still loading stops loading; it serves no session yet.
  if (state === "loading" && !names.length) return tr("stops loading");
  return usedBy(names);
}

/** The session's project from its folder, as hgs names it. */
export function projectOf(folder: string): string {
  let base = folder.trim().replace(/\\/g, "/");
  while (base.length > 1 && base.endsWith("/")) base = base.slice(0, -1);
  base = base.split("/").pop() ?? "";
  base = base.replace(/[:.]/g, "_");
  return base || "root";
}

/** The tag New session suggests for a model. */
export function tagFor(model: string): string {
  return (model.split("/").pop() ?? "").toLowerCase().replace(/[^a-z0-9-]+/g, "-").slice(0, 40).replace(/^-+|-+$/g, "");
}

export interface AgentChoice { id: "codex" | "claude"; name: string; ok: boolean; how: string }
/** Codex needs an OpenAI-compatible API, Claude Code an Anthropic one. */
export function agentChoices(server: ModelServer, model?: ModelServerModel): AgentChoice[] {
  // model-catalog/2: the service's API names decide, one row for both agents.
  const apis = model?.catalog?.api_models;
  if (server.protocol === "model-catalog/2" && apis) {
    const name = (wire: string) => apis.find((api) => api.wire === wire)?.id;
    const openaiName = name("openai"), anthropicName = name("anthropic");
    return [
      { id: "codex", name: tr("Codex"), ok: !!openaiName, how: openaiName ? tr("OpenAI API name %1 for this session", openaiName) : tr("%1 has no OpenAI API name on %2", model!.name, server.name) },
      { id: "claude", name: tr("Claude Code"), ok: !!anthropicName, how: anthropicName ? tr("Anthropic API name %1 for this session", anthropicName) : tr("%1 has no Anthropic API name on %2", model!.name, server.name) },
    ];
  }
  const openai = server.kind === "openai";
  return [
    { id: "codex", name: tr("Codex"), ok: openai, how: openai ? tr("OpenAI-compatible provider for this session") : tr("needs an OpenAI-compatible API; %1 serves Anthropic", server.name) },
    { id: "claude", name: tr("Claude Code"), ok: !openai, how: !openai ? tr("Anthropic base URL for this session") : tr("needs an Anthropic-compatible API; %1 serves OpenAI", server.name) },
  ];
}

/** What a model-server session gets, as its dialog shows it. */
export function sessionSummary(agent: string, server: ModelServer, model: string, row?: ModelServerModel): string {
  const facts = row?.catalog;
  if (server.protocol === "model-catalog/2" && facts) {
    const wire = agent === "codex" ? "openai" : "anthropic";
    const api = facts.api_models.find((entry) => entry.wire === wire)?.id ?? model;
    const window = facts.context_manual || facts.context;
    return `agent     ${agent}, ${tr("on this computer")}\nprovider  ${server.name}\nservice   ${model}  ${facts.variant}\nmodel     ${api}\ncontext   ${window ? contextText(window) : tr("not reported")}${facts.context_manual ? `  ${tr("manual")}` : ""}\nkey       ${server.has_key ? tr("from the private store") : tr("none")}`;
  }
  return `agent     ${agent}, ${tr("on this computer")}\nprovider  ${server.name}  ${server.agent_url}\nmodel     ${model}\nkey       ${server.has_key ? tr("from the private store") : tr("none")}`;
}

/** A context window as the tab shows it: 262k, 1M. */
export function contextText(tokens: number): string {
  if (tokens >= 1_000_000) return `${+(tokens / 1_000_000).toFixed(2)}M`;
  return `${Math.round(tokens / 1024)}k`;
}

/** The catalog facts of a service as one line of the Model servers tab. */
export function catalogLine(facts: ModelServerCatalogModel): string {
  const parts: string[] = [];
  const window = facts.context_manual || facts.context;
  if (window) parts.push(facts.context_manual ? tr("%1 context, manual", contextText(window)) : tr("%1 context", contextText(window)));
  if (facts.memory_gib > 0 || facts.needs_gib > 0) parts.push(tr("%1 of %2 GiB", facts.memory_gib.toFixed(1), facts.needs_gib.toFixed(0)));
  if (facts.fit) parts.push(fitText(facts.fit));
  if (facts.tok_s > 0) parts.push(facts.code_tok_s > 0 ? tr("%1 tok/s, code %2", facts.tok_s.toFixed(0), facts.code_tok_s.toFixed(0)) : tr("%1 tok/s", facts.tok_s.toFixed(0)));
  return parts.join("   ");
}

export function fitText(fit: string): string {
  switch (fit) {
    case "fits": return tr("fits");
    case "does not fit": return tr("does not fit");
    case "unloads all others": return tr("unloads all others");
    case "blocked by exclusive": return tr("blocked by an exclusive model");
    default: return fit;
  }
}

/** One variant in the quality menu: its window, memory and speed. */
export function variantText(variant: ModelServerVariant): string {
  const parts = [variant.id, tr("%1 context", contextText(variant.context))];
  if (variant.needs_gib > 0) parts.push(tr("%1 GiB", variant.needs_gib.toFixed(0)));
  if (variant.tok_s > 0) parts.push(variant.code_tok_s > 0 ? tr("%1 tok/s, code %2", variant.tok_s.toFixed(0), variant.code_tok_s.toFixed(0)) : tr("%1 tok/s", variant.tok_s.toFixed(0)));
  return parts.join("   ");
}

export function canStart(agent: string, model: string, state: string | undefined, folder: string, tag: string): boolean {
  return !!agent && !!model && (!state || state === "running" || state === "listed") && !!folder.trim() && !!tag.trim();
}

/** #703 (#658 N2): the server's root, without a typed trailing /v1 or /. */
export function rootOf(baseUrl: string): string {
  let root = baseUrl.trim().replace(/\/+$/, "");
  if (root.endsWith("/v1")) root = root.slice(0, -3);
  return root.replace(/\/+$/, "");
}

/** Settings' hint under Server root. */
export function urlHint(typed: string): string {
  const text = typed.trim();
  if (!text) return tr("The address the server answers at, without /v1: http(s)://HOST[:PORT][/path]. Zerus reads its model list at ROOT/v1/models, gives Codex ROOT/v1 and Claude Code ROOT.");
  const root = rootOf(text);
  let hint = tr("Model list: %1\nCodex: %2\nClaude Code: %3", `${root}/v1/models`, `${root}/v1`, root);
  if (text.replace(/\/+$/, "").endsWith("/v1")) hint += "\n" + tr("Saved as its root %1: the /v1 you typed is added where it is needed.", root);
  return hint;
}

export type SshMode = "none" | "machine" | "alias";
/** The SSH segment a saved server opens on. */
export function sshMode(ssh: string, machines: readonly string[]): SshMode {
  return !ssh ? "none" : machines.includes(ssh) ? "machine" : "alias";
}

/** Settings' form for a server (or a new one). */
export function formOf(server: ModelServer | undefined): ModelServerForm {
  return {
    was: server?.name ?? "", name: server?.name ?? "", base_url: server?.base_url ?? "", kind: server?.kind === "anthropic" ? "anthropic" : "openai",
    ssh: server?.ssh ?? "", key_change: "", key: "", status: server?.status_text ?? "", up: server?.up_text ?? "", down: server?.down_text ?? "", logs: server?.logs_text ?? "",
  };
}

/** The key field: stored (Replace, Remove), removed on save, or typed. */
export function keyMode(hasKey: boolean, change: ModelServerForm["key_change"]): { stored: boolean; field: boolean; actions: boolean; text: string } {
  const stored = hasKey && change === "";
  return {
    stored: stored || change === "remove", field: !stored && change !== "remove", actions: stored,
    text: change === "remove" ? tr("removed when you save") : tr("in the private store"),
  };
}

/** The list row's note: not read, or read only. */
export function listNote(server: ModelServer, entry: StatusEntry | undefined): string {
  if (entry?.known && entry.status?.error) return tr("not read");
  return server.read_only ? tr("read only") : "";
}
