import type { Text } from "./text";
// The batch the Rust side returns from `next_batch` (zerus-client-core
// `Batch`, snake_case JSON). Rows arrive as compact view rows projected in
// Rust; the webview never sees raw watch frames.

/** One computer's id: "" is this computer, otherwise its `hgs @alias`. */
export type ComputerId = string;

/**
 * A session row as the core projects it (client/core `view.rs`). `status` is a
 * presentation kind (never `offline`: the view applies that while a computer is
 * not live), `caption` the Qt status text without clock parts. Unknown values
 * are null; times are seconds since the Unix epoch.
 */
/**
 * A session's Keep cache warm state as its computer reports it. The listing
 * carries the first seven fields; the Details read and a confirmed change add
 * the rest (docs/reference.md, Keep cache warm).
 */
export interface KeepWarmState {
  enabled: boolean;
  /** Seconds since the epoch on the agent's computer; null is Always. */
  until: number | null;
  revision: number;
  /** off, waiting, warming, cold, uncertain, stopped, paused or unsupported. */
  state: string;
  next_at: number | null;
  reason: string;
  /** A keep-warm ping is running now. */
  turn_active: boolean;
  supported?: boolean;
  reason_code?: string;
  ttl_seconds?: number | null;
  ttl_source?: string;
  interval_seconds?: number | null;
  cache_expires_at?: number | null;
}

export interface ViewRow {
  conversation_state?: string;
  /** This device's retained pre-cutover queue, not host acknowledgement. */
  local_read_reply?: boolean;
  local_read_attention?: boolean;
  name: string;
  launch_id?: string | null;
  /**
   * The listing's durable session ID, when canonical. The Rust dispatcher
   * reads it from its own copy of the row for explicit actions; the webview
   * never passes it for one. It passes it only in a composer's SessionRef,
   * where it keys the drafts.
   */
  session_id?: string;
  title?: string;
  agent?: string;
  status?: string;
  caption?: string;
  phase?: string | null;
  /** `approval` or `question` while the session asks for one. */
  attention?: string | boolean | null;
  unread?: boolean;
  muted?: boolean;
  /** The row's notification preferences pass Qt's validation, and their revision. */
  shared_notifications?: boolean;
  notification_revision?: number | null;
  /** The host's Keep cache warm state (`hgs keep-warm`), when it reports one. */
  keep_warm?: KeepWarmState | null;
  /** The host's per-child question route switch (zerus-sa9qn.32). */
  question_routing?: { send_to_parent: boolean; revision: number } | null;
  /** `created`, in whole seconds (it keys a conversation-less attention mark). */
  created?: number;
  /** Exact/fallback Qt row-density hashes with this computer's configured identity. */
  compact_keys?: [string, string];
  /** What a local attention mark compares (Qt's `attentionFingerprint`). */
  attention_fingerprint?: string;
  /** The session needs the person's action (approval, input, error, recovery). */
  needs_action?: boolean;
  /** Qt's SessionInfo::needsAttention from the listing alone, before this device's marks (no children). */
  needs_attention?: boolean;
  /** A subagent asks while the session runs, before an acknowledgement (Qt's window adds it). */
  child_attention?: boolean;
  /** The host's preferences conflict with the confirmed ones (Qt's notificationPreferencesError). */
  notification_error?: string;
  /** `status` and `caption` with the attention acknowledged. */
  acknowledged_status?: string;
  acknowledged_caption?: string;
  /** A finished reply of a live conversation; the marks decide whether it shows unread. */
  unread_candidate?: boolean;
  machine?: string;
  project?: string;
  /** The folder people know the checkout by (a name, not a path). */
  folder?: string;
  branch?: string | null;
  model?: string | null;
  effort?: string | null;
  summary?: string;
  updated_at?: number | null;
  /** When an archived row was archived (epoch seconds). */
  archived_at?: number | null;
  /** When the current turn, compaction or background job started (the Working clock). */
  turn_started?: number | null;
  run_id?: string | null;
  conversation_id?: string | null;
  /** The raw `state` (`running`, `paused`, `stopped`, `archived`, ...). */
  state?: string;
  tracked?: boolean;
  resumable?: boolean;
  activity?: string;
  process_state?: string;
  /** `process_work.count`, -1 when unknown. */
  background?: number;
  /** Listed optional-question count; null while unknown, retained by Rust for the same conversation. */
  optional_question_count?: number | null;
  archive_id?: string | null;
  /** Native children by exact agent ID; never the hook-profile fallback. */
  subagents?: Record<string, Record<string, unknown>>;
  subagent_counts_complete?: boolean;
  /** The latest reply and the attention it raised, for marking them read. */
  reply_id?: string | null;
  attention_signature?: string | null;
  shared_read?: boolean;
  read_reply_id?: string | null;
  read_attention_signature?: string | null;
  /** The operations the computer reports as allowed; null when it reports none. */
  allowed_actions?: string[] | null;
  action_reasons?: Record<string, string>;
  /** The raw working directory (`folder` is the name people know the checkout by). */
  cwd?: string;
  /** Recorded listing binding; never inferred from a default account. */
  account_provider?: string;
  account_id?: string;
  account_home?: string;
  account_binding_reported?: boolean;
  /** The canonical working directory (New session's prefill); absent when the row has none. */
  cwd_canonical?: string;
  /** The checkout's root, "" outside Git (the worktree picker's session counts). */
  git_root?: string;
  git_metadata_state?: string;
  git_detached?: boolean;
  /** Verified linked checkout name; absent for main or unknown checkouts. */
  worktree?: string | null;
  /** The session that launched or adopted this one (core `ParentLink`). */
  parent?: ParentLinkOut | null;
  git_worktree?: boolean;
  /** A supervised child: its requests draw no attention (core `quiet`). */
  quiet?: boolean;
  terminal_supported?: boolean;
  terminal_reported?: boolean;
  terminal_attach_supported?: boolean;
  terminal_attach_reported?: boolean;
  bulk_lifecycle_supported?: boolean;
}

/** A child's parent as its computer lists it; `state` is live, saved, archived or missing. */
export interface ParentLinkOut { machine: string; session_id: string; name: string; conversation_id: string; run_id: string; state: string;
  /** When the link was recorded, Unix seconds; 0 when unknown. */
  at?: number }
export interface RowOut { computer: ComputerId; key: string; row: ViewRow }
export interface KeyOut { computer: ComputerId; key: string; reason?: string | null }
export interface ReplaceOut { computer: ComputerId; rows: RowOut[] }
export type ComputerStatus = "probing" | "unsupported" | "live" | "polled" | "reconnecting" | "ended";
export interface ComputerOut { computer: ComputerId; status: ComputerStatus | string; attempt: number }
/**
 * A connection supervisor's restart schedule (core `RetryOut`): `waiting`
 * carries the delay and the time left, measured from the supervisor's own
 * monotonic deadline when the report was built; `serial` names the wait.
 */
export interface RetryReport { phase: "idle" | "waiting" | "attempting" | string; serial?: number; attempt?: number; delay_ms?: number | null; remaining_ms?: number | null }
export interface RetryOut extends RetryReport { computer: ComputerId }
/** One service connection row; `incarnation` and `retry` are absent from an older service. */
export interface FleetComputer { id: string; status: string; attempt: number; incarnation?: number | null; retry?: RetryReport | null }
/** The restart delays the background service uses: `default`, `file`, or `kept` when its saved file is not used. */
export interface FleetRetryDelays { delays_seconds: [number, number, number, number]; source: string; diagnostic?: string | null }
/** This computer's saved reconnect timings (`hgs machine retry-delays`); `invalid` keeps a file that is not used. */
export interface RetryDelays { delays_seconds: [number, number, number, number]; defaults: [number, number, number, number]; source: "default" | "file" | "invalid" | string; diagnostic: string | null }
export type ActivityKind = "events" | "replace" | "event" | "state" | "ended";
export interface ActivityOut { computer: ComputerId; key: string; kind: ActivityKind | string; payload: unknown }
/** The existing watch tasks topic; a list replaces the open project's board. */
export interface TasksOut { computer: ComputerId; key: string; kind: string; payload: unknown }

// The Sessions list's grouping by the shared project organization
// (client/core `swarm/grouping.rs`). The Rust side keeps the organization;
// the webview gets each row's project and rank, and the ordered headers.

/** One row's place: its project and its rank inside it. */
export interface GroupOut { computer: ComputerId; key: string; group: string; rank: number; archive_request?: number }
export interface HeaderOut {
  id: string;
  name: string;
  /** The project's colour; null uses the palette slot. */
  color: string | null;
  accessible: boolean;
  collapsed: boolean;
  is_default: boolean;
  palette: number;
}
/** The ordered headers and whether edits can run, sent when they change. */
export interface GroupingOut {
  /** A catalog is presented; before it, the list shows no headers. */
  loaded: boolean;
  default_project: string;
  headers: HeaderOut[];
  ready: boolean;
  /** Why edits cannot run, for the disabled entries. */
  reason: string | null;
  busy: boolean;
  status: string;
  status_error: boolean;
  conflicts: number;
  /** A one-time warning, such as saved project changes that could not be applied; absent when there is none. */
  notice?: string;
  /** The saved layout is retained and layout changes cannot persist this startup. */
  layout_save_paused?: boolean;
}
/** How a project edit ended (client/core `swarm::sync::EditOutcome`), plus the reserved ID for Create. */
export type EditOutcome = (
  | { outcome: "done"; conflicts: number }
  | { outcome: "kept"; detail: string }
  | { outcome: "refused"; detail: string }) & { project?: string; folder?: string };

/** Native ownership presentation only; this can never authorize a writer. */
export interface ModeReview {
  request_id: string;
  configuration: string;
  selection: string;
  disposition: "required" | "reboot_required" | "reviewed_unknown";
  passive_available: boolean;
}
/** A first explicit background choice offer (Windows). Routing only, never consent. */
export interface FirstChoiceOffer {
  configuration: string;
  selection: string;
}
export interface NotificationOwnership {
  /** Local startup recovery presentation; never a service grant. */
  native_recovery?: boolean;
  /** Native helper observation only; never permission to start a writer. */
  activation?: { pending: boolean; running: boolean; needs_resume: boolean; code: string;
    review?: ModeReview | null; automatic_blocked?: boolean; request_id?: string | null; active?: boolean; business_ready?: boolean;
    first?: FirstChoiceOffer | null };
  status: null | { abi: string; configuration: string; journal: string | null; revision: number;
    state: "passive" | "preparing" | "service" | "draining" | "revoked" | "native" | "identity_changed" | "unavailable";
    rearm_required: boolean; request_id: string | null; owner_epoch: string | null; identity_current: boolean };
  request_id: string | null;
  pending: boolean;
  running: boolean;
  code: string;
}
/** Native refresh selection and result display; never consent or authority. */
export interface ServiceRefreshView {
  configuration?: string | null;
  offer?: null | { configuration: string; epoch: string; selection: string };
  request_id?: string | null;
  running: boolean;
  settled: boolean;
  code: string;
}
/** Desktop service presentation has separate provenance from device-local marks. */
export interface DesktopService {
  /** Bounded display reason from the checked original service scope; no authority. */
  executable_upgrade?: null | { state: "draining" | "manual_refresh_required" };
  /** Native intent/current-scope presentation, never a permission. */
  notification_preferences?: null | {
    status: null | { abi: string; scope: { configuration: string; service_epoch: string };
      journal: string | null; revision: number; configured: boolean; preferences: { optional_questions: boolean } };
    pending: boolean;
    request_id: string | null;
    running: boolean;
    available: boolean;
    code: string;
  };
  connected: boolean;
  reconnecting?: boolean;
  epoch: string | null;
  generation: number | null;
  background: Record<string, { ready: boolean; code: string }>;
  /** Configured connection presentation only, never source or action authority. */
  fleet_computers?: null | FleetComputer[];
  /** The restart delays the service's supervisors use; absent from an older service. */
  fleet_retry_delays?: null | FleetRetryDelays;
  attention_counts: null | {
    epoch: string; scope: "shared_host_state"; display_only: true; authority: false; local_marks_included: false;
    aggregate: { complete: boolean; total: number | null; attention: number | null; known_total: number; known_attention: number };
    computers: { computer: string; complete: boolean; online: boolean; total: number | null; attention: number | null }[];
  };
  automatic_approvals: boolean | null;
  /** The person's global question route switch; null where nothing routes (zerus-sa9qn.32). */
  question_routes?: boolean | null;
  /** The approval categories the service's committed policy keeps on; absent from an older service. */
  automatic_approval_categories?: string[] | null;
  /** Held task message copies a person may review; display only, never a release. */
  task_message_inputs?: null | { review: number; full: boolean };
  details_available: boolean;
  /** Native frozen-read transfer; completion is durable queueing, not host ACK. */
  pending_read_import?: null | { required: boolean; complete: boolean; running: boolean; code: string; through: number; superseded: number; delivered: false };
  /** Best-effort Staged report only; its outcome never grants or gates service work. */
  retained_source_import?: null | { required: boolean; complete: boolean; running: boolean; code: string; phase: string | null; creations_capture?: { code: string; paused: boolean; capture_id: string | null }; local_read_capture?: { code: string; paused: boolean; capture_id: string | null; view_ready?: boolean; view_revision?: string }; drafts_capture?: { code: string; writes: boolean; paused: boolean; capture_id: string | null; manifests: number }; downloads_capture?: null | { code: string; capture_id: string | null }; downloads_report?: { required: boolean; complete: boolean; running: boolean; code: string; phase: string | null }; drafts_report?: { required: boolean; complete: boolean; partial?: boolean; running: boolean; code: string; phase: string | null }; worktree_cache_capture?: { code: string; paused: boolean; capture_id: string | null }; worktree_cache_report?: { required: boolean; complete: boolean; running: boolean; code: string; phase: string | null }; local_reads_report?: { required: boolean; complete: boolean; running: boolean; code: string; phase: string | null }; creations_report?: { required: boolean; complete: boolean; running: boolean; code: string; phase: string | null } };
  notification_ownership?: NotificationOwnership;
  code: string;
}

export interface Batch {
  git?: GitOut[];
  desktop_service?: DesktopService;
  upserts: RowOut[];
  removes: KeyOut[];
  replaces: ReplaceOut[];
  computers: ComputerOut[];
  /** Restart schedules of this app's own connection supervisors, after `computers`. */
  retries?: RetryOut[];
  machines?: MachineOut[];
  activity: ActivityOut[];
  tasks?: TasksOut[];
  /** Each row's project and rank, for the rows whose grouping changed. */
  groups?: GroupOut[];
  /** The headers and the edit state, when they changed. */
  grouping?: GroupingOut;
  /** The open composer's view, when it changed (src-tauri `next_batch` envelope). */
  composer?: ComposerView;
  /** The draft keys (`computer + "\n" + name`, plus `"\n" + archive id`) of the nonempty drafts, when they changed. */
  draft_keys?: string[];
  /** The Processes tab's view (src-tauri processes.rs), when it changed. */
  processes_view?: ProcessesView;
  /** Notices and Terminal requests that arrived after their call, at most 16. */
  composer_events?: ComposerEvent[];
}

export interface GitOut {
  computer: ComputerId;
  key: string;
  kind: "state" | "unavailable";
  payload: { value?: Record<string, unknown>; rev?: number; code?: string };
}

/** A row of the Processes tab (processes.rs `RowView`). */
export interface ProcessRow {
  id: string;
  /** The command as the row shows it; `command_full` is its tooltip and what Copy command copies. */
  command: string;
  command_full: string;
  owner: string;
  status: string;
  tooltip: string;
  tone: "danger" | "muted" | "warning" | "success";
  duration: string;
}

/** The Processes tab (processes.rs `ProcessesView`), Qt's ProcessView. */
export interface ProcessesView {
  /** Qt's view key: another one resets the tab's scroll. Empty without a session. */
  key: string;
  summary: string;
  summary_tooltip: string;
  notes: string;
  groups: { group: "live" | "recorded" | "finished"; title: string; rows: ProcessRow[] }[];
  selected: string[];
  current: string | null;
  stop: { label: string; enabled: boolean; tooltip: string };
  stop_notice: string;
  busy: boolean;
  failed: string | null;
  /** The current process; `output` is null until a read answered for this selection. */
  detail: { id: string; command: string; details: string; output: string | null; notice: string } | null;
  /** The tab's count; 0 for a recorded session. */
  count: number;
}

/** The session the Processes tab follows (processes.rs `ProcessTarget`). */
export interface ProcessTarget {
  computer: string;
  key: string;
  name: string;
  run_id: string;
  conversation_id: string;
  archive_id: string | null;
  machine: string;
  online: boolean;
  /** Qt's `!recorded`: online, not archived or paused, a running process and no inspect error. */
  live: boolean;
  /** The session's `host_generation` from its Activity details. */
  generation: string | null;
}

export const emptyBatch = (): Batch => ({ upserts: [], removes: [], replaces: [], computers: [], machines: [], activity: [] });

/** A batch with nothing to apply. A composer view or a draft key set is content: Rust sends each once. */
export const isEmptyBatch = (b: Batch) =>
  !b.git?.length && !b.upserts.length && !b.removes.length && !b.replaces.length && !b.computers.length && !b.machines?.length && !b.activity.length
  && !b.desktop_service && !b.tasks?.length && !b.groups?.length && !b.grouping && !b.processes_view && !b.composer && !b.draft_keys && !b.composer_events?.length;

/** One Activity event as `inspect` returns it; unknown fields are kept. */
export interface ActivityEvent {
  seq?: number;
  type?: string;
  source?: string;
  detail?: string;
  at?: number;
  tool?: string;
  agent_id?: string;
  origin?: string;
  /** `keep_warm` for every event of a keep-warm ping turn. */
  turn_origin?: string;
  report?: string;
  sender?: string;
  activity_key?: string;
  message_id?: string;
  /** OpenCode's inbox: a waiting message is `queued`, and after Stop it stays `not_delivered` until it is sent or removed. */
  delivery_state?: string;
  attachments?: EventAttachment[];
  voice?: Record<string, unknown>;
  /** Codex's question replies of a user event, parsed in Rust (R7's annotate_replies). */
  question_replies?: Reply[];
  /** A fork's row from the session it began from (that session's journal name):
   * read-only history before the fork (zerus-b4qk). */
  inherited_from?: string;
  [field: string]: unknown;
}

/** The session an action applies to (client/core `actions.rs`). */
export interface Target { computer: ComputerId; name: string; run_id: string; conversation_id: string }

export type Action =
  | { kind: "fork"; tag: string; archive_id: string | null }
  | { kind: "settings"; model: string; effort: string; expected_pending_id?: string; without_terminal: boolean }
  | { kind: "interrupt"; turn_started: number }
  | { kind: "mark_read"; reply_id: string | null; attention_signature: string | null }
  | { kind: "pause" }
  | { kind: "resume" }
  | { kind: "rename"; new_name: string; archive_id: string | null }
  | { kind: "archive" }
  | { kind: "terminate" }
  | { kind: "kill" }
  | { kind: "terminate_shell"; identity: UntrackedShellIdentity }
  | { kind: "start_fresh" }
  | { kind: "forget" }
  | { kind: "forget_binding" }
  | { kind: "restore"; archive_id: string }
  | { kind: "forget_archive"; archive_id: string }
  /** Computer-wide: the target's session name is empty and ignored. */
  | { kind: "pause_all"; count: number }
  | { kind: "resume_all"; count: number }
  | { kind: "recovery"; job_id: string; action: "now" | "cancel" };

export interface WorktreeCleanup { status: string; path: string; common_dir: string; reason?: string | null }
export interface UntrackedShellIdentity {
  version: number; socket: string; socket_dev: string; socket_ino: string;
  server_pid: number; server_start: string; session_id: string; session_created: number;
  pane_id: string; pane_pid: number; pane_start: string; shell: string;
}

export type Outcome =
  | { outcome: "done"; receipt: unknown; cleanup: WorktreeCleanup | null; cleanup_capture?: import("./store/terminationCleanup").RetainedCapture | null }
  | { outcome: "failed"; detail: string }
  | { outcome: "uncertain"; detail: string };

/** The inspect fields that enable header actions (client/core `details.rs`). */
export interface Details {
  run_id: string;
  conversation_id: string;
  auth_required: boolean;
  expected_id: string;
  resume_message_can_send: boolean;
  fork_supported?: boolean;
  fork_reason?: string;
  /** The conversation a fork came from; `null` when this is no fork, absent from an older hgs. */
  fork_parent_id?: string | null;
  allowed_actions: string[] | null;
  action_reasons: Record<string, string>;
  /** The host can interrupt this turn (Stop). */
  interrupt_supported: boolean;
  /** The turn an interrupt is pinned to. */
  turn_started: number;
  /** The inspected phase ("" when not reported). */
  phase: string;
  /** The session's working directory; null when the inspection does not report one (an empty string stays empty). */
  cwd?: string | null;
  /** The canonical working directory; null when it is not a string (the row's then applies). */
  cwd_canonical?: string | null;
  /** Null when the inspection does not report it. */
  terminal_supported?: boolean | null;
  terminal_attach_supported?: boolean | null;
}

/** A read mark whose delivery is still pending (client/core `readmarks.rs`). */
export interface ReadMark {
  computer: ComputerId;
  name: string;
  run_id: string;
  conversation_id: string;
  reply_id: string | null;
  attention_signature: string | null;
  /** The durable session ID the Rust queue keys the mark by, when its row lists one (zerus-ucck P2c-2b). */
  session_id?: string | null;
}

/** A computer's machine card from its `catalog.meta` (client/core `machine.rs`); an older host leaves fields null. */
export interface MachineMetrics {
  cpu_percent: number | null;
  cpu_count: number | null;
  /** `available`, `warming` or `unavailable`. */
  cpu_state: string | null;
  cpu_interval_seconds: number | null;
  load_1: number | null;
  load_5: number | null;
  load_15: number | null;
  memory_used_bytes: number | null;
  memory_total_bytes: number | null;
  memory_available_bytes: number | null;
  uptime_seconds: number | null;
  /** Unix seconds. */
  sampled_at: number | null;
}
/** This Windows computer's session host. */
export interface LocalHost { state: string | null; pid: number | null; protocol: number | null; conpty: string | null; integrity: string | null; windows_session: number | null; error: string | null }
export interface MachineMeta {
  machine_id?: string;
  host: string | null;
  hgs_version: string | null;
  platform: string | null;
  ok: boolean | null;
  metrics: MachineMetrics;
  presence: { idle_seconds: number | null; locked: boolean | null; locked_seconds: number | null };
  /** False only on a control console. */
  local_sessions: boolean | null;
  local_host: LocalHost | null;
}
export interface MachineOut { computer: ComputerId; machine: MachineMeta }

// The screen views the Rust side projects (client/core `views`, client/src-tauri `screens.rs`).

export type Screen = "overview" | "projects" | "machines" | "accounts";
export interface ProjectedWindow { id: string; label: string; used: number; minutes: number | null; resets_at: number | null }
export interface Identity { email: string; name: string; plan: string; organization: string; account_id: string; auth_method: string }
export interface Usage {
  /** The account profile id, when the source reports one. */
  id?: string;
  status: string;
  provider: string;
  label: string;
  auth_status: string;
  checked_at: number | null;
  /** Provider cooldown; older peers may omit it. */
  retry_at?: number | null;
  /** The values come from the cached account status until inspect answers. */
  seeded: boolean;
  identity: Identity;
  windows: ProjectedWindow[];
  balances: { kind: string; balance: string; currency: string }[];
  /** DeepSeek's API-key `is_available`: false once the balance no longer covers API calls; older peers omit it. */
  balance_available?: boolean | null;
  /** DeepSeek API-key spend from balance readings; other accounts and older peers omit it. */
  spend?: Spend | null;
  /** This computer's DeepSeek session usage; other accounts and older peers omit it. */
  local_usage?: LocalUsage | null;
  credits: { balance: string; unlimited: boolean } | null;
  refreshing: boolean;
  refresh_error: boolean;
  offline: boolean;
}
export interface SpendAmount { currency: string; spent: string; granted: string }
/** `period` is `today` or `month`; `gap_from` names the previous balance check when the first counted interval began earlier. */
export interface SpendPeriod { period: string; amounts: SpendAmount[]; top_up: boolean; gap_from: number | null }
export interface Spend { since: number; today: string; periods: SpendPeriod[] }
export interface ModelUsage { model: string; requests: number; cache_hit: number; cache_miss: number; output: number }
export interface DayUsage { day: string; models: ModelUsage[] }
/** `partial`: the totals are incomplete. `reading`: logs wait for a later refresh. `unreadable`: logs set aside, not counted. */
export interface LocalUsage { today: string; partial: boolean; reading: boolean; unreadable: number; days: DayUsage[] }
export interface SessionUsageRef { host: ComputerId; name: string; run_id: string }
export const emptyUsage = (provider = ""): Usage => ({
  status: "loading", provider, label: "", auth_status: "", checked_at: null, seeded: false,
  identity: { email: "", name: "", plan: "", organization: "", account_id: "", auth_method: "" },
  windows: [], balances: [], credits: null, refreshing: false, refresh_error: false, offline: false,
});
export type MemberState = "ready" | "offline" | "not-installed" | "apikey" | "signed-out" | "keychain" | "desktop" | "unavailable" | "expired" | "checking" | "unknown";
export interface OpenCodeInfo { providers: { label: string; connection: string; models: string }[]; truncated: boolean; stale_catalog_at: number | null; stopped: boolean }
export interface Profile {
  key: string; host: ComputerId; machine: string; id: string; label: string; provider: string;
  native: boolean; installed: boolean; is_default: boolean; home: string; auth_revision: string; online: boolean;
  state: MemberState; detail: Text; signed_in: boolean;
  permission: string; permission_mode?: string; permission_label: Text; permission_hint: Text; model_supported: boolean; model_text: Text;
  /** The mode is the Zerus default (bypass for Claude, Codex and Kimi), not a choice (T-6hcfnu). */
  permission_default?: boolean;
  /** This computer's model and effort defaults, empty for provider settings (zerus-f0uz8). */
  model?: string; effort?: string;
  /** `account` (shared), `computer`, or empty when this computer keeps no scope. */
  permission_scope?: string;
  /** The account's color and emoji as this computer keeps them, empty when unset; `marks_supported` is false for an older hgs (zerus-sa9qn.6). */
  mark_color?: string; mark_emoji?: string; marks_supported?: boolean;
  usage: Usage; opencode: OpenCodeInfo | null;
}
export interface Account {
  key: string; label: string; provider: string; installed: boolean; machines: string[]; members: Profile[];
  usage: Usage; permission: Text; identity_lines: Text[]; identity_note: Text | null;
  /** zerus-f0uz8: permissions kept per computer; members' model or permission settings differ. */
  permissions_per_computer?: boolean; model_differs?: boolean; permission_differs?: boolean;
}
export interface AccountHost { computer: ComputerId; name: string; label: string; online: boolean }
export interface BindingEvidence { host: ComputerId; provider: string; id: string; home: string; group: string }
export interface AccountsView {
  /** Current and removed profiles; evidence only, not extra account choices. */
  binding_evidence?: BindingEvidence[];
  binding_evidence_complete?: boolean;
  catalog_loading?: boolean;
  /** Computation order from the screens hub; absent on older snapshots. */
  revision?: number;
  hosts: AccountHost[]; accounts: Account[]; overview: Account[]; snapshot: boolean; loading: boolean;
  pending: number; loaded_machines: number; busy: boolean; notices: string[]; no_hosts: string | null;
  /** Accounts left out by the per-view bound. */
  truncated: number;
}
export interface MachineRow { alias: string; enabled: boolean; source: string; inherited: boolean; hostname: string; user: string; port: number | null; identity_file: string; origin: string }
export interface MachineConnection { hostname: string; user: string; port: number | null; identity_file: string; enabled: boolean }
export type MachineChange = { kind: "set"; connection: MachineConnection; create: boolean } | { kind: "reset" | "remove" };
export interface MachineReference { alias: string; revision: string }
export type MachineSetupRequest = { kind: "install"; reference: MachineReference; source: string } | { kind: "enable"; machine_id: string };
export interface MachineSetupOutcome { success: boolean; uncertain: boolean; text: string; output: string }
export interface MachineChangeRequest { alias: string; revision: string; change: MachineChange }
export interface Effective { alias: string; pending: boolean; error: boolean; text: string; hostname: string; user: string; port: string; key: string }
export interface CheckResult { alias: string; pending: boolean; error: boolean; text: string }
export interface MachinesView {
  /** Computation order from the screens hub; absent on older snapshots. */
  revision?: number;
  loaded: boolean; refreshing: boolean; error: string | null; local: string; environment_override: boolean;
  /** Exact configuration revision, separate from the hub computation order. */
  connection_revision?: string;
  machines: MachineRow[]; effective: Effective | null; check: CheckResult | null; busy: boolean;
  /** This computer's saved reconnect timings; null from an older hgs. */
  retry_delays?: RetryDelays | null;
}
export interface Folder {
  /** The catalog's id, "" when it has none. */
  id: string;
  /** New session's identity of the folder (`FolderOut.key`). */
  key: string;
  name: string; path: string; computer: string; machine: string;
  /** The computer's identity, "" for none (New session lists an identified folder's computer only when it is configured). */
  machine_id?: string;
}
export interface Project { id: string; name: string; color: string | null; accessible: boolean; is_default: boolean; palette: number; folders: Folder[] }
export interface ProjectsView {
  /** Computation order from the screens hub; absent on older snapshots. */
  revision?: number;
  loaded: boolean; status: string; status_error: boolean; conflicts: number;
  unconnected: string[]; notice: string | null; projects: Project[];
  /** The catalog's swarm, "" before one arrives (New session's project inference). */
  swarm_id: string;
}

export const emptyAccounts = (notices: string[] = []): AccountsView => ({
  hosts: [], accounts: [], overview: [], snapshot: notices.length > 0, loading: false, pending: 0, loaded_machines: 0, busy: false, notices, no_hosts: null, truncated: 0,
});
export const emptyMachines = (error: string | null = null): MachinesView => ({
  loaded: false, refreshing: false, error, local: "", environment_override: false, machines: [], effective: null, check: null, busy: false,
});
export const emptyProjects = (status = ""): ProjectsView => ({
  loaded: false, status, status_error: status !== "", conflicts: 0, unconnected: [], notice: null, projects: [], swarm_id: "",
});

// New session (client/src-tauri `newsession.rs` over client/core `launch.rs`,
// `repository.rs` and `worktrees.rs`; the accounts from `screens.rs`).

/** A computer's folder form. */
export type PathStyle = "posix" | "windows";
/** What Start is doing. */
export type LaunchStage = "idle" | "checking_folder" | "creating_worktree";
/** A repository clone's state, as the form shows it (core `launch::Repository`). */
export interface LaunchRepository {
  busy: boolean; cloning: boolean; can_submit: boolean; created: boolean; uncertain: boolean; fields_locked: boolean;
  new_project: boolean; new_project_name: string; destination: string;
}
/** The form's state (core `launch::Form`); the three derived fields may be left out. */
export interface LaunchForm {
  no_machine: boolean; can_enable_local_sessions: boolean; online: boolean; agent: string; name: string;
  account_count: number; account_selected: boolean; account_loading: boolean; account_preset: boolean;
  path: string; listed_folder: boolean; folder_online: boolean; prefill_pending: boolean;
  /** Qt's `m_catalogRequest`. */
  catalog_pending: boolean;
  catalog_worktrees: boolean; auto_preferred: boolean;
  automatic_available?: boolean; creation_failed: boolean; project_worktree?: boolean;
  implicit_inbox: boolean; branch: string; automatic_path?: string; style: PathStyle; stage: LaunchStage;
  repository: LaunchRepository | null; clone_launch_pending: boolean;
}
/** What the form shows (core `launch::Shown`). */
export interface LaunchShown {
  start_enabled: boolean; start_text: Text; preview: Text | null; outside_project: boolean; name_problem: Text | null;
  notice: Text | null; automatic: boolean; auto_enabled: boolean; none_enabled: boolean;
  worktree_target: string; worktree_tooltip: Text | null; choose_existing_enabled: boolean; create_manually_enabled: boolean;
  worktree_options_enabled: boolean; fields_enabled: boolean; browse_enabled: boolean; cancel_enabled: boolean;
  open_terminal_enabled: boolean; open_terminal_tooltip: Text; clone_launch_due: boolean;
}
/** A saved project with its folders on each computer ("" is this computer). */
export interface LaunchProject { id: string; name?: string; folders: { machine: string; path: string }[] }
export interface NewSessionFormInput {
  form: LaunchForm;
  /** The folder's catalog (`m_worktreeCatalog`). */
  catalog: unknown;
  /** The verified catalog of this computer and folder, or null. */
  verified: unknown;
  project: LaunchProject | null;
  /** This computer "" or the alias, as the project's folders name it. */
  host: string;
  /** This computer's machine card: the core derives `can_enable_local_sessions` from it. */
  local?: MachineMeta | null;
}
export interface NewSessionFormView { shown: LaunchShown; automatic_available: boolean; automatic_path: string; project_worktree: boolean }
/** What Start does (core `launch::Start`). */
export type LaunchStart = "nothing" | "background" | "clone" | { check_folder: { with_worktree: boolean } };
export interface LaunchRepositoryChoice {
  url: string; parent: string; name: string; destination: string; created_path: string; new_project: boolean; project_name: string;
}
/** The form's choices for background creation (core `launch::Choices`). */
export interface LaunchChoices {
  host: string; machine: string; agent: string; account: string; name: string; project: string; project_name: string;
  implicit_inbox: boolean; swarm: string; path: string; auto_preferred: boolean; automatic_available: boolean;
  branch: string; catalog: unknown; automatic_path: string; listed_folder: boolean; project_worktree: boolean;
  model: string; effort: string; open_terminal: boolean; repository: LaunchRepositoryChoice | null; predecessor: string;
}
/** Background creation's plan (core `launch::Plan`, Qt's `creationRequested` object). */
export interface LaunchPlan {
  host: string; machine: string; cmd: string; account: string; tag: string; group: string; group_name: string;
  implicit_inbox: boolean; swarm: string; target: string; source: string; auto: boolean; branch: string; catalog: unknown;
  add_folder: boolean; model: string; effort: string; terminal: boolean; repository: boolean;
  destination?: string; url?: string; parent?: string; repository_name?: string; new_project?: boolean;
  project_name?: string; clone_done?: boolean; acquisition_confirmed?: boolean; predecessor?: string;
}
/** What a new dialog starts with. `remembered` stays empty until the settings store arrives. */
export interface NewSessionContext {
  branch: string;
  own_windows_session: number | null;
  /** The platform this client was built for: this computer's folder style before it reports a platform. */
  build_style: PathStyle;
  remembered: { agent: string; accounts: Record<string, Record<string, string>>; open_terminal: boolean };
  /** The catalog's swarm, "" before one arrives: a folder's project inference counts only from it. */
  swarm: string;
}
/**
 * A project folder as the organization holds it (client/core `swarm::Folder`, Qt's
 * SessionOrganization::Folder): `project_folders` answers at most 200 of them.
 */
export interface FolderOut {
  /** The catalog's id, "" when it has none; never made up. */
  id: string;
  /** The folder's identity here: `id`, else `machine` + "\n" + `path`. Never sent to hgs as a folder id. */
  key: string;
  machine: string;
  path: string;
  /** "" when the catalog names none. */
  name: string;
  /** The computer's identity, "" for none. */
  machine_id: string;
  machine_name: string;
}
/** What Create manually… created (`newsession.rs` `CreatedWorktree`). */
export interface CreatedWorktree {
  path: string;
  /** The catalog the dialog opened with, listing the new checkout as verified (`launch::created_catalog`). */
  catalog: Record<string, unknown>;
}
export interface RepositoryFields {
  url: string; parent: string; name: string;
  /** The Clone to field's text. */
  field: string;
  /** `PARENT/NAME`, as Start and the plan take it. */
  destination: string;
  suggested: string; can_submit: boolean;
}
/**
 * `creation_submit`'s answer (Task 5): the admitted creation's id, or the refusal's text. A rejected call is
 * neither: its outcome is unknown, so the dialog stays submitted and nothing sends it again.
 */
export type CreationAdmission = { admitted: string } | { refused: string };
/**
 * An admitted creation's card (Task 5's `creations.rs` `CreationCard`): the operation's fields, with the caption,
 * `can_resume` and `can_edit` from the core (`creation::caption`, `can_resume`, `Creations::can_edit`).
 */
export interface CreationCard {
  /** Current launch attempt, rotated on retry; absent on older hosts. */
  launch_id?: string;
  id: string; tag: string;
  /** The operation's state (`saving` ... `failed`, `bound`). */
  state: string;
  caption: string; error: string;
  /** Seconds since the Unix epoch. */
  created: number;
  /** Last recorded state change in desktop-clock Unix seconds; absent on older hosts. */
  updated?: number;
  /** The destination project's id. */
  group: string;
  /** "" for this computer, else the alias. */
  host: string;
  /** The computer's identity: its machine card's host, else the alias. */
  machine: string;
  cmd: string; account: string;
  /** The folder the agent starts in. */
  target: string;
  show_pending: boolean; launch_dispatched: boolean; stop_requested: boolean; worktree_receipts: boolean;
  /** Metadata-only stop: the original UUID and its unknown past remain retained. */
  worktree_fenced?: boolean;
  worktree_pending?: boolean;
  can_resume: boolean; can_edit: boolean;
  /** The exact bound session's full name, empty until bound or on an older host. */
  bound_session: string;
}
/** What a card's buttons ask of its exact operation (`creation_action`). */
export type CreationAction = "resume" | "check" | "stop" | "hide" | "show";
/** The `creations` event: a changed card, or an evicted operation. */
export type CreationEvent = CreationCard | { id: string; removed: true };
/** New session's account rows of the dialog's computer (`screens.rs`). */
export interface LaunchAccounts {
  /** A catalog reply (good or failed) was filed since the dialog chose this computer. */
  loaded: boolean;
  /** "Could not load accounts: %1" for the last reply's failure; the rows stay the last good catalog's since then. */
  error: string | null;
  /** The catalog's raw rows, at most 256, for the OpenCode fields. */
  profiles: Record<string, unknown>[];
  groups: Account[];
}

// The Terminal tab (src-tauri/src/terminal.rs).
export interface TerminalTarget { computer: ComputerId; name: string; run_id: string; machine: string; conversation_id?: string }
export interface TerminalConnected { generation: number; status: string; reused: boolean }
export interface TerminalStatus { supported: boolean; available: boolean; reason: string; idle: string; connecting: string; disconnected: string }
/** The next raw output is history; parse it before accepting input. */
export interface TerminalReplay { kind: "replay" }
export type TerminalEvent = ArrayBuffer | TerminalFinished | TerminalReplay;
export interface TerminalFinished { kind: "finished"; code: number; status: string }
/** The core's `TerminalFacts`: a row's terminal fields. */
export interface TerminalFacts {
  cmd: string;
  terminal_supported: boolean;
  terminal_reported: boolean;
  terminal_attach_supported: boolean;
  terminal_attach_reported: boolean;
  terminal_reason: string;
  terminal_attach_reason: string;
}

/** One checked page of a computer's archive (`archive_page`). */
export interface ArchiveRow extends ViewRow { group: string; rank: number }
export interface ArchivePage {
  rows: ArchiveRow[];
  next_cursor: string;
  previous_cursor: string | null;
  total: number;
  /** The host counted only up to a cap (zerus-080n8): the total shows as "N+". */
  total_capped?: boolean;
  position: number | null;
  /** zerus-080n8: the position is a lower bound. */
  position_capped?: boolean;
  /** zerus-080n8: the host's scan stopped at its budget before a full page; it continues. */
  partial?: boolean;
  /** zerus-080n8: rows the host looked at for this page. */
  searched?: number;
  /** zerus-5czy6: a bounded page; an exact one (an older host too) is not. */
  bounded?: boolean;
}

/** One read of an archived session's recorded Activity (`archived_activity`, `hgs inspect --archive`). */
export interface ArchivedActivity {
  /** The reply's events, unchanged. */
  events: ActivityEvent[];
  replace_events: boolean;
  conversation_id: string;
  run_id: string;
  cleared_conversations: string[];
  /** The journal cursor the next read asks `--after`. */
  cursor: number;
  /** The other top-level fields, what `buildTranscript` reads as the summary. */
  summary: Record<string, unknown>;
}

// The composer (src-tauri/src/composer): the mirrors of its serde shapes.

/** One attachment of the open draft; `path` is its `zerus-file` path. */
export interface ImageSize { width: number; height: number }
export interface AttachmentView { name: string; mime: string; reference: string; bytes: number; path: string; excerpt: string | null; image_size?: ImageSize | null }
export type ChipTone = "quiet" | "danger" | "warning" | "success";
/** The cache chip (core `cache::Chip`); hidden when not `visible`. */
export interface Chip { visible: boolean; tone: ChipTone; label: string; short_label: string }
/** The Context chip (core `session_usage::ContextChip`); hidden when not `visible`. */
export interface ContextChip { visible: boolean; text: string; tooltip: string; warning: boolean; tone: ChipTone; percent: number | null; accessible_name: string }
/** Clear session and the cache popover's Clear context, with the dialog the confirmation shows. */
export interface ClearControl {
  enabled: boolean; dismiss: boolean; title: string; text: string; confirm: string; run: string; conversation: string;
  /** The cache popover's Clear context (Qt 3792). */
  cache_enabled: boolean;
  /** A context reset runs: "Clearing…". */
  running: boolean;
  /** The tooltips' reason sentences, empty for the usual tooltip. */
  reason: string; cache_reason: string;
}
/** One local message row (core `local_messages::LocalMessage`). */
export interface LocalMessage {
  id: string; text: string; status: "sending" | "sent" | "error"; attachments: Record<string, unknown>[]; run_id: string; conversation_id: string;
  submitted_at: number; error: string; uncertain: boolean; message_id: string | null; submitted_text: string | null; source: string | null; voice: Record<string, unknown> | null;
}
/** The native queue (core input_queue::QueueView), kept separate from local messages. */
export interface QueueView { id: string; text: string; hint: string; can_send_now: boolean; enabled: boolean; label: string; run_id?: string; conversation_id?: string }
/** What one Remove of a not-delivered OpenCode message came to (src-tauri composer/inbox.rs). */
export type InboxRemoveReply = { outcome: "removed" | "changed" | "busy" } | { outcome: "failed" | "uncertain"; detail: string };

/** A held message: the request of its local row, its exact text and its file names. */
export interface HeldMessageView { request_id: string; text: string; attachments: string[] }
export interface ComposerView {
  handle: number; key: string; subagent?: { agent_id: string; send_supported: boolean; hint: string } | null; loaded: boolean; rev: number; text_epoch: number; text: string; position: number; anchor: number;
  attachments: AttachmentView[]; removed: string | null; attaching: boolean; sending: boolean; recording?: boolean; waiting: string | null; uncertain: boolean;
  /** The desktop service still sends this draft's message: no Allow retry, and its status is not an error. */
  delivery_pending?: boolean;
  /** Advisory only: matching text in loaded history is never receipt evidence. */
  history_text_match?: boolean;
  status: string; status_error: boolean; can_send: boolean; send_label: Text; available: boolean; unavailable_reason: string; block_reason: string | null;
  attachments_available: boolean; attachments_reason: string; session_status: string; session_status_error: boolean; session_blocks_input: boolean;
  /** The open session's last read failed (Qt's m_inspectError). */
  inspect_error: boolean;
  cache: Chip; cache_warning: string; cache_details: string; context: ContextChip; local: LocalMessage[];
  cancel_send: boolean; cancel_locked?: boolean; compacting: boolean; clear: ClearControl;
  /** The message a "Compact and continue" holds while the desktop service compacts first: it left the composer but is not sent yet (zerus-pabah). */
  held?: HeldMessageView | null;
  /** The agent's question card shown in the composer (A2). */
  question: QuestionView | null;
  /** False while a required card stands in for the editor and the action row (Qt's setInputVisible). */
  input_visible: boolean;
  queue: QueueView | null;
  /** The not-delivered OpenCode message of this session whose removal runs, and whether any removal runs (no other Remove can be clicked). */
  inbox_removing?: string | null;
  inbox_busy?: boolean;
  /** The interrupted prompt Rust offers an empty field (zerus-5wmh.13.4): taken once, only while this field is still empty and nothing typed is on its way. */
  offer?: { id: number; text: string } | null;
}

// The question card (src-tauri composer/questions.rs, core questions.rs).
export interface ButtonView { visible: boolean; enabled: boolean; label: Text; review_label: Text | null }
export interface OptionView { id: string; label: Text; description: Text; markdown: boolean; checked: boolean; enabled: boolean; visible: boolean }
export interface TextView { visible: boolean; enabled: boolean; value: string; placeholder: Text; accessible_name: Text; max_units: number; max_chars: number }
export interface FormView { id: string; tab: Text; prompt: string; body: string; markdown: boolean; monospace_body: boolean; multi: boolean;
  mode: Text; mode_visible: boolean; options: OptionView[]; other: OptionView | null; text: TextView | null }
export interface QueueNav { previous: boolean; next: boolean; previous_tip: Text; next_tip: Text }
export interface CardView { heading: Text; progress: Text | null; pending: Text | null; pending_tip: Text; nav: QueueNav | null; tabs_visible: boolean;
  page: number; forms: FormView[]; status: Text; status_error: boolean; available: boolean; locked: boolean; review: boolean; created_at: number;
  submit: ButtonView; approve: ButtonView; deny: ButtonView; skip: ButtonView; previous: ButtonView; next: ButtonView; retry: boolean; skip_tip: Text; retry_tip: Text }
/**
 * `revision` grows when the card is built again (a new card, a changed form,
 * a restored answer): the review starts again and the fields reset. `epoch`
 * grows with every change of the answers from Rust, each revision's included:
 * the fields take Rust's values again, and an edit names the epoch it is based on.
 */
export interface QuestionView { answering?: boolean; card: string; revision: number; epoch: number; asked: string; asked_tip: string; terminal_label: Text; native_ui: boolean; view: CardView }
export type QuestionEdit = { kind: "option"; item: string; option: string; checked: boolean } | { kind: "other"; item: string; checked: boolean } | { kind: "text"; item: string; text: string };
export interface ColdAnswer { title: string; text: string; proceed: string }
export type QuestionReply = { kind: "sending" } | { kind: "busy" } | { kind: "cold"; token: number; prompt: ColdAnswer } | { kind: "notice"; text: string };
/** Notices and Terminal requests that arrive after their call (`next_batch`'s `composer_events`). */
export type ComposerEvent = { kind: "notice"; text: string; error: boolean; transient: boolean } | { kind: "open_terminal"; key: string };
/** A Codex question reply, parsed in Rust (`question_replies`, `prompt_replies`). */
export interface Reply { question: string; answer: string }
export type ColdAction = "send" | "compact" | "clear";
export interface ColdChoice { action: ColdAction; label: string; enabled: boolean; tooltip: string }
export interface ColdPrompt { title: string; text: string; informative: string; choices: ColdChoice[] }
export type StartChoice = "resume" | "resume_compact" | "fresh";
export interface StartOption { choice: StartChoice; label: Text; enabled: boolean; tooltip: Text | null }
export interface StartQuestion { title: Text; text: Text; informative: Text; options: StartOption[] }
export type SendReply =
  | { kind: "sending" } | { kind: "busy" } | { kind: "blocked"; reason: string } | { kind: "notice"; text: string; error: boolean }
  | { kind: "cold"; token: number; prompt: ColdPrompt } | { kind: "clear_context"; token: number } | { kind: "start"; token: number; question: StartQuestion };
export type Answer = { kind: "cold"; action: ColdAction } | { kind: "start"; choice: StartChoice } | { kind: "cancel" };
/** Where Inspect on a failed local row leads. */
export type Inspect = { kind: "native_ui" } | { kind: "terminal" } | { kind: "refresh"; notice: string };
/** One exact native child; sends still name the parent run and conversation. */
export interface SubagentRef { agent_id: string; parent_conversation_id: string }
/** The session or child a composer belongs to (src-tauri `SessionRef`): `key` is the parent catalog key. */
/** `session_id`: the row's durable ID, which keys its drafts (src-tauri `session_draft_key`). */
export interface SessionRef { computer: ComputerId; key: string; name: string; archive_id: string | null; subagent?: SubagentRef; session_id?: string | null }
/** The detail tab that shows, for the composer's inspect retry (Qt's poll interval). */
export type ComposerTab = "activity" | "terminal" | "native" | "processes";
/** A file of an Activity message, as the journal lists it. */
export interface EventAttachment {
  name?: string;
  mime?: string;
  bytes?: number;
  /** `[Image #n]` or `[File #n]` in the message text. */
  reference?: string;
  /** The message's request, which `hgs attachment` reads the file by; none when it saved no reference. */
  request_id?: string;
  /** The file's place in the message, -1 when it has none. */
  index?: number;
  path?: string;
  size?: number;
  kind?: string;
}

/** One attachment of an Activity message, as `hgs attachment` reads it (core `reads::AttachmentRef`). */
export interface AttachmentRef {
  /** The session's name. */
  name: string;
  request_id: string;
  index: number;
  conversation_id: string;
  archive_id: string | null;
  /** A subagent's ID for a subagent's message. */
  agent_id: string | null;
}

/** An attachment in the private cache: `path` is its `zerus-file` path (`cache/{digest}/{name}`). */
export interface Fetched {
  path: string; name: string; mime: string; bytes: number;
  /** An image whose header gives its size, at most Qt's 64M pixels: a card may show its thumbnail. */
  thumbnail: boolean;
  image_size?: ImageSize | null;
  /** A task's file: where `task_attachment_read` reads it. Its `path` is empty until it is read. */
  source?: TaskFileSource;
}
/** A file a task page names: the entry (null for the task's own files), its number from 1 and its metadata. */
export interface TaskFileSource {
  entry: string | null; file: number;
  meta: { name: string; mime: string; reference: string; size: number; sha256: string };
}

// Saved drafts (src-tauri `composer/saved.rs`).
/** Which store a Saved drafts row comes from: this computer's, Qt's, or the dev app's (only the import's conflicts). */
export type DraftSource = "client" | "qt" | "dev";
/** Whose conversation a draft key names (core `drafts::OwnerKind`). */
export type DraftOwnerKind = "unknown" | "message" | "subagent" | "question" | "voice" | "task" | "task_description" | "task_comment";
export interface SavedDraftItem {
  key: string; kind: DraftOwnerKind;
  /** The key's machine: a host alias, "" for this computer; a subagent key names the machine. */
  computer: string; session: string;
  /** The durable ID of a draft keyed by it; "" for a key that names its session. */
  session_id: string;
  /** The owner's parts joined by " / " (the row's accessible text). */
  title: string;
  /** What the row shows as its title. */
  caption: string; machine: string; meta: string; detail: string;
  /** Seconds since the epoch; 0 when unknown. */
  modified: number; attachments: number; source: DraftSource; archived: boolean;
}
/** `next` is an opaque token for the following page. */
export interface SavedDraftsPage { items: SavedDraftItem[]; next: string | null }
/** The G3 drafts capture's status (draft_preload::Status), shown in Saved drafts in every mode. */
export interface DraftCaptureStatus { code: string; capture_id: string | null; manifests: number; writes: boolean; paused: boolean }
/** The import's conflicts; `next` is the following page's offset. */
export interface ConflictsPage { items: SavedDraftItem[]; next: number | null }
export interface SavedDraftDetail { text: string; attachments: number; detail: string; restorable: boolean }

// The update service's view (src-tauri `updates.rs`), shared by Settings and the status bar.
export type UpdateChannel = "stable" | "nightly" | "dev";
export interface UpdatesView {
  busy: boolean;
  available: boolean;
  /** Interface prose from the core and app (core Text); the webview translates it. */
  heading: Text;
  installed: Text;
  latest: Text;
  comparison: Text | null;
  /** RFC 3339 time, or "Never". */
  last_checked: string;
  cached: boolean;
  error: Text | null;
  channel: UpdateChannel;
  automatic: boolean;
  release_label: Text;
  release_enabled: boolean;
  download_label: Text | null;
  details: { ownership: Text; command: Text | null; copy_name: Text | null; instructions: Text | null };
  restart_required: boolean;
  login_item_deprioritized: boolean;
  version: string;
  /** A source or unknown dev-app installation shows status in Settings only. */
  quiet: boolean;
}

export const emptyUpdatesView = (): UpdatesView => ({
  busy: false,
  available: false,
  heading: { source: "Updates have not been checked" },
  installed: { source: "" },
  latest: { source: "Not checked" },
  comparison: null,
  last_checked: "Never",
  cached: false,
  error: null,
  channel: "stable",
  automatic: true,
  release_label: { source: "View release" },
  release_enabled: false,
  download_label: null,
  details: { ownership: { source: "" }, command: null, copy_name: null, instructions: null },
  restart_required: false,
  login_item_deprioritized: false,
  version: "",
  quiet: false,
});
