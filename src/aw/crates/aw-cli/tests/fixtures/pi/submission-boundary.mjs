/** Final SDK projection guards cover mutation and alternate native entrypoints. */
import test from 'node:test';
import assert from 'node:assert/strict';
import { inputBoundary } from '../../../../aw-adapter/native/pi/submission.mjs';

function fixture() {
  const calls = [];
  const session = { agent: {
    prompt: async messages => { calls.push(messages); },
    continue: async () => { calls.push('continue'); },
    hasQueuedMessages: () => false,
  }, hasExtensionHandlers: () => false, isStreaming: false };
  return { session, calls, boundary: inputBoundary(session) };
}
const message = text => [{ role: 'user', content: [{ type: 'text', text }] }];

test('raw text and native continuations retain a single submission authorization', async () => {
  const { session, calls, boundary } = fixture();
  for (const text of ['中文', '', '  ']) {
    boundary.validate(text, { images: [], source: 'interactive', expandPromptTemplates: false });
    await boundary.run(text, async () => {
      await session.agent.prompt(message(text));
      await session.agent.continue();
      await assert.rejects(session.agent.prompt(message(text)), /AW/);
    });
  }
  assert.equal(calls.length, 6);
  assert.throws(() => session.agent.continue(), /AW/);
});

test('transformed or additional user content never enters the native Agent', async () => {
  for (const messages of [message('changed'), [...message('original'), ...message('extra')],
    [{ role: 'user', content: [{ type: 'text', text: 'original' }, { type: 'image', data: 'x' }] }]]) {
    const { session, calls, boundary } = fixture();
    await boundary.run('original', async () => {
      await assert.rejects(session.agent.prompt(messages), /AW/);
    });
    assert.deepEqual(calls, []);
  }
});

test('input and post-check transformation handlers are rejected before a scan starts', () => {
  const { session, boundary } = fixture();
  for (const kind of ['input', 'context', 'before_provider_request']) {
    session.hasExtensionHandlers = event => event === kind;
    assert.throws(() => boundary.validate('text'), new RegExp(`${kind} extension handlers`));
  }
  session.hasExtensionHandlers = () => false;
  session.agent.hasQueuedMessages = () => true;
  assert.throws(() => boundary.validate('text'), /AW/);
});

test('managed input methods cannot be reassigned or redefined', () => {
  const { session } = fixture();
  for (const [object, key] of [[session, 'steer'], [session, 'sendUserMessage'],
    [session.agent, 'prompt'], [session.agent, 'continue'], [session.agent, 'followUp'],
    ...['transformContext', 'convertToLlm', 'onPayload'].map(key => [session.agent, key])]) {
    assert.throws(() => { object[key] = () => {}; }, TypeError);
    assert.throws(() => Object.defineProperty(object, key, { value: () => {} }), TypeError);
  }
});

test('direct post-check callbacks cannot replace, mutate or append user content', async () => {
  for (const key of ['transformContext', 'convertToLlm']) {
    for (const change of ['replace', 'mutate', 'append']) {
      const input = message('original');
      const agent = { prompt: async () => {}, continue: async () => {},
        [key]: async messages => {
          if (change === 'replace') return message('secret');
          if (change === 'mutate') messages[0].content[0].text = 'secret';
          else messages.push(...message('secret'));
          return messages;
        } };
      const boundary = inputBoundary({ agent });
      await assert.rejects(boundary.run('original', async () => {
        await agent.prompt(input);
        // Pi can convert callback errors into assistant error messages.
        await agent[key](input).catch(() => {});
      }), /AW Pi input.submit.*user content/);
      assert.deepEqual(input, message('original'));
    }
  }
});

test('serialized payload mutation is rejected and unchanged callbacks retain their result', async () => {
  for (const mutate of [true, false]) {
    const payload = { input: [{ role: 'user', content: 'original' }] };
    let retained;
    const agent = { prompt: async () => {}, continue: async () => {},
      transformContext: async messages => messages,
      convertToLlm: async messages => messages,
      onPayload: async function (value) {
        assert.equal(this.marker, 'native receiver');
        retained = value;
        if (mutate) value.input[0].content = 'secret';
      } };
    const boundary = inputBoundary({ agent });
    const run = () => boundary.run('original', async () => {
      await agent.prompt(message('original'));
      assert.deepEqual(await agent.convertToLlm(await agent.transformContext(message('original'))), message('original'));
      const result = await agent.onPayload.call({ marker: 'native receiver' }, payload);
      retained.input[0].content = 'late mutation';
      assert.deepEqual(result, payload);
    });
    if (mutate) await assert.rejects(run(), /AW Pi input.submit.*payload/);
    else await run();
    assert.equal(payload.input[0].content, 'original');
  }
});

test('model transport rechecks authorized user content and the guarded payload callback', async () => {
  for (const changed of ['text', 'payload-callback', 'none']) {
    const { session, boundary } = fixture();
    const run = () => boundary.run('original', async () => {
      await session.agent.prompt(message('original'));
      const context = { messages: message(changed === 'text' ? 'secret' : 'original') };
      const options = { onPayload: changed === 'payload-callback' ? () => ({}) : session.agent.onPayload };
      const guarded = boundary.model(context, options);
      context.messages[0].content[0].text = 'late mutation';
      assert.equal(guarded.messages[0].content[0].text, 'original');
    });
    if (changed === 'none') await run();
    else await assert.rejects(run(), /AW Pi input.submit/);
  }
});
