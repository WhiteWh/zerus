// Deterministic, keyless model for tests/test_dsh_hint_native.py. It answers
// every request with fixed text and logs one census line per request. This
// fixture is never loaded by the installed HGS host.
import { appendFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';
import { census, text } from './dsh-hint-census.mjs';
const require = createRequire(process.env.HGS_DSH_TEST_PACKAGE + '/package.json');
const { LlmAdapter } = await import(pathToFileURL(require.resolve('@deepseek-ai/dsh-llm')));
class Adapter extends LlmAdapter {
  constructor(config) { super(); this.config = config; }
  providerInfo(provider) { return { id: provider, name: 'HGS hint model' }; }
  async listModels(provider) { return [{ provider, id: 'hint-a', name: 'Hint A' }]; }
  // DeepSeek's own route declares an in-history system prompt; the test runs both kinds.
  async resolveModel(provider, model) {
    return { provider, id: model, name: model, ...(this.config.inHistory ? { systemPromptUpdate: 'in-history' } : {}) };
  }
  async *stream(options) {
    const { hint, inHistory, log } = this.config;
    const entry = census(options.messages, hint, inHistory);
    // The typed prompt names the turn; a hand-built side request fills the system slot.
    entry.prompt = text(options.messages?.filter(m => m.role === 'user' && m.source?.kind === 'user').at(-1));
    entry.slot = typeof options.system === 'string' ? options.system.split(hint).length - 1 : null;
    appendFileSync(log, JSON.stringify(entry) + '\n');
    const reply = 'Hint fixture reply';
    yield { type: 'block-start', index: 0, blockType: 'text' };
    yield { type: 'text-delta', index: 0, text: reply };
    yield { type: 'block-end', index: 0, block: { type: 'text', text: reply } };
    yield { type: 'usage', usage: { inputTokens: 1, outputTokens: 1 } };
    yield { type: 'finish', reason: { kind: 'stop' } };
  }
}
export const inject = ['llm'];
export function apply(ctx, config) {
  ctx.llm.registerAdapter(['hgs-hint'], new Adapter(config));
}
