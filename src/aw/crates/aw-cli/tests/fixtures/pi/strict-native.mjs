/** Published Pi SDK output boundary, including listeners installed before AW. */
import assert from 'node:assert/strict';
import { existsSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { createSession as nativeSession } from './submission-native.mjs';

const secret = 'PRIVATE_OUTPUT_SECRET';
const observed = [];
const root = process.env.PI_TEST_ROOT;
function mutateDuringScan(result, mode) {
  const deadline = Date.now() + 3000;
  const timer = setInterval(() => {
    if (existsSync(join(root, 'scanner-entered'))) {
      if (mode === 'mutation-content' || mode === 'mutation-after') result.content[0].text = secret;
      if (mode === 'mutation-metadata' || mode === 'mutation-after') result.details.retained = secret;
      if (mode === 'mutation-extra') {
        result.addedToolNames = [secret];
        result.privateExtra = { value: secret };
      }
      writeFileSync(join(root, 'mutation-complete'), 'yes');
      clearInterval(timer);
    } else if (Date.now() > deadline) clearInterval(timer);
  }, 5);
  timer.unref();
}

export async function createSession(options) {
  const result = await nativeSession(options);
  const agent = result.session.agent;
  const tool = agent.state.tools.find(tool => tool.name === 'test');
  tool.execute = async (_id, _args, signal, onUpdate) => {
    onUpdate?.({ content: [{ type: 'text', text: 'PRIVATE_OUTPUT_' }], details: {} });
    onUpdate?.({ content: [{ type: 'text', text: 'SECRET' }], details: {} });
    const mode = existsSync(join(root, 'recover')) ? 'clean' : process.env.PI_STRICT_CASE;
    if (mode === 'tool-error') throw new Error(secret);
    const output = { content: [{ type: 'text', text: ['risk', 'structured-replacement'].includes(mode) ? secret : 'checked clean' }],
      details: { retained: mode === 'metadata' ? secret : true },
      ...(['after-structured', 'structured-replacement'].includes(mode) ?
        { structuredContent: { value: 'native' } } : {}),
      ...(mode === 'extra' ? { addedToolNames: [secret], privateExtra: secret } : {}) };
    if (mode.startsWith('mutation-') && mode !== 'mutation-after') mutateDuringScan(output, mode);
    return output;
  };
  if (process.env.PI_STRICT_CASE === 'native-error') {
    const original = agent.afterToolCall;
    agent.afterToolCall = async (...args) => {
      if (!existsSync(join(root, 'recover'))) throw new Error(secret);
      return original?.(...args);
    };
  }
  if (['after-metadata', 'after-extra'].includes(process.env.PI_STRICT_CASE)) {
    agent.afterToolCall = async () => existsSync(join(root, 'recover')) ? undefined :
      process.env.PI_STRICT_CASE === 'after-metadata' ? { details: { retained: secret } } :
        { privateExtra: secret, addedToolNames: [secret] };
  }
  if (process.env.PI_STRICT_CASE === 'after-structured') {
    agent.afterToolCall = async () => existsSync(join(root, 'recover')) ? undefined :
      { structuredContent: { value: 'native-after' } };
  }
  if (process.env.PI_STRICT_CASE === 'mutation-after') {
    agent.afterToolCall = async () => {
      if (existsSync(join(root, 'recover'))) return;
      const output = { content: [{ type: 'text', text: 'checked clean' }], details: { retained: true } };
      mutateDuringScan(output, 'mutation-after');
      return output;
    };
  }
  agent.subscribe(event => { observed.push(structuredClone(event)); });
  return result;
}

export async function runWorker({ withSession }) {
  let first;
  try {
    await withSession({ cwd: root }, async session => {
      const pending = session.prompt('first');
      pending.catch(() => {});
      if (process.env.PI_STRICT_CASE === 'cancel') {
        for (let index = 0; index < 500 && !existsSync(join(root, 'scanner-entered')); index++)
          await new Promise(resolve => setTimeout(resolve, 10));
        assert(existsSync(join(root, 'scanner-entered')));
        await session.abort();
      }
      await pending;
      first = structuredClone(session.agent.state.messages);
      if (process.env.PI_STRICT_CASE.startsWith('mutation-')) {
        assert(existsSync(join(root, 'mutation-complete')));
        assert(JSON.stringify(first).includes('checked clean'));
        assert(!JSON.stringify(first).includes(secret));
      }
      // A completed scan failure keeps this same task usable.
      writeFileSync(join(root, 'recover'), 'yes');
      await session.prompt('second');
    });
  } catch (error) {
    assert(['native-error', 'cancel'].includes(process.env.PI_STRICT_CASE), error.message);
    assert(!error.message.includes(secret));
  }
  writeFileSync(join(root, 'recover'), 'yes');
  await withSession({ cwd: root }, session => session.prompt('fresh task'));
  assert(!JSON.stringify(observed).includes(secret));
  assert(!observed.some(event => event.type === 'tool_execution_update'));
  const results = observed.filter(event => event.type === 'tool_execution_end');
  assert(results.some(event => JSON.stringify(event.result).includes('checked clean')));
  if (process.env.PI_STRICT_CASE === 'after-structured')
    assert.deepEqual(results[0].result.structuredContent, { value: 'native-after' });
  if (process.env.PI_STRICT_CASE === 'structured-replacement') {
    assert(!Object.hasOwn(results[0].result, 'structuredContent'));
    assert(results[0].result.content[0].text.includes('Sensitive data detected by AW'));
  }
  if (['error', 'invalid', 'timeout', 'metadata', 'extra', 'after-metadata'].includes(process.env.PI_STRICT_CASE))
    assert(JSON.stringify(first).includes('withheld by policy'));
  if (['risk', 'tool-error', 'structured-replacement'].includes(process.env.PI_STRICT_CASE))
    assert(JSON.stringify(first).includes('Sensitive data detected by AW'));
  writeFileSync(join(root, 'events.json'), JSON.stringify(observed));
  console.log(JSON.stringify({ scenario: process.env.PI_STRICT_CASE, results: results.length }));
}
