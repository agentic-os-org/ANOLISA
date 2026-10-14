const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const test = require('node:test');

// Regression suite for display-control contracts that used to lie about the
// data:
//   - ConversationList model legend: a hidden series must be removed from the
//     Recharts stack (hide), not repainted transparent (still occupies stack
//     height and still scales the Y axis).
//   - SystemAuditPage case sort: a local sort over one server page cannot
//     order the whole result set, so the control is disabled and annotated
//     while more than one page exists.
//   - RiskEnforcementPage violations pager: the offset must be clamped when
//     the list shrinks, or the slice goes empty while Pagination hides itself
//     (total <= limit) and strands the table with no way back.
//
// The third test drives the real page with the same minimal hooks driver and
// fetch-deferral stubs as tests/stale-load-deferred.test.cjs.

const babel = require('@babel/core');

const readSource = (relativePath) => readFileSync(join(process.cwd(), relativePath), 'utf8');

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
    useEffect(fn) {
      driver._effects.push(fn);
    },
    useCallback(fn) {
      return fn;
    },
    useMemo(factory) {
      return factory();
    },
  };
  return driver;
}

function loadPageModule(relativePath, moduleStubs, driver, extra = '') {
  const code = transpile(relativePath) + extra;
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
    throw new Error(`unexpected require from ${relativePath}: ${name}`);
  };
  const fn = new Function('require', 'module', 'exports', code);
  fn(requireStub, module, module.exports);
  return module.exports;
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

for (const [label, model, tools, idle, expected] of [
  ['half-second model', 0.5, 0, 0, [['model', 0.5]]],
  ['one-second model', 1, 0, 0, [['model', 1]]],
  ['half-second tool', 0, 0.5, 0, [['tools', 0.5]]],
  ['one-second tool', 0, 1, 0, [['tools', 1]]],
  ['mixed durations', 0.5, 2, 0.2, [['model', 0.5], ['tools', 2]]],
  ['long model and tool', 2, 3, 0, [['model', 2], ['tools', 3]]],
  ['all zero', 0, 0, 0, []],
  ['short idle', 0, 0, 0.5, []],
  ['one-second idle', 0, 0, 1, []],
  ['long idle', 0, 0, 1.1, [['idle', 1.1]]],
]) {
  test(`optimization: performance slices preserve ${label}`, () => {
    const driver = createHooksDriver();
    const recharts = Object.fromEntries(['Cell', 'Pie', 'PieChart', 'ResponsiveContainer', 'Tooltip']
      .map((name) => [name, Object.assign(() => null, { displayName: name })]));
    const duration = loadPageModule('src/utils/formatDuration.ts', {}, driver);
    const { PerfSection } = loadPageModule('src/pages/OptimizationPage.tsx', {
      recharts,
      'react-router-dom': { useNavigate: () => () => {}, useParams: () => ({}) },
      '../i18n': { useI18n: () => ({ t: (key) => key }), useLocaleTag: () => 'en-US' },
      '../utils/apiClient': {},
      '../components/CopyButton': {},
      '../utils/formatDuration': duration,
      '../utils/accuracyAttribution': {},
      '../utils/richText': {},
      '../components/TokenFlameChart': { default: () => null },
    }, driver, '\nexports.PerfSection = PerfSection;');
    const perf = Object.freeze({
      wall_secs: model + tools + idle, model_secs: model, tool_secs: tools, idle_secs: idle,
      tool_count: tools > 0 ? 1 : 0, tool_calls: [], top_slow: [], idle_gaps: [], frag_idle_secs: 0,
    });
    const { element } = driver.render(PerfSection, { perf, issues: null, issuesState: 'idle' });
    const pie = collectElements(element, (node) => node.type === recharts.Pie)[0];
    assert.ok(pie, 'the production performance chart exists');
    const expectedSlices = expected.map(([kind, secs]) => [`opt.perf.slice.${kind}`, secs]);
    assert.deepEqual(pie.props.data.map((slice) => [slice.name, slice.secs]), expectedSlices);
    const legendLabels = collectElements(element, (node) => node.type === 'span'
      && node.props?.className === 'text-gray-600').map((node) => node.children[0]);
    assert.deepEqual(legendLabels, expectedSlices.map(([name]) => name));
    const centre = collectElements(element, (node) => node.type === 'span'
      && node.props?.className === 'text-sm font-semibold text-gray-900')[0];
    assert.equal(centre.children[0], duration.formatDurationSecs(perf.wall_secs));
  });
}

function collectElements(node, predicate, out = []) {
  if (node == null || typeof node !== 'object') return out;
  if (Array.isArray(node)) {
    for (const child of node) collectElements(child, predicate, out);
    return out;
  }
  if (predicate(node)) out.push(node);
  collectElements(node.children, predicate, out);
  return out;
}

// ─── Bug 3: model legend hides the series, it does not repaint it ────────────

test('conversation-list: hiding a model series uses hide, not a transparent fill', () => {
  const source = readSource('src/pages/ConversationList.tsx');
  const barsStart = source.indexOf('{models.map((m, i) => (');
  assert.ok(barsStart >= 0, 'the model bar series must exist');
  const barsEnd = source.indexOf('</BarChart>', barsStart);
  const bars = source.slice(barsStart, barsEnd);

  assert.match(
    bars,
    /hide=\{hidden\.has\(modelKey\(m\)\)\}/,
    'the model Bar must receive hide so Recharts drops it from the stack and the Y-axis domain',
  );
  assert.doesNotMatch(
    bars,
    /transparent/,
    'a transparent fill still reserves the stack height and still scales the Y axis',
  );
});

test('conversation-list: model names cannot overwrite the time-axis label', () => {
  const driver = createHooksDriver();
  const recharts = Object.fromEntries([
    'LineChart', 'Line', 'BarChart', 'Bar', 'XAxis', 'YAxis', 'CartesianGrid',
    'Tooltip', 'Legend', 'ResponsiveContainer',
  ].map((name) => [name, Object.assign(() => null, { displayName: name })]));
  const buckets = { exports: {} };
  new Function('require', 'module', 'exports', transpile('src/utils/timeseriesBuckets.ts'))(
    () => ({}), buckets, buckets.exports,
  );
  const page = loadPageModule('src/pages/ConversationList.tsx', {
    recharts,
    'react-router-dom': { useNavigate: () => () => {}, useSearchParams: () => [new URLSearchParams(), () => {}] },
    '../i18n': { useI18n: () => ({ t: (key) => key }), useLocaleTag: () => 'en-US' },
    '../utils/apiClient': {}, '../utils/datetime': {},
    '../utils/timeseriesBuckets': buckets.exports,
    '../components/InterruptionBadge': {}, '../components/InterruptionPanel': {},
    '../components/EvaluationBadge': {}, '../components/EvaluationPanel': {},
    '../components/DateTimePicker': {}, '../components/SessionIdHelp': {},
    '../components/SessionResourceChart': {},
  }, driver, '\nexports.ModelTimeseriesChart = ModelTimeseriesChart;');
  const startNs = Date.UTC(2026, 9, 1) * 1_000_000;
  const sourceData = [
    { bucket_start_ns: startNs, model: '', total_tokens: 5 },
    { bucket_start_ns: startNs, model: 'label', total_tokens: 37 },
    { bucket_start_ns: startNs, model: 'gpt-4.1', total_tokens: 12 },
  ];
  const frozenData = sourceData.map((row) => Object.freeze(row));
  const props = {
    data: frozenData,
    startNs, endNs: startNs + 60_000_000_000, bucketCount: 1,
  };
  const rendered = driver.render(page.ModelTimeseriesChart, props);
  const chart = collectElements(rendered.element, (node) => node.type === recharts.BarChart)[0];
  const axis = collectElements(rendered.element, (node) => node.type === recharts.XAxis)[0];
  const bars = collectElements(rendered.element, (node) => node.type === recharts.Bar);
  const bar = bars.find((node) => node.props.name === 'label');
  const data = chart.props.data[0];
  const { getValueByDataKey, getStackedData } = require('recharts/lib/util/ChartUtils');
  const read = (key) => getValueByDataKey(data, key);
  assert.match(read(axis.props.dataKey), /^\d{2}:\d{2}$/,
    'the X axis must stay a time label even when the model has that name');
  assert.equal(read(bar.props.dataKey), 37);
  assert.equal(bar.props.name, 'label');
  assert.equal(read(bars.find((node) => node.props.name === 'gpt-4.1').props.dataKey), 12,
    'ordinary dotted model identifiers retain their exact token value');
  const stack = getStackedData(chart.props.data, bars, 'none');
  assert.deepEqual(stack.map((series) => [series[0][0], series[0][1]]), [[0, 5], [5, 17], [17, 54]],
    'the real Recharts stacking consumer retains both exact model totals');
  assert.deepEqual(axis.props.ticks, [read(axis.props.dataKey)]);
  const legend = collectElements(rendered.element, (node) => node.type === recharts.Legend)[0];
  legend.props.onClick({ dataKey: bar.props.dataKey, value: bar.props.name });
  const hiddenRender = driver.render(page.ModelTimeseriesChart, props);
  const hiddenBars = collectElements(hiddenRender.element, (node) => node.type === recharts.Bar);
  const hiddenBar = hiddenBars.find((node) => node.props.name === 'label');
  assert.equal(hiddenBar.props.hide, true, 'the same named series still toggles in the legend');
  assert.equal(hiddenBars.find((node) => node.props.name === 'gpt-4.1').props.hide, false,
    'toggling one series leaves the other visible');
  const emptyBar = hiddenBars.find((node) => node.props.name === '');
  const nextLegend = collectElements(hiddenRender.element, (node) => node.type === recharts.Legend)[0];
  nextLegend.props.onClick({ dataKey: emptyBar.props.dataKey, value: emptyBar.props.dataKey, payload: emptyBar.props });
  const emptyRender = driver.render(page.ModelTimeseriesChart, props);
  const emptyHiddenBar = collectElements(emptyRender.element, (node) => node.type === recharts.Bar)
    .find((node) => node.props.name === '');
  assert.equal(emptyHiddenBar.props.hide, true, 'an empty recorded model name remains a valid series key');
});

// ─── Bug 4: the risk sort only covers one page of the server result ──────────

test('system-audit: risk sort is disabled and annotated while more than one page exists', () => {
  const source = readSource('src/pages/SystemAuditPage.tsx');

  assert.match(
    source,
    /const caseSortLocked = caseTotal > CASE_PAGE_SIZE;/,
    'the page must know when the local sort cannot cover the whole result',
  );

  const selectStart = source.indexOf('value={caseSort}');
  assert.ok(selectStart >= 0, 'the case sort control must exist');
  const select = source.slice(selectStart, source.indexOf('</select>', selectStart));
  assert.match(select, /disabled=\{caseSortLocked\}/,
    'the sort select must be disabled when only the loaded page would be sorted');
  assert.match(select, /title=\{caseSortLocked \?/,
    'the disabled control must explain why, instead of silently sorting one page');
});

// ─── Bug 5: violation offset must be clamped when the list shrinks ───────────

test('risk enforcement: the violation offset is clamped when the list shrinks', async () => {
  const { calls, stubs } = deferredFetchStubs([
    'fetchEnforcementHealth',
    'fetchEnforcementBindings',
    'fetchEnforcementViolations',
  ]);
  const driver = createHooksDriver();
  const moduleStubs = {
    'react-router-dom': { useSearchParams: () => [new URLSearchParams(), () => {}] },
    '../i18n': { useI18n: () => ({ t: (key) => key }), useLocaleTag: () => 'en-US' },
    '../utils/apiClient': {
      ...stubs,
      createCredentialBinding: async () => {},
      detachEnforcementBinding: async () => {},
      enforcementSupportsMode: () => true,
      enforcementViolationTotal: (violations) => violations.length,
    },
    // The mode badge reads a binding's effective mode; these pager fixtures
    // carry no bindings, so the stored-mode fallback is enough.
    '../utils/enforcementMode': {
      effectiveBindingMode: (request) => request.policy_mode ?? 'observe',
    },
    '../components/Pagination': componentStub('Pagination'),
  };
  const pageModule = loadPageModule('src/pages/RiskEnforcementPage.tsx', moduleStubs, driver);
  const page = pageModule.RiskEnforcementPage;
  assert.equal(typeof page, 'function', 'RiskEnforcementPage must be a component');

  const violation = (i) => ({
    event_id: `event-${i}`,
    binding_id: 'binding-1',
    agent_id: `agent-${i}`,
    session_id: null,
    policy_id: 'policy',
    policy_revision: '1',
    pid: 100 + i,
    ppid: null,
    process_start_time: 0,
    operation: 'open',
    target: '/tmp/secret',
    effect: 'notify',
    blocked: false,
    killed: false,
    rule_id: null,
    reason: null,
    occurred_at_ns: 1_000 + i,
    observed_at_ns: 1_000 + i,
    actplane_revision: '1',
  });
  const health = {
    ready: true,
    backend: 'test',
    capabilities: {
      max_active_bindings: 10,
      credential_observe: true,
      credential_audit: true,
      credential_enforce: true,
      policy_handoff: true,
      alternate_pid_retarget: false,
      test_development: true,
    },
    message: null,
  };
  const violationRows = (element) => collectElements(
    element,
    (node) => node.type === 'tr' && typeof node.props?.key === 'string' && node.props.key.startsWith('event-'),
  );

  // First load answers with 25 violations (slots: 2 violations, 19 offset).
  let rendered = driver.render(page);
  rendered.effects[0]();
  assert.equal(calls.fetchEnforcementViolations.length, 1);
  calls.fetchEnforcementHealth[0].resolve(health);
  calls.fetchEnforcementBindings[0].resolve({ bindings: [] });
  calls.fetchEnforcementViolations[0].resolve({
    violations: Array.from({ length: 25 }, (_, i) => violation(i)),
  });
  await settle();
  await settle();
  assert.equal(driver.slots[2].value.length, 25, 'sanity: the first load must store 25 violations');

  // The user pages to offset 20, the last page.
  driver.slots[19].setter(20);
  rendered = driver.render(page);
  assert.equal(violationRows(rendered.element).length, 5, 'sanity: offset 20 shows the last five rows');

  // A refresh prunes the list down to 15 items while the offset stays at 20.
  rendered.effects[0]();
  calls.fetchEnforcementHealth[1].resolve(health);
  calls.fetchEnforcementBindings[1].resolve({ bindings: [] });
  calls.fetchEnforcementViolations[1].resolve({
    violations: Array.from({ length: 15 }, (_, i) => violation(i)),
  });
  await settle();
  await settle();

  // React would run the clamp effect after the violations write; effect order
  // is [loadAll, highlight-binding, clamp-offset].
  rendered = driver.render(page);
  if (typeof rendered.effects[2] === 'function') rendered.effects[2]();
  rendered = driver.render(page);

  assert.equal(
    driver.slots[19].value,
    14,
    "the offset must be clamped to the shortened list's last index",
  );
  assert.equal(violationRows(rendered.element).length, 1,
    'the clamped page must not be empty: it must fall back to the remaining row');
});

// ─── Bug 5: the sessions stat card reports a page instead of the total ───────

test('system-audit: the sessions card does not fall back to the loaded page length', () => {
  const source = readSource('src/pages/SystemAuditPage.tsx');

  // `sessions` is one page of at most SESSION_PAGE_SIZE rows (`fetchAuditSessions`
  // with `limit: SESSION_PAGE_SIZE`, the response's `total` goes to sessionTotal),
  // so falling back to `sessions.length` printed the page size as a headline count
  // whenever the summary request failed — while the neighbouring cards read 0 and
  // the sessions tab of the same page showed "1–20 / N". The server total is
  // already in scope and is what the case card next to it uses.
  assert.match(
    source,
    /labelKey="audit\.stats\.sessions\.label" value=\{summary\?\.affected_sessions \?\? sessionTotal\}/,
    'the sessions card must fall back to the server total, not the loaded page',
  );
  assert.doesNotMatch(
    source,
    /affected_sessions \?\? sessions\.length/,
    'the loaded page length must never be reported as the session count',
  );
});

test('conversation-list: case variants across buckets form one model series', () => {
  const driver = createHooksDriver();
  const recharts = Object.fromEntries([
    'LineChart', 'Line', 'BarChart', 'Bar', 'XAxis', 'YAxis', 'CartesianGrid',
    'Tooltip', 'Legend', 'ResponsiveContainer',
  ].map((name) => [name, Object.assign(() => null, { displayName: name })]));
  const buckets = { exports: {} };
  new Function('require', 'module', 'exports', transpile('src/utils/timeseriesBuckets.ts'))(
    () => ({}), buckets, buckets.exports,
  );
  const page = loadPageModule('src/pages/ConversationList.tsx', {
    recharts,
    'react-router-dom': { useNavigate: () => () => {}, useSearchParams: () => [new URLSearchParams(), () => {}] },
    '../i18n': { useI18n: () => ({ t: (key) => key }), useLocaleTag: () => 'en-US' },
    '../utils/apiClient': {}, '../utils/datetime': {},
    '../utils/timeseriesBuckets': buckets.exports,
    '../components/InterruptionBadge': {}, '../components/InterruptionPanel': {},
    '../components/EvaluationBadge': {}, '../components/EvaluationPanel': {},
    '../components/DateTimePicker': {}, '../components/SessionIdHelp': {},
    '../components/SessionResourceChart': {},
  }, driver, '\nexports.ModelTimeseriesChart = ModelTimeseriesChart;');
  const startNs = Date.UTC(2026, 9, 1) * 1_000_000;
  const sourceData = [
    { bucket_start_ns: startNs, model: 'gpt-4o', total_tokens: 37 },
    { bucket_start_ns: startNs + 30_000_000_000, model: 'GPT-4o', total_tokens: 12 },
  ];
  const frozenData = sourceData.map((row) => Object.freeze(row));
  const props = {
    data: frozenData,
    startNs, endNs: startNs + 60_000_000_000, bucketCount: 2,
  };
  const rendered = driver.render(page.ModelTimeseriesChart, props);
  const chart = collectElements(rendered.element, (node) => node.type === recharts.BarChart)[0];
  const axis = collectElements(rendered.element, (node) => node.type === recharts.XAxis)[0];
  const bars = collectElements(rendered.element, (node) => node.type === recharts.Bar);
  assert.equal(bars.length, 1, 'case variants in separate time buckets must be one model series');
  const { getValueByDataKey, getStackedData } = require('recharts/lib/util/ChartUtils');
  assert.deepEqual(chart.props.data.map((row) => getValueByDataKey(row, bars[0].props.dataKey)), [37, 12]);
  assert.deepEqual(getStackedData(chart.props.data, bars, 'none')[0].map((point) => point[1] - point[0]), [37, 12]);
  const renderWith = (data) => driver.render(page.ModelTimeseriesChart, { ...props, data });
  const reverse = renderWith([...sourceData].reverse());
  const reverseBars = collectElements(reverse.element, (node) => node.type === recharts.Bar);
  assert.deepEqual(reverseBars.map((node) => node.props.name), bars.map((node) => node.props.name),
    'a bucket order change must not rename the one model series');
  const legend = collectElements(reverse.element, (node) => node.type === recharts.Legend)[0];
  legend.props.onClick({ dataKey: reverseBars[0].props.dataKey, value: reverseBars[0].props.name, payload: reverseBars[0].props });
  const hidden = renderWith(sourceData);
  assert.equal(collectElements(hidden.element, (node) => node.type === recharts.Bar)[0].props.hide, true);
  const lowercaseOnly = renderWith(sourceData.map((row) => ({ ...row, model: 'gpt-4o' })));
  const lowerBar = collectElements(lowercaseOnly.element, (node) => node.type === recharts.Bar)[0];
  assert.equal(lowerBar.props.name, 'gpt-4o');
  assert.equal(lowerBar.props.hide, true, 'a case-only refresh must preserve hidden model identity');
  const lowerLegend = collectElements(lowercaseOnly.element, (node) => node.type === recharts.Legend)[0];
  assert.equal(lowerLegend.props.formatter(lowerBar.props.name, { dataKey: lowerBar.props.dataKey, payload: lowerBar.props, color: '#123' }).props.style.color, '#aaa');
  lowerLegend.props.onClick({ dataKey: lowerBar.props.dataKey, value: lowerBar.props.name, payload: lowerBar.props });
  const uppercaseOnly = renderWith(sourceData.map((row) => ({ ...row, model: 'GPT-4o' })));
  const upperBar = collectElements(uppercaseOnly.element, (node) => node.type === recharts.Bar)[0];
  assert.equal(upperBar.props.hide, false, 'showing a model with a new spelling clears its existing hidden identity');
  const upperLegend = collectElements(uppercaseOnly.element, (node) => node.type === recharts.Legend)[0];
  upperLegend.props.onClick({ dataKey: upperBar.props.dataKey, value: upperBar.props.name, payload: upperBar.props });
  const lowercaseAgain = renderWith(sourceData.map((row) => ({ ...row, model: 'gpt-4o' })));
  assert.equal(collectElements(lowercaseAgain.element, (node) => node.type === recharts.Bar)[0].props.hide, true);
  const unicode = renderWith([
    { bucket_start_ns: startNs, model: 'Ä', total_tokens: 2 },
    { bucket_start_ns: startNs + 30_000_000_000, model: 'ä', total_tokens: 3 },
  ]);
  const unicodeBars = collectElements(unicode.element, (node) => node.type === recharts.Bar);
  assert.equal(unicodeBars.length, 2, 'SQLite NOCASE does not collapse non-ASCII model letters');
  const ordinary = renderWith([
    { bucket_start_ns: startNs, model: 'gpt-4o', total_tokens: 2 },
    { bucket_start_ns: startNs + 30_000_000_000, model: 'claude-sonnet', total_tokens: 3 },
  ]);
  assert.equal(collectElements(ordinary.element, (node) => node.type === recharts.Bar).length, 2,
    'unrelated models stay separate');
});
