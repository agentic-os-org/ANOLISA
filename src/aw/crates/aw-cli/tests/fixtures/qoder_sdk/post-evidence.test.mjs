import assert from 'node:assert/strict';
import test from 'node:test';
import { failedDelivery, modelText } from './post-evidence.mjs';

test('failed result evidence requires model text and the final error envelope', () => {
  const sentinel = 'AW_NATIVE_POST_RESULT';
  const tool = { type: 'tool_use', input: { command: `printf ${sentinel}` } };
  const messages = [{ type: 'assistant', message: { content: [tool] } }];
  const delivered = { is_error: true, content: sentinel };
  assert.equal(modelText(messages), '');
  assert.throws(() => failedDelivery(delivered, messages, sentinel), /model text/);
  messages[0].message.content.push({ type: 'text', text: sentinel });
  failedDelivery(delivered, messages, sentinel);
  for (const value of [undefined, {}, { is_error: false }, { is_error: 'true' }]) {
    assert.throws(() => failedDelivery(value, messages, sentinel));
  }
  assert.throws(() => failedDelivery({ is_error: true, content: 'lost' }, messages, sentinel), /checked stderr/);
  messages[0].message.content.pop();
  messages.push({ type: 'result', result: sentinel });
  failedDelivery(delivered, messages, sentinel);
});
