const assert = require('node:assert/strict');
const { join } = require('node:path');
const test = require('node:test');

// Behavioral regression for the ATIF viewer load-ownership fix (#5620):
// the transpiled production page is driven against the hooks driver from
// stale-load-deferred.test.cjs with deferred fetch responses AND deferred
// FileReader callbacks, so tests reproduce the exact interleavings:
//
//   - a network response resolving after a local import must not replace
//     the imported document (the import used to bypass loadRequestIdRef),
//   - two imports completing out of order must keep the newer file,
//   - an invalidated read must not surface an obsolete parse/read error or
//     finish another request's loading state,
//   - a FileReader failure must reach a terminal error callback that
//     releases the loading state of the current import.

const babel = require('@babel/core');

function transpile(relativePath) {
  const out = babel.transformFileSync(join(process.cwd(), relativePath), {
    presets: [
      ['@babel/preset-env', { targets: { node: 'current' } }],
      ['@babel/preset-typescript', { isTSX: true, allExtensions: true }],
      ['@babel/preset-react', { runtime: 'classic' }],
    ],
  });
  return out.code;
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

function createHooksDriver() {
  const slots = [];
  const driver = {
    slots,
    render(Component, props = {}) {
      driver._cursor = 0;
      driver._effects = [];
      driver._callbacks = [];
      const element = Component(props);
      return { element, effects: driver._effects, callbacks: driver._callbacks };
    },
    useState(initial) {
      const slot = slots[driver._cursor] ?? {
        value: typeof initial === 'function' ? initial() : initial,
      };
      slots[driver._cursor] = slot;
      if (!slot.setter) {
        slot.setter = (update) => {
          slot.value = typeof update === 'function'
            ? update(slot.value)
            : update;
        };
      }
      driver._cursor += 1;
      return [slot.value, slot.setter];
    },
    useRef(initial) {
      const slot = slots[driver._cursor] ?? { value: { current: initial } };
      slots[driver._cursor] = slot;
      driver._cursor += 1;
      return slot.value;
    },
    useEffect(fn) {
      driver._effects.push(fn);
    },
    useCallback(fn) {
      driver._callbacks.push(fn);
      return fn;
    },
    useMemo(factory) {
      return factory();
    },
  };
  return driver;
}

function deferredFetchStubs(names) {
  const calls = {};
  const stubs = {};
  for (const name of names) {
    calls[name] = [];
    stubs[name] = (...args) => {
      const d = deferred();
      calls[name].push({ args, ...d });
      return d.promise;
    };
  }
  return { calls, stubs };
}

const componentStub = (name) => ({ [name]: () => null });

// The viewer renders rounds through the shared round model that was extracted
// into src/utils/roundModel.ts; the page module cannot be instantiated without
// it, so every behavioural case in this file failed at setup with
// "unexpected require from src/pages/AtifViewerPage.tsx: ../utils/roundModel".
// Transpiled from source (its only imports are `import type`, erased) so the
// page still renders through the real model rather than a hand-written stand-in.
const roundModel = (() => {
  const module = { exports: {} };
  const fn = new Function('require', 'module', 'exports', transpile('src/utils/roundModel.ts'));
  fn(
    (name) => {
      throw new Error(`roundModel must not require anything at runtime: ${name}`);
    },
    module,
    module.exports,
  );
  return module.exports;
})();

// Captures FileReader instances so tests resolve reads by hand.
class FakeFileReader {
  constructor() {
    this.onload = null;
    this.onerror = null;
    FakeFileReader.instances.push(this);
  }
  readAsText(file) {
    this._file = file;
  }
}
FakeFileReader.instances = [];

const atifDoc = (sessionId) => ({
  schema_version: 'ATIF/1.0',
  session_id: sessionId,
  steps: [],
});

function renderViewerPage() {
  const { calls, stubs } = deferredFetchStubs([
    'fetchAtifBySession',
    'fetchAtifByConversation',
    'fetchTrajectoryAtif',
    'fetchSessionSavings',
  ]);
  const driver = createHooksDriver();
  const urlParams = new URLSearchParams('');
  const setParams = (next) => {
    for (const [k, v] of next.entries ? next.entries() : []) urlParams.set(k, v);
  };
  const moduleStubs = {
    'react-router-dom': { useSearchParams: () => [urlParams, setParams] },
    '../i18n': {
      useI18n: () => ({ t: (key) => key }),
      useLocaleTag: () => 'en',
    },
    '../utils/roundModel': roundModel,
    '../utils/savings': savingsModule,
    '../utils/apiClient': { ...stubs },
    '../components/SubagentGraph': componentStub('SubagentGraph'),
    '../components/CausalAttributionPanel': componentStub('CausalAttributionPanel'),
    '../utils/trajectoryTree': {
      buildTrajectoryTree: () => null,
      findNodeByPath: () => ({ doc: null }),
      findNodeByRef: () => null,
      encodeNodePath: (p) => p.join('/'),
      decodeNodePath: (s) => (s ? s.split('/') : []),
    },
  };
  global.FileReader = FakeFileReader;
  const pageModule = loadPageModule('src/pages/AtifViewerPage.tsx', moduleStubs, driver);
  const page = pageModule.AtifViewerPage;
  assert.equal(typeof page, 'function', 'AtifViewerPage must be a component');
  const rendered = driver.render(page);
  // Sanity: the hook-slot map must match the page's real hook order.
  assert.equal(driver.slots[4] !== undefined && 'setter' in driver.slots[4], true,
    'slot 4 must be the doc state');
  assert.equal(typeof driver.slots[14].value.current, 'number',
    'slot 14 must be the load request-id ref');
  return {
    calls, driver, rendered, urlParams,
    handleLoad: rendered.callbacks[3],
    handleFileImport: rendered.callbacks[4],
  };
}


// TokenSavingsPage and AtifViewerPage render savings rates through the shared
// src/utils/savings.ts; transpiled from source so the page still runs the real
// formula rather than a hand-written stand-in.
const savingsModule = (() => {
  const module = { exports: {} };
  const fn = new Function('require', 'module', 'exports', transpile('src/utils/savings.ts'));
  fn(
    (name) => {
      throw new Error(`savings must not require anything at runtime: ${name}`);
    },
    module,
    module.exports,
  );
  return module.exports;
})();

function loadPageModule(relativePath, moduleStubs, driver) {
  const code = transpile(relativePath);
  const module = { exports: {} };
  const hooks = {
    useState: driver.useState,
    useRef: driver.useRef,
    useEffect: driver.useEffect,
    useCallback: driver.useCallback,
    useMemo: driver.useMemo,
  };
  const reactStub = {
    __esModule: true,
    default: { createElement: (type, props, ...children) => ({ type, props, children }), ...hooks },
    createElement: (type, props, ...children) => ({ type, props, children }),
    ...hooks,
  };
  const requireStub = (name) => {
    if (name === 'react') return reactStub;
    if (moduleStubs[name]) return moduleStubs[name];
    if (name === '../utils/trajectoryTextFilter') return loadPageModule('src/utils/trajectoryTextFilter.ts', moduleStubs, driver);
    throw new Error(`unexpected require from ${relativePath}: ${name}`);
  };
  const fn = new Function('require', 'module', 'exports', code);
  fn(requireStub, module, module.exports);
  return module.exports;
}

function importFile(handleFileImport, name, payload) {
  FakeFileReader.instances.length = 0;
  handleFileImport({
    target: { files: [{ name }], value: 'sentinel' },
  });
  assert.equal(FakeFileReader.instances.length, 1, 'the import must start one FileReader read');
  return FakeFileReader.instances[0];
}

const completeRead = (reader, payload) =>
  reader.onload({ target: { result: payload } });

test('atif viewer: a late network response must not replace an imported document', async () => {
  const ctx = renderViewerPage();
  const network = ctx.handleLoad('session', 'net-session');
  assert.equal(ctx.calls.fetchAtifBySession.length, 1,
    'handleLoad must issue the session fetch');
  assert.equal(ctx.driver.slots[5].value, true, 'the network load starts loading');

  // The user imports a local document while the request is in flight.
  const reader = importFile(ctx.handleFileImport, 'local.json', JSON.stringify(atifDoc('imported-doc')));
  completeRead(reader, JSON.stringify(atifDoc('imported-doc')));
  await settle();
  assert.equal(ctx.driver.slots[4].value.session_id, 'imported-doc',
    'the import must land while the request is pending');

  // The older network response resolves LAST.
  ctx.calls.fetchAtifBySession[0].resolve(atifDoc('network-doc'));
  await network;
  await settle();
  await settle();

  assert.equal(ctx.driver.slots[4].value.session_id, 'imported-doc',
    'the late network response must not replace the imported document');
  assert.equal(ctx.driver.slots[5].value, false,
    'the import must own the loading state');
  assert.equal(ctx.driver.slots[6].value, null, 'no error may surface');
});

test('atif viewer: an import completing after a newer network load must not clobber it', async () => {
  const ctx = renderViewerPage();
  const reader = importFile(ctx.handleFileImport, 'old.json', JSON.stringify(atifDoc('old-import')));

  // A network load supersedes the pending import and completes first.
  const network = ctx.handleLoad('session', 'net-session');
  ctx.calls.fetchAtifBySession[0].resolve(atifDoc('network-doc'));
  await network;
  await settle();
  await settle();
  assert.equal(ctx.driver.slots[4].value.session_id, 'network-doc',
    'the newer network load must own the document');

  // The stale import read completes LAST with valid JSON.
  completeRead(reader, JSON.stringify(atifDoc('old-import')));
  await settle();

  assert.equal(ctx.driver.slots[4].value.session_id, 'network-doc',
    'the invalidated read must not replace the document');
  assert.equal(ctx.driver.slots[6].value, null,
    'the invalidated read must not surface an obsolete error');
  assert.equal(ctx.driver.slots[5].value, false,
    'the invalidated read must not touch the loading state');
});

test('atif viewer: out-of-order imports keep the newer file', async () => {
  const ctx = renderViewerPage();
  const first = importFile(ctx.handleFileImport, 'a.json', JSON.stringify(atifDoc('file-a')));

  // A second import supersedes the first and completes immediately.
  const second = importFile(ctx.handleFileImport, 'b.json', JSON.stringify(atifDoc('file-b')));
  completeRead(second, JSON.stringify(atifDoc('file-b')));
  await settle();
  assert.equal(ctx.driver.slots[4].value.session_id, 'file-b');

  // The older read lands last.
  completeRead(first, JSON.stringify(atifDoc('file-a')));
  await settle();

  assert.equal(ctx.driver.slots[4].value.session_id, 'file-b',
    'the older import must not overwrite the newer file');
  assert.equal(ctx.driver.slots[5].value, false, 'loading must be released');
});

test('atif viewer: an invalidated read error must stay silent', async () => {
  const ctx = renderViewerPage();
  // First import carries malformed JSON and never completes...
  const stale = importFile(ctx.handleFileImport, 'broken.json', 'not json at all');

  // ...while a newer import completes successfully.
  const fresh = importFile(ctx.handleFileImport, 'good.json', JSON.stringify(atifDoc('good-doc')));
  completeRead(fresh, JSON.stringify(atifDoc('good-doc')));
  await settle();
  assert.equal(ctx.driver.slots[4].value.session_id, 'good-doc');

  // The stale read completes with malformed content and then fails on read.
  completeRead(stale, 'not json at all');
  await settle();
  assert.equal(ctx.driver.slots[6].value, null,
    'an obsolete parse error must not surface for the invalidated read');
  assert.equal(ctx.driver.slots[4].value.session_id, 'good-doc',
    'the invalidated read must not replace the document');
  assert.equal(ctx.driver.slots[5].value, false,
    'the invalidated read must not finish another request\'s loading state');

  if (typeof stale.onerror === 'function') {
    stale.onerror(new Error('read failed'));
    await settle();
    assert.equal(ctx.driver.slots[6].value, null,
      'an invalidated read failure must stay silent');
    assert.equal(ctx.driver.slots[5].value, false);
  }
});

test('atif viewer: a terminal read failure reports an error and releases loading', async () => {
  const ctx = renderViewerPage();
  const reader = importFile(ctx.handleFileImport, 'locked.json', '');
  assert.equal(typeof reader.onerror, 'function',
    'the import must register a terminal FileReader error callback');

  reader.onerror(new Error('read failed'));
  await settle();

  assert.equal(ctx.driver.slots[6].value, 'atif.loadFailed',
    'the read failure must surface an error');
  assert.equal(ctx.driver.slots[5].value, false,
    'the failed import must release the loading state');
});
