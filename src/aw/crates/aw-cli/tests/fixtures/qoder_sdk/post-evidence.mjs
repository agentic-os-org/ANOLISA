/** Acceptance predicates shared by live runs and negative evidence regressions. */
import assert from 'node:assert/strict';

export function modelText(messages) {
  return messages.flatMap(message => message.type === 'assistant'
    ? (message.message?.content ?? []).filter(block => block.type === 'text').map(block => block.text)
    : message.type === 'result' && typeof message.result === 'string' ? [message.result] : []).join('\n');
}

export function failedDelivery(delivered, messages, sentinel) {
  assert(delivered, 'failed native result was not delivered');
  assert.equal(delivered.is_error, true, 'final delivery lost the error flag');
  assert(JSON.stringify(delivered.content).includes(sentinel), 'final delivery lost checked stderr');
  assert(modelText(messages).includes(sentinel), 'model text did not quote checked stderr');
}
