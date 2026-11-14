/** Map final Anthropic tool blocks to native Claude callback identities. */
import { createHash } from 'node:crypto';
import { isDeepStrictEqual } from 'node:util';

export const WITHHELD = 'AW withheld the final tool result';
const digest = value => createHash('sha256').update(JSON.stringify(value ?? null)).digest('hex');
const cacheControl = value => value === undefined || (value?.type === 'ephemeral' &&
  Object.keys(value).every(key => ['type', 'ttl'].includes(key)) &&
  (value.ttl === undefined || ['5m', '1h'].includes(value.ttl)));

export function createToolGate(records, invoke) {
  const receipts = new Map();
  return async (body, signal) => {
    if (Buffer.byteLength(body) > 16 * 1024 * 1024) throw Error('AW model request exceeds limit');
    const request = JSON.parse(body);
    const session = JSON.parse(request.metadata?.user_id ?? '{}').session_id;
    if (typeof session !== 'string' || !session || !Array.isArray(request.messages)) {
      throw Error('AW final gate requires native session and messages');
    }
    const identities = records();
    const calls = new Map();
    const seen = new Set();
    const checked = [];
    for (const message of request.messages) {
      if (!Array.isArray(message.content)) continue;
      for (const block of message.content) {
        if (block.type === 'tool_use') {
          if (message.role !== 'assistant' || calls.has(block.id)) {
            throw Error('AW final gate found an ambiguous tool call');
          }
          calls.set(block.id, block);
        }
        if (block.type !== 'tool_result') continue;
        if (Object.keys(block).some(key => !['type', 'tool_use_id', 'content', 'is_error', 'cache_control'].includes(key)) ||
            (block.is_error !== undefined && typeof block.is_error !== 'boolean') || !cacheControl(block.cache_control)) {
          throw Error('AW final gate found an unsupported result envelope');
        }
        const id = block.tool_use_id;
        if (message.role !== 'user' || typeof id !== 'string' || !id || seen.has(id)) {
          throw Error('AW final gate found an ambiguous tool result');
        }
        seen.add(id);
        const call = calls.get(id);
        const native = identities[id];
        const nativeDigest = digest(native);
        checked.push({ block, id, nativeDigest });
        const key = JSON.stringify([session, id]);
        const hash = digest(block);
        const previous = receipts.get(key);
        if (previous) {
          if (previous.call !== digest(call) || previous.native !== nativeDigest) block.content = WITHHELD;
          else if (previous.source === hash) { if (previous.withheld) block.content = WITHHELD; }
          else if (previous.delivered !== hash) block.content = WITHHELD;
          continue;
        }
        const text = typeof block.content === 'string' || (Array.isArray(block.content) &&
          block.content.every(item => item?.type === 'text' && typeof item.text === 'string' &&
            Object.keys(item).every(key => ['type', 'text', 'cache_control'].includes(key)) && cacheControl(item.cache_control)));
        let allowed = text && native && call && native.session_id === session &&
          native.tool_name === call.name && isDeepStrictEqual(native.tool_input, call.input) &&
          !native.ambiguous && receipts.size < 1024;
        if (allowed) {
          const payload = { ...native, hook_event_name: 'AWFinalToolResult',
            tool_response: { content: typeof block.content === 'string'
              ? [{ type: 'text', text: block.content }] : block.content,
            isError: block.is_error === true } };
          try { allowed = await invoke(payload, signal); }
          catch { allowed = false; }
        }
        if (!allowed) block.content = WITHHELD;
        if (receipts.size < 1024) receipts.set(key, {
          call: digest(call), native: nativeDigest, source: hash,
          delivered: digest(block), withheld: !allowed,
        });
      }
    }
    const current = records();
    for (const { block, id, nativeDigest } of checked) {
      if (digest(current[id]) !== nativeDigest) block.content = WITHHELD;
    }
    return JSON.stringify(request);
  };
}
