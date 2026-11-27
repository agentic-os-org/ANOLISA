const assert = require('node:assert/strict');
const test = require('node:test');

const { copyText } = require(process.env.AGENTSIGHT_CLIPBOARD_BUILD);

function browser(t, { secure = true, writeText, execCommand = () => true } = {}) {
  const descriptors = new Map(
    ['navigator', 'window', 'document'].map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)]),
  );
  const children = [];
  const copies = [];
  const restorations = [];
  const initialFocus = {
    isConnected: true,
    focus(options) {
      restorations.push(options);
      document.activeElement = this;
    },
  };
  let selected = false;
  const document = {
    activeElement: initialFocus,
    createElement(tag) {
      assert.equal(tag, 'textarea');
      return {
        value: '',
        style: {},
        focus() { document.activeElement = this; },
        select() { selected = true; },
        remove() { document.body.removeChild(this); },
      };
    },
    body: {
      appendChild(node) { children.push(node); },
      removeChild(node) {
        children.splice(children.indexOf(node), 1);
        if (document.activeElement === node) document.activeElement = this;
      },
    },
    execCommand(command) {
      assert.equal(command, 'copy');
      assert.ok(selected);
      assert.equal(children.length, 1);
      assert.equal(document.activeElement, children[0]);
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
  return { children, copies, document, initialFocus, restorations };
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

test('the fallback suppresses done when execCommand returns false', (t) => {
  const state = browser(t, { execCommand: () => false });
  let completions = 0;
  copyText('session-1', () => { completions += 1; });
  assert.deepEqual(state.copies, ['session-1']);
  assert.equal(completions, 0);
  assert.deepEqual(state.children, []);
});

test('the fallback cleans up and suppresses done when execCommand throws', (t) => {
  const state = browser(t, { execCommand: () => { throw new Error('copy unsupported'); } });
  let completions = 0;
  copyText('session-1', () => { completions += 1; });
  assert.deepEqual(state.copies, ['session-1']);
  assert.equal(completions, 0);
  assert.deepEqual(state.children, []);
});

test('a rejected clipboard write whose fallback fails or throws does not invoke done', async (t) => {
  for (const execCommand of [
    () => false,
    () => { throw new Error('copy unsupported'); },
  ]) {
    const state = browser(t, {
      writeText: () => Promise.reject(new Error('permission denied')),
      execCommand,
    });
    let completions = 0;
    copyText('optimization details', () => { completions += 1; });
    await new Promise(setImmediate);
    assert.deepEqual(state.copies, ['optimization details']);
    assert.equal(completions, 0);
    assert.deepEqual(state.children, []);
  }
});

for (const [outcome, execCommand, expectedFeedback] of [
  ['succeeds', () => true, 1],
  ['returns false', () => false, 0],
  ['throws', () => { throw new Error('copy unsupported'); }, 0],
]) {
  test(`restores focus without scrolling when fallback ${outcome}`, (t) => {
    const state = browser(t, { execCommand });
    let completions = 0;
    copyText('keyboard copy', () => { completions += 1; });
    assert.equal(state.document.activeElement, state.initialFocus);
    assert.deepEqual(state.restorations, [{ preventScroll: true }]);
    assert.equal(completions, expectedFeedback);
    assert.deepEqual(state.children, []);
  });
}

test('restores the control focused when the asynchronous fallback starts', async (t) => {
  let rejectWrite;
  const state = browser(t, {
    writeText: () => new Promise((_, reject) => { rejectWrite = reject; }),
  });
  const laterFocus = {
    isConnected: true,
    focus() { state.document.activeElement = this; },
  };
  copyText('delayed copy', () => {});
  laterFocus.focus();
  rejectWrite(new Error('permission denied'));
  await new Promise(setImmediate);
  assert.equal(state.document.activeElement, laterFocus);
  assert.deepEqual(state.restorations, []);
});

test('leaves focus alone when a copy handler moves it elsewhere', (t) => {
  const other = { isConnected: true };
  const state = browser(t, {
    execCommand() {
      state.document.activeElement = other;
      return true;
    },
  });
  copyText('keyboard copy', () => {});
  assert.equal(state.document.activeElement, other);
  assert.deepEqual(state.restorations, []);
  assert.deepEqual(state.children, []);
});

test('does not focus a control that was removed during copying', (t) => {
  const state = browser(t, {
    execCommand() {
      state.initialFocus.isConnected = false;
      return true;
    },
  });
  copyText('keyboard copy', () => {});
  assert.equal(state.document.activeElement, state.document.body);
  assert.deepEqual(state.restorations, []);
  assert.deepEqual(state.children, []);
});
