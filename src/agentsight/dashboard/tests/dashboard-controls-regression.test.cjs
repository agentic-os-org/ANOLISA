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
    /hide=\{hidden\.has\(m\)\}/,
    'the model Bar must receive hide so Recharts drops it from the stack and the Y-axis domain',
  );
  assert.doesNotMatch(
    bars,
    /transparent/,
    'a transparent fill still reserves the stack height and still scales the Y axis',
  );
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
