/** Project the verified legacy request into the shared text-result gate. */
import { createHash } from 'node:crypto';
import { createToolGate } from './claude-final-gate.mjs';

export function createQoderGate(records, invoke) {
  const gate = createToolGate(records, invoke);
  let contextDigests;
  return async (body, signal) => {
    if (Buffer.byteLength(body) > 16 * 1024 * 1024) throw Error('AW Qoder request exceeds limit');
    const request = JSON.parse(body);
    if (typeof request.session_id !== 'string' || !request.session_id ||
        !Array.isArray(request.messages) || request.session_type !== 'qodercli' ||
        request.agent_id !== 'agent_common' || request.chat_task !== 'FREE_INPUT') {
      throw Error('AW Qoder final gate requires a foreground legacy request');
    }
    // Reject notification/compaction turns: only this Query's original context
    // can accompany final results on the verified foreground path.
    const prefix = request.messages.filter(message => ['system', 'user'].includes(message.role));
    if (prefix.length !== 2 || prefix[0].role !== 'system' || prefix[1].role !== 'user' ||
        request.messages[0] !== prefix[0] || request.messages[1] !== prefix[1]) {
      throw Error('AW Qoder final gate does not cover additional context turns');
    }
    const stable = { session: request.session_id, system: request.system,
      systemMessage: prefix[0], userMessage: prefix[1], ...Object.fromEntries(
        Object.entries(request.chat_context ?? {}).map(([key, value]) => [`context.${key}`, value])) };
    const digests = Object.fromEntries(Object.entries(stable).map(([key, value]) =>
      [key, createHash('sha256').update(JSON.stringify(value ?? null, (key, item) => {
        if (key !== 'cache_control') return item;
        if (item?.type !== 'ephemeral' || Object.keys(item).some(name => !['type', 'ttl'].includes(name)) ||
            (item.ttl !== undefined && !['5m', '1h'].includes(item.ttl))) {
          throw Error('AW Qoder unsupported context cache annotation');
        }
        return undefined;
      })).digest('hex')]));
    if (contextDigests) {
      const changed = [...new Set([...Object.keys(digests), ...Object.keys(contextDigests)])]
        .filter(key => digests[key] !== contextDigests[key]);
      if (changed.length) throw Error(`AW Qoder foreground context changed: ${changed.join(',')}`);
    }
    contextDigests ??= digests;
    const messages = [], results = [];
    for (const message of request.messages) {
      if (!['system', 'user', 'assistant', 'tool'].includes(message.role)) {
        throw Error('AW Qoder final gate found an unsupported message role');
      }
      if (message.role === 'assistant' && message.tool_calls) {
        if (!Array.isArray(message.tool_calls)) throw Error('AW invalid Qoder calls');
        messages.push({ role: 'assistant', content: message.tool_calls.map(call => {
          if (call.type !== 'function' || call.function?.name !== 'Bash') throw Error('AW unsupported Qoder tool');
          return { type: 'tool_use', id: call.id, name: call.function.name,
            input: JSON.parse(call.function.arguments) };
        }) });
      }
      if (message.role === 'tool') {
        if (Object.keys(message).some(key => !['role', 'content', 'tool_call_id', 'is_error'].includes(key)) ||
            typeof message.content !== 'string' ||
            (message.is_error !== undefined && typeof message.is_error !== 'boolean')) throw Error('AW unsupported Qoder result envelope');
        const native = records()[message.tool_call_id];
        if (native && (message.is_error === true) !== native.is_error) {
          throw Error('AW Qoder result error state differs from native Post');
        }
        const block = { type: 'tool_result', tool_use_id: message.tool_call_id,
          content: message.content, is_error: message.is_error === true };
        results.push({ message, index: messages.length });
        messages.push({ role: 'user', content: [block] });
      }
    }
    const checked = JSON.parse(await gate(JSON.stringify({
      metadata: { user_id: JSON.stringify({ session_id: request.session_id }) }, messages,
    }), signal));
    if (signal?.aborted) throw signal.reason;
    for (const { message, index } of results) message.content = checked.messages[index].content[0].content;
    return JSON.stringify(request);
  };
}
