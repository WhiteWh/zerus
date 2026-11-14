import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { createServer, type ViteDevServer } from "vite";
import solid from "vite-plugin-solid";

// Task Settings > Model servers keeps the list at its 260 px and gives
// the editor the remaining width, as Qt's ModelServerSettings does; every
// field row spans the editor and the server root's long model list wraps in
// the form instead of running one character per line. The section is rendered
// as the client renders it, and the stylesheet rules that hold at 1570 px and
// at 900 px are read from disk (no layout engine in the node test run).
const entry = "\0model-servers-layout";
const translations = "\0model-servers-i18n";
const store = "\0model-servers-store";
const machines = "\0model-servers-machines";
const confirm = "\0model-servers-confirm";
let server: ViteDevServer | undefined;
let render: () => string;

const servers = [{
  name: "gb10-claude", base_url: "http://192.168.1.79:4000", kind: "openai", ssh: "", read_only: false, has_key: false,
  logs: [], down: [], status_text: "", up_text: "", down_text: "", logs_text: "",
}];

beforeAll(async () => {
  const { fileURLToPath } = await import(/* @vite-ignore */ ["node", "url"].join(":")) as { fileURLToPath(url: URL): string };
  server = await createServer({
    configFile: false, root: fileURLToPath(new URL("../../../../", import.meta.url)),
    server: { middlewareMode: true, watch: null }, logLevel: "silent",
    plugins: [{
      name: "model-servers-layout-mocks", enforce: "pre",
      resolveId(id) {
        if ([entry, translations, store, machines, confirm].includes(id)) return id;
        if (/(^|\/)i18n$/.test(id)) return translations;
        if (id.endsWith("/store/modelServers")) return store;
        if (id.endsWith("/store/machines")) return machines;
        if (id.endsWith("/store/teamConfirm")) return confirm;
        return undefined;
      },
      load(id) {
        if (id === translations) return `export const trNoop = (s) => s;
          export const tr = (s, ...args) => s.replace(/%(\\d+)/g, (_, n) => String(args[n - 1]));`;
        if (id === machines) return `export const machineCards = {};`;
        if (id === confirm) return `export const teamConfirm = { ask: async () => true };`;
        if (id === store) return `const servers = ${JSON.stringify(servers)};
          export const modelServers = {
            servers: () => servers,
            server: (name) => servers.find((s) => s.name === name),
            entry: () => ({ known: true, reading: false, readAt: 0, status: { models: [], error: "", running: 0 } }),
            refreshList: async () => {},
            setActive: () => {},
            test: async () => ({ tone: "", text: { source: "" } }),
            testLogs: async () => ({ tone: "", text: { source: "" } }),
            save: async () => {}, remove: async () => {}, refreshStatus: () => {},
            logs: async () => ({ lines: [], truncated: false }), sessions: async () => ({}),
            down: async () => {}, startSession: async () => {}, upRunning: () => false, up: async () => {},
          };
          export const words = (error) => String(error && error.message ? error.message : error);`;
        if (id === entry) return `import {createComponent} from 'solid-js';
          import {renderToString} from 'solid-js/web';
          import {ModelServersSection} from '/src/ui/screens/settings/ModelServersSection.tsx';
          export const render = () => renderToString(() => createComponent(ModelServersSection, {}));`;
        return undefined;
      },
    }, solid({ ssr: true })],
  });
  const module = await server.ssrLoadModule(entry);
  render = module.render;
});

afterAll(async () => { await server?.close(); });

const fs = (await import(/* @vite-ignore */ ["node", "fs"].join(":"))) as { readFileSync(path: URL, encoding: "utf8"): string };
const repo = new URL("../../../../../", import.meta.url);
const css = fs.readFileSync(new URL("client/src/ui/styles/model-servers.css", repo), "utf8");
const rule = (selector: string) => css.split("\n").find((line) => {
  const text = line.trim();
  return text.startsWith(`${selector} {`) || text.startsWith(`${selector},`);
}) ?? "";

// Solid's SSR output marks reactive regions and hydration keys; the field
// component wraps its input, so the markup is read without those markers.
const plain = (html: string) => html.replace(/<!--[^>]*-->/g, "").replace(/ data-hk="[^"]*"/g, "");

describe("Settings > Model servers' layout (task #350)", () => {
  it("keeps the long model list in a wrapping note of the editor, not a squeezed cell", () => {
    const html = render();
    // A selected server fills the form at once: the model list is the long
    // three-line note the owner saw one character per line.
    expect(html).toContain("Model list: http://192.168.1.79:4000/v1/models");
    expect(html).toContain("Codex: http://192.168.1.79:4000/v1");
    expect(html).toContain("Claude Code: http://192.168.1.79:4000");
    const note = html.match(/<p class="ms-note">([^<]*)<\/p>/)?.[1] ?? "";
    expect(note).toContain("Claude Code:");
    // The note is the editor's own block, as wide as the form.
    expect(html).toMatch(/<p class="ms-note">Model list:/);
  });

  it("puts every labelled field in its own row with the control taking the editor's width", () => {
    const html = plain(render());
    // Name, Server root, API key, SSH host, Status, Up, Down and Logs.
    expect(html.match(/class="ms-field"/g)).toHaveLength(8);
    expect(html).toContain('<div class="ms-field"><label for="msf-name">Name</label>');
    expect(html).toContain('<div class="ms-control"><input id="msf-url"');
    expect(html).toContain('<div class="ms-field"><span>SSH host</span>');
    // No bare field or row is a direct child of the form (the grid that
    // shifted a column once a result placeholder was empty is gone).
    expect(html).not.toMatch(/<form class="ms-form"><label/);
    expect(html).not.toContain('<form class="ms-form"><div class="row"');
    expect(html).toContain('<div class="ms-buttons">');
  });

  it("gives the editor the remaining width at 1570 px and at 900 px", () => {
    // The settings column's 850 px cap is lifted for this section (1570 px
    // used less than half the window), and the list is a fixed grid column so
    // it can never shrink to one character per line.
    expect(rule(".set-content:has(> .ms-settings)")).toMatch(/max-width: none;/);
    expect(rule(".ms-settings-body")).toMatch(/display: grid;/);
    expect(rule(".ms-settings-body")).toMatch(/grid-template-columns: 260px minmax\(0, 1fr\);/);
    expect(rule(".ms-server-list")).not.toMatch(/(^|\s)width:/);
    // The editor takes its column and its controls fill or wrap inside it.
    expect(rule(".ms-form")).toMatch(/min-width: 0;/);
    expect(rule(".ms-form")).toMatch(/flex-direction: column;/);
    expect(rule(".ms-field")).toMatch(/grid-template-columns: 120px minmax\(0, 1fr\);/);
    expect(rule(".ms-field > label")).toMatch(/text-align: right;/);
    expect(rule(".ms-control")).toMatch(/flex-wrap: wrap;/);
    expect(rule(".ms-control > .field")).toMatch(/flex: 1 1 220px;/);
    expect(rule(".ms-control > .field")).toMatch(/min-width: 0;/);
    // Below 900 px the same column holds; a very narrow window stacks it.
    expect(css).toContain("@media (max-width: 720px)");
  });
});
