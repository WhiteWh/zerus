import { createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { tr } from "../../i18n";

export type Project = { id: string; name: string };
export type Sync = { state: "local" } | { state: "shared" } | { state: "waiting" } | { state: "synced"; at: number } | { state: "error"; text: string };
export interface Computer { id: string; name: string; access: string; sync: Sync; tasks_outdated: boolean }
export interface Preview { id: string; alias: string; initialized: boolean; same_swarm: boolean; projects: Project[] }
export interface SwarmView {
  id: string; catalog: { swarm_id: string; conflicts: number; computers: Computer[]; revision?: number } | null;
  projects: Project[]; aliases: string[]; preview: Preview | null;
  status: string; error: boolean; busy: boolean; changing: boolean;
  can_sync: boolean; can_join: boolean; can_preview: boolean; mode: string;
}
export type Call = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
/** One live native view, with coalesced events and no poller or operation replay. */
export function createSwarm(call: Call = invoke) {
  const [frame, setFrame] = createSignal<SwarmView>();
  const [working, setWorking] = createSignal(false);
  const [error, setError] = createSignal("");
  let gone = false, reading = false, dirty = false, epoch = 0;
  const adopt = (next: SwarmView) => {
    if (frame() && next.id !== frame()!.id) throw Error("This project swarm dialog expired. Open it again.");
    if (!gone) setFrame(next);
  };
  const pull = async () => {
    if (gone || !frame() || working() || reading || !dirty) return;
    reading = true; dirty = false;
    const generation = epoch;
    try {
      const next = await call<SwarmView>("project_swarm_view", { id: frame()!.id });
      if (!gone && generation === epoch) adopt(next);
    } catch (e) { if (!gone && generation === epoch) setError(String(e)); }
    finally { reading = false; if (dirty) void pull(); }
  };
  const changed = () => { if (!gone) { dirty = true; void pull(); } };
  const open = async () => {
    if (gone || working() || frame()) return;
    setWorking(true);
    try {
      const next = await call<SwarmView>("project_swarm_open");
      if (gone) { await call("project_swarm_close", { id: next.id }); return; }
      adopt(next); dirty = true;
    } catch (e) { if (!gone) setError(String(e)); }
    finally { setWorking(false); if (dirty) void pull(); }
  };
  const action = async (kind: "preview" | "join" | "sync" | "resolve", args: Record<string, unknown>) => {
    const current = frame();
    if (gone || working() || !current || current.busy || !(kind === "resolve" ? current.can_sync : current[`can_${kind}`])) return false;
    ++epoch; setWorking(true); setError("");
    try { adopt(await call<SwarmView>(`project_swarm_${kind}`, { id: current.id, ...args })); return true; }
    catch (e) { if (!gone) setError(String(e)); return false; }
    finally { setWorking(false); dirty = true; void pull(); }
  };
  const close = async () => {
    if (frame()?.changing || working()) return false;
    try { if (frame()) await call("project_swarm_close", { id: frame()!.id }); return true; }
    catch (e) { if (!gone) setError(String(e)); return false; }
  };
  const dispose = () => {
    if (gone) return;
    gone = true; ++epoch; dirty = false;
    if (frame()) void call("project_swarm_close", { id: frame()!.id }).catch(() => {});
  };
  return { frame, working, error, open, changed, close, dispose,
    preview: (alias: string) => action("preview", { alias }),
    join: (mappings: [string, string][], keep: boolean) => action("join", { mappings, keep }),
    sync: () => action("sync", {}),
    resolve: (review: string, confirmed: boolean) => action("resolve", { review, confirmed }),
  };
}
export const syncText = (computer: Computer) => {
  const state = computer.sync;
  const text = state.state === "local" ? tr("Local catalog") : state.state === "shared" ? tr("Shared through swarm") : state.state === "waiting" ? tr("Waiting to sync") : state.state === "error" ? state.text : tr("Synced %1", new Date(state.at * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false }));
  return text + (computer.tasks_outdated && state.state !== "error" ? ". Update Zerus there to share tasks" : "");
};

/** Qt preselects identical keys: the CLI merges them even without a mapping. */
export const seedMappings = (local: Project[], peer: Project[]) => {
  const ids = new Set(peer.map((p) => p.id));
  return new Map(local.filter((p) => ids.has(p.id)).map((p) => [p.id, p.id]));
};
export const initialAlias = (aliases: string[], preferred?: string) => aliases.find((a) => a === preferred) ?? aliases[0] ?? "";
