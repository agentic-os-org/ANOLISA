/** Shared scenario assertions require rejected input to make zero model requests. */
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

const root = process.env.FAKE_ROOT;
export const evidence = new WeakMap();
export async function createSession({ sdk, cwd, extensionEvent = null }) {
  const session = new sdk.AgentSession(cwd);
  session.extensionEvent = extensionEvent;
  evidence.set(session, []);
  session.agent.streamFunction = async (_model, context) => {
    evidence.get(session).push(context.messages);
    // An accidental model dispatch must fail independently of the outer rejection assertion.
    assert(!context.messages.some(message => message.content.some(block =>
      ['risk', 'api_key=synthetic-secret', 'cancel'].includes(block.text))), 'REJECTED INPUT REACHED MODEL');
  };
  return { session };
}
export async function runWorker({ createSession }) {
  const scenario = process.env.PI_SUBMISSION_CASE ?? 'normal';
  const rejectedExtensions = [];
  const rejectedTransforms = [];
  const first = await createSession({ cwd: root, directTransform: scenario === 'transforms' ? 'identity' : null });
  const { session } = first;
  const calls = () => evidence.get(session).length;
  const reject = async (body, match = /AW|blocked|active prompt/) => {
    const count = calls();
    await assert.rejects(body, match);
    assert.equal(calls(), count, 'rejected input reached model');
  };
  try {
    if (scenario === 'capacity') {
      for (let index = 0; index < 1024; index++) await reject(() => session.prompt('risk'));
      await session.prompt('after capacity boundary');
      assert.equal(calls(), 1);
    } else if (scenario === 'normal') {
      for (const text of ['plain text', '中文输入', '  ', '']) {
        const count = calls();
        await session.prompt(text);
        assert.equal(calls(), count + 1);
        assert.equal(evidence.get(session).at(-1).at(-1).content[0].text, text);
      }
      for (const text of ['risk', 'api_key=synthetic-secret']) {
        let preflight;
        await reject(() => session.prompt(text, { preflightResult: result => { preflight = result; } }));
        assert.equal(preflight, false);
        await session.prompt('recovered');
      }
      await session.prompt('/literal', { expandPromptTemplates: false });
      let accepted;
      await session.prompt('native options', { streamingBehavior: 'steer', images: [],
        source: 'interactive', preflightResult: value => { accepted = value; } });
      assert.equal(accepted, true);
      for (const [text, options] of [['x', { images: [{ type: 'image', data: 'x' }] }],
        ['/template', {}], ['x', { source: 'extension' }], ['x', { streamingBehavior: 'invalid' }]])
        await reject(() => session.prompt(text, options));
      for (const method of ['steer', 'followUp', 'sendUserMessage', 'sendCustomMessage'])
        assert.throws(() => session[method]('risk'), /AW/);
      assert.throws(() => session.agent.steer({ role: 'user', content: 'risk' }), /AW/);
      assert.throws(() => session.agent.followUp({ role: 'user', content: 'risk' }), /AW/);
      await reject(() => session.agent.prompt([{ role: 'user', content: [{ type: 'text', text: 'risk' }] }]));
      assert.throws(() => session.agent.continue(), /AW/);
      for (const extensionEvent of ['input', 'context', 'before_provider_request']) {
        const extension = await createSession({ cwd: root, extensionEvent });
        try {
          await assert.rejects(extension.session.prompt('plain text'),
            new RegExp(`${extensionEvent} extension handlers`));
          assert.equal(evidence.get(extension.session).length, 0);
          assert(!existsSync(join(root, 'extension-executed')), 'unsupported extension executed');
          rejectedExtensions.push(extensionEvent);
        } finally { await extension.close(); }
      }
      const second = await createSession({ cwd: root });
      try { await Promise.all([session.prompt('first session'), second.session.prompt('second session')]); }
      finally { await second.close(); }
    } else if (scenario === 'transforms') {
      for (const directTransform of ['transformContext', 'convertToLlm', 'onPayload']) {
        const transformed = await createSession({ cwd: root, directTransform });
        try {
          await assert.rejects(transformed.session.prompt('plain text'),
            /AW Pi input.submit forbids changing/);
          assert.equal(evidence.get(transformed.session).length, directTransform === 'onPayload' ? 1 : 0);
          rejectedTransforms.push(directTransform);
        } finally { await transformed.close(); }
      }
      await session.prompt('plain text');
      assert.equal(calls(), 1, 'unchanged direct callback did not reach the model');
    } else if (scenario === 'tools-report') {
      await session.prompt('tool round');
      assert.equal(calls(), 2);
      assert(evidence.get(session).at(-1).some(message => message.role === 'toolResult' &&
        message.content[0].text === 'native tool output'));
      await reject(() => session.prompt('risk'));
      await session.prompt('another tool round');
      assert.equal(calls(), 4);
    } else if (['error', 'invalid', 'timeout'].includes(scenario)) {
      if (process.env.PI_SUBMISSION_REPORT === '1') await session.prompt(scenario);
      else await reject(() => session.prompt(scenario));
      await session.prompt('recovered');
    } else {
      const pending = session.prompt('cancel');
      const rejected = assert.rejects(pending, /AW|closed|cancel/);
      for (let index = 0; index < 300 && !existsSync(join(root, 'scan-started')); index++) await delay(10);
      assert(existsSync(join(root, 'scan-started')), 'scan never started');
      await reject(() => session.prompt('concurrent'));
      await (scenario === 'dispose' ? session.dispose() : session.abort());
      await rejected;
      await delay(100);
      assert.equal(calls(), 0, 'late scanner reply dispatched model');
      await reject(() => session.prompt('closed'));
    }
    const responses = evidence.get(session).responses;
    if (responses) {
      assert.equal(responses.length, calls(), 'native model stream did not complete');
      assert(responses.every(message => ['stop', 'toolUse'].includes(message.stopReason)),
        JSON.stringify(responses));
    }
    const events = readFileSync(join(root, 'submissions.jsonl'), 'utf8').trim().split('\n').map(JSON.parse);
    assert(events.every(event => !event.tool && event.turn_id && event.session_id));
    // Both steps share an identity; distinct accepted submissions never reuse it.
    const groups = new Map();
    for (const event of events) {
      const key = event.session_id + '/' + event.turn_id;
      if (groups.has(key)) assert.deepEqual(groups.get(key), event);
      groups.set(key, event);
    }
    if (scenario === 'capacity') {
      assert.equal(groups.size, 1025);
    } else if (scenario === 'normal') {
      assert.equal(new Set(events.map(event => event.session_id)).size, 2);
      assert.equal(groups.size, 12);
    }
    console.log(JSON.stringify({ passed: true, scenario, model_calls: calls(), submissions: groups.size, rejected_extensions: rejectedExtensions, rejected_transforms: rejectedTransforms }));
  } finally { await first.close(); }
}
