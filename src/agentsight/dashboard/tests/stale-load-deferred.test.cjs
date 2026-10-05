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

// ─── SecurityObservabilityPage ────────────────────────────────────────────────
//
// Hook-slot map (useState and useRef share one cursor, exactly like React;
// verified by the sanity checks below — if a page edit reorders the hooks,
// the sanity assertion names the slot that moved):
//   2 activeTab ('overview') · 3 status · 20 eventDetail · 23 securitySessions
//   26 selectedSessionId · 38/39/40/41 overview/events/sessions/eventDetail refs
// useCallback order: loadStatus, loadOverview, loadEvents, loadSessions,
//   loadEventDetail, handleRefresh.

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
