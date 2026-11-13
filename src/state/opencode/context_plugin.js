// OpenCode V2 Promise plugin. Only the service launch overlay loads this file;
// nothing is installed into a person's OpenCode configuration or workspace.
import { execFile } from "node:child_process";

export default {
  id: "zerus.agent-hint.context",
  setup: async (ctx) => {
    await ctx.session.hook("context", async (event) => {
      try {
        const output = await new Promise((resolve, reject) => {
          execFile(ctx.options.hgs, ["--opencode-agent-hint", ctx.options.nonce,
            String(process.pid), ctx.location.directory, event.sessionID], {
            timeout: 750, maxBuffer: 4096, windowsHide: true,
            env: { PATH: process.env.PATH, HOME: process.env.HOME,
              XDG_CONFIG_HOME: process.env.XDG_CONFIG_HOME,
              XDG_STATE_HOME: process.env.XDG_STATE_HOME,
              OPENCODE_CONFIG_DIR: process.env.OPENCODE_CONFIG_DIR,
              HGS_STATE_DIR: ctx.options.state },
          }, (error, stdout) => error ? reject(error) : resolve(stdout));
        });
        const { context } = JSON.parse(output);
        if (typeof context !== "string" || context.length === 0 || context.length > 256) return;
        // Preserve retained entry-backed instructions, including an unmarked
        // native entry; never append a second effective copy or edit messages.
        if (JSON.stringify([event.system, event.messages]).includes(context)) return;
        event.system.push({ type: "text", text: context });
      } catch {
        // Optional context must never fail an agent turn or echo lookup errors.
      }
    });
  },
};
