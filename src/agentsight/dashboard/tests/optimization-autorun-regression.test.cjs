const assert = require('node:assert/strict');
const { join } = require('node:path');
const test = require('node:test');

// Behavioral regression for the optimization page's auto-run:
//
// The page loads the stored results, and a separate effect fills in whatever
// the visit did not find — "all dimensions idle" means a first visit, so it
// starts the six dimension analyses (each one an LLM call) without the user
// asking. A history *read* that failed also leaves the progress map idle, so
// the page could not tell "this session was never analyzed" from "the read
// failed", and auto-started the paid analyses off the back of a 5xx.
//
// The real page code is transpiled with the dashboard's babel toolchain and
// driven with the minimal hooks driver used by the sibling deferred suites:
// every effect is recorded and the test decides when to fire it, exactly like
// React firing effects after a render.

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
      driver._memoIndex = 0;
      driver._callbackIndex = 0;
      driver._effects = [];
      const element = Component(props);
      return { element, effects: driver._effects };
    },
    useState(initial) {
      const slot = slots[driver._cursor] ?? {
        value: typeof initial === 'function' ? initial() : initial,
      };
      slots[driver._cursor] = slot;
      if (!slot.setter) {
        slot.setter = (update) => {
          slot.value = typeof update === 'function' ? update(slot.value) : update;
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
    useEffect(fn, deps) {
      fn.__deps = Array.isArray(deps) ? deps : [];
      driver._effects.push(fn);
    },
    useCallback(fn, deps) {
      const index = driver._callbackIndex;
      driver._callbackIndex += 1;
      const depList = Array.isArray(deps) ? deps : [];
      const cached = driver._callbackCache?.[index];
      const stable = cached
        && cached.deps.length === depList.length
        && cached.deps.every((dep, i) => Object.is(dep, depList[i]));
      if (!stable) {
        driver._callbackCache = driver._callbackCache ?? [];
        driver._callbackCache[index] = { fn, deps: depList };
      }
      return stable ? cached.fn : fn;
    },
    useMemo(factory, deps) {
      const index = driver._memoIndex;
      driver._memoIndex += 1;
      const depList = Array.isArray(deps) ? deps : [];
      const cached = driver._memos?.[index];
      if (cached
          && cached.deps.length === depList.length
          && cached.deps.every((dep, i) => Object.is(dep, depList[i]))) {
        return cached.value;
      }
      const value = factory();
      driver._memos = driver._memos ?? [];
      driver._memos[index] = { value, deps: depList };
      return value;
    },
  };
  return driver;
}

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
  // The page under test is a dispatcher: `OptimizationPage` renders
  // `SessionAnalysisView`, which is the component that owns the hooks under
  // test. Rendering a function element eagerly (instead of returning the
  // element) drives that child exactly like React mounting it, so its hooks
  // land in the driver.
  const createElement = (type, props, ...children) => {
    const element = { type, props, children };
    if (typeof type === 'function') return type({ ...props, children });
    return element;
  };
  const reactStub = {
    __esModule: true,
    default: { createElement, ...hooks },
    createElement,
    ...hooks,
  };
  const requireStub = (name) => {
    if (name === 'react') return reactStub;
    if (moduleStubs[name]) return moduleStubs[name];
    throw new Error(`unexpected require from ${relativePath}: ${name}`);
  };
  const fn = new Function('require', 'module', 'exports', code);
  fn(requireStub, module, module.exports);
  return module.exports;
}

function buildPage(moduleStubs) {
  const driver = createHooksDriver();
  const module = loadPageModule('src/pages/OptimizationPage.tsx', moduleStubs, driver);
  const Page = module.OptimizationPage;
  assert.equal(typeof Page, 'function', 'OptimizationPage must be a component');
  // React fires every effect of a render once; the driver records them, so a
  // test drives the page by firing them all after each render.
  const render = () => {
    const rendered = driver.render(Page, {});
    for (const effect of rendered.effects) effect();
    return rendered;
  };
  return { driver, render };
}

function baseStubs(overrides = {}) {
  return {
    'react-router-dom': { useNavigate: () => () => {}, useParams: () => ({ sessionId: 'sess-1' }) },
    recharts: {
      Cell: () => null,
      Pie: () => null,
      PieChart: () => null,
      ResponsiveContainer: () => null,
      Tooltip: () => null,
    },
    '../utils/apiClient': {
      fetchOptimizeResults: async () => ({}),
      fetchOptimizeHistory: async () => ({ runs: [] }),
      runOptimizeDimension: async () => ({}),
      ApiRequestError: class ApiRequestError extends Error {},
      ...overrides.apiClient,
    },
    '../components/CopyButton': { copyText: async () => {} },
    '../utils/formatDuration': { formatDurationSecs: (seconds) => String(seconds) },
    '../i18n': { useI18n: () => ({ t: (key) => key }), useLocaleTag: () => 'en-US' },
    '../utils/accuracyAttribution': { fixLocusDiverges: () => false, fixLocusLabel: (value) => value },
    '../utils/richText': { RichText: ({ children }) => children },
    '../components/TokenFlameChart': { __esModule: true, default: () => null },
  };
}

test('optimization: a failed history read does not auto-start the paid analyses', async () => {
  const history = deferred();
  const dimensionCalls = [];
  const { render } = buildPage(baseStubs({
    apiClient: {
      fetchOptimizeResults: () => history.promise,
      runOptimizeDimension: (_sessionId, dimension) => {
        dimensionCalls.push(dimension);
        return new Promise(() => {});
      },
    },
  }));

  render();                       // mount: the history read starts, pending
  history.reject(new Error('boom'));  // the read failed
  await settle();
  await settle();
  render();                       // the page re-renders with loading finished

  assert.deepEqual(
    dimensionCalls,
    [],
    'a failed history read must not be mistaken for "never analyzed" and start the dimension analyses',
  );
});

test('optimization: a first visit still starts every dimension', async () => {
  const dimensionCalls = [];
  const { render } = buildPage(baseStubs({
    apiClient: {
      // A successful read that carries no stored dimension: a genuine first
      // visit, which is the case the auto-run exists for.
      fetchOptimizeResults: async () => ({}),
      runOptimizeDimension: (_sessionId, dimension) => {
        dimensionCalls.push(dimension);
        return new Promise(() => {});
      },
    },
  }));

  render();
  await settle();
  await settle();
  render();

  assert.deepEqual(
    dimensionCalls.sort(),
    ['accuracy', 'cost', 'cost-waste', 'perf', 'perf-issues', 'summary'],
    'a session with no stored results must still analyze every dimension',
  );
});
