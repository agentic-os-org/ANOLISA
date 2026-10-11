const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const test = require('node:test');

// Regression suite for the ATIF trajectory round model
// (src/utils/roundModel.ts), extracted from AtifViewerPage so the round
// grouping, initial highlighted/default selection and per-round statistics
// can be exercised directly without reconstructing the whole page.
//
// Three layers:
//   1. source-level: the production viewer delegates to the shared utility
//      and no longer embeds its own copies;
//   2. direct fixtures against the tsc-compiled utility on immutable inputs,
//      pinning grouping, labels, keys, step identities, selection priority,
//      preview/timestamp choice and token/tool totals;
//   3. a compiled production viewer integration: the REAL AtifViewerPage
//      (transpiled with the dashboard's babel toolchain) loading a document
//      through its own loader and rendering rounds through the REAL
//      transpiled round model, driven by the same minimal hooks driver used
//      by the stale-load suite.

const readSource = (relativePath) => readFileSync(join(process.cwd(), relativePath), 'utf8');

// ─── 1. Source-level delegation ───────────────────────────────────────────────

test('the viewer imports the round model from the shared typed utility', () => {
  const viewer = readSource('src/pages/AtifViewerPage.tsx');
  assert.match(viewer, /import type \{ Round \} from '\.\.\/utils\/roundModel';/);
  assert.match(viewer, /import \{ groupIntoRounds, initialRound, roundStats \} from '\.\.\/utils\/roundModel';/);
  // The local copies must be gone (this is what fails on the pre-refactor page).
  assert.ok(!/interface Round \{/.test(viewer), 'the viewer must not define its own Round interface');
  assert.ok(!/interface RoundStats \{/.test(viewer), 'the viewer must not define its own RoundStats interface');
  assert.ok(!/^function groupIntoRounds/m.test(viewer), 'groupIntoRounds must live in the utility');
  assert.ok(!/^function initialRound/m.test(viewer), 'initialRound must live in the utility');
  assert.ok(!/^function roundStats/m.test(viewer), 'roundStats must live in the utility');

  const model = readSource('src/utils/roundModel.ts');
  assert.match(model, /export function groupIntoRounds/);
  assert.match(model, /export function initialRound/);
  assert.match(model, /export function roundStats/);
  assert.match(model, /export interface Round \{/);
  assert.match(model, /export interface RoundStats \{/);
});

// ─── 2. Direct fixtures on the compiled utility ───────────────────────────────

const {
  groupIntoRounds,
  initialRound,
  roundStats,
} = require(process.env.AGENTSIGHT_ROUND_MODEL_BUILD);

// A translator with the viewer's exact contract; labels are deterministic
// so the fixtures never branch on translated text.
const t = (key, params) => {
  if (key === 'atif.round') return `Round ${params.n}`;
  if (key === 'atif.preamble') return 'Preamble';
  throw new Error(`unexpected message key ${key}`);
};

const step = (step_id, source, extra = {}) => ({ step_id, source, ...extra });

// Freeze an object graph so any mutation attempt throws in strict mode:
// the round model must treat its inputs as immutable.
function deepFreeze(value) {
  if (value && typeof value === 'object') {
    Object.freeze(value);
    for (const key of Object.keys(value)) deepFreeze(value[key]);
  }
  return value;
}

function fixtureSteps() {
  return [
    step(0, 'system', { message: 'you are a coding agent', timestamp: '2026-10-05T01:00:00Z' }),
    step(1, 'user', {
      message: 'fix the\n   login   bug now',
      timestamp: '2026-10-05T01:01:00Z',
      metrics: { prompt_tokens: 12, completion_tokens: 7 },
    }),
    step(2, 'agent', {
      timestamp: '2026-10-05T01:02:00Z',
      tool_calls: [
        { tool_call_id: 'tc-1', function_name: 'read_file' },
        { tool_call_id: 'tc-2', function_name: 'edit_file' },
      ],
    }),
    step(3, 'user', {
      message: 'also add tests',
      metrics: { prompt_tokens: 5, completion_tokens: 0 },
    }),
    step(4, 'agent', {
      timestamp: '2026-10-05T01:05:00Z',
      tool_calls: [{ tool_call_id: 'tc-9', function_name: 'run_tests' }],
      observation: { results: [{ source_call_id: 'tc-9' }] },
      metrics: { prompt_tokens: 40, completion_tokens: 9 },
    }),
  ];
}

test('groupIntoRounds: grouping, labels, keys and step identities', () => {
  const steps = deepFreeze(fixtureSteps());
  const rounds = groupIntoRounds(steps, t);

  assert.deepEqual(rounds.map((r) => r.key), [0, 1, 2]);
  assert.deepEqual(rounds.map((r) => r.label), ['Preamble', 'Round 1', 'Round 2']);
  assert.deepEqual(rounds.map((r) => r.isPreamble), [true, false, false]);

  // Step identities are preserved: each grouped step is the input object.
  assert.deepEqual(rounds[0].steps.map((s) => s.step_id), [0]);
  assert.deepEqual(rounds[1].steps.map((s) => s.step_id), [1, 2]);
  assert.deepEqual(rounds[2].steps.map((s) => s.step_id), [3, 4]);
  assert.strictEqual(rounds[0].userStep, null);
  assert.strictEqual(rounds[1].userStep, steps[1]);
  assert.strictEqual(rounds[2].userStep, steps[3]);

  // The frozen input must be untouched.
  assert.deepEqual(steps.map((s) => s.step_id), [0, 1, 2, 3, 4]);
});

test('groupIntoRounds: a leading user step yields no synthetic preamble', () => {
  const steps = deepFreeze([
    step(1, 'user', { message: 'hi' }),
    step(2, 'agent', {}),
    step(3, 'user', { message: 'again' }),
  ]);
  const rounds = groupIntoRounds(steps, t);
  assert.deepEqual(rounds.map((r) => r.label), ['Round 1', 'Round 2']);
  assert.deepEqual(rounds.map((r) => r.isPreamble), [false, false]);
  assert.deepEqual(rounds.map((r) => r.steps.map((s) => s.step_id)), [[1, 2], [3]]);
});

test('groupIntoRounds: empty input yields no rounds', () => {
  assert.deepEqual(groupIntoRounds(deepFreeze([]), t), []);
});

test('initialRound: selection priority', () => {
  const steps = fixtureSteps();
  const rounds = groupIntoRounds(steps, t);
  assert.equal(initialRound(rounds, deepFreeze(new Set())), 0, 'no highlight selects the first round');
  assert.equal(initialRound([], new Set()), null, 'no rounds selects nothing');
  // Highlighted section keys carry a suffix ("-toolcalls"/"-observation"),
  // so selection must match the numeric prefix against the owning round.
  assert.equal(initialRound(rounds, deepFreeze(new Set(['4-toolcalls']))), 2);
  assert.equal(initialRound(rounds, deepFreeze(new Set(['1-observation', '2-toolcalls']))), 1);
  // The earliest owning round wins when several rounds are highlighted.
  assert.equal(initialRound(rounds, deepFreeze(new Set(['4-observation', '2-toolcalls']))), 1);
  // Unmatched highlight falls back to the first round.
  assert.equal(initialRound(rounds, deepFreeze(new Set(['999-observation']))), 0);
});

test('roundStats: token/tool totals, preview and timestamp choice', () => {
  const steps = deepFreeze(fixtureSteps());
  const rounds = groupIntoRounds(steps, t);

  const preamble = roundStats(rounds[0]);
  assert.equal(preamble.toolCallCount, 0);
  assert.equal(preamble.promptSum, 0);
  assert.equal(preamble.completionSum, 0);
  assert.equal(preamble.preview, 'you are a coding agent', 'no user step: first step carrying a message');
  assert.equal(preamble.firstTs, '2026-10-05T01:00:00Z');

  const first = roundStats(rounds[1]);
  assert.equal(first.toolCallCount, 2);
  assert.equal(first.promptSum, 12, 'steps without metrics contribute 0');
  assert.equal(first.completionSum, 7);
  assert.equal(first.preview, 'fix the login bug now', 'the user step message wins and whitespace collapses');
  assert.equal(first.firstTs, '2026-10-05T01:01:00Z', 'first step that carries a timestamp');

  const second = roundStats(rounds[2]);
  assert.equal(second.toolCallCount, 1);
  assert.equal(second.promptSum, 45);
  assert.equal(second.completionSum, 9);
  assert.equal(second.preview, 'also add tests');
  assert.equal(second.firstTs, '2026-10-05T01:05:00Z', 'skips the timestamp-less user step');
});

test('roundStats: a round with no content reports empty stats', () => {
  const rounds = groupIntoRounds(deepFreeze([step(7, 'user', {})]), t);
  const stats = roundStats(rounds[0]);
  assert.equal(stats.toolCallCount, 0);
  assert.equal(stats.promptSum, 0);
  assert.equal(stats.completionSum, 0);
  assert.equal(stats.preview, '');
  assert.equal(stats.firstTs, undefined);
});

// ─── 3. Compiled production viewer integration ───────────────────────────────
//
// The REAL AtifViewerPage is transpiled with the dashboard's own babel
// toolchain and rendered through the hooks driver; '../utils/roundModel' is
// wired to the REAL transpiled utility (and the real trajectoryTree), so the
// assertions exercise the production viewer's loading and rendering path on
// top of the extracted model.

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

function loadModuleFromCode(code, moduleStubs) {
  const module = { exports: {} };
  const reactStub = {
    __esModule: true,
    default: {
      createElement: (type, props, ...children) => ({ type, props, children }),
      Fragment: Symbol.for('react.fragment'),
    },
    createElement: (type, props, ...children) => ({ type, props, children }),
  };
  const requireStub = (name) => {
    if (name === 'react') return reactStub;
    if (moduleStubs[name]) return moduleStubs[name];
    throw new Error(`unexpected require: ${name}`);
  };
  const fn = new Function('require', 'module', 'exports', code);
  fn(requireStub, module, module.exports);
  return module.exports;
}

// The viewer renders savings rates through the shared src/utils/savings.ts;
// transpiled from source so the page still runs the real formula rather than a
// hand-written stand-in.
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

function realModule(relativePath) {
  return loadModuleFromCode(transpile(relativePath), {
    // roundModel type-imports i18n/types (erased); trajectoryTree likewise.
  });
}

function createHooksDriver() {
  const slots = [];
  const driver = {
    slots,
    render(Component, props = {}) {
      driver._cursor = 0;
      const element = Component(props);
      return element;
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
    useEffect(fn) { driver._pendingEffect = fn; },
    useCallback(fn) { return fn; },
    useMemo(factory) { return factory(); },
  };
  return driver;
}

/** Collect every string in a createElement-stub element tree. */
function collectText(node, out = []) {
  if (typeof node === 'string' || typeof node === 'number') {
    out.push(String(node));
  } else if (Array.isArray(node)) {
    node.forEach((child) => collectText(child, out));
  } else if (node && typeof node === 'object' && node.children) {
    collectText(node.children, out);
  }
  return out;
}

/** Every element in the tree matching predicate(element). */
function findElements(node, predicate, out = []) {
  if (Array.isArray(node)) {
    node.forEach((child) => findElements(child, predicate, out));
  } else if (node && typeof node === 'object' && node.type) {
    if (predicate(node)) out.push(node);
    findElements(node.children, predicate, out);
  }
  return out;
}

function integrationDocument() {
  return {
    schema_version: 'ATIF/1.0',
    session_id: 'sess-integration',
    steps: fixtureSteps(),
  };
}

// Deterministic translator: round labels like the direct fixtures, everything
// else renders as "<key> <json params>" so the chips are assertable.
const viewerT = (key, params) => {
  if (key === 'atif.round') return `Round ${params.n}`;
  if (key === 'atif.preamble') return 'Preamble';
  return params ? `${key} ${JSON.stringify(params)}` : key;
};

function loadViewer(searchParamsInit, driver, apiStubs) {
  const roundModel = realModule('src/utils/roundModel.ts');
  const trajectoryTree = realModule('src/utils/trajectoryTree.ts');
  const trajectoryTextFilter = realModule('src/utils/trajectoryTextFilter.ts');
  const spHolder = { params: new URLSearchParams(searchParamsInit) };
  const moduleStubs = {
    'react-router-dom': {
      useSearchParams: () => [spHolder.params, (next) => { spHolder.params = new URLSearchParams(next); }],
    },
    '../i18n': {
      useI18n: () => ({ t: viewerT }),
      useLocaleTag: () => 'en-US',
    },
    '../utils/apiClient': apiStubs,
    '../utils/roundModel': roundModel,
    '../utils/savings': savingsModule,
    '../utils/download': realModule('src/utils/download.ts'),
    '../utils/trajectoryTree': trajectoryTree,
    '../utils/trajectoryTextFilter': trajectoryTextFilter,
    '../components/SubagentGraph': { SubagentGraph: () => null },
    '../components/CausalAttributionPanel': { CausalAttributionPanel: () => null },
  };
  // The page module needs the driver's React hooks; loadModuleFromCode above
  // has no driver, so bind react through the driver here.
  const hooks = {
    useState: driver.useState,
    useRef: driver.useRef,
    useEffect: driver.useEffect,
    useCallback: driver.useCallback,
    useMemo: driver.useMemo,
  };
  const reactWithHooks = {
    __esModule: true,
    default: {
      createElement: (type, props, ...children) => ({ type, props, children }),
      Fragment: Symbol.for('react.fragment'),
      ...hooks,
    },
    createElement: (type, props, ...children) => ({ type, props, children }),
    ...hooks,
  };
  const code = transpile('src/pages/AtifViewerPage.tsx');
  const module = { exports: {} };
  const requireStub = (name) => {
    if (name === 'react') return reactWithHooks;
    if (moduleStubs[name]) return moduleStubs[name];
    throw new Error(`unexpected require from AtifViewerPage: ${name}`);
  };
  new Function('require', 'module', 'exports', code)(requireStub, module, module.exports);
  return module.exports.AtifViewerPage;
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

async function renderLoadedViewer({ highlightCallId } = {}) {
  const deferreds = [];
  const defer = () => {
    const d = {};
    d.promise = new Promise((resolve) => { d.resolve = resolve; });
    deferreds.push(d);
    return d.promise;
  };
  const apiStubs = {
    fetchAtifBySession: () => defer(),
    fetchAtifByConversation: () => Promise.reject(new Error('not used')),
    fetchTrajectoryAtif: () => Promise.reject(Object.assign(new Error('gone'), { status: 404 })),
    fetchSessionSavings: () => Promise.resolve({ items: [] }),
  };
  const driver = createHooksDriver();
  const search = highlightCallId
    ? `type=session&id=sess-integration&highlight_call_id=${highlightCallId}`
    : 'type=session&id=sess-integration';
  const Page = loadViewer(search, driver, apiStubs);

  driver.render(Page);                    // mount render; records the auto-load effect
  const autoLoad = driver._pendingEffect; // eslint-disable-line no-underscore-dangle
  const loadPromise = autoLoad();         // mount auto-load fires handleLoad
  assert.equal(deferreds.length, 1, 'handleLoad must fetch the session document');
  deferreds[0].resolve(integrationDocument());
  await loadPromise;
  await settle();

  const rendered = driver.render(Page);   // re-render with the loaded document
  return { driver, rendered };
}

test('integration: the production viewer groups and renders rounds via the shared model', async () => {
  const { rendered } = await renderLoadedViewer({});

  // Round-list descriptors, in column order: preamble, then user rounds.
  const listItems = findElements(rendered, (el) => el.props && el.props.onSelect && el.props.round && Array.isArray(el.props.round.steps));
  assert.equal(listItems.length, 3, 'three rounds: preamble + two user rounds');
  assert.deepEqual(
    listItems.map((el) => el.props.round.label),
    ['Preamble', 'Round 1', 'Round 2'],
  );
  assert.deepEqual(listItems.map((el) => el.props.round.key), [0, 1, 2]);

  // Render a list item through the production component: labels, totals and
  // the preview all flow through the extracted model.
  const round1Item = listItems[1];
  const round1Text = collectText(round1Item.type(round1Item.props)).join(' ');
  assert.ok(round1Text.includes('Round 1'), 'the round label renders');
  assert.ok(round1Text.includes('fix the login bug now'), 'the preview collapses the user message');
  assert.ok(round1Text.includes('common.inOut {"in":"12","out":"7"}'), 'token totals render');
  assert.ok(round1Text.includes('common.steps {"n":2}'), 'the step count renders');

  // Default selection (no highlight): the first round's detail is shown.
  const detail = findElements(rendered, (el) => el.props && el.props.round && el.props.expandedSections instanceof Set)[0];
  assert.ok(detail, 'a round detail column renders');
  assert.equal(detail.props.round.key, 0);
  assert.equal(detail.props.round.label, 'Preamble');
});

test('integration: a highlighted tool call selects the owning round', async () => {
  // Round 2's agent step (step_id 4) owns tool_call_id 'tc-9'.
  const { rendered } = await renderLoadedViewer({ highlightCallId: 'tc-9' });
  const detail = findElements(rendered, (el) => el.props && el.props.round && el.props.expandedSections instanceof Set)[0];
  assert.equal(detail.props.round.key, 2, 'selection must prefer the highlighted round');
  assert.equal(detail.props.round.label, 'Round 2');
  assert.deepEqual(
    detail.props.expandedSections,
    new Set(['4-toolcalls', '4-observation']),
    'the highlight keys must expand the owning step sections',
  );
});
