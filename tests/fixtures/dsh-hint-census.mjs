// Where a model request carries the hgs hint, for tests/test_dsh_hint_native.py.
// Pure: it imports no harness, so a plain node can check it.

export const text = message => (Array.isArray(message?.content) ? message.content : [])
  .filter(block => block?.type === 'text' && typeof block.text === 'string')
  .map(block => block.text).join('');

/**
 * Occurrences of `hint` in each system message of `messages`, in order, and in
 * every other message, and in the effective system prompt: the last nonempty
 * system message on an in-history route, else a leading system message.
 */
export function census(messages, hint, inHistory) {
  const count = value => (hint ? value.split(hint).length - 1 : 0);
  const system = [];
  let other = 0;
  for (const message of Array.isArray(messages) ? messages : []) {
    if (message?.role === 'system') system.push({ hints: count(text(message)), empty: text(message) === '' });
    else other += count(JSON.stringify(message ?? null));
  }
  const leading = messages?.[0]?.role === 'system';
  const effective = inHistory ? system.findLast(node => !node.empty) : leading ? system[0] : undefined;
  return { system: system.map(node => node.hints), other, leading, effective: effective?.hints ?? 0 };
}
