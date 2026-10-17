const assert = require('node:assert/strict');
const test = require('node:test');

const { copyText } = require(process.env.AGENTSIGHT_CLIPBOARD_BUILD);

function browser(t, { secure = true, writeText, execCommand = () => true } = {}) {
  const descriptors = new Map(
    ['navigator', 'window', 'document'].map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)]),
  );
  const children = [];
  const copies = [];
  let focused = false;
  let selected = false;
  const document = {
    createElement(tag) {
      assert.equal(tag, 'textarea');
      return {
        value: '',
        style: {},
        focus() { focused = true; },
        select() { selected = true; },
      };
    },
    body: {
      appendChild(node) { children.push(node); },
      removeChild(node) { children.splice(children.indexOf(node), 1); },
    },
    execCommand(command) {
      assert.equal(command, 'copy');
      assert.ok(focused && selected);
      assert.equal(children.length, 1);
      copies.push(children[0].value);
      return execCommand();
    },
  };
  const navigator = { clipboard: writeText ? { writeText } : undefined };
  for (const [key, value] of Object.entries({ navigator, window: { isSecureContext: secure }, document })) {
    Object.defineProperty(globalThis, key, { configurable: true, value });
  }
  t.after(() => {
    for (const [key, descriptor] of descriptors) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else delete globalThis[key];
    }
  });
  return { children, copies };
}

test('secure clipboard waits for completion before showing feedback', async (t) => {
  let resolveWrite;
  let written;
  const state = browser(t, {
    writeText(text) {
      written = text;
      return new Promise((resolve) => { resolveWrite = resolve; });
    },
  });
  let completions = 0;
  copyText('session-1', () => { completions += 1; });
  assert.equal(written, 'session-1');
  assert.equal(completions, 0);
  resolveWrite();
  await new Promise(setImmediate);
  assert.equal(completions, 1);
  assert.deepEqual(state.copies, []);
  assert.deepEqual(state.children, []);
});

test('an insecure origin uses the fallback even when clipboard is present', (t) => {
  const state = browser(t, {
    secure: false,
    writeText() { throw new Error('clipboard must not be used over HTTP'); },
  });
  let completions = 0;
  copyText('会话\nsecond line', () => { completions += 1; });
  assert.deepEqual(state.copies, ['会话\nsecond line']);
  assert.deepEqual(state.children, []);
  assert.equal(completions, 1);
});

test('a browser without the Clipboard API uses the fallback', (t) => {
  const state = browser(t);
  let completed = false;
  copyText('', () => { completed = true; });
  assert.deepEqual(state.copies, ['']);
  assert.deepEqual(state.children, []);
  assert.equal(completed, true);
});

test('a rejected clipboard write falls back once and removes its textarea', async (t) => {
  const state = browser(t, { writeText: () => Promise.reject(new Error('permission denied')) });
  let completions = 0;
  copyText('optimization details', () => { completions += 1; });
  await new Promise(setImmediate);
  assert.deepEqual(state.copies, ['optimization details']);
  assert.deepEqual(state.children, []);
  assert.equal(completions, 1);
});

test('the fallback retains legacy feedback when execCommand reports failure', (t) => {
  const state = browser(t, { execCommand: () => false });
  let completions = 0;
  copyText('session-1', () => { completions += 1; });
  assert.equal(completions, 1);
  assert.deepEqual(state.children, []);
});

test('the fallback retains legacy cleanup and feedback if execCommand throws', (t) => {
  const state = browser(t, { execCommand: () => { throw new Error('copy unsupported'); } });
  let completions = 0;
  copyText('session-1', () => { completions += 1; });
  assert.equal(completions, 1);
  assert.deepEqual(state.children, []);
});
