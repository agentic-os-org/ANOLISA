const assert = require('node:assert/strict');
const { join } = require('node:path');
const { readFileSync } = require('node:fs');
const test = require('node:test');

// Pins the token-savings page's selected-session CSV export (#5589): users
// pick the sessions they are comparing with per-row / select-all checkboxes
// and download just those rows through the page's existing CSV path, while
// the complete export keeps working. The subset must follow displayed row
// order, come only from the loaded snapshot (never another request), reset
// when a new successful query replaces the snapshot, stay keyed by session
// id across detail expansion, and both export controls must stay disabled
// while querying, after an error or without their applicable rows.
//
// The dashboard has no component-test harness, so — like the token-savings
// failed-query suite — these tests transpile the real page with the
// dashboard's babel toolchain and drive it against a hooks driver with
// deferred fetch stubs. Effects are replayed with React-style dependency
// comparison so a re-render alone cannot clear the selection.

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
  const effectDeps = [];
  const pendingEffects = [];
  const driver = {
    slots,
    render(Component, props = {}) {
      driver._cursor = 0;
      driver._effectCursor = 0;
      driver._callbacks = [];
      const element = Component(props);
      return { element, callbacks: driver._callbacks };
    },
    // React runs an effect only when its dependency identity changed; the
    // selection reset must key on the sessions snapshot, not every render.
    flushEffects() {
      const due = pendingEffects.splice(0);
      for (const fn of due) fn();
      return due.length;
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
      // Effects are identified by their call order within a render.
      const index = driver._effectCursor;
      driver._effectCursor += 1;
      const prev = effectDeps[index];
      const changed = !prev
        || !deps
        || prev.length !== deps.length
        || deps.some((d, i) => d !== prev[i]);
      effectDeps[index] = deps;
      if (changed) pendingEffects.push(fn);
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

function loadPageModule(relativePath, moduleStubs, hooks) {
  const code = transpile(relativePath);
  const module = { exports: {} };
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

/** Yield every element in the stub element tree, descending into fragments. */
function* walk(node) {
  if (!node || typeof node !== 'object') return;
  if (Array.isArray(node)) {
    for (const child of node) yield* walk(child);
    return;
  }
  if (typeof node.type === 'string' || typeof node.type === 'function') {
    yield node;
  }
  yield* walk(node.children);
}

const textOf = (node) =>
  [...walk(node)]
    .flatMap((el) => (Array.isArray(el.children) ? el.children : []))
    .filter((child) => typeof child === 'string')
    .join('');

const findAll = (node, predicate) => [...walk(node)].filter(predicate);

const findButtonByText = (node, text) =>
  findAll(node, (el) => el.type === 'button' && textOf(el).includes(text))[0];

const findSessionRow = (node, sessionId) =>
  findAll(node, (el) => typeof el.type === 'function' && el.type.name === 'SessionRow'
    && el.props && el.props.session && el.props.session.session_id === sessionId)[0];

function makeHarness() {
  const { calls, stubs } = deferredFetchStubs([
    'fetchTokenSavings',
    'fetchAgentNames',
  ]);
  const driver = createHooksDriver();
  const downloads = [];
  // The transpiled module resolves react hooks through one stub; dispatch
  // them to whichever component is rendering so a SessionRow rendered in
  // isolation keeps its own expansion state out of the page's slots.
  const harness = { currentDriver: driver, rowDrivers: new Map() };
  const hooks = {
    useState: (...args) => harness.currentDriver.useState(...args),
    useRef: (...args) => harness.currentDriver.useRef(...args),
    useEffect: (...args) => harness.currentDriver.useEffect(...args),
    useCallback: (...args) => harness.currentDriver.useCallback(...args),
    useMemo: (...args) => harness.currentDriver.useMemo(...args),
  };
  const moduleStubs = {
    'react-router-dom': {
      useSearchParams: () => [new URLSearchParams(''), () => undefined],
    },
    recharts: {
      ...componentStub('PieChart'), ...componentStub('Pie'), ...componentStub('Cell'),
      ...componentStub('ResponsiveContainer'),
    },
    '../utils/apiClient': { ...stubs },
    '../utils/savingsCsv': { downloadSavingsCsv: (rows) => downloads.push(rows) },
    '../components/DateTimePicker': componentStub('DateTimePicker'),
    '../components/SessionIdHelp': componentStub('SessionIdHelp'),
    '../i18n': {
      useI18n: () => ({ t: (key) => key }),
      useLocaleTag: () => 'en',
    },
  };
  const pageModule = loadPageModule('src/pages/TokenSavingsPage.tsx', moduleStubs, hooks);
  const page = pageModule.TokenSavingsPage;
  assert.equal(typeof page, 'function', 'TokenSavingsPage must be a component');
  const rendered = driver.render(page);
  const handleQuery = rendered.callbacks.find(
    (cb) => String(cb).includes('fetchTokenSavings'),
  );
  assert.ok(handleQuery, 'the page must expose its query callback');
  Object.assign(harness, {
    calls, driver, page, handleQuery, downloads,
    render: () => {
      harness.currentDriver = driver;
      return driver.render(page).element;
    },
  });
  return harness;
}

/**
 * Render and replay post-commit effects until stable — React commits a
 * render, then runs its effects, so no interaction can ever land between a
 * render and its effects (a pending snapshot-reset effect must not fire
 * after a user click, only after the query that queued it).
 */
function commit(harness) {
  let tree = harness.render();
  for (let guard = 0; guard < 5 && harness.driver.flushEffects() > 0; guard += 1) {
    tree = harness.render();
  }
  return tree;
}

/** Resolve one successful handleQuery fetch, then commit it. */
async function resolveQuery(harness, index, payload) {
  harness.calls.fetchTokenSavings[index].resolve(payload);
  await settle();
  await settle();
  return commit(harness);
}

/** Reject one failed handleQuery fetch, then commit it. */
async function rejectQuery(harness, index, message) {
  harness.calls.fetchTokenSavings[index].reject(new Error(message));
  await settle();
  await settle();
  return commit(harness);
}

/**
 * Render a SessionRow against a per-row hooks driver (so its expansion state
 * survives re-renders, like a mounted row) and return its element tree.
 */
function renderSessionRow(harness, tree, sessionId) {
  const element = findSessionRow(tree, sessionId);
  assert.ok(element, `session row ${sessionId} must be rendered`);
  let rowDriver = harness.rowDrivers.get(sessionId);
  if (!rowDriver) {
    rowDriver = createHooksDriver();
    harness.rowDrivers.set(sessionId, rowDriver);
  }
  harness.currentDriver = rowDriver;
  const rowTree = rowDriver.render(element.type, element.props).element;
  harness.currentDriver = harness.driver;
  return { element, tree: rowTree, rowDriver };
}

const summary = {
  total_input_tokens: 9000,
  total_output_tokens: 3000,
  total_tokens: 12000,
  baseline_tokens: 20000,
  total_saved_tokens: 6000,
  total_compounded_saved: 6000,
  savings_rate: 0.3,
  compounded_savings_rate: 0.3,
  total_tool_saved: 3600,
  total_mcp_saved: 2400,
  total_compounded_tool_saved: 3600,
  total_compounded_mcp_saved: 2400,
};

const sessionOf = (id, agent) => ({
  session_id: id,
  agent_name: agent,
  total_input_tokens: 3000,
  total_output_tokens: 1000,
  total_tokens: 4000,
  baseline_tokens: 6666,
  saved_tokens: 2000,
  compounded_saved: 2000,
  savings_rate: 0.3,
  compounded_savings_rate: 0.3,
  optimization_items: [],
});

const rangeWithSessions = (...sessions) => ({
  stats_available: true,
  summary,
  sessions,
  optimization_tips: [],
});

const idsOf = (rows) => rows.map((row) => row.session_id);

test('selecting rows exports only the chosen sessions in displayed order, without new requests', async () => {
  const harness = makeHarness();
  const first = harness.handleQuery();

  // No export controls before any query completed.
  let tree = harness.render();
  assert.ok(!findButtonByText(tree, 'ts.exportCsv'), 'no export button before the first query');

  tree = await resolveQuery(harness, 0, rangeWithSessions(
    sessionOf('sess-A', 'agent-A'),
    sessionOf('sess-B', 'agent-B'),
    sessionOf('sess-C', 'agent-C'),
  ));
  await first;
  await settle();
  tree = commit(harness);

  const exportAll = findButtonByText(tree, 'ts.exportCsv');
  const exportSelected = findButtonByText(tree, 'ts.exportSelectedCsv');
  assert.ok(exportAll, 'the complete export button must render');
  assert.ok(exportSelected, 'the selected export button must render');
  assert.equal(exportSelected.props.disabled, true, 'selected export disabled while nothing is selected');

  // Click the checkboxes in an order that differs from the displayed one.
  for (const id of ['sess-C', 'sess-A']) {
    const { tree: rowTree } = renderSessionRow(harness, tree, id);
    const checkbox = findAll(rowTree, (el) => el.type === 'input' && el.props.type === 'checkbox')[0];
    assert.ok(checkbox, `row ${id} must render a checkbox`);
    const stopPropagation = [];
    checkbox.props.onClick({ stopPropagation: () => stopPropagation.push(true) });
    checkbox.props.onChange();
    assert.deepEqual(stopPropagation, [true], 'the row checkbox must stop click propagation');
    tree = commit(harness);
  }

  // Selection is keyed by id: only the two chosen rows are selected.
  assert.equal(findSessionRow(tree, 'sess-A').props.selected, true);
  assert.equal(findSessionRow(tree, 'sess-B').props.selected, false);
  assert.equal(findSessionRow(tree, 'sess-C').props.selected, true);
  assert.notEqual(findButtonByText(tree, 'ts.exportSelectedCsv').props.disabled, true);

  // Selected export keeps displayed order, complete export keeps every row.
  findButtonByText(tree, 'ts.exportSelectedCsv').props.onClick();
  findButtonByText(tree, 'ts.exportCsv').props.onClick();
  assert.deepEqual(idsOf(harness.downloads[0]), ['sess-A', 'sess-C']);
  assert.deepEqual(idsOf(harness.downloads[1]), ['sess-A', 'sess-B', 'sess-C']);

  // The subset export is served from the loaded snapshot only.
  assert.equal(harness.calls.fetchTokenSavings.length, 1, 'selection must not trigger requests');
});

test('the select-all checkbox selects, keeps and clears every session', async () => {
  const harness = makeHarness();
  const first = harness.handleQuery();
  let tree = await resolveQuery(harness, 0, rangeWithSessions(
    sessionOf('sess-A', 'agent-A'),
    sessionOf('sess-B', 'agent-B'),
    sessionOf('sess-C', 'agent-C'),
  ));
  await first;
  await settle();
  tree = commit(harness);

  const headerCheckbox = () => findAll(tree, (el) => el.type === 'input' && el.props.type === 'checkbox'
    && el.props['aria-label'] === 'ts.selectAllSessions')[0];
  assert.ok(headerCheckbox(), 'the header must render the select-all checkbox');
  assert.equal(headerCheckbox().props.checked, false, 'nothing selected initially');

  headerCheckbox().props.onChange();
  tree = commit(harness);
  for (const id of ['sess-A', 'sess-B', 'sess-C']) {
    assert.equal(findSessionRow(tree, id).props.selected, true, `select-all must select ${id}`);
  }
  assert.equal(headerCheckbox().props.checked, true);

  // Deselecting one row drops the select-all state but keeps the others.
  const { tree: rowB } = renderSessionRow(harness, tree, 'sess-B');
  findAll(rowB, (el) => el.type === 'input' && el.props.type === 'checkbox')[0].props.onChange();
  tree = commit(harness);
  assert.equal(findSessionRow(tree, 'sess-B').props.selected, false);
  assert.equal(findSessionRow(tree, 'sess-A').props.selected, true);
  assert.equal(headerCheckbox().props.checked, false, 'partial selection must uncheck select-all');

  // From partial, select-all reselects everything; clicking again clears all.
  headerCheckbox().props.onChange();
  tree = commit(harness);
  assert.equal(findSessionRow(tree, 'sess-B').props.selected, true);
  headerCheckbox().props.onChange();
  tree = commit(harness);
  for (const id of ['sess-A', 'sess-B', 'sess-C']) {
    assert.equal(findSessionRow(tree, id).props.selected, false, `clear-all must drop ${id}`);
  }
  assert.equal(
    findButtonByText(tree, 'ts.exportSelectedCsv').props.disabled,
    true,
    'selected export disabled once the selection is cleared',
  );
});

test('a new successful query resets the selection', async () => {
  const harness = makeHarness();
  const first = harness.handleQuery();
  await resolveQuery(harness, 0, rangeWithSessions(
    sessionOf('sess-A', 'agent-A'),
    sessionOf('sess-B', 'agent-B'),
  ));
  await first;
  await settle();

  let tree = harness.render();
  const { tree: rowA } = renderSessionRow(harness, tree, 'sess-A');
  findAll(rowA, (el) => el.type === 'input' && el.props.type === 'checkbox')[0].props.onChange();
  tree = commit(harness);
  assert.equal(findSessionRow(tree, 'sess-A').props.selected, true);

  // A newer successful query replaces the snapshot: selection must reset.
  const second = harness.handleQuery();
  tree = await resolveQuery(harness, 1, rangeWithSessions(sessionOf('sess-D', 'agent-D')));
  await second;
  await settle();
  tree = commit(harness);
  assert.equal(findSessionRow(tree, 'sess-D').props.selected, false);
  assert.equal(
    findButtonByText(tree, 'ts.exportSelectedCsv').props.disabled,
    true,
    'the reset selection must disable the selected export',
  );
  const exportAll = findButtonByText(tree, 'ts.exportCsv');
  assert.notEqual(exportAll.props.disabled, true, 'complete export still works after reset');
});

test('both export controls are gated on querying, errors and empty results', async () => {
  const harness = makeHarness();

  // While a query is pending the results section (and its controls) is hidden.
  const first = harness.handleQuery();
  let tree = harness.render();
  assert.ok(!findButtonByText(tree, 'ts.exportCsv'), 'no export controls while querying');
  tree = await resolveQuery(harness, 0, rangeWithSessions());
  await first;
  await settle();
  tree = commit(harness);

  // Empty results: both controls render disabled.
  let exportAll = findButtonByText(tree, 'ts.exportCsv');
  let exportSelected = findButtonByText(tree, 'ts.exportSelectedCsv');
  assert.equal(exportAll.props.disabled, true, 'complete export disabled without sessions');
  assert.equal(exportSelected.props.disabled, true, 'selected export disabled without sessions');

  // A failed query must keep the controls disabled under the error banner.
  const second = harness.handleQuery();
  tree = await rejectQuery(harness, 1, 'boom');
  await second;
  await settle();
  tree = commit(harness);
  exportAll = findButtonByText(tree, 'ts.exportCsv');
  exportSelected = findButtonByText(tree, 'ts.exportSelectedCsv');
  assert.ok(exportAll, 'the table controls must render after the error settles');
  assert.equal(exportAll.props.disabled, true, 'complete export disabled after a query error');
  assert.equal(exportSelected.props.disabled, true, 'selected export disabled after a query error');
});

test('checkbox clicks preserve row expansion', async () => {
  const harness = makeHarness();
  const first = harness.handleQuery();
  let tree = await resolveQuery(harness, 0, rangeWithSessions(sessionOf('sess-A', 'agent-A')));
  await first;
  await settle();
  tree = commit(harness);

  // Expand the row by clicking it, like a user investigating a session.
  const expandedDetail = (rowTree) =>
    findAll(rowTree, (el) => el.type === 'td' && el.props && el.props.colSpan === 7).length;
  let row = renderSessionRow(harness, tree, 'sess-A');
  const clickableRow = findAll(row.tree, (el) => el.type === 'tr' && el.props && typeof el.props.onClick === 'function')[0];
  clickableRow.props.onClick();
  row.rowDriver.flushEffects();
  row = renderSessionRow(harness, tree, 'sess-A');
  assert.equal(expandedDetail(row.tree), 1, 'the row must expand after the row click');

  // Clicking the checkbox must neither collapse it nor leak the click.
  const checkbox = findAll(row.tree, (el) => el.type === 'input' && el.props.type === 'checkbox')[0];
  let stopped = 0;
  checkbox.props.onClick({ stopPropagation: () => { stopped += 1; } });
  checkbox.props.onChange();
  row.rowDriver.flushEffects();
  const after = renderSessionRow(harness, tree, 'sess-A');
  assert.equal(expandedDetail(after.tree), 1, 'the row must stay expanded after the checkbox click');
  assert.equal(stopped, 1, 'the checkbox must swallow its own click');
  tree = commit(harness);
  assert.equal(findSessionRow(tree, 'sess-A').props.selected, true, 'the checkbox still selects');
});

test('source pin: the page wires selection, snapshot reset and disabled gates', () => {
  const source = readFileSync(join(process.cwd(), 'src/pages/TokenSavingsPage.tsx'), 'utf8');

  // Selection resets together with the snapshot replacement.
  const resetCall = source.indexOf('setSelectedSessionIds(new Set());');
  assert.ok(resetCall >= 0, 'the page must clear the selection');
  const effectEnd = source.indexOf('}, [sessions]);', resetCall);
  assert.ok(effectEnd > resetCall, 'the reset must be an effect keyed on the sessions snapshot');
  assert.equal(
    source.slice(resetCall, effectEnd).replace(/\s+/g, ' ').trim(),
    'setSelectedSessionIds(new Set());',
    'the snapshot effect must only clear the selection',
  );

  // The row checkbox must not toggle the row expansion.
  assert.ok(
    /type="checkbox"[\s\S]{0,400}onClick=\{\(e\) => e\.stopPropagation\(\)\}/.test(source),
    'the row checkbox must stop click propagation',
  );

  // The subset export reads only the loaded snapshot and its own gate.
  assert.ok(
    source.includes('onClick={() => downloadSavingsCsv(selectedSessions)}'),
    'the selected export must reuse the CSV download with the filtered snapshot',
  );
  assert.ok(
    source.includes('disabled={loading || !!error || selectedSessions.length === 0}'),
    'the selected export must be disabled while querying, on error or with nothing selected',
  );
  assert.ok(
    source.includes('disabled={loading || !!error || sessions.length === 0}'),
    'the complete export keeps its existing gate',
  );
});
