import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { createServer, type ViteDevServer } from "vite";
import solid from "vite-plugin-solid";

// Task #368: the fork's dense New session dialog ('B - Dense, labels left'),
// Qt's NewSessionDialog buildDenseLayout behind the skin's views.newSession.
// The dialog and the dense menu are production code; the store, the bridge and
// the child dialogs are stand-ins fed from globalThis.
const entry = "\0newsession-render";
const STUBS: Record<string, string> = {
  "src/i18n": `export const trNoop = (s) => s; export const tr = (s, ...args) => s.replace(/%(\\d+)/g, (_, n) => String(args[n - 1]));`,
  "src/text": `export const trText = (value) => typeof value === "string" ? value : (value && value.text) || "";`,
  "src/bridge": `export const listDirectories = async () => ({}); export const createDirectory = async () => ({}); export const worktreeSnapshot = async () => ({}); export const worktreeCatalog = async () => ({}); export const createWorktree = async () => ({}); export const pickLocalFolder = async () => undefined;`,
  "src/store/newSession": `export const newSession = () => globalThis.__ns.state;
    export const newSessionActions = new Proxy({}, { get: () => () => {} });
    export const closeNewSession = () => {}; export const startNewSession = async () => {};`,
  "src/store/machines": `export const machineCards = { "": { host: "workstation", platform: "win32", ok: true } };`,
  "src/store/catalog": `export const catalog = { computers: {}, rows: {} };
    export const agentKind = (agent) => { const a = (agent ?? "").toLowerCase(); if (a === "dsh") return "deepseek"; return ["codex","claude","kimi","opencode","ollama","deepseek"].includes(a) ? a : "claude"; };`,
  "src/store/overview": `export const connectionOf = () => ({ tile: "online" }); export const osOf = () => "windows";`,
  "src/store/ui": `export const now = () => 1_000_000;`,
  "src/skin/runtime": `export const skin = () => ({ layout: { "new-session": globalThis.__ns.view }, appearance: globalThis.__ns.appearance });`,
  "src/ui/sessions/newsession-ui": `export const launchWindows = (usage) => (usage?.windows ?? []).map((w) => ({ minutes: w.minutes, used: w.used, resetsIn: w.resetsIn, label: w.label }));`,
  "src/ui/sessions/newsession-view": `export const accountKey = (row) => row.key; export const computerKey = (row) => row.target;
    export const folderKey = (row) => row.key + "\\n" + row.path; export const projectKey = (row) => (row.newProject ? "\\nnew" : row.id);
    export const createConfirm = () => ({ text: () => undefined, ask: async () => true, answer() {}, cancel() {} });
    export const dialogKey = () => "native";`,
  "src/ui/components/MachineTile": `export const MachineTile = (p) => "[" + p.state + "]";`,
  "src/ui/components/UsageMeter": `export const UsageMeters = () => "[meters]";`,
  "src/ui/components/FolderBrowser": `export const FolderBrowserDialog = () => null;`,
  "src/ui/sessions/LaunchWarning": `export const LaunchWarning = () => null;`,
  "src/ui/sessions/ModelPicker": `export const ModelPopover = () => null;`,
  "src/ui/sessions/WorktreePicker": `export const ChooseWorktreeDialog = () => null;`,
  "src/ui/sessions/NewWorktreeDialog": `export const NewWorktreeDialog = () => null;`,
  "src/ui/sessions/ClaudeAppDialog": `export const ClaudeAppDialog = () => null;`,
  "src/ui/sessions/worktree-dialogs": `export const sameSessions = () => true;`,
  "src/ui/lib/icons": `export const Icon = (p) => "[" + p.name + (p.class ? " " + p.class : "") + "]"; export const AgentGlyph = (p) => "[" + p.agent + "]";`,
  "src/ui/lib/keys": `export const composing = () => false;`,
  // Task 407: upstream's Dropdown (8f2b428de) calls createUniqueId() at render
  // time, and this harness renders with Solid's plain renderToString, which has
  // no use for unique ids; a field stands in as a plain control, the dialog's
  // layout being what these tests read.
  "src/ui/lib/Dropdown": `export const Dropdown = (p) => "[" + (p.id ?? "dropdown") + "]";`,
};
const stubId = (key: string) => `\0newsession-stub:${key}`;
let server: ViteDevServer | undefined;
let renderDialog: () => string;
let renderMenu: () => string;

const account = (provider: string, id: string, email: string, five: number, week: number, status = "ready") => ({
  key: `${provider}\n${id}`, id, provider, host: "", shell: provider === "sh", profile: undefined, members: [],
  name: email, summary: "", tone: undefined, tooltip: email, accessible: email,
  usage: {
    status, provider, identity: { email, name: "", plan: "", organization: "", account_id: "", auth_method: "" },
    windows: five < 0 ? [] : [{ minutes: 300, used: five, resetsIn: 100 }, { minutes: 10080, used: week, resetsIn: 9000 }],
    balances: provider === "dsh" ? [{ kind: "balance", balance: "3.50", currency: "CNY" }] : [],
  },
});
const accounts = [
  account("claude", "a", "a@example.com", 15, 40),
  account("claude", "b", "b@example.com", 70, 20),
  account("codex", "c", "c@example.com", 40, 30),
  account("dsh", "d", "d@example.com", -1, -1),
  account("sh", "", "", -1, -1),
];
const form = {
  start_enabled: true, start_text: "Start session", fields_enabled: true, cancel_enabled: true, browse_enabled: true,
  automatic: true, auto_enabled: true, none_enabled: true, choose_existing_enabled: true, create_manually_enabled: true,
  name_problem: "", open_terminal_enabled: true, open_terminal_tooltip: "", preview: "", outside_project: false,
  worktree_target: "", worktree_tooltip: "",
};
const state = (view: string, appearance = "dark") => ({
  projects: [{ id: "p1", name: "Orbit", color: null, palette: 0, newProject: false }], project: 0,
  newProjectVisible: false, newProjectRow: false, newProjectName: "",
  computers: [{ label: "workstation", target: "", online: true }], computer: 0, host: "", style: "windows", noMachine: false,
  repository: false, url: "", destination: "", repositoryParent: "", focusUrl: 0,
  folders: [{ key: "f1", path: "D:\\work\\orbit", name: "orbit", tooltip: "D:\\work\\orbit" }], folder: 0, folderPath: "D:\\work\\orbit",
  autoPreferred: true, worktrees: { catalog: {}, automatic: true as boolean | undefined, chooseVisible: true, createVisible: true },
  accounts, account: 0, accountStatus: "", refreshEnabled: true, agent: "claude", accountId: "a",
  name: "orbit-2", branch: "main", openTerminal: false, form, error: "", starting: false, submitted: false,
  models: [], model: "", modelVisible: true, modelButton: "Native default model", modelEnabled: true, pickerOpen: false, openCode: true,
  picker: { groups: [], notice: "", all: { checked: false, enabled: false, tooltip: "" }, selected: undefined, query: "", favoritesAvailable: false },
  efforts: [{ value: "", label: "Default" }], effort: "", effortVisible: true,
  service: { visible: false, reason: "", offer: false, text: "", enabled: false, tooltip: "" },
  __view: view, __appearance: appearance,
});

beforeAll(async () => {
  const { fileURLToPath } = await import(/* @vite-ignore */ ["node", "url"].join(":")) as { fileURLToPath(url: URL): string };
  const root = fileURLToPath(new URL("../../../", import.meta.url));
  server = await createServer({
    configFile: false, root,
    cacheDir: fileURLToPath(new URL("../../../node_modules/.vite-newsession", import.meta.url)),
    server: { middlewareMode: true, watch: null }, logLevel: "silent",
    plugins: [{
      name: "newsession-render-mocks", enforce: "pre",
      async resolveId(id, importer) {
        if (id === entry || id.startsWith("\0newsession-stub:")) return id;
        if (!importer || id.startsWith("\0") || !id.startsWith(".")) return undefined;
        const resolved = await this.resolve(id, importer, { skipSelf: true });
        const path = resolved?.id.replace(/\\/g, "/").replace(/\.(tsx?|jsx?)$/, "") ?? "";
        const key = Object.keys(STUBS).find((stub) => path.endsWith(`/${stub}`));
        return key ? stubId(key) : undefined;
      },
      load(id) {
        if (id.startsWith("\0newsession-stub:")) return STUBS[id.slice("\0newsession-stub:".length)];
        if (id === entry) return `import {createComponent} from 'solid-js';
          import {renderToString} from 'solid-js/web';
          import {NewSessionDialog, Choice} from '/src/ui/sessions/NewSessionDialog.tsx';
          export const renderDialog = () => renderToString(() => createComponent(NewSessionDialog, {}));
          export const renderMenu = () => renderToString(() => createComponent(Choice, {
            id: "m", label: "Project", dense: true, check: true, header: "Project", open: true,
            keys: ["a", "b", "c"], selected: "a", disabled: false, onOpen: () => {}, onPick: () => {},
            option: (key) => key.toUpperCase(),
            tip: (key) => (key === "c" ? "b is offline" : undefined),
            actions: [{ label: "New project…", icon: "add", run: () => {} }],
          }));`;
        return undefined;
      },
    }, solid({ ssr: true })],
  });
  const loaded = await server.ssrLoadModule(entry);
  renderDialog = loaded.renderDialog;
  renderMenu = loaded.renderMenu;
});

afterAll(async () => { await server?.close(); });

const prepare = (view: string, appearance = "dark") => {
  const value = state(view, appearance);
  (globalThis as Record<string, unknown>).__ns = { state: value, view, appearance };
  return value;
};
const plain = (html: string) => html.replace(/<!--[^>]*-->/g, "");

describe("the dense New session view (task #368)", () => {
  it.each(["dense", "classic"])("shows None only when the folder itself is selected in %s", (layout) => {
    const state = prepare(layout);
    const pressed = () => {
      const controls = plain(renderDialog()).match(/<div[^>]*aria-label="Worktree"[^>]*>([\s\S]*?)<\/div>/)?.[1] ?? "";
      return [...controls.matchAll(/aria-pressed="(true|false)"/g)].map((match) => match[1]);
    };
    state.worktrees.automatic = undefined;
    expect(pressed()).toEqual(["false", "false"]);
    state.worktrees.automatic = false;
    expect(pressed()).toEqual(["false", "true"]);
  });

  it("is the skin's default view and draws Qt's grid, captions, rail and menus", () => {
    prepare("dense");
    const html = plain(renderDialog());
    // Its own 520px frame (nsd-dlg) with the header's Name caption.
    expect(html).toMatch(/class="[^"]*nsd-dlg/);
    expect(html).toContain("nsd-header");
    for (const caption of ["Name", "Target", "Folder", "Agent", "Model"]) expect(html).toContain(`>${caption}<`);
    // The agent rail: one tile per agent in Qt's order, the chosen one on.
    const tiles = [...html.matchAll(/data-agent="([a-z]+)"/g)].map((match) => match[1]);
    expect(tiles).toEqual(["codex", "claude", "kimi", "dsh", "ollama", "sh"]);
    const claude = html.match(/<button[^>]*data-agent="claude"[^>]*>/)?.[0] ?? "";
    expect(claude).toMatch(/\bon\b/);
    expect(claude).toContain('aria-pressed="true"');
    // Claude has two accounts: the count and its dots; the chip names the chosen one.
    expect(html).toMatch(/data-agent="claude"[\s\S]*?2 acc/);
    // Codex's meters: the 5h and week shares and the 5h reset time.
    expect(html).toMatch(/data-agent="codex"[\s\S]*?40%[\s\S]*?30%[\s\S]*?01:40/);
    // Kimi has no account: "no acct" then "+ add".
    expect(html).toMatch(/data-agent="kimi"[\s\S]*?no acct[\s\S]*?\+ add/);
    // DeepSeek's balance, and the terminal's narrow tile.
    expect(html).toMatch(/data-agent="dsh"[\s\S]*?¥3\.50/);
    const shell = html.match(/<button[^>]*data-agent="sh"[^>]*>/)?.[0] ?? "";
    expect(shell).toMatch(/\bnarrow\b/);
    // The mono path line, the model row and the footer's buttons.
    expect(html).toContain("nsd-path");
    expect(html).toContain('id="ns-model"');
    expect(html).toContain("nsd-start");
    expect(html).toContain(">Start session<");
  });

  it("keeps the classic layout when the skin names it", () => {
    prepare("classic");
    const html = plain(renderDialog());
    expect(html).not.toContain("nsd-dlg");
    expect(html).not.toContain("nsd-tile");
    expect(html).toContain('class="dlg-head"');
    expect(html).toContain("ns-body");
  });

  it("restores the Source switch and the permission line in the dense layout (task #433)", () => {
    const value = prepare("dense");
    const account = value.accounts[value.account];
    (account as unknown as { profile?: unknown }).profile = { permission_label: { source: "account", text: "Ask" }, permission_default: false, permission_hint: "From the account" };
    const html = plain(renderDialog());
    expect(html).toContain('data-source="folder"');
    expect(html).toContain('data-source="repository"');
    expect(html).toContain('data-perm');
    expect(html).toContain("Permissions: Ask");
    expect(html).toContain("Change it on the Accounts screen.");
  });

  it("opens a dense menu with its header, a check at the choice, an unavailable row with its reason and the view's action", () => {
    prepare("dense");
    const html = plain(renderMenu());
    expect(html).toContain("nsd-menu-header");
    expect(html).toContain(">Project<");
    // The check marks the current choice only.
    expect(html.match(/nsd-check/g)?.length).toBe(1);
    expect(html).toMatch(/data-key="a"[\s\S]*?nsd-check/);
    // b is the current choice, c is unavailable with its reason, and the action follows.
    expect(html).toMatch(/data-key="c"[^>]*aria-disabled="true"/);
    expect(html).toMatch(/data-key="c"[\s\S]*?b is offline/);
    expect(html).toContain("New project…");
  });
});

describe("the dense view's stylesheet (task #368)", () => {
  it("is Qt's grid, the 520px frame and skin tokens only", async () => {
    const { readFileSync } = await import(/* @vite-ignore */ ["node", "fs"].join(":")) as { readFileSync(path: string): { toString(): string } };
    const { fileURLToPath } = await import(/* @vite-ignore */ ["node", "url"].join(":")) as { fileURLToPath(url: URL): string };
    const css = String(readFileSync(fileURLToPath(new URL("../styles/newsession.css", import.meta.url)))).replace(/\r\n/g, "\n");
    expect(css).toContain("grid-template-columns: 50px minmax(0, 1fr) 30px 148px;");
    expect(css).toContain("dialog.ns-dlg.nsd-dlg { width: min(520px");
    const dense = css.slice(css.indexOf("/* Task #368")).replace(/\/\*[\s\S]*?\*\//g, "");
    // Light and dark both come from the skin: no literal colours in the dense block.
    expect(dense).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
    expect(dense).not.toMatch(/rgba?\(/);
    expect(dense).toContain("var(--z-color-");
  });
});
