import assert from 'node:assert/strict';
import test from 'node:test';
import { createToolGate, WITHHELD } from './final-gate.mjs';

const native = { session_id: 'session', tool_use_id: 'call', tool_name: 'Bash',
  tool_input: { command: 'test' }, cwd: '/work' };
function request(content = 'safe') {
  return { metadata: { user_id: JSON.stringify({ session_id: 'session' }) }, messages: [
    { role: 'assistant', content: [{ type: 'tool_use', id: 'call', name: 'Bash', input: native.tool_input }] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'call', content }] },
  ] };
}
const result = body => JSON.parse(body).messages[1].content[0].content;

test('native identity and actual final bytes reach policy; denied and failed results are withheld', async () => {
  for (const outcome of [true, false, 'error']) {
    let received;
    const gate = createToolGate(() => ({ call: native }), async payload => {
      received = payload;
      if (outcome === 'error') throw Error('timeout');
      return outcome;
    });
    assert.equal(result(await gate(JSON.stringify(request('late-secret')))), outcome === true ? 'late-secret' : WITHHELD);
    assert.equal(received.tool_response.content[0].text, 'late-secret');
    assert.equal(received.tool_use_id, 'call');
  }
});

test('unknown, mismatched, duplicate and non-text identities fail closed', async () => {
  for (const record of [undefined, { ...native, session_id: 'other' },
    { ...native, tool_name: 'Read' }, { ...native, ambiguous: true }]) {
    const gate = createToolGate(() => ({ call: record }), () => assert.fail('unbound policy call'));
    assert.equal(result(await gate(JSON.stringify(request()))), WITHHELD);
  }
  const gate = createToolGate(() => ({ call: native }), () => assert.fail('non-text policy call'));
  assert.equal(result(await gate(JSON.stringify(request([{ type: 'image', source: 'secret' }])))), WITHHELD);
  const duplicate = request();
  duplicate.messages[1].content.push(duplicate.messages[1].content[0]);
  await assert.rejects(gate(JSON.stringify(duplicate)), /ambiguous/);
  for (const extra of [{ secret: 'late' }, { cache_control: { type: 'ephemeral', ttl: 'secret' } }]) {
    const unknown = request();
    Object.assign(unknown.messages[1].content[0], extra);
    await assert.rejects(gate(JSON.stringify(unknown)), /unsupported result envelope/);
  }
});

test('receipts bind bytes and tool identity across history and retries', async () => {
  let checks = 0;
  const gate = createToolGate(() => ({ call: native }), async () => { checks++; return true; });
  const safe = JSON.stringify(request());
  assert.equal(result(await gate(safe)), 'safe');
  assert.equal(result(await gate(safe)), 'safe');
  assert.equal(result(await gate(JSON.stringify(request('secret')))), WITHHELD);
  const renamed = request();
  renamed.messages[0].content[0].name = 'Read';
  assert.equal(result(await gate(JSON.stringify(renamed))), WITHHELD);
  assert.equal(checks, 1);
  for (const content of ['x'.repeat(1024 * 1024), [{ type: 'text', text: 'structured' }]]) {
    for (const allow of [true, false]) {
      let calls = 0;
      const replay = createToolGate(() => ({ call: native }), async () => { calls++; return allow; });
      const source = JSON.stringify(request(content));
      const delivered = await replay(source);
      assert.deepEqual(result(delivered), allow ? content : WITHHELD);
      assert.deepEqual(result(await replay(source)), allow ? content : WITHHELD);
      assert.deepEqual(result(await replay(delivered)), allow ? content : WITHHELD);
      assert.equal(calls, 1);
    }
  }
});

test('identity changes during asynchronous policy checking invalidate delivery', async () => {
  const records = { call: native };
  const gate = createToolGate(() => structuredClone(records), async () => {
    records.call = { ...native, ambiguous: true };
    return true;
  });
  assert.equal(result(await gate(JSON.stringify(request()))), WITHHELD);
  assert.equal(result(await gate(JSON.stringify(request()))), WITHHELD);
});
