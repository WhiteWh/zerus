import { createSignal } from "solid-js";
import { tr } from "../i18n";
import type {
  DraftMember,
  NewTeamView,
  TaskView,
  TeamCall,
  TeamOutcome,
  TeamSettings,
} from "../ui/teams/types";
import { membersWithIds } from "./newTeamForm";

export const DRAFT_FIELDS = ["name", "goal", "addendum", "settings", "members", "tasks", "working_mode", "plan_mode"] as const;
export type DraftField = (typeof DRAFT_FIELDS)[number];

export interface NewTeamFields {
  name: string;
  goal: string;
  addendum: string;
  folder: string;
  settings: TeamSettings;
  members: DraftMember[];
  tasks: TaskView[];
  /** Task #308: auto, a preset, or none for the settings as they are. */
  working_mode: string;
  /** Task #8 (team ede724c4): standard or free; the owner chooses it, the lead offers one. */
  plan_mode: PlanMode;
}

export type PlanMode = "standard" | "free";
export const planModeOf = (value: unknown): PlanMode => (value === "free" ? "free" : "standard");

export const emptyFields = (): NewTeamFields => ({
  name: "",
  goal: "",
  addendum: "",
  folder: "",
  settings: {},
  members: [],
  tasks: [],
  working_mode: "auto",
  plan_mode: "standard",
});

export interface NewTeamPort {
  action(call: TeamCall): Promise<TeamOutcome>;
}

/**
 * WhiteWh fork: a form opened for another computer sends every call there.
 * Its draft lives on that computer; a save or Create team sent here finds
 * no such team ("Unknown team").
 */
export function portOn(computer: () => string, send: (call: TeamCall) => Promise<TeamOutcome>): NewTeamPort {
  return { action: (call) => send({ ...call, computer: call.computer || computer() }) };
}

export interface ComputerChoice {
  alias: string;
  label: string;
  online: boolean;
}

export type RowError = { code: "role" } | { code: "computer-offline"; alias: string };

/**
 * A member row's live error: no role, or a computer that is not online.
 * Without `computers` (a form for another computer, whose aliases this one
 * does not know) only the role is checked.
 */
export function memberRowErrors(member: DraftMember, computers?: ComputerChoice[]): RowError[] {
  const errors: RowError[] = [];
  if (!member.role.trim()) errors.push({ code: "role" });
  if (member.computer && computers) {
    const computer = computers.find((candidate) => candidate.alias === member.computer);
    if (!computer || !computer.online) errors.push({ code: "computer-offline", alias: member.computer });
  }
  return errors;
}

/** Only the settings that differ, as `hgs team settings` merges them. */
export function settingsChanges(before: TeamSettings, after: TeamSettings): TeamSettings {
  const changes: TeamSettings = {};
  for (const key of new Set([...Object.keys(before), ...Object.keys(after)])) {
    if (JSON.stringify(before[key]) !== JSON.stringify(after[key])) {
      changes[key] = after[key] ?? null;
    }
  }
  return changes;
}

/**
 * A polled draft merged into the open form: every field the owner has not
 * edited follows the lead; a field the owner edited keeps the owner's value.
 * The lead's version still arrives in `view.draft.unseen`, so the form can
 * mark it, but it never overwrites unsaved typing.
 */
export function mergeDraft(
  local: NewTeamFields,
  dirty: ReadonlySet<DraftField>,
  view: NewTeamView,
): NewTeamFields {
  const merged: NewTeamFields = { ...local };
  for (const field of DRAFT_FIELDS) {
    if (field === "working_mode" || field === "plan_mode") continue;
    if (!dirty.has(field)) {
      (merged as unknown as Record<string, unknown>)[field] = view[field as keyof NewTeamView];
    }
  }
  if (!dirty.has("working_mode")) merged.working_mode = view.working_mode?.accepted ?? "auto";
  if (!dirty.has("plan_mode")) merged.plan_mode = planModeOf(view.plan_mode);
  merged.folder = view.folder || local.folder;
  return merged;
}

export interface StartPlanningInput {
  computer?: string;
  folder: string;
  name?: string;
  lead: DraftMember;
  project?: string;
  project_id?: string;
  template?: string;
}

export type NewTeamResult = { ok: true; outcome: TeamOutcome } | { ok: false; error: string };

export function createNewTeam(port: NewTeamPort) {
  const [open, setOpen] = createSignal(false);
  const [view, setView] = createSignal<NewTeamView | null>(null);
  const [fields, setFields] = createSignal<NewTeamFields>(emptyFields());
  const [dirty, setDirty] = createSignal<Set<DraftField>>(new Set());
  const [unseen, setUnseen] = createSignal<string[]>([]);
  const [draftId, setDraftId] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const [created, setCreated] = createSignal("");
  // Task 42: the draft revision the next save names, a save on its way, and
  // whether the owner wrote the protocol addendum (a template no longer
  // replaces it).
  const [revision, setRevision] = createSignal(0);
  const [sending, setSending] = createSignal(false);
  const [addendumEdited, setAddendumEdited] = createSignal(false);

  const openNew = () => {
    // Opening the tab creates nothing: no draft, no session, no call.
    setOpen(true);
    setView(null);
    setFields(emptyFields());
    setDirty(new Set<DraftField>());
    setUnseen([]);
    setDraftId("");
    setCreated("");
    setError("");
    setRevision(0);
    setAddendumEdited(false);
  };
  const close = () => setOpen(false);
  /**
   * Task 200 (Qt TeamNewTab::openDraft, F5): the form shows a draft again,
   * from its lead's session or the drafts list. Nothing is sent: the draft's
   * view (`new:<id>`) fills the fields as the lead last saved them, and the
   * owner's edits save against its revision from then on.
   */
  const openDraft = (team: string) => {
    if (!team || team === draftId()) return;
    openNew();
    setDraftId(team);
  };

  const edit = (field: DraftField, value: NewTeamFields[DraftField]) => {
    setFields({ ...fields(), [field]: value } as NewTeamFields);
    setDirty(new Set<DraftField>([...dirty(), field]));
    if (field === "addendum") setAddendumEdited(true);
  };
  /** Several fields at once (a template pick): the members, settings, addendum and tasks it sets. */
  const editMany = (patch: Partial<Pick<NewTeamFields, DraftField>>) => {
    setFields({ ...fields(), ...patch } as NewTeamFields);
    setDirty(new Set<DraftField>([...dirty(), ...(Object.keys(patch) as DraftField[])]));
  };
  const setFolder = (value: string) => setFields({ ...fields(), folder: value });

  const applyView = (next: NewTeamView) => {
    // Revisions only grow: a read that left before the last save is old.
    if (next.draft.revision < revision()) return;
    setFields(mergeDraft(fields(), dirty(), next));
    if (!dirty().has("addendum") && next.addendum) setAddendumEdited(true);
    setView(next);
    setUnseen(next.draft.unseen);
    setDraftId(next.team);
    setRevision(next.draft.revision);
  };

  /** A field as the draft takes it: texts trimmed, members with their ids. */
  const valueOf = (field: DraftField): unknown => {
    const value = fields()[field];
    if (field === "members") return membersWithIds(value as DraftMember[]);
    return typeof value === "string" ? value.trim() : value;
  };

  /**
   * Saves the owner's edited fields to the draft (Qt flush): against the
   * revision read; a refusal naming a newer revision is sent again once on
   * top of it. A field changed again while the save went out stays to save.
   */
  const flush = async (retried = false): Promise<NewTeamResult> => {
    if (!draftId() || dirty().size === 0 || sending()) return { ok: true, outcome: { outcome: "done", receipt: {}, request_id: "" } };
    const sent = new Map<DraftField, string>();
    const set: Record<string, unknown> = {};
    for (const field of dirty()) { set[field] = valueOf(field); sent.set(field, JSON.stringify(fields()[field])); }
    setSending(true);
    try {
      const outcome = await port.action({ team: draftId(), team_name: fields().name, confirmed: false,
        action: { kind: "draft_update", revision: revision(), set } });
      if (outcome.outcome === "done") {
        const next = Number(outcome.receipt.draft_revision ?? revision() + 1);
        setRevision(Number.isFinite(next) ? next : revision() + 1);
        setDirty(new Set<DraftField>([...dirty()].filter((field) => sent.get(field) !== JSON.stringify(fields()[field]))));
        if (Array.isArray(outcome.receipt.unseen)) setUnseen(outcome.receipt.unseen.map(String));
        setError("");
        return { ok: true, outcome };
      }
      const detail = outcome.outcome === "confirm" ? outcome.confirmation.text : outcome.detail;
      const stale = /Draft revision is (\d+) now/.exec(detail);
      if (stale && !retried) {
        setRevision(Number(stale[1]));
        setSending(false);
        return await flush(true);
      }
      setError(detail);
      return { ok: false, error: "update" };
    } catch (cause) {
      setError(String(cause));
      return { ok: false, error: "update" };
    } finally {
      setSending(false);
    }
  };

  const startPlanning = async (input: StartPlanningInput): Promise<NewTeamResult> => {
    if (!input.folder.trim()) return { ok: false, error: "folder" };
    if ((input.name ?? "").trim().length > 80) return { ok: false, error: "name" };
    setBusy(true);
    setError("");
    try {
      const outcome = await port.action({
        computer: input.computer ?? "",
        team: "",
        team_name: input.name ?? "",
        confirmed: false,
        action: {
          kind: "draft_start",
          folder: input.folder,
          name: input.name,
          lead: input.lead,
          project: input.project,
          project_id: input.project_id,
          template: input.template,
        },
      });
      if (outcome.outcome !== "done") {
        setError(outcome.outcome === "confirm" ? outcome.confirmation.text : outcome.detail);
        return { ok: false, error: "start" };
      }
      const team = String(outcome.receipt.team ?? "");
      setDraftId(team);
      setRevision(Number(outcome.receipt.revision ?? 1) || 1);
      setFields({ ...fields(), folder: input.folder, name: input.name ?? fields().name });
      // What the form holds so far is the draft's first content (Qt): the
      // folder, name and lead went with the start; the rest is saved next,
      // and members only become sessions with Create team.
      const first = new Set<DraftField>(["settings"]);
      const now = fields();
      if (now.goal.trim()) first.add("goal");
      if (now.addendum.trim()) first.add("addendum");
      if (now.members.length) first.add("members");
      if (now.tasks.length) first.add("tasks");
      setDirty(new Set<DraftField>([...dirty(), ...first]));
      return { ok: true, outcome };
    } catch (cause) {
      setError(String(cause));
      return { ok: false, error: "start" };
    } finally {
      setBusy(false);
    }
  };

  const createTeam = async (options: { confirmWidening?: boolean } = {}): Promise<NewTeamResult> => {
    const current = view();
    if (!current || !draftId()) return { ok: false, error: "no-draft" };
    setBusy(true);
    setError("");
    try {
      // Unsaved edits go out first; the create follows the save.
      if (dirty().size > 0) {
        setBusy(false);
        const saved = await flush();
        setBusy(true);
        if (!saved.ok) {
          setError(tr("the draft did not save first: %1", error()));
          return { ok: false, error: "update" };
        }
      }
      void current;
      const outcome = await port.action({
        team: draftId(),
        team_name: fields().name,
        confirmed: false,
        // Owner, 2026-11-09: a draft with wider member permissions needs the
        // owner's explicit confirmation; the form asks for it (NewTeamFooter).
        action: { kind: "draft_create", approve: false, confirm_widening: !!options.confirmWidening },
      });
      if (outcome.outcome !== "done") {
        setError(outcome.outcome === "confirm" ? outcome.confirmation.text : outcome.detail);
        return { ok: false, error: "create" };
      }
      setCreated(draftId());
      return { ok: true, outcome };
    } catch (cause) {
      setError(String(cause));
      return { ok: false, error: "create" };
    } finally {
      setBusy(false);
    }
  };

  const saveSettings = async (patch: TeamSettings): Promise<NewTeamResult> => {
    const current = view();
    if (!current) return { ok: false, error: "no-team" };
    const changes = settingsChanges((current.settings as TeamSettings) ?? {}, patch);
    if (Object.keys(changes).length === 0) return { ok: true, outcome: { outcome: "done", receipt: {}, request_id: "" } };
    setBusy(true);
    setError("");
    try {
      const outcome = await port.action({
        team: current.team,
        team_name: current.name,
        confirmed: false,
        action: { kind: "settings", settings: changes },
      });
      if (outcome.outcome !== "done") {
        setError(outcome.outcome === "confirm" ? outcome.confirmation.text : outcome.detail);
        return { ok: false, error: "settings" };
      }
      return { ok: true, outcome };
    } catch (cause) {
      setError(String(cause));
      return { ok: false, error: "settings" };
    } finally {
      setBusy(false);
    }
  };

  /**
   * Discard draft (Qt discardDraft): nothing of it is created; with stopLead
   * the lead's session stops too. The owner confirmed it in the form's dialog.
   */
  const discard = async (stopLead: boolean): Promise<NewTeamResult> => {
    if (!draftId()) return { ok: false, error: "no-draft" };
    setBusy(true);
    setError("");
    try {
      const outcome = await port.action({ team: draftId(), team_name: fields().name, confirmed: true,
        action: { kind: "draft_discard", stop_lead: stopLead } });
      if (outcome.outcome !== "done") {
        setError(outcome.outcome === "confirm" ? outcome.confirmation.text : outcome.detail);
        return { ok: false, error: "discard" };
      }
      const keepOpen = open();
      openNew();
      setOpen(keepOpen);
      return { ok: true, outcome };
    } catch (cause) {
      setError(String(cause));
      return { ok: false, error: "discard" };
    } finally {
      setBusy(false);
    }
  };

  /** Show the lead's version (Qt useLeadVersion): the owner's unsaved fields go; the draft as read shows. */
  const useLeadVersion = () => {
    setDirty(new Set<DraftField>());
    const current = view();
    if (current) setFields(mergeDraft(fields(), new Set<DraftField>(), current));
  };

  /** Marks a field the lead changed as seen; several at once for one mark (the plan mode and its reason). */
  const markSeen = async (field: string | string[]) => {
    const seen = (Array.isArray(field) ? field : [field]).filter((value) => unseen().includes(value));
    if (!draftId() || seen.length === 0) return;
    setUnseen(unseen().filter((value) => !seen.includes(value)));
    await port.action({
      team: draftId(),
      team_name: fields().name,
      confirmed: false,
      action: { kind: "draft_seen", fields: seen },
    });
  };

  return {
    open,
    openNew,
    openDraft,
    close,
    view,
    fields,
    dirty,
    unseen,
    draftId,
    busy,
    error,
    created,
    edit,
    editMany,
    setFolder,
    applyView,
    flush,
    discard,
    useLeadVersion,
    revision,
    sending,
    addendumEdited,
    startPlanning,
    createTeam,
    saveSettings,
    markSeen,
  };
}

export type NewTeamStore = ReturnType<typeof createNewTeam>;
