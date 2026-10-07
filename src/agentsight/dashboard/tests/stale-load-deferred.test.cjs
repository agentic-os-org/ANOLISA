const assert = require('node:assert/strict');
const { join } = require('node:path');
const test = require('node:test');

// Behavioral companion to stale-load-regression.test.cjs: the source-ordering
// assertions there cannot capture the interleavings themselves, so these
// tests execute the REAL page code (transpiled from src/pages/*.tsx with the
// dashboard's own babel toolchain) against a minimal hooks driver and
// fetch stubs whose responses resolve in a controlled order. Each test
// reproduces the exact stale-write interleaving from the review: the older
// request's response lands LAST and must not overwrite the newer data.
//
// The hooks driver mirrors React's hook semantics closely enough for these
// pages: one cursor shared by useState/useRef, setters that support both
// value and functional updates, and re-renders by re-invoking the component
// function (effects are recorded, never auto-run — each test decides when
// an effect fires and runs the previous effect's cleanup itself, exactly
// like React does before re-running a changed effect).

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
  const slots = [];          // useState/useRef storage, indexed by hook order
  const driver = {
    slots,
    // useMemo/useCallback caches keyed by call order; they have always been
    // kept out of the shared cursor, and the existing page slot maps depend
    // on that. Tracking their deps lets tests emulate React's re-run rules.
    _memos: [],
    _callbackCache: [],
    render(Component, props = {}) {
      driver._cursor = 0;
      driver._memoIndex = 0;
      driver._callbackIndex = 0;
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
    useEffect(fn, deps) {
      // Record the dependency array so a test can emulate React's decision to
      // re-run an effect only when one of its dependencies changed.
      fn.__deps = Array.isArray(deps) ? deps : [];
      driver._effects.push(fn);
    },
    useCallback(fn, deps) {
      const index = driver._callbackIndex;
      driver._callbackIndex += 1;
      const depList = Array.isArray(deps) ? deps : [];
      const cached = driver._callbackCache[index];
      const stable = cached
        && cached.deps.length === depList.length
        && cached.deps.every((dep, i) => Object.is(dep, depList[i]));
      if (!stable) driver._callbackCache[index] = { fn, deps: depList };
      const value = stable ? cached.fn : fn;
      driver._callbacks.push(value);
      return value;
    },
    useMemo(factory, deps) {
      const index = driver._memoIndex;
      driver._memoIndex += 1;
      const depList = Array.isArray(deps) ? deps : [];
      const cached = driver._memos[index];
      if (cached
          && cached.deps.length === depList.length
          && cached.deps.every((dep, i) => Object.is(dep, depList[i]))) {
        return cached.value;
      }
      const value = factory();
      driver._memos[index] = { value, deps: depList };
      return value;
    },
  };
  return driver;
}

function loadPageModule(relativePath, moduleStubs, driver) {
  const code = transpile(relativePath);
  const module = { exports: {} };
  const hooks = driver
    ? {
      useState: driver.useState,
      useRef: driver.useRef,
      useEffect: driver.useEffect,
      useCallback: driver.useCallback,
      useMemo: driver.useMemo,
    }
    : {};
  const reactStub = {
    __esModule: true,
    default: { createElement: (type, props, ...children) => ({ type, props, children }), ...hooks },
    createElement: (type, props, ...children) => ({ type, props, children }),
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

// Records every call to `names` as a deferred the test resolves by hand.
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

// RichText only sanitizes its text for display, and the sanitizer has its own
// suite; here it yields the text so the page tree keeps the same content.
const richTextStub = { RichText: ({ children }) => children };

// Depth-first walk over the classic-runtime element tree produced by the
// react stub in loadPageModule.
function findElement(node, predicate) {
  if (node == null || typeof node !== 'object') return null;
  if (Array.isArray(node)) {
    for (const child of node) {
      const found = findElement(child, predicate);
      if (found) return found;
    }
    return null;
  }
  if (predicate(node)) return node;
  return findElement(node.children, predicate);
}

function elementText(node) {
  if (node == null || typeof node === 'boolean') return [];
  if (typeof node === 'string' || typeof node === 'number') return [String(node)];
  if (Array.isArray(node)) return node.flatMap(elementText);
  return elementText(node.children);
}

// ─── SecurityObservabilityPage ────────────────────────────────────────────────
//
// Hook-slot map (useState and useRef share one cursor, exactly like React;
// verified by the sanity checks below — if a page edit reorders the hooks,
// the sanity assertion names the slot that moved):
//   2 activeTab ('overview') · 3 status · 20 eventDetail · 23 securitySessions
//   26 selectedSessionId · 38/39/40/41 overview/events/sessions/eventDetail refs
// useCallback order: loadStatus, loadOverview, loadEvents, loadSessions,
//   loadEventDetail, queryEvents, clearEventFilters, handleRefresh.

function renderSecurityPage() {
  const { calls, stubs } = deferredFetchStubs([
    'fetchSecurityCountBy',
    'fetchSecurityEvent',
    'fetchSecurityEvents',
    'fetchSecurityRuns',
    'fetchSecuritySessions',
    'fetchSecurityStatus',
    'fetchSecuritySummary',
    'fetchSecurityTimeline',
  ]);
  const driver = createHooksDriver();
  const moduleStubs = {
    '../i18n': { useI18n: () => ({ t: (key) => key }) },
    '../components/DateTimePicker': componentStub('DateTimePicker'),
    '../utils/apiClient': { ...stubs },
    './security/EventDetailDrawer': componentStub('EventDetailDrawer'),
    './security/EventsTab': componentStub('EventsTab'),
    './security/OverviewTab': componentStub('OverviewTab'),
    './security/TimelineTab': componentStub('TimelineTab'),
    './security/common': { ...componentStub('StatePill'), ...componentStub('StatusPanel') },
    './security/types': {
      EMPTY_EVENT_FILTERS: {},
      EVENT_PAGE_SIZE: 50,
      OVERVIEW_EVENT_SAMPLE_LIMIT: 5,
    },
    './security/utils': {
      buildObservabilityContextById: () => ({}),
      errorMessage: (error) => String((error && error.message) || error),
      mapToCountItems: () => [],
      msToNs: (ms) => ms * 1_000_000,
    },
  };
  const pageModule = loadPageModule('src/pages/SecurityObservabilityPage.tsx', moduleStubs, driver);
  const page = pageModule.SecurityObservabilityPage;
  assert.equal(typeof page, 'function', 'SecurityObservabilityPage must be a component');
  driver.render(page);
  // Sanity: the hook-slot map above must match the page's real hook order.
  assert.equal(driver.slots[2].value, 'overview', 'slot 2 must be activeTab');
  assert.equal(driver.slots[38] && typeof driver.slots[38].value.current, 'number',
    'slot 38 must be a request-id ref');
  // The page starts unavailable (status null); make the daemon reachable so
  // loadSessions' availability guard lets it run, then re-render so the
  // loaders close over the available state.
  driver.slots[3].setter({ state: 'daemon_reachable', data: {} });
  const rendered = driver.render(page);
  return { calls, driver, rendered, page };
}

const DIMENSIONS = ['summary', 'perf', 'perfIssues', 'cost', 'costWaste', 'accuracy'];
const WIRE_DIMENSIONS = ['summary', 'perf', 'perf-issues', 'cost', 'cost-waste', 'accuracy'];

function dimensionPayload(dim, marker) {
  return dim === 'accuracy'
    ? { extraction: { final_answer: marker }, failures: [], issues: [] }
    : { marker };
}

function storedReport(marker) {
  return Object.fromEntries([
    ['summary', dimensionPayload('summary', marker)],
    ['perf', dimensionPayload('perf', marker)],
    ['perf_issues', dimensionPayload('perfIssues', marker)],
    ['cost', dimensionPayload('cost', marker)],
    ['cost_waste', dimensionPayload('costWaste', marker)],
    ['accuracy', dimensionPayload('accuracy', marker)],
  ]);
}

function renderOptimizationPage() {
  const driver = createHooksDriver();
  const { calls, stubs } = deferredFetchStubs(['fetchOptimizeResults', 'runOptimizeDimension']);
  class ApiRequestError extends Error {
    constructor() {
      super('LLM not configured');
      this.status = 400;
      this.body = { error: 'llm_not_configured' };
    }
  }
  const module = loadPageModule(
    'src/pages/OptimizationPage.tsx',
    {
      'react-router-dom': {
        useParams: () => ({ sessionId: 'session-a' }),
        useNavigate: () => () => {},
      },
      recharts: {},
      '../utils/apiClient': { ...stubs, ApiRequestError },
      '../components/CopyButton': {},
      '../utils/formatDuration': {},
      '../i18n': { useI18n: () => ({ t: (key) => key }), useLocaleTag: () => 'en' },
      '../utils/accuracyAttribution': {},
      '../components/TokenFlameChart': {},
      '../utils/richText': richTextStub,
    },
    driver,
  );
  // Obtain the actual private session component from the exported route's
  // element, without injecting exports or replacing production functions.
  const page = driver.render(module.OptimizationPage).element.type;
  let cleanup;
  async function visit(sessionId, history = storedReport('current')) {
    if (cleanup) cleanup();
    const rendered = driver.render(page, { sessionId });
    cleanup = rendered.effects[0]();
    calls.fetchOptimizeResults.at(-1).resolve(history);
    await settle();
    return driver.render(page, { sessionId });
  }
  return { driver, calls, page, visit, unmount: () => cleanup(), ApiRequestError };
}

for (const fails of [false, true]) {
  test(`optimization: A-B-A drops every old dimension ${fails ? 'failure' : 'result'}`, async () => {
    const probe = renderOptimizationPage();
    const initial = await probe.visit('session-a');
    initial.callbacks[1](DIMENSIONS);
    assert.deepEqual(
      probe.calls.runOptimizeDimension.map((call) => call.args),
      WIRE_DIMENSIONS.map((dim) => ['session-a', dim]),
    );
    await probe.visit('session-b');
    await probe.visit('session-a');
    const report = probe.driver.slots[0].value;
    const progress = probe.driver.slots[1].value;
    for (const [index, call] of probe.calls.runOptimizeDimension.entries()) {
      if (fails) call.reject(new probe.ApiRequestError());
      else call.resolve(dimensionPayload(DIMENSIONS[index], 'old'));
    }
    await settle();
    assert.deepEqual(probe.driver.slots[0].value, report, 'the current report must survive');
    assert.deepEqual(
      probe.driver.slots[1].value,
      progress,
      'current completion flags must survive',
    );
    assert.equal(probe.driver.slots[3].value, false, 'old configuration errors must be ignored');
    assert.equal(probe.driver.slots[4].value, null, 'old accuracy errors must be ignored');
  });
}

test('optimization: ordinary A-B navigation still drops old results', async () => {
  const probe = renderOptimizationPage();
  const initial = await probe.visit('session-a');
  initial.callbacks[1](DIMENSIONS);
  await probe.visit('session-b');
  const report = probe.driver.slots[0].value;
  probe.calls.runOptimizeDimension.forEach((call, index) =>
    call.resolve(dimensionPayload(DIMENSIONS[index], 'old')),
  );
  await settle();
  assert.deepEqual(probe.driver.slots[0].value, report);
});

for (const fails of [false, true]) {
  test(`optimization: a new analysis supersedes the previous ${fails ? 'failure' : 'result'}`, async () => {
    const probe = renderOptimizationPage();
    let rendered = await probe.visit('session-a');
    rendered.callbacks[2]();
    rendered = probe.driver.render(probe.page, { sessionId: 'session-a' });
    rendered.callbacks[2]();
    const calls = probe.calls.runOptimizeDimension;
    assert.equal(calls.length, 12);
    calls
      .slice(6)
      .forEach((call, index) => call.resolve(dimensionPayload(DIMENSIONS[index], 'new-run')));
    await settle();
    const report = probe.driver.slots[0].value;
    calls.slice(0, 6).forEach((call, index) => {
      if (fails) call.reject(new probe.ApiRequestError());
      else call.resolve(dimensionPayload(DIMENSIONS[index], 'old-run'));
    });
    await settle();
    assert.deepEqual(probe.driver.slots[0].value, report);
    assert.ok(Object.values(probe.driver.slots[1].value).every((value) => value === 'done'));
    assert.equal(probe.driver.slots[3].value, false);
    assert.equal(probe.driver.slots[4].value, null);
  });
}

test('optimization: automatic analysis only fills missing dimensions', async () => {
  const probe = renderOptimizationPage();
  const history = storedReport('stored');
  delete history.summary;
  delete history.cost_waste;
  const rendered = await probe.visit('session-a', history);
  rendered.effects[1]();
  assert.deepEqual(
    probe.calls.runOptimizeDimension.map((call) => call.args[1]),
    ['summary', 'cost-waste'],
  );
  const perf = probe.driver.slots[0].value.perf;
  probe.calls.runOptimizeDimension[0].resolve(dimensionPayload('summary', 'fresh'));
  probe.calls.runOptimizeDimension[1].resolve(dimensionPayload('costWaste', 'fresh'));
  await settle();
  assert.deepEqual(probe.driver.slots[0].value.perf, perf);
  assert.equal(probe.driver.slots[0].value.summary.marker, 'fresh');
  assert.equal(probe.driver.slots[0].value.cost_waste.marker, 'fresh');
  assert.ok(Object.values(probe.driver.slots[1].value).every((value) => value === 'done'));
});

test('optimization: unmount invalidates dimensions and current failures remain visible', async () => {
  const probe = renderOptimizationPage();
  const rendered = await probe.visit('session-a');
  rendered.callbacks[1](['summary', 'accuracy']);
  probe.calls.runOptimizeDimension[0].reject(new probe.ApiRequestError());
  await settle();
  assert.equal(probe.driver.slots[1].value.summary, 'error');
  assert.equal(probe.driver.slots[3].value, true);
  const report = probe.driver.slots[0].value;
  const progress = probe.driver.slots[1].value;
  probe.unmount();
  probe.calls.runOptimizeDimension[1].resolve(dimensionPayload('accuracy', 'unmounted'));
  await settle();
  assert.deepEqual(probe.driver.slots[0].value, report);
  assert.deepEqual(probe.driver.slots[1].value, progress);
});

for (const fails of [false, true]) {
  test(`security status: locale reload drops the older ${fails ? 'failure' : 'response'}`, async () => {
    const { calls, driver, rendered, page } = renderSecurityPage();
    const cleanup = rendered.effects[0]();
    const next = driver.render(page); // i18n stub supplies the new locale's t
    if (cleanup) cleanup();
    next.effects[0]();
    assert.equal(calls.fetchSecurityStatus.length, 2);
    calls.fetchSecurityStatus[1].resolve({ state: 'daemon_reachable', data: { marker: 'new' } });
    await settle();
    if (fails) calls.fetchSecurityStatus[0].reject(new Error('old failure'));
    else calls.fetchSecurityStatus[0].resolve({ state: 'daemon_unreachable', data: {} });
    await settle();
    assert.equal(driver.slots[3].value?.data?.marker, 'new');
    assert.equal(driver.slots[4].value, false);
    assert.equal(driver.slots[5].value, null);
  });
}

test('security status: old finally does not clear the newer loading flag', async () => {
  const { calls, driver, rendered } = renderSecurityPage();
  const first = rendered.callbacks[0]();
  const second = rendered.callbacks[0]();
  calls.fetchSecurityStatus[0].reject(new Error('old failure'));
  await first;
  assert.equal(driver.slots[4].value, true);
  assert.equal(driver.slots[5].value, null);
  calls.fetchSecurityStatus[1].reject(new Error('current failure'));
  await second;
  assert.equal(driver.slots[3].value, null);
  assert.equal(driver.slots[4].value, false);
  assert.equal(driver.slots[5].value, 'current failure');
});

test('security status: unmount invalidates pending success and failure', async () => {
  for (const fails of [false, true]) {
    const { calls, driver, rendered } = renderSecurityPage();
    const cleanup = rendered.effects[0]();
    const status = driver.slots[3].value;
    if (cleanup) cleanup();
    if (fails) calls.fetchSecurityStatus[0].reject(new Error('after unmount'));
    else calls.fetchSecurityStatus[0].resolve({ state: 'daemon_unreachable', data: {} });
    await settle();
    assert.deepEqual(driver.slots[3].value, status);
    assert.equal(driver.slots[5].value, null);
  }
});

test('security sessions: an older loadSessions response must not survive a newer overview batch', async () => {
  const { calls, driver, rendered } = renderSecurityPage();
  const loadOverview = rendered.callbacks[1];
  const loadSessions = rendered.callbacks[3];

  // 1. Enter the timeline tab: loadSessions starts with the old range.
  const sessionsPromise = loadSessions();
  assert.equal(calls.fetchSecuritySessions.length, 1, 'loadSessions must fetch sessions');

  // 2. The time range changes: a newer overview batch (which also fetches
  //    sessions for the cards) supersedes the in-flight loadSessions.
  const overviewPromise = loadOverview();
  assert.equal(calls.fetchSecuritySummary.length, 1);
  assert.equal(calls.fetchSecuritySessions.length, 2, 'overview batch must fetch sessions too');

  // 3. The overview batch resolves first with the NEW range's sessions.
  calls.fetchSecuritySummary[0].resolve({ data: {} });
  for (let i = 0; i < 4; i += 1) calls.fetchSecurityCountBy[i].resolve({ data: { items: [] } });
  calls.fetchSecurityEvents[0].resolve({ data: { items: [] } });
  calls.fetchSecuritySessions[1].resolve({
    state: 'ok',
    data: { items: [{ session_id: 'new-range-session' }] },
  });
  await overviewPromise;
  await settle();

  // 4. The OLD loadSessions response lands last with the old range.
  calls.fetchSecuritySessions[0].resolve({
    state: 'ok',
    data: { items: [{ session_id: 'old-range-session' }] },
  });
  await sessionsPromise;
  await settle();
  await settle();

  const sessions = driver.slots[23].value;
  assert.ok(sessions, 'securitySessions must be set');
  assert.deepEqual(
    sessions.data.items.map((s) => s.session_id),
    ['new-range-session'],
    'the older loadSessions response must not overwrite the newer overview batch',
  );
  assert.equal(
    driver.slots[26].value,
    'new-range-session',
    'selectedSessionId must follow the surviving (newest) sessions response',
  );
});

test('security sessions: a superseded loadSessions still clears its loading flag', async () => {
  const { calls, driver, rendered } = renderSecurityPage();
  const loadOverview = rendered.callbacks[1];
  const loadSessions = rendered.callbacks[3];

  // 1. Entering the timeline runs loadSessions, which owns the sessions
  //    spinner (slot 24) that disables the session <select>.
  const sessionsPromise = loadSessions();
  assert.equal(calls.fetchSecuritySessions.length, 1);
  assert.equal(driver.slots[24].value, true, 'slot 24 must be sessionsLoading');

  // 2. A newer overview batch (which also fetches sessions for the cards)
  //    supersedes the in-flight loadSessions; it never touches the spinner.
  const overviewPromise = loadOverview();
  assert.equal(calls.fetchSecuritySessions.length, 2);
  calls.fetchSecuritySummary[0].resolve({ state: 'ok', data: {} });
  for (let i = 0; i < 4; i += 1) calls.fetchSecurityCountBy[i].resolve({ state: 'ok', data: { items: [] } });
  calls.fetchSecurityEvents[0].resolve({ state: 'ok', data: { items: [] } });
  calls.fetchSecuritySessions[1].resolve({
    state: 'ok',
    data: { items: [{ session_id: 'new-range-session' }] },
  });
  await overviewPromise;
  await settle();
  assert.equal(driver.slots[24].value, true,
    'the overview batch must not clear the sessions spinner');

  // 3. The superseded response lands last. Its state writes are dropped, but
  //    it must still clear the spinner or the <select> stays disabled forever.
  calls.fetchSecuritySessions[0].resolve({
    state: 'ok',
    data: { items: [{ session_id: 'old-range-session' }] },
  });
  await sessionsPromise;
  await settle();
  assert.equal(driver.slots[24].value, false,
    'a superseded loadSessions must still clear sessionsLoading');
});

test('security event detail: clicking A then B with A resolving last must still show B', async () => {
  const { calls, driver, rendered } = renderSecurityPage();
  const loadEventDetail = rendered.callbacks[4];

  const first = loadEventDetail('event-a');
  const second = loadEventDetail('event-b');
  assert.equal(calls.fetchSecurityEvent.length, 2);

  // B resolves first and becomes the drawer content; A is slower and lands last.
  calls.fetchSecurityEvent[1].resolve({ state: 'ok', data: { event_id: 'event-b' } });
  calls.fetchSecurityEvent[0].resolve({ state: 'ok', data: { event_id: 'event-a' } });
  await first;
  await second;
  await settle();

  assert.equal(driver.slots[20].value?.data?.event_id, 'event-b',
    'the drawer body must match the newest click, not the slowest response');
  assert.equal(driver.slots[21].value, false,
    'the detail loading flag must be cleared by the newest request only');
});

test('security events query: the Query button re-issues the request with unchanged filters', async () => {
  const { calls, driver, page } = renderSecurityPage();

  // Enter the events tab: the dep-driven effect issues the first page request.
  driver.slots[2].setter('events');
  const rendered = driver.render(page);
  rendered.effects[2]();
  assert.equal(calls.fetchSecurityEvents.length, 1, 'entering the tab must load the first page');
  calls.fetchSecurityEvents[0].resolve({
    state: 'ok',
    data: { items: [], total: 0, offset: 0, limit: 25, next_offset: null },
  });
  await settle();

  // Render the REAL EventsTab with the props the page passed, then invoke the
  // Query button's actual onClick. The draft filters are still the same object
  // as the applied ones, which is exactly the case that used to make React
  // bail out of the state write and skip the reload.
  const tabElement = findElement(rendered.element, (node) => (
    node.props && typeof node.props.loadEvents === 'function' && node.props.eventFilters
  ));
  assert.ok(tabElement, 'the events tab must be rendered');
  const eventsTabDriver = createHooksDriver();
  const eventsTabModule = loadPageModule('src/pages/security/EventsTab.tsx', {
    '../../i18n': { useI18n: () => ({ t: (key) => key }) },
    './EventTable': componentStub('EventTable'),
    './types': { EMPTY_EVENT_FILTERS: {} },
  }, eventsTabDriver);
  const tabRendered = eventsTabDriver.render(eventsTabModule.EventsTab, tabElement.props);
  const queryButton = findElement(tabRendered.element, (node) => (
    node.type === 'button' && elementText(node).includes('common.query')
  ));
  assert.ok(queryButton, 'the Query button must exist');
  await queryButton.props.onClick();

  assert.equal(
    calls.fetchSecurityEvents.length,
    2,
    'Query must re-issue the request even when the draft filters are unchanged',
  );
});

// ─── SkillMetricsPage ─────────────────────────────────────────────────────────
//
// Hook-slot map: 0 startMs · 1 endMs · 2 agentName · 3 agents · 4 granularity
// Effects per render: [loadData, agent-list].

test('skill metrics: an older range agent list must not overwrite the newer range list', async () => {
  const { calls, stubs } = deferredFetchStubs(['fetchSkillMetrics', 'fetchAgentNames']);
  const driver = createHooksDriver();
  const moduleStubs = {
    '../i18n': { useI18n: () => ({ t: (key) => key }) },
    recharts: {
      ...componentStub('BarChart'), ...componentStub('Bar'), ...componentStub('XAxis'),
      ...componentStub('YAxis'), ...componentStub('Tooltip'), ...componentStub('ResponsiveContainer'),
    },
    '../utils/apiClient': { ...stubs },
    '../components/DateTimePicker': componentStub('DateTimePicker'),
  };
  const pageModule = loadPageModule('src/pages/SkillMetricsPage.tsx', moduleStubs, driver);
  const page = pageModule.SkillMetricsPage;
  assert.equal(typeof page, 'function', 'SkillMetricsPage must be a component');
  let rendered = driver.render(page);
  assert.deepEqual(driver.slots[3].value, [], 'slot 3 must be the agents list');

  // Range 1: the effect fires and its response will be slow.
  const cleanup1 = rendered.effects[1]();
  assert.equal(calls.fetchAgentNames.length, 1);

  // Range 2: the user picks a new window; React cleans the previous effect
  // up before running the new one, then the new fetch is issued.
  driver.slots[0].setter(1_000);
  driver.slots[1].setter(2_000);
  rendered = driver.render(page);
  if (typeof cleanup1 === 'function') cleanup1();
  rendered.effects[1]();
  assert.equal(calls.fetchAgentNames.length, 2);
  assert.deepEqual(
    calls.fetchAgentNames[1].args,
    [1_000 * 1_000_000, 2_000 * 1_000_000],
    'the second agent-list fetch must use the new range',
  );

  // The new range answers first, the old range's slow response lands last.
  calls.fetchAgentNames[1].resolve(['beta']);
  calls.fetchAgentNames[0].resolve(['alpha']);
  await settle();
  await settle();

  assert.deepEqual(driver.slots[3].value, ['beta'],
    'the older range response must not overwrite the newer range agent list');
});

test('skill metrics: a failed reload must not keep the previous report', async () => {
  const { calls, stubs } = deferredFetchStubs(['fetchSkillMetrics', 'fetchAgentNames']);
  const driver = createHooksDriver();
  const moduleStubs = {
    '../i18n': { useI18n: () => ({ t: (key) => key }) },
    recharts: {
      ...componentStub('BarChart'), ...componentStub('Bar'), ...componentStub('XAxis'),
      ...componentStub('YAxis'), ...componentStub('Tooltip'), ...componentStub('ResponsiveContainer'),
    },
    '../utils/apiClient': { ...stubs },
    '../components/DateTimePicker': componentStub('DateTimePicker'),
  };
  const pageModule = loadPageModule('src/pages/SkillMetricsPage.tsx', moduleStubs, driver);
  const page = pageModule.SkillMetricsPage;

  // The first load answers with a report for the default window.
  let rendered = driver.render(page);
  const firstLoad = rendered.effects[0]();
  assert.equal(calls.fetchSkillMetrics.length, 1);
  calls.fetchSkillMetrics[0].resolve({ event_count: 7, loads: { total_loads: 1, loads: {} } });
  await firstLoad;
  await settle();
  assert.ok(driver.slots[5].value, 'sanity: the first report must be stored');

  // The agent filter changes (slot 2 is agentName); React re-runs the loader
  // effect and the new request fails.
  driver.slots[2].setter('alpha');
  rendered = driver.render(page);
  const secondLoad = rendered.effects[0]();
  assert.equal(calls.fetchSkillMetrics.length, 2, 'the filter change must re-issue the load');
  calls.fetchSkillMetrics[1].reject(new Error('boom'));
  await secondLoad;
  await settle();

  assert.equal(
    driver.slots[5].value,
    null,
    "a failed reload must not keep the previous agent's/range's report under the new controls",
  );
  assert.equal(driver.slots[7].value, 'boom', 'the error banner must explain the newest failure');
});

test('security overview: a failed card must not keep the previous range payload', async () => {
  const { calls, driver, rendered } = renderSecurityPage();
  const loadOverview = rendered.callbacks[1];

  // First batch: every card answers, with a distinctive sample event.
  const first = loadOverview();
  calls.fetchSecuritySummary[0].resolve({ state: 'ok', data: {} });
  for (let i = 0; i < 4; i += 1) calls.fetchSecurityCountBy[i].resolve({ state: 'ok', data: { items: [] } });
  calls.fetchSecurityEvents[0].resolve({ state: 'ok', data: { items: [{ event_id: 'probe-event' }] } });
  calls.fetchSecuritySessions[0].resolve({ state: 'ok', data: { items: [] } });
  await first;
  await settle();

  const eventsSlot = driver.slots.findIndex(
    (slot) => slot && slot.value && slot.value.data && Array.isArray(slot.value.data.items)
      && slot.value.data.items[0] && slot.value.data.items[0].event_id === 'probe-event',
  );
  assert.ok(eventsSlot >= 0, 'the overview sample must land in some slot');

  // The range changes; every card answers except the event sample.
  const second = loadOverview();
  calls.fetchSecuritySummary[1].resolve({ state: 'ok', data: {} });
  for (let i = 4; i < 8; i += 1) calls.fetchSecurityCountBy[i].resolve({ state: 'ok', data: { items: [] } });
  calls.fetchSecurityEvents[1].reject(new Error('boom'));
  calls.fetchSecuritySessions[1].resolve({ state: 'ok', data: { items: [] } });
  await second;
  await settle();

  assert.equal(
    driver.slots[eventsSlot].value,
    null,
    "a failed card must not show the previous range's events under the new range",
  );
});


// ─── CausalAttributionPanel ──────────────────────────────────────────────────
//
// Hook-slot map: 0 complaint · 1 loading · 2 error · 3 caseData · 4 cached
// 5 history · 6 selectedAltIdx · 7 stageIdx · 8 elapsed · 9 requestVersion ref
// Effect per render: [history load + request-version bump].

function findNode(node, predicate) {
  if (node == null || typeof node !== 'object') return null;
  if (Array.isArray(node)) {
    for (const child of node) {
      const found = findNode(child, predicate);
      if (found) return found;
    }
    return null;
  }
  if (predicate(node)) return node;
  if (Array.isArray(node.children)) {
    for (const child of node.children) {
      const found = findNode(child, predicate);
      if (found) return found;
    }
  }
  return null;
}

function renderCausalPanel() {
  const { calls, stubs } = deferredFetchStubs(['runCausalAttribution']);
  const driver = createHooksDriver();
  const moduleStubs = {
    '../utils/apiClient': { ...stubs },
  };
  const module = loadPageModule('src/components/CausalAttributionPanel.tsx', moduleStubs, driver);
  const panel = module.CausalAttributionPanel;
  assert.equal(typeof panel, 'function', 'CausalAttributionPanel must be a component');
  const rendered = driver.render(panel, {
    sessionId: 'sess-1',
    roundIndex: 0,
    roundLabel: '第 1 轮',
  });
  assert.equal(driver.slots[0].value, '', 'slot 0 must be the complaint field');
  assert.equal(driver.slots[3].value, null, 'slot 3 must be caseData');
  rendered.effects[0](); // mount: load this (session, round)'s history
  return { calls, driver, panel, rendered };
}

test('causal attribution: a run superseded by a round switch must discard its result', async () => {
  // The attribution call takes seconds. If the user switches rounds while it
  // is in flight, the late response used to write its case, cache flag,
  // selected alternative, and history entry unconditionally — rendering the
  // old round's verdict and graph under the new round's label. The run must
  // be bound to the request version of the (session, round) it was started
  // for and drop its result once that version is superseded.
  const previousWindow = global.window;
  global.window = { setInterval: global.setInterval, clearInterval: global.clearInterval };
  let calls;
  try {
    const setup = renderCausalPanel();
    const { driver, panel } = setup;
    calls = setup.calls;

    // Type a complaint and re-render so the run button enables.
    driver.slots[0].setter('这轮引用靠谱吗？');
    let rendered = driver.render(panel, {
      sessionId: 'sess-1',
      roundIndex: 0,
      roundLabel: '第 1 轮',
    });
    const runButton = findNode(
      rendered.element,
      (node) =>
        node.type === 'button'
        && Array.isArray(node.children)
        && node.children.filter((c) => typeof c === 'string').join('').includes('发起归因'),
    );
    assert.ok(runButton, 'the run button must exist');
    assert.equal(typeof runButton.props.onClick, 'function');

    // Round 1's run starts and stays in flight.
    const runPromise = runButton.props.onClick();
    assert.equal(calls.runCausalAttribution.length, 1, 'the run must issue one request');
    assert.equal(calls.runCausalAttribution[0].args[0].round_index, 0);

    // The user switches to round 2 while round 1's attribution is pending.
    rendered = driver.render(panel, {
      sessionId: 'sess-1',
      roundIndex: 1,
      roundLabel: '第 2 轮',
    });
    rendered.effects[0](); // the prop change bumps the request version

    // Round 1's slow response lands last with a distinctive case.
    calls.runCausalAttribution[0].resolve({
      case: {
        id: 'case-round-1',
        title: 'round 1',
        verdict: 'round 1 verdict',
        outcome: 'fail',
        nodes: [],
        edges: [],
      },
      cached: false,
    });
    await runPromise;
    await settle();
    await settle();

    assert.equal(
      driver.slots[3].value,
      null,
      "the superseded round's verdict must not render under round 2's label",
    );
    assert.deepEqual(
      driver.slots[5].value,
      [],
      "the superseded run's history entry must not be filed under round 1's replacement",
    );
    assert.equal(driver.slots[1].value, false, 'round 2 must not be left loading by round 1');
  } finally {
    // An assertion before the deferred lands must not leave the run's
    // interval keeping the test process alive.
    (calls ? calls.runCausalAttribution : []).forEach((call) =>
      call.resolve({ case: null, cached: false }),
    );
    if (previousWindow === undefined) delete global.window;
    else global.window = previousWindow;
  }
});

// ─── AgentSessionsPage: unchanged poll must not repeat the paid search ───────
//
// Hook-slot map: 0 merged · 6 search · 7 semanticEnabled · 11 autoRefresh ·
// 12 loadRequestIdRef. Effects per render: [loadData, auto-refresh,
// optimize-config, clear-semantic, debounce, reset-page].

const sameDeps = (a, b) =>
  a.length === b.length && a.every((dep, i) => Object.is(dep, b[i]));

test('agent sessions: an unchanged 10 s poll must not re-issue the semantic search', async () => {
  const { calls, stubs } = deferredFetchStubs([
    'fetchSessions',
    'fetchTrajectories',
    'fetchOptimizeConfig',
    'semanticSearchSessions',
  ]);
  // A stable `t` identity matters: it is a useCallback dependency, and a new
  // function per render would make React rebuild every callback each time.
  const t = (key) => key;
  const driver = createHooksDriver();
  const moduleStubs = {
    'react-router-dom': { useNavigate: () => () => {} },
    '../i18n': { useI18n: () => ({ t }), useLocaleTag: () => 'en-US' },
    '../utils/apiClient': { ...stubs },
    '../utils/semanticSearchFilter': { applySemanticRanking: (rows) => rows },
    '../utils/sessionModel': { mergeSessions: (ebpf) => ebpf },
    '../components/CopyButton': componentStub('CopyButton'),
  };
  const pageModule = loadPageModule('src/pages/AgentSessionsPage.tsx', moduleStubs, driver);
  const page = pageModule.AgentSessionsPage;
  assert.equal(typeof page, 'function', 'AgentSessionsPage must be a component');

  const realSetTimeout = global.setTimeout;
  const realClearTimeout = global.clearTimeout;
  const realSetInterval = global.setInterval;
  const realClearInterval = global.clearInterval;
  const settle = () => new Promise((resolve) => realSetTimeout(resolve, 0));
  const timers = [];
  const intervals = [];
  const mounted = []; // mounted effect per index: { deps, cleanup }

  // Emulate React: mount runs every effect; afterwards an effect re-runs only
  // when a dependency changed, after running its previous cleanup. Timers and
  // intervals are captured instead of really waiting 500 ms / 10 s.
  const reactRunEffects = (rendered) => {
    rendered.effects.forEach((fn, index) => {
      const deps = fn.__deps;
      const prev = mounted[index];
      if (prev && sameDeps(prev.deps, deps)) return;
      if (prev && typeof prev.cleanup === 'function') prev.cleanup();
      global.setTimeout = (cb) => { timers.push(cb); return timers.length; };
      global.clearTimeout = () => {};
      global.setInterval = (cb) => { intervals.push(cb); return intervals.length; };
      global.clearInterval = () => {};
      let cleanup;
      try {
        cleanup = fn();
      } finally {
        global.setTimeout = realSetTimeout;
        global.clearTimeout = realClearTimeout;
        global.setInterval = realSetInterval;
        global.clearInterval = realClearInterval;
      }
      mounted[index] = { deps, cleanup };
    });
  };

  let rendered = driver.render(page);
  assert.equal(driver.slots[6].value, '', 'slot 6 must be the search input');
  assert.equal(driver.slots[11].value, false, 'slot 11 must be autoRefresh');
  assert.equal(typeof driver.slots[12].value.current, 'number', 'slot 12 must be a request-id ref');
  reactRunEffects(rendered);

  // The initial load resolves with a fixed session set.
  const makeSessions = () => Array.from({ length: 6 }, (_, i) => ({
    session_id: `session-${i}`,
    agent_name: 'claude',
    model: 'm',
    conversation_count: 1,
    total_input_tokens: 10,
    total_output_tokens: 5,
    first_user_query: `question ${i}`,
    last_user_query: `answer ${i}`,
    last_seen_ns: Date.now() * 1_000_000,
  }));
  calls.fetchTrajectories[0].resolve([]);
  calls.fetchSessions[0].resolve(makeSessions());
  calls.fetchOptimizeConfig[0].resolve({ configured: true });
  await settle();
  await settle();

  rendered = driver.render(page);
  assert.equal(driver.slots[7].value, true, 'semantic search must be enabled');
  reactRunEffects(rendered);

  // Turn on auto-refresh: the interval callback becomes tickable by hand.
  driver.slots[11].setter(true);
  rendered = driver.render(page);
  reactRunEffects(rendered);
  assert.equal(intervals.length, 1, 'auto-refresh must register one interval');

  // Type a query: the debounce schedules the LLM call.
  driver.slots[6].setter('find the widget');
  rendered = driver.render(page);
  reactRunEffects(rendered);
  assert.equal(timers.length, 1, 'the query must schedule one debounced search');

  timers[0]();
  assert.equal(calls.semanticSearchSessions.length, 1, 'the debounce must issue one search');
  calls.semanticSearchSessions[0].resolve({
    results: [{ session_id: 'session-0', relevance: 'high' }],
  });
  await settle();
  rendered = driver.render(page);
  reactRunEffects(rendered);

  // Tick the 10 s poll; the server returns the same session set (fresh JSON).
  intervals[0]();
  calls.fetchTrajectories[1].resolve([]);
  calls.fetchSessions[1].resolve(makeSessions());
  await settle();
  await settle();

  const timersBeforePoll = timers.length;
  rendered = driver.render(page);
  reactRunEffects(rendered);
  // A browser would fire whatever the re-armed debounce scheduled.
  if (timers.length > timersBeforePoll) timers[timers.length - 1]();

  assert.ok(calls.fetchSessions.length >= 2, 'sanity: the poll must have reloaded the data');
  assert.equal(
    calls.semanticSearchSessions.length,
    1,
    'an unchanged poll must not re-issue the paid semantic search',
  );
});

// ─── AtifViewerPage: import vs in-flight load, live input vs loaded id ───────

// The viewer renders rounds through the shared round model extracted into
// src/utils/roundModel.ts, so the page module cannot be instantiated without
// it ("unexpected require ... ../utils/roundModel"). Transpiled from source
// (its only imports are `import type`, erased) so the page still renders
// through the real model rather than a hand-written stand-in.
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

// One user step so the round view (and the causal panel next to it) renders.
const atifDoc = (id) => ({
  schema_version: 'ATIF-v1.0',
  session_id: id,
  steps: [{ step_id: 1, source: 'user', content: 'hello' }],
});

function findElementByType(node, type) {
  if (!node || typeof node !== 'object') return null;
  if (node.type === type) return node;
  for (const child of node.children ?? []) {
    const found = findElementByType(child, type);
    if (found) return found;
  }
  return null;
}

function renderAtifPage() {
  const { calls, stubs } = deferredFetchStubs([
    'fetchAtifBySession',
    'fetchAtifByConversation',
    'fetchTrajectoryAtif',
    'fetchSessionSavings',
  ]);
  const t = (key) => key;
  // One stable URLSearchParams/setter instance, as react-router provides.
  const searchParams = new URLSearchParams();
  const setSearchParams = () => {};
  const panelStub = componentStub('CausalAttributionPanel');
  const driver = createHooksDriver();
  const moduleStubs = {
    'react-router-dom': { useSearchParams: () => [searchParams, setSearchParams] },
    '../i18n': { useI18n: () => ({ t }), useLocaleTag: () => 'en-US' },
    '../utils/apiClient': { ...stubs },
    '../utils/roundModel': roundModel,
    '../utils/savings': (() => {
      const out = transpile('src/utils/savings.ts');
      const mod = { exports: {} };
      new Function('require', 'module', 'exports', out)(() => ({}), mod, mod.exports);
      return mod.exports;
    })(),
    '../utils/trajectoryTextFilter': (() => {
      // The page imports the real filter so a query matches the same rounds the
      // browser does; it has no runtime imports of its own.
      const out = transpile('src/utils/trajectoryTextFilter.ts');
      const mod = { exports: {} };
      new Function('require', 'module', 'exports', out)(() => ({}), mod, mod.exports);
      return mod.exports;
    })(),
    '../components/SubagentGraph': componentStub('SubagentGraph'),
    '../components/CausalAttributionPanel': panelStub,
    '../utils/trajectoryTree': {
      buildTrajectoryTree: () => null,
      findNodeByPath: () => null,
      findNodeByRef: () => null,
      encodeNodePath: (path) => (Array.isArray(path) ? path.join('/') : ''),
      decodeNodePath: () => [],
    },
  };
  const pageModule = loadPageModule('src/pages/AtifViewerPage.tsx', moduleStubs, driver);
  const page = pageModule.AtifViewerPage;
  assert.equal(typeof page, 'function', 'AtifViewerPage must be a component');
  const rendered = driver.render(page);
  assert.equal(driver.slots[2].value, '', 'slot 2 must be queryId');
  return { calls, driver, rendered, page, panelStub };
}

// The document slot moves when hooks are added, so locate it by content.
const docBySession = (driver, id) => driver.slots.find(
  (slot) => slot && slot.value && typeof slot.value === 'object'
    && slot.value.schema_version && slot.value.session_id === id,
);

test('atif viewer: the causal panel follows the loaded id, not the edited input', async () => {
  const { calls, driver, page, rendered, panelStub } = renderAtifPage();

  const load = rendered.callbacks[3]('session', 'session-a');
  calls.fetchAtifBySession[0].resolve(atifDoc('session-a'));
  await load;
  await settle();
  calls.fetchSessionSavings[0].resolve({ items: [] });

  // The user edits the input without pressing Load.
  driver.slots[2].setter('session-b');
  const rerendered = driver.render(page);

  const panel = findElementByType(rerendered.element, panelStub.CausalAttributionPanel);
  assert.ok(panel, 'the causal panel must render once a document is loaded');
  assert.equal(panel.props.sessionId, 'session-a',
    'editing the input must not retarget attribution at an unloaded id');
  assert.equal(panel.props.idKind, 'session');
});

// ─── ConversationList ─────────────────────────────────────────────────────────
//
// Hook-slot map: 0 startMs · 1 endMs · 2 agentNames · 3 selectedAgent ·
// 4 agentNamesLoading · 5 sessions · 6 loading · 7 error.
// useCallback order: handleResolvedEvent, syncParams, loadAgentNames, runQuery,
//   handleQuery.

test('conversation list: the Query button must use the selected end time, not now', async () => {
  const { calls, stubs } = deferredFetchStubs([
    'fetchSessions',
    'fetchTraces',
    'fetchAgentNames',
    'fetchTimeseries',
    'fetchInterruptionCount',
    'fetchInterruptionStats',
    'fetchInterruptionSessionCounts',
    'fetchInterruptionConversationCounts',
    'fetchLatestEvaluation',
    'fetchTokenSavings',
  ]);
  const driver = createHooksDriver();
  const moduleStubs = {
    'react-router-dom': {
      useNavigate: () => () => {},
      useSearchParams: () => [{ get: () => null }, () => {}],
    },
    recharts: {
      ...componentStub('LineChart'), ...componentStub('Line'), ...componentStub('BarChart'),
      ...componentStub('Bar'), ...componentStub('XAxis'), ...componentStub('YAxis'),
      ...componentStub('CartesianGrid'), ...componentStub('Tooltip'), ...componentStub('Legend'),
      ...componentStub('ResponsiveContainer'),
    },
    '../components/InterruptionBadge': componentStub('InterruptionBadge'),
    '../components/InterruptionPanel': componentStub('InterruptionPanel'),
    '../components/EvaluationBadge': componentStub('EvaluationBadge'),
    '../components/EvaluationPanel': componentStub('EvaluationPanel'),
    '../components/DateTimePicker': componentStub('DateTimePicker'),
    '../components/SessionIdHelp': componentStub('SessionIdHelp'),
    '../components/SessionResourceChart': componentStub('SessionResourceChart'),
    '../i18n': { useI18n: () => ({ t: (key) => key }), useLocaleTag: () => 'en-US' },
    '../utils/datetime': { formatNsPadded: (ns) => String(ns) },
    '../utils/timeseriesBuckets': {
      fillModelBuckets: (data) => data,
      fillTokenBuckets: (data) => data,
    },
    '../utils/apiClient': {
      ...stubs,
      conversationInterruptionKey: (sessionId, conversationId) => `${sessionId}\u0000${conversationId}`,
      UNASSIGNED_INTERRUPTION_BUCKET: '__unassigned__',
    },
  };
  const pageModule = loadPageModule('src/pages/ConversationList.tsx', moduleStubs, driver);
  const page = pageModule.ConversationList;
  assert.equal(typeof page, 'function', 'ConversationList must be a component');
  driver.render(page);
  // Sanity: slots 0/1 are the query time-range state.
  const startMs = driver.slots[0].value;
  assert.equal(typeof startMs, 'number', 'slot 0 must be startMs');

  const selectedEnd = Date.UTC(2024, 0, 2, 3, 4, 5);
  driver.slots[1].setter(selectedEnd);
  const rendered = driver.render(page);
  const handleQuery = rendered.callbacks[4];
  assert.equal(typeof handleQuery, 'function', 'callbacks[4] must be handleQuery');

  void handleQuery();
  assert.equal(driver.slots[1].value, selectedEnd,
    'handleQuery must not overwrite the selected end time');
  assert.equal(calls.fetchSessions.length, 1, 'the query must fetch sessions');
  assert.deepEqual(
    calls.fetchSessions[0].args,
    [startMs * 1_000_000, selectedEnd * 1_000_000],
    'the sessions fetch must carry the selected end time',
  );
});

// ─── InterruptionPanel ────────────────────────────────────────────────────────
//
// Hook-slot map: 0 events · 1 loading · 2 error; useCallback: load.

function collectElements(node, out = []) {
  if (!node || typeof node !== 'object') return out;
  if (Array.isArray(node)) {
    for (const child of node) collectElements(child, out);
    return out;
  }
  if (node.type) out.push(node);
  if (node.children) collectElements(node.children, out);
  return out;
}

test('interruption panel: resolved events are neither counted nor offered Resolve', async () => {
  const payload = [
    {
      interruption_id: 'unresolved-1', resolved: false, severity: 'high',
      interruption_type: 'loop', occurred_at_ns: 1, call_id: null, detail: null,
    },
    {
      interruption_id: 'resolved-1', resolved: true, severity: 'high',
      interruption_type: 'loop', occurred_at_ns: 2, call_id: null, detail: null,
    },
  ];
  const tCalls = [];
  const t = (key, params) => {
    tCalls.push({ key, params });
    return key;
  };
  const driver = createHooksDriver();
  const moduleStubs = {
    '../utils/apiClient': {
      fetchSessionInterruptions: async () => payload,
      fetchConversationInterruptions: async () => payload,
      resolveInterruption: async () => {},
    },
    '../i18n': {
      useI18n: () => ({ t }),
      useLocaleTag: () => 'en-US',
      interruptionTypeKey: () => null,
    },
    '../utils/datetime': { formatNs: (ns) => String(ns) },
  };
  const module = loadPageModule('src/components/InterruptionPanel.tsx', moduleStubs, driver);
  const Panel = module.InterruptionPanel;
  assert.equal(typeof Panel, 'function', 'InterruptionPanel must be a component');

  let rendered = driver.render(Panel, { sessionId: 's1' });
  assert.equal(driver.slots[1].value, true, 'slot 1 must be the loading flag');
  await rendered.callbacks[0]();
  rendered = driver.render(Panel, { sessionId: 's1' });

  const countCall = tCalls.find((call) => call.key === 'comp.interrupt.unresolvedCount');
  assert.ok(countCall, 'the panel must render the unresolved count');
  assert.equal(countCall.params.n, 1, 'only unresolved events may be counted');

  const elements = collectElements(rendered.element);
  const rows = elements.filter(
    (el) => el.props && el.props.event && typeof el.props.onResolved === 'function',
  );
  assert.deepEqual(
    rows.map((el) => el.props.event.interruption_id),
    ['unresolved-1'],
    'resolved events must not get a row',
  );

  // The single row must expose exactly one Resolve affordance.
  const rowDriver = createHooksDriver();
  const rowRender = rowDriver.render(rows[0].type, rows[0].props);
  const resolveButtons = collectElements(rowRender.element).filter(
    (el) => el.props && el.props.title === 'comp.interrupt.markResolvedTitle',
  );
  assert.equal(resolveButtons.length, 1, 'the unresolved row must offer Resolve');
});
