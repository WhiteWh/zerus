// Settings > Mobile connection as its Qt panels drive it. The reads, their
// answers and the one call of each kind that may run live in the app
// (mobile.rs); the page asks for them and shows the newest view.

export interface Controls { change: boolean; enrollment: boolean; start: boolean; stop: boolean; check: string | null; invitations: boolean }
export interface Workspace {
  state: string; encrypted: boolean; summary: string; connection: string; pending: string | null; running: boolean;
  config_revision: string; server_url: string; controls: Controls;
  connector_state?:string; stop_supported?:boolean; node_id?:string; generation?:string|null; pending_state?:string|null; pending_node_id?:string|null; pending_action?:string|null; migration_reason?:string|null;
}
export interface Phone { id: string; name: string; platform: string; paired_at: number | null; last_seen_at: number | null; added_from: string | null; revoke: string }
export interface PhoneList { host: string; workspace_id: string; workspace_name: string; encrypted: boolean; server_url: string; invite: string; phones: Phone[] }
export interface Phones { lists: PhoneList[]; lines: string[] }
export interface View {
  workspace: Workspace | null; workspace_error: string | null; workspace_busy: boolean;
  phones: Phones | null; phones_busy: boolean;
  delivery: string | null; delivery_busy: boolean; delivery_status: string;
  delivery_outdated: string | null; delivery_worker: string | null;
  /** The timing could not be read: nothing to choose until it is. */
  delivery_failed: boolean;
  /** Grows with every change in the app. */
  version: number;
}

export interface MobileIo {
  view(): Promise<View>;
  open(): Promise<View>;
  refreshWorkspace(): Promise<View>;
  refreshPhones(): Promise<View>;
  setDelivery(value: string): Promise<View>;
}

export interface MobileState { view?: View; error?: string }

export function createMobile(io: MobileIo, show: (state: MobileState) => void) {
  let state: MobileState = {};
  const update = (change: Partial<MobileState>) => { state = { ...state, ...change }; show(state); };
  // A reply can arrive after a newer view (an answer's event was quicker).
  const take = (view: View) => { if (!state.view || view.version >= state.view.version) update({ view, error: undefined }); };
  const fail = (error: unknown) => update({ error: String(error) });
  const call = (run: () => Promise<View>) => { run().then(take, fail); };
  return {
    state: () => state,
    /** Opening the section: the app reads what is not being read already. */
    open() { call(() => io.open()); },
    /** The app says an answer came. */
    changed() { call(() => io.view()); },
    // An effect receipt may beat its busy=false event. The native lane remains
    // authoritative; an explicit post-receipt read must not use that stale flag.
    refreshWorkspace(afterAction = false) { if (afterAction || !state.view?.workspace_busy) call(() => io.refreshWorkspace()); },
    refreshPhones() { if (!state.view?.phones_busy) call(() => io.refreshPhones()); },
    setDelivery(value: string) {
      const view = state.view;
      if (!view || view.delivery_busy || view.delivery_failed || view.delivery === value) return;
      call(() => io.setDelivery(value));
    },
  };
}
