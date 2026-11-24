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

for (const cached of [{ summary: 'older cached evaluation' }, null]) {
  test(`a background evaluation lookup cannot replace a completed manual result (${cached ? 'cached' : 'empty'})`, async () => {
    const driver = createHooksDriver();
    const { calls, stubs } = deferredFetchStubs(['fetchTraces', 'fetchLatestEvaluation']);
    const EvaluationPanel = () => null;
    const dependencies = {
      recharts: {},
      'react-router-dom': { useNavigate: () => () => {}, useSearchParams: () => [new URLSearchParams(), () => {}] },
      '../i18n': { useI18n: () => ({ t: (key) => key }), useLocaleTag: () => 'en-US' },
      '../utils/apiClient': { ...stubs, conversationInterruptionKey: (s, c) => `${s}:${c}` },
      '../utils/datetime': { formatNsPadded: () => 'time' },
      '../utils/timeseriesBuckets': {},
      '../components/EvaluationPanel': { EvaluationPanel },
    };
    for (const name of ['InterruptionBadge', 'InterruptionPanel', 'EvaluationBadge', 'DateTimePicker', 'CopyButton', 'SessionIdHelp', 'SessionResourceChart']) {
      dependencies[`../components/${name}`] = componentStub(name);
    }
    const { TraceSubTable } = loadPageModule('src/pages/ConversationList.tsx', dependencies, driver,
      '\nmodule.exports.TraceSubTable = TraceSubTable;');
    const props = { sessionId: 'session', detailsId: 'details', conversationInterruptionCounts: new Map() };
    let rendered = driver.render(TraceSubTable, props);
    rendered.effects[0]();
    calls.fetchTraces[0].resolve(['first', 'second'].map((conversation_id) => ({
      conversation_id, call_count: 1, total_input_tokens: 10, total_output_tokens: 5,
      start_ns: 1, end_ns: 2, user_query: 'question', model: 'model',
    })));
    await settle();
    rendered = driver.render(TraceSubTable, props);
    rendered.effects[1]();
    assert.deepEqual(calls.fetchLatestEvaluation.map(call => call.args[0]), ['first', 'second']);
    // Open the evaluation disclosure while its automatic cached lookup waits.
    const buttons = collectElements(rendered.element, node => node.type === 'button');
    const button = buttons.find(node => node.props.onClick?.toString().includes('setExpandedEvaluationPanel'));
    assert.ok(button);
    button.props.onClick({ stopPropagation() {} });
    rendered = driver.render(TraceSubTable, props);
    const panel = collectElements(rendered.element, node => node.type === EvaluationPanel)[0];
    const fresh = { summary: 'fresh manual evaluation' };
    panel.props.onResult(fresh);
    calls.fetchLatestEvaluation[0].resolve(cached);
    const other = { summary: 'other conversation evaluation' };
    calls.fetchLatestEvaluation[1].resolve(other);
    await settle();
    rendered = driver.render(TraceSubTable, props);
    assert.equal(collectElements(rendered.element, node => node.type === EvaluationPanel)[0].props.initialResult, fresh);
    assert.equal(driver.slots[6].value.get('second'), other,
      'protecting the manual result must still hydrate unrelated rows');
  });
}

// Mirrors the shared helper's contract: the evidence link may only name a case
// belonging to a visible (agent-filtered) violation row. The real derivation is
// covered against the compiled module in apiClient-regression.
const firstCorrelatedCaseIdStub = (violations, agentIdFilter) => (
  (agentIdFilter ? violations.filter((v) => v.agent_id === agentIdFilter) : violations)
    .find((v) => v.case_id)?.case_id
);

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
      '../utils/clipboard': { copyText: () => {} },
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


function protectionFixture(api) {
  const driver = createHooksDriver();
  const { AgentCard } = loadPageModule('src/pages/AgentHealthPage.tsx', {
    '../utils/apiClient': api,
    '../i18n': { useI18n: () => ({ t: key => key }), useLocaleTag: () => 'en-US', INTERRUPTION_TYPES: [] },
    '../utils/datetime': { formatNs: String },
    '../constants/policyIds': { CREDENTIAL_POLICY_ID: 'credential' },
    '../components/CacheDiagnosisPanel': componentStub('CacheDiagnosisPanel'),
  }, driver, '\nexports.AgentCard = AgentCard;');
  const notices = [];
  const props = { agent: { pid: 123, agent_name: 'fixture', role: 'gateway', status: 'healthy',
    workspace_path: '/project', ports: [8080], last_seen: 0 }, related: [],
    onDelete() {}, onRestart() {}, restarting: false, addToast: message => notices.push(message) };
  const render = () => driver.render(AgentCard, props).element;
  const button = label => collectElements(render(), node => node.type === 'button' && node.children.includes(label))[0];
  const open = () => button('保护设置').props.onClick();
  return { driver, render, button, open, notices };
}

for (const [draft, trusted] of [['', ['192.0.2.1:443']], ['198.51.100.1:443', []]]) {
  test(`one-click protection uses preview targets instead of the draft (${draft || 'empty'})`, async () => {
    const { calls, stubs } = deferredFetchStubs(['fetchAgentProtectionPreview', 'createCredentialBinding']);
    const fixture = protectionFixture(stubs);
    if (draft) {
      fixture.open();
      calls.fetchAgentProtectionPreview[0].resolve({ workspace_path: '/project', source_paths: ['/project/.env'], trusted_endpoints: [] });
      await settle();
      const target = collectElements(fixture.render(), node => node.type === 'input' && node.props.placeholder?.includes('10.0.0.8'))[0];
      target.props.onChange({ target: { value: draft } });
      fixture.button('取消').props.onClick();
    }
    collectElements(fixture.render(), node => node.props?.role === 'switch')[0].props.onClick();
    const preview = calls.fetchAgentProtectionPreview.at(-1);
    preview.resolve({ workspace_path: '/project', source_paths: ['/project/.env'], trusted_endpoints: trusted });
    await settle();
    assert.deepEqual(calls.createCredentialBinding[0].args[0].trusted_endpoints, trusted,
      'one-click activation must use the preview returned for this activation');
    assert.deepEqual(calls.createCredentialBinding[0].args[0].source_paths, ['/project/.env']);
    calls.createCredentialBinding[0].resolve({ request: { binding_id: 'created' } });
    await settle();
  });
}

test('manual protection keeps the explicitly edited target', async () => {
  const { calls, stubs } = deferredFetchStubs(['fetchAgentProtectionPreview', 'createCredentialBinding']);
  const fixture = protectionFixture(stubs);
  fixture.open();
  calls.fetchAgentProtectionPreview[0].resolve({ workspace_path: '/project', source_paths: ['/project/.env'], trusted_endpoints: ['192.0.2.1:443'] });
  await settle();
  collectElements(fixture.render(), node => node.type === 'input' && node.props.placeholder?.includes('10.0.0.8'))[0]
    .props.onChange({ target: { value: '198.51.100.1:443' } });
  fixture.button('开启审计保护').props.onClick();
  assert.deepEqual(calls.createCredentialBinding[0].args[0].trusted_endpoints, ['198.51.100.1:443']);
  calls.createCredentialBinding[0].resolve({ request: { binding_id: 'created' } });
  await settle();
});

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
    '../components/CopyButton': componentStub('CopyButton'),
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
      firstCorrelatedCaseId: firstCorrelatedCaseIdStub,
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
    0,
    'shrinking from 25 to 15 rows (<= limit 20) must align offset back to page 1 (0), not row 14',
  );
  assert.equal(
    violationRows(rendered.element).length,
    15,
    'all 15 remaining violations must be visible once Pagination hides itself for total <= limit',
  );

  // Multi-page shrink: 45 violations on page 3 (offset 40) shrinking to 35
  // must land on page 2 (offset 20, showing 15 rows), not an unaligned offset 34.
  rendered.effects[0]();
  calls.fetchEnforcementHealth[2].resolve(health);
  calls.fetchEnforcementBindings[2].resolve({ bindings: [] });
  calls.fetchEnforcementViolations[2].resolve({
    violations: Array.from({ length: 45 }, (_, i) => violation(i)),
  });
  await settle();
  await settle();

  driver.slots[19].setter(40);
  rendered = driver.render(page);
  assert.equal(violationRows(rendered.element).length, 5, 'sanity: offset 40 shows the last five of 45 rows');

  rendered.effects[0]();
  calls.fetchEnforcementHealth[3].resolve(health);
  calls.fetchEnforcementBindings[3].resolve({ bindings: [] });
  calls.fetchEnforcementViolations[3].resolve({
    violations: Array.from({ length: 35 }, (_, i) => violation(i)),
  });
  await settle();
  await settle();

  rendered = driver.render(page);
  if (typeof rendered.effects[2] === 'function') rendered.effects[2]();
  rendered = driver.render(page);

  assert.equal(
    driver.slots[19].value,
    20,
    'shrinking from 45 to 35 rows must align offset to the last full page start (20), not row 34',
  );
  assert.equal(
    violationRows(rendered.element).length,
    15,
    'page 2 of 35 rows must render all 15 rows on that page',
  );

  // Boundary: exact single-page shrink (20 rows) and empty list (0 rows).
  rendered.effects[0]();
  calls.fetchEnforcementHealth[4].resolve(health);
  calls.fetchEnforcementBindings[4].resolve({ bindings: [] });
  calls.fetchEnforcementViolations[4].resolve({
    violations: Array.from({ length: 20 }, (_, i) => violation(i)),
  });
  await settle();
  await settle();

  rendered = driver.render(page);
  if (typeof rendered.effects[2] === 'function') rendered.effects[2]();
  rendered = driver.render(page);

  assert.equal(driver.slots[19].value, 0, 'shrinking to exactly one page (20 rows) must reset offset to 0');
  assert.equal(violationRows(rendered.element).length, 20, 'all 20 rows must render on page 1');

  rendered.effects[0]();
  calls.fetchEnforcementHealth[5].resolve(health);
  calls.fetchEnforcementBindings[5].resolve({ bindings: [] });
  calls.fetchEnforcementViolations[5].resolve({ violations: [] });
  await settle();
  await settle();

  rendered = driver.render(page);
  if (typeof rendered.effects[2] === 'function') rendered.effects[2]();
  rendered = driver.render(page);

  assert.equal(driver.slots[19].value, 0, 'shrinking to an empty list must keep offset at 0');
  assert.equal(violationRows(rendered.element).length, 0, 'empty list renders zero violation rows');
});

test('read-denial template is capability-gated and submits a file read binding', async () => {
  for (const supported of [false, true]) {
    const driver = createHooksDriver();
    const requests = [];
    const page = loadPageModule('src/pages/RiskEnforcementPage.tsx', {
      'react-router-dom': { useSearchParams: () => [new URLSearchParams(), () => {}] },
      '../i18n': { useI18n: () => ({ t: (key) => key }), useLocaleTag: () => 'en-US' },
      '../utils/apiClient': {
        fetchEnforcementHealth: async () => ({ ready: true, capabilities: { file_read_protection: supported } }),
        fetchEnforcementBindings: async () => ({ bindings: [] }),
        fetchEnforcementViolations: async () => ({ violations: [] }),
        enforcementSupportsMode: () => false,
        enforcementViolationTotal: () => 0,
        createFileReadBinding: async (request) => requests.push(request),
        createCredentialBinding: async () => assert.fail('read denial must not apply a credential policy'),
      },
      '../utils/enforcementMode': {
        effectiveBindingMode: () => 'enforce',
        firstCorrelatedCaseId: firstCorrelatedCaseIdStub,
      },
      '../components/Pagination': componentStub('Pagination'),
    }, driver).RiskEnforcementPage;
    let rendered = driver.render(page);
    rendered.effects[0]();
    await settle();
    rendered = driver.render(page);
    const template = collectElements(rendered.element, (node) => node.type === 'select')[0];
    template.props.onChange({ target: { value: 'deny_read' } });
    rendered = driver.render(page);
    const required = collectElements(rendered.element, (node) => node.type === 'input' && node.props.required);
    ['fixture', '123', '/tmp/secret.key'].forEach((value, index) => required[index].props.onChange({ target: { value } }));
    rendered = driver.render(page);
    const submit = collectElements(rendered.element, (node) => node.type === 'button' && node.props.type === 'submit')[0];
    assert.equal(submit.props.disabled, !supported);
    await collectElements(rendered.element, (node) => node.type === 'form')[0].props.onSubmit({ preventDefault() {} });
    assert.equal(requests.length, supported ? 1 : 0);
    if (supported) assert.deepEqual(requests[0], { agent_id: 'fixture', root_pid: 123, path: '/tmp/secret.key', session_id: undefined });
  }
});

async function renderRiskEnforcementCapabilities(capabilities) {
  const { calls, stubs } = deferredFetchStubs([
    'fetchEnforcementHealth',
    'fetchEnforcementBindings',
    'fetchEnforcementViolations',
  ]);
  const driver = createHooksDriver();
  const pageModule = loadPageModule('src/pages/RiskEnforcementPage.tsx', {
    'react-router-dom': { useSearchParams: () => [new URLSearchParams(), () => {}] },
    '../i18n': {
      useI18n: () => ({ t: (key, params) => params?.mode ? `${key} ${params.mode}` : key }),
      useLocaleTag: () => 'en-US',
    },
    '../utils/apiClient': {
      ...stubs,
      createCredentialBinding: async () => {},
      detachEnforcementBinding: async () => {},
      enforcementSupportsMode: () => true,
      enforcementViolationTotal: (violations) => violations.length,
    },
    '../utils/enforcementMode': {
      effectiveBindingMode: (request) => request.policy_mode ?? 'observe',
      firstCorrelatedCaseId: firstCorrelatedCaseIdStub,
    },
    '../components/Pagination': componentStub('Pagination'),
  }, driver);

  const rendered = driver.render(pageModule.RiskEnforcementPage);
  rendered.effects[0]();
  calls.fetchEnforcementHealth[0].resolve({
    ready: true,
    backend: 'actplane',
    capabilities,
    message: null,
  });
  calls.fetchEnforcementBindings[0].resolve({ bindings: [] });
  calls.fetchEnforcementViolations[0].resolve({ violations: [] });
  await settle();
  await settle();
  return driver.render(pageModule.RiskEnforcementPage).element;
}

function collectText(node, out = []) {
  if (node == null || typeof node === 'boolean') return out;
  if (typeof node === 'string' || typeof node === 'number') {
    out.push(String(node));
    return out;
  }
  if (Array.isArray(node)) {
    for (const child of node) collectText(child, out);
    return out;
  }
  if (typeof node === 'object') collectText(node.children, out);
  return out;
}

const productionCapabilities = {
  max_active_bindings: 10,
  credential_observe: true,
  credential_audit: true,
  credential_enforce: false,
  policy_handoff: false,
  alternate_pid_retarget: false,
  test_development: false,
};

for (const { name, capabilities, expected, absent } of [
  {
    name: 'file delete guard reports its runtime mode',
    capabilities: {
      ...productionCapabilities,
      file_delete_guard: true,
      file_delete_guard_mode: 'inode',
    },
    expected: ['risk.capability.prod', 'risk.capability.fileDeleteGuardWithMode inode'],
    absent: ['risk.capability.testDev'],
  },
  {
    name: 'legacy health without guard fields keeps the existing display',
    capabilities: productionCapabilities,
    expected: ['risk.capability.prod'],
    absent: ['risk.capability.fileDeleteGuard'],
  },
  {
    name: 'test backend and a mode-less guard keep independent labels',
    capabilities: {
      ...productionCapabilities,
      credential_enforce: true,
      policy_handoff: true,
      test_development: true,
      file_delete_guard: true,
      file_delete_guard_mode: null,
    },
    expected: ['risk.capability.testDev', 'risk.capability.fileDeleteGuard'],
    absent: ['risk.capability.prod', 'risk.capability.fileDeleteGuardWithMode'],
  },
]) {
  test(`risk enforcement: ${name}`, async () => {
    const text = collectText(await renderRiskEnforcementCapabilities(capabilities));
    for (const value of expected) {
      assert.ok(text.includes(value), `missing ${value} in ${JSON.stringify(text)}`);
    }
    for (const prefix of absent) {
      assert.ok(!text.some((value) => value.startsWith(prefix)),
        `unexpected ${prefix} in ${JSON.stringify(text)}`);
    }
  });
}

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
    '../components/CopyButton': componentStub('CopyButton'),
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

test('conversation-list: session disclosure and time presets expose accessible state', async () => {
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
  const DateTimePicker = () => null;
  const recharts = Object.fromEntries([
    'LineChart', 'Line', 'BarChart', 'Bar', 'XAxis', 'YAxis', 'CartesianGrid',
    'Tooltip', 'Legend', 'ResponsiveContainer',
  ].map((name) => [name, Object.assign(() => null, { displayName: name })]));
  const page = loadPageModule('src/pages/ConversationList.tsx', {
    recharts,
    'react-router-dom': {
      useNavigate: () => () => {},
      useSearchParams: () => [new URLSearchParams(), () => {}],
    },
    '../i18n': { useI18n: () => ({ t: (key) => key }), useLocaleTag: () => 'en-US' },
    '../utils/apiClient': {
      ...stubs,
      conversationInterruptionKey: (sessionId, conversationId) => `${sessionId}:${conversationId}`,
      UNASSIGNED_INTERRUPTION_BUCKET: '__unassigned__',
    },
    '../utils/datetime': { formatNsPadded: () => '2026-10-28 00:00:00' },
    '../utils/timeseriesBuckets': { fillModelBuckets: () => [], fillTokenBuckets: () => [] },
    '../components/InterruptionBadge': componentStub('InterruptionBadge'),
    '../components/InterruptionPanel': componentStub('InterruptionPanel'),
    '../components/EvaluationBadge': componentStub('EvaluationBadge'),
    '../components/EvaluationPanel': componentStub('EvaluationPanel'),
    '../components/DateTimePicker': { DateTimePicker },
    '../components/SessionIdHelp': componentStub('SessionIdHelp'),
    '../components/SessionResourceChart': componentStub('SessionResourceChart'),
    '../components/CopyButton': componentStub('CopyButton'),
  }, driver);

  let rendered = driver.render(page.ConversationList);
  for (const effect of rendered.effects) effect();
  calls.fetchAgentNames[0].resolve([]);
  calls.fetchSessions[0].resolve([{
    session_id: 'customer session 1',
    conversation_count: 1,
    first_seen_ns: 1,
    last_seen_ns: 2,
    total_input_tokens: 3,
    total_output_tokens: 4,
    model: 'gpt-4o',
    agent_name: 'fixture-agent',
  }]);
  calls.fetchTimeseries[0].resolve({ token_series: [], model_series: [] });
  calls.fetchInterruptionCount[0].resolve(null);
  calls.fetchInterruptionStats[0].resolve([]);
  calls.fetchInterruptionSessionCounts[0].resolve([]);
  calls.fetchInterruptionConversationCounts[0].resolve([]);
  calls.fetchTokenSavings[0].resolve({ sessions: [] });
  await settle();
  await settle();
  rendered = driver.render(page.ConversationList);

  const nodeText = (node) => {
    if (node == null || typeof node === 'boolean') return '';
    if (typeof node === 'string' || typeof node === 'number') return String(node);
    if (Array.isArray(node)) return node.map(nodeText).join('');
    return nodeText(node.children);
  };
  const presets = (element) => collectElements(element, (node) => node.type === 'button')
    .filter((node) => /^common\.last(?:1h|6h|24h|7d)$/.test(nodeText(node)));
  let buttons = presets(rendered.element);
  assert.equal(buttons.length, 4);
  assert.deepEqual(buttons.map((button) => button.props['aria-pressed']), [false, false, true, false],
    'the default last-24-hours range must expose its selected preset');

  buttons[0].props.onClick();
  rendered = driver.render(page.ConversationList);
  buttons = presets(rendered.element);
  assert.equal(buttons[0].props['aria-pressed'], true);
  assert.deepEqual(buttons.slice(1).map((button) => button.props['aria-pressed']), [false, false, false]);

  const startPicker = collectElements(rendered.element, (node) => node.type === DateTimePicker)
    .find((node) => node.props.label === 'common.startTime');
  startPicker.props.onChange(12345);
  rendered = driver.render(page.ConversationList);
  for (const button of presets(rendered.element)) assert.equal(button.props['aria-pressed'], false);

  const disclosure = collectElements(rendered.element, (node) => node.type === 'button'
    && typeof node.props['aria-expanded'] === 'boolean')[0];
  assert.ok(disclosure, 'the session ID must be a native disclosure button');
  assert.equal(disclosure.props.type, 'button');
  assert.equal(disclosure.props['aria-expanded'], false);
  assert.ok(disclosure.props['aria-label'].includes('customer session 1'));
  assert.match(disclosure.props['aria-controls'], /^session-details-\d+$/,
    'the controlled ID must remain a single DOM-safe IDREF for arbitrary session IDs');
  assert.equal(collectElements(disclosure, (node) => node.props?.['aria-hidden'] === 'true').length, 1,
    'the decorative expansion arrow must be hidden from assistive technology');

  let propagationStopped = false;
  disclosure.props.onClick({ stopPropagation: () => { propagationStopped = true; } });
  assert.ok(propagationStopped, 'the button click must not also toggle through the row handler');
  rendered = driver.render(page.ConversationList);
  const expanded = collectElements(rendered.element, (node) => node.type === 'button'
    && typeof node.props['aria-expanded'] === 'boolean')[0];
  assert.equal(expanded.props['aria-expanded'], true);
  const details = collectElements(rendered.element, (node) => node.type?.name === 'TraceSubTable')[0];
  assert.ok(details, 'the disclosure must render the session details');
  assert.equal(details.props.detailsId, expanded.props['aria-controls']);

  expanded.props.onClick({ stopPropagation: () => {} });
  rendered = driver.render(page.ConversationList);
  assert.equal(collectElements(rendered.element, (node) => node.type?.name === 'TraceSubTable').length, 0);
});


for (const outcome of ['success', 'failure', 'unmount']) {
  test(`agent activity: superseded poll ${outcome} cannot change the displayed snapshot`, async t => {
    const driver = createHooksDriver();
    const { calls, stubs } = deferredFetchStubs(['fetchAgentHealth']);
    const { AgentActivitySection } = loadPageModule('src/pages/AgentHealthPage.tsx', {
      '../utils/apiClient': stubs,
      '../i18n': { useI18n: () => ({ t: (key, args) => key + (args ? JSON.stringify(args) : '') }), useLocaleTag: () => 'en-US', INTERRUPTION_TYPES: [] },
      '../utils/datetime': { formatNs: String },
      '../constants/policyIds': {},
      '../components/CacheDiagnosisPanel': componentStub('CacheDiagnosisPanel'),
    }, driver, '\nexports.AgentActivitySection = AgentActivitySection;');
    const previousInterval = global.setInterval, previousClear = global.clearInterval;
    let poll;
    global.setInterval = fn => { poll = fn; return 1; };
    global.clearInterval = () => {};
    t.after(() => { global.setInterval = previousInterval; global.clearInterval = previousClear; });
    const fixture = name => ({ agents: [{ agent_name: name, source: 'trajectories', trajectory_steps: 9, trajectory_tokens: 100, last_seen_ns: 0 }], warnings: [name] });
    const cleanup = driver.render(AgentActivitySection).effects[0]();
    poll();
    calls.fetchAgentHealth[1].resolve(fixture('new-agent'));
    await settle();
    const snapshot = () => JSON.stringify(driver.render(AgentActivitySection).element);
    const expected = snapshot();
    assert.ok(expected.includes('new-agent'));
    if (outcome === 'unmount') {
      poll();
      cleanup();
      calls.fetchAgentHealth[2].resolve(fixture('after-unmount'));
    }
    if (outcome === 'failure') calls.fetchAgentHealth[0].reject(new Error('obsolete failure'));
    else calls.fetchAgentHealth[0].resolve(fixture('old-agent'));
    await settle();
    assert.equal(snapshot(), expected, 'metrics and source warnings belong to the newest live poll');
    cleanup();
  });
}

for (const [tab, api, row] of [
  ['cases', 'fetchSecurityCases', { case_id: 'old-case', agent_id: 'agent', summary: 'old evidence', severity: 'high', status: 'open', risk_score: 50, updated_at_ns: 1 }],
  ['sessions', 'fetchAuditSessions', { session_id: 'old-session', security_event_count: 3 }],
  ['events', 'fetchAuditEvents', { event_id: 'old-event', session_id: 'old-session', event_type: 'file_access' }],
]) {
  test(`system audit: failed ${tab} reload removes the previous page and recovers`, async () => {
    const driver = createHooksDriver();
    const { calls, stubs } = deferredFetchStubs([
      'fetchAuditSummary', 'fetchSecurityCases', 'fetchAuditSessions',
      'fetchAuditEvents', 'fetchEnforcementBindings',
    ]);
    const { SystemAuditPage } = loadPageModule('src/pages/SystemAuditPage.tsx', {
      'react-router-dom': { useNavigate: () => () => {}, useSearchParams: () => [new URLSearchParams(), () => {}] },
      '../i18n': { useI18n: () => ({ t: key => key, locale: 'en' }), useLocaleTag: () => 'en-US' },
      '../components/ContainmentDialog': componentStub('ContainmentDialog'),
      '../components/ContainmentLifecycleCard': componentStub('ContainmentLifecycleCard'),
      '../components/Pagination': componentStub('Pagination'),
      '../utils/enforcementMode': { effectiveBindingMode: () => 'audit' },
      '../utils/ruleReason': { translateRuleReason: text => text },
      '../utils/apiClient': stubs,
    }, driver);
    const answer = async (failed = false, items = [row]) => {
      for (const name of Object.keys(calls)) {
        const pending = calls[name].at(-1);
        if (name === api && failed) pending.reject(new Error('database unavailable'));
        else if (name === 'fetchAuditSummary') pending.resolve({ data: { total: 17 } });
        else if (name === 'fetchEnforcementBindings') pending.resolve({ bindings: [] });
        else pending.resolve({ data: { items: name === api ? items : [], total: name === api ? 80 : 0, next_offset: name === api ? 50 : null } });
      }
      await settle();
    };
    const rowKey = row.case_id || row.event_id || row.session_id;
    const rows = rendered => collectElements(rendered.element, el => el.props?.key === rowKey);
    let rendered = driver.render(SystemAuditPage);
    rendered.effects[0]();
    await answer();
    rendered = driver.render(SystemAuditPage);
    collectElements(rendered.element, el => el.type === 'button' && el.children.includes(`audit.tab.${tab}`))[0].props.onClick();
    rendered = driver.render(SystemAuditPage);
    assert.ok(rows(rendered).length, 'the initial row is visible');
    const refresh = () => collectElements(driver.render(SystemAuditPage).element,
      el => el.type === 'button' && el.children.includes('common.refresh'))[0].props.onClick();
    refresh();
    await answer(true);
    rendered = driver.render(SystemAuditPage);
    assert.equal(rows(rendered).length, 0, 'failed reload must not show an old row as current');
    assert.ok(collectElements(rendered.element, el => el.children?.includes('database unavailable')).length);
    assert.ok(collectElements(rendered.element, el => el.props?.labelKey === 'audit.stats.totalEvents.label' && el.props.value === 17).length,
      'an independently successful summary remains available');
    if (tab === 'events') assert.equal(collectElements(rendered.element, el => el.children?.includes('加载更多')).length, 0,
      'a failed first page cannot retain its old continuation');
    refresh();
    await answer();
    rendered = driver.render(SystemAuditPage);
    assert.ok(rows(rendered).length, 'successful retry restores the row');
  });
}
