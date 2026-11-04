import { createComputed, createMemo, createRoot, createSignal, on, untrack, type Accessor } from "solid-js";
import { machineChange, machinePickKey, machineSsh } from "../bridge";
import { toast } from "./notices";
import type { MachineChangeRequest, MachineConnection, MachineReference, MachinesView } from "../types";
import { loadMachines, machinesPage } from "./machines";
import { fleetRefresh } from "../bridge";
import { tr } from "../i18n";

export const NEW_MACHINE = "@new";
export const LOCAL_MACHINE = "@local";
export interface MachineDraft extends Omit<MachineConnection, "port"> { alias: string; port: string; revision: string; dirty: boolean }
export type MachinePrompt = { kind: "discard"; next: string; refresh: boolean } | { kind: "remove"; alias: string; revision: string };
const blank = (): MachineDraft => ({ alias: "", hostname: "", user: "", port: "", identity_file: "", enabled: true, revision: "", dirty: false });
export interface MachineTools { pickKey: () => Promise<string | null>; ssh: (reference: MachineReference) => Promise<string | null> }
const aliasValid = (alias: string) => /^[A-Za-z0-9_.][A-Za-z0-9_.-]{0,79}$/.test(alias);

/** One page draft, one explicit write. Reads never replace unsaved fields. */
export function createMachineEditor(view: Accessor<MachinesView>, write: (request: MachineChangeRequest) => Promise<void>, reload: (force?: boolean) => Promise<unknown>, changed: () => Promise<unknown>, tools?: MachineTools) {
  const [selected, setSelected] = createSignal(new URLSearchParams(location.search).get("machine") ?? LOCAL_MACHINE);
  const [draft, setDraft] = createSignal<MachineDraft>(blank());
  const [writing, setWriting] = createSignal(false);
  const [tool, setTool] = createSignal<"key" | "ssh">();
  const [notice, setNotice] = createSignal<{ text: string; error: boolean }>();
  const [prompt, setPrompt] = createSignal<MachinePrompt>();
  const busy = () => writing() || !!tool() || view().busy;
  const isNew = () => selected() === NEW_MACHINE;
  const duplicate = () => isNew() && (draft().alias.trim() === view().local || view().machines.some((m) => m.alias === draft().alias.trim()));
  const valid = () => aliasValid(draft().alias.trim()) && /^\d{0,5}$/.test(draft().port) && Number(draft().port) <= 65535;
  const seed = (key: string) => {
    const row = view().machines.find((m) => m.alias === key);
    setDraft({ ...blank(), alias: key === NEW_MACHINE ? "" : key === LOCAL_MACHINE ? view().local : key,
      hostname: row?.hostname ?? "", user: row?.user ?? "", port: row?.port ? String(row.port) : "", identity_file: row?.identity_file ?? "", enabled: row?.enabled ?? true, revision: view().connection_revision ?? "" });
  };
  const apply = (next: string) => { setSelected(next); seed(next); setNotice(undefined); };
  const edit = <K extends keyof MachineDraft>(key: K, value: MachineDraft[K]) => {
    if (busy() || selected() === LOCAL_MACHINE) return;
    setDraft((old) => ({ ...old, [key]: value, dirty: true }));
  };
  const choose = (next: string, refresh = false) => {
    if (busy() || prompt() || (next === selected() && !refresh)) return;
    if (draft().dirty) { setPrompt({ kind: "discard", next, refresh }); return; }
    apply(next);
    if (refresh) void reload(true);
  };
  const cancel = (expected?: MachinePrompt) => { if (!expected || prompt() === expected) setPrompt(undefined); };
  const mutation = async (request: MachineChangeRequest) => {
    if (busy() || selected() === LOCAL_MACHINE) return false;
    setWriting(true); setNotice(undefined);
    try {
      await write(request);
      await reload();
      apply(request.change.kind === "remove" ? LOCAL_MACHINE : request.alias);
      setNotice({ error: false, text: request.change.kind === "remove"
        ? `${request.alias} was removed from this Zerus. Its agents keep running and its files stay unchanged.`
        : request.change.kind === "reset" ? `SSH overrides of ${request.alias} were reset to the shared configuration.` : tr("Connection saved for %1.", request.alias) });
      // Reconnect the existing supervisor so its local peer catalog picks up
      // the saved configuration; never restart agents or create a reader.
      void changed().catch(() => {});
      return true;
    } catch (error) { setNotice({ error: true, text: String(error) }); return false; }
    finally { setWriting(false); }
  };
  const save = () => {
    if (!valid() || duplicate() || !view().loaded || !draft().revision || !(draft().dirty || isNew())) return Promise.resolve(false);
    const d = untrack(draft);
    return mutation({ alias: d.alias.trim(), revision: d.revision, change: { kind: "set", create: isNew(), connection: {
      hostname: d.hostname.trim(), user: d.user.trim(), port: Number(d.port) || null, identity_file: d.identity_file.trim(), enabled: d.enabled,
    } } });
  };
  const reset = () => draft().dirty || isNew() ? Promise.resolve(false) : mutation({ alias: selected(), revision: draft().revision, change: { kind: "reset" } });
  const remove = () => { if (!busy() && !isNew() && !draft().dirty && selected() !== LOCAL_MACHINE) setPrompt({ kind: "remove", alias: selected(), revision: draft().revision }); };
  const confirm = () => {
    const p = prompt(); if (!p || busy()) return;
    setPrompt(undefined);
    if (p.kind === "discard") { apply(p.next); if (p.refresh) void reload(true); return; }
    if (selected() !== p.alias || view().connection_revision !== p.revision || draft().dirty) {
      setNotice({ text: tr("The connection changed. Review it before removing."), error: true }); return;
    }
    void mutation({ alias: p.alias, revision: p.revision, change: { kind: "remove" } });
  };
  const pickKey = async () => {
    if (!tools || busy() || selected() === LOCAL_MACHINE) return false;
    const original = untrack(draft), selection = selected();
    setTool("key");
    try {
      const path = await tools.pickKey();
      if (path === null) return false;
      if (selected() !== selection || draft() !== original || view().connection_revision !== original.revision) {
        setNotice({ text: tr("Machine settings changed elsewhere. Reload the page before saving."), error: true });return false;
      }
      setDraft({ ...original, identity_file: path, dirty: true });return true;
    } catch (error) { setNotice({ text: String(error), error: true });return false; }
    finally { setTool(undefined); }
  };
  const openSsh = async () => {
    if (!tools || busy() || draft().dirty || isNew() || selected() === LOCAL_MACHINE || !draft().revision) return false;
    const reference = { alias: selected(), revision: draft().revision };
    setTool("ssh");setNotice(undefined);
    try { await tools.ssh(reference);return true; }
    catch (error) { setNotice({ text: String(error), error: true });return false; }
    finally { setTool(undefined); }
  };
  const connectionVersion = createMemo(() => `${view().loaded}\n${view().connection_revision ?? ""}`);
  createComputed(on(connectionVersion, () => {
    if (!view().loaded || writing() || tool() || draft().dirty || prompt()) return;
    if (selected() !== LOCAL_MACHINE && !isNew() && !view().machines.some((m) => m.alias === selected())) apply(LOCAL_MACHINE);
    else seed(selected());
  }));
  return { selected, draft, notice, prompt, busy, isNew, valid, duplicate, edit, choose, save, reset, remove, confirm, cancel, pickKey, openSsh, clearNotice: () => setNotice(undefined) };
}

export const machineEditor = createRoot(() => createMachineEditor(machinesPage, machineChange, loadMachines, fleetRefresh, {
  pickKey: machinePickKey,
  ssh: async (reference) => {
    const advisory = await machineSsh(reference);
    toast("SSH terminal", advisory ?? tr("Opened a terminal: ssh %1", reference.alias), advisory ? "warn" : "info");
    return advisory;
  },
}));
