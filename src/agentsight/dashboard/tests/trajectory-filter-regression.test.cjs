const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { runInNewContext } = require('node:vm');
const test = require('node:test');

const document = (id) => ({ schema_version: 'ATIF-v1.7', session_id: id, steps: [] });
function viewer({ readFailure = false } = {}) {
  const states = [];
  const refs = [];
  const readers = [];
  const requests = [];
  const exports = {};
  const downloads = [];
  let searchParams = new URLSearchParams();
  const translate = (key, values) => values ? key + JSON.stringify(values) : key;
  const effects = [], dependencies = [], cleanups = [];
  let stateIndex = 0;
  let refIndex = 0;
  let effectIndex = 0;
  const react = {
    useState(initial) {
      const index = stateIndex++;
      if (!(index in states)) states[index] = typeof initial === 'function' ? initial() : initial;
      return [states[index], (value) => { states[index] = typeof value === 'function' ? value(states[index]) : value; }];
    },
    useRef(initial) { return refs[refIndex++] ??= { current: initial }; },
    useCallback: (callback) => callback,
    useMemo: (callback) => callback(),
    useEffect(callback, deps) {
      const index = effectIndex++;
      if (!dependencies[index] || deps.some((dep, i) => !Object.is(dep, dependencies[index][i]))) {
        effects.push(() => { cleanups[index]?.(); cleanups[index] = callback(); });
      }
      dependencies[index] = deps;
    },
  };
  runInNewContext(readFileSync(process.env.AGENTSIGHT_ATIF_PAGE_BUILD, 'utf8'), {
    exports, URLSearchParams,
    setTimeout: () => 0,
    Blob, URL: { createObjectURL: (blob) => { downloads.push(blob); return 'blob:test'; }, revokeObjectURL() {} },
    document: { createElement: () => ({ click() {} }) },
    FileReader: class {
      constructor() { readers.push(this); }
      readAsText() { if (readFailure) throw new Error('read failed'); }
    },
    require(name) {
      if (name === 'react') return react;
      if (name === 'react/jsx-runtime') return {
        jsx: (type, props) => ({ type, props }), jsxs: (type, props) => ({ type, props }),
      };
      if (name === 'react-router-dom') return { useSearchParams: () => [searchParams, (next) => { searchParams = new URLSearchParams(next); }] };
      if (name === '../i18n') return { useI18n: () => ({ t: translate }), useLocaleTag: () => 'en-US' };
      if (name === '../utils/apiClient') return {
        fetchAtifBySession: () => new Promise((resolve, reject) => requests.push({ resolve, reject })),
        fetchSessionSavings: async () => ({ items: [], total_compounded_saved: 0 }),
      };
      // The savings card reads its rate through this helper; the search tests do
      // not assert the card, so the pure baseline formula stands in here.
      if (name === '../utils/savings') return {
        compoundedSavingsRate: (saved, baseline) => (baseline > 0 ? (saved / baseline) * 100 : 0),
      };
      if (name === '../utils/trajectoryTree') return require(process.env.AGENTSIGHT_TRAJECTORY_TREE_BUILD);
      if (name === '../utils/trajectoryTextFilter') return require(process.env.AGENTSIGHT_TRAJECTORY_FILTER_BUILD);
      if (name === '../utils/roundModel') return require(process.env.AGENTSIGHT_ROUND_MODEL_BUILD);
      if (name.startsWith('../components/')) return {};
      throw new Error(`Unexpected ATIF viewer dependency ${name}`);
    },
  });
  function render() {
    stateIndex = 0; refIndex = 0; effectIndex = 0;
    const page = exports.AtifViewerPage();
    for (const effect of effects.splice(0)) effect();
    return page;
  }
  function nodes(node, predicate) {
    if (!node || typeof node !== 'object') return [];
    return [...(predicate(node) ? [node] : []),
      ...[node.props?.children].flat(Infinity).flatMap((child) => nodes(child, predicate))];
  }
  return {
    requests, downloads,
    params: () => new URLSearchParams(searchParams),
    visitParams(value) { searchParams = new URLSearchParams(value); },
    queryId() { return nodes(render(), (node) => node.type === 'input' && node.props.type === 'text')[0].props.value; },
    nodes: (predicate) => nodes(render(), predicate),
    rounds: () => nodes(render(), (node) => node.type?.name === 'RoundListItem'),
    detail: () => nodes(render(), (node) => node.type?.name === 'RoundDetail')[0],
    filter(value) { nodes(render(), (node) => node.type === 'input' && node.props.type === 'search')[0].props.onChange({ target: { value } }); },
    unmount() { for (const cleanup of cleanups) cleanup?.(); },
    load(id) {
      nodes(render(), (node) => node.type === 'input' && node.props.type === 'text')[0].props.onChange({ target: { value: id } });
      // Enter remains available while the load button shows the pending state.
      return nodes(render(), (node) => node.type === 'input' && node.props.type === 'text')[0].props.onKeyDown({ key: 'Enter' });
    },
    import() {
      const input = nodes(render(), (node) => node.type === 'input' && node.props.type === 'file')[0];
      const target = { files: [{ name: 'example.json' }], value: 'example.json' };
      input.props.onChange({ target });
      assert.equal(target.value, '', 'the same file can be selected again');
      return readers[readers.length - 1];
    },
    complete(reader, doc) { reader.onload({ target: { result: JSON.stringify(doc) } }); render(); },
    loading() { return nodes(render(), (node) => node.type === 'button' && node.props.children === 'atif.loading').length > 0; },
    error() { return nodes(render(), (node) => node.props?.className === 'bg-red-50 border border-red-200 rounded-xl p-4 text-red-600 text-sm')[0]?.props.children.at(-1); },
    session() { return nodes(render(), (node) => node.type === 'span' && node.props.className === 'text-xs text-gray-400 font-mono truncate')[0]?.props.children; },
  };
}


const trajectory = (id = 'root') => ({ schema_version: 'ATIF-v1.7', session_id: id, steps: [
  { step_id: 1, source: 'system', message: 'Preamble' },
  { step_id: 2, source: 'user', message: 'Find ALPHA [literal]' },
  { step_id: 3, source: 'agent', message: 'Working', reasoning_content: 'ReasoningNeedle',
    tool_calls: [{ tool_call_id: 'one', function_name: 'ReadFile', arguments: { path: '/tmp/文件.py' } }],
    observation: { results: [{ content: { result: 'ObservationNeedle' } }] } },
  { step_id: 4, source: 'user', message: 'Second request' },
  { step_id: 5, source: 'agent', message: 'Finished beta' },
] });
function loaded(doc = trajectory()) { const page = viewer(); page.complete(page.import(), doc); return page; }
const keys = (page) => page.rounds().map((node) => node.props.round.key);

test('search trims case-insensitive literal queries and restores all rounds', () => {
  const page = loaded();
  assert.deepEqual(keys(page), [0, 1, 2]);
  page.filter('  alpha [LITERAL]  ');
  assert.deepEqual(keys(page), [1]);
  assert.equal(page.nodes((node) => node.props?.['aria-live'] === 'polite')[0].props.children,
    'atif.matchingRounds{"matched":1,"total":3}');
  page.nodes((node) => node.type === 'button' && node.props.children === 'atif.clearRoundFilter')[0].props.onClick();
  assert.deepEqual(keys(page), [0, 1, 2]);
  page.filter('   ');
  assert.deepEqual(keys(page), [0, 1, 2]);
});
test('reasoning, tool names, JSON arguments and observations find their containing round', () => {
  const page = loaded();
  for (const query of ['reasoningneedle', 'READFILE', '文件.py', 'observationneedle']) {
    page.filter(query);
    assert.deepEqual(keys(page), [1], query);
  }
});
test('no matches preserve selected detail and clearing preserves original round keys', () => {
  const page = loaded();
  page.rounds()[2].props.onSelect();
  page.filter('never matches');
  assert.deepEqual(keys(page), []);
  assert.equal(page.detail().props.round.key, 2);
  assert.equal(page.nodes((node) => node.type === 'p' && node.props.children === 'atif.noMatchingRounds').length, 1);
  page.filter('alpha');
  assert.deepEqual(keys(page), [1]);
  assert.equal(page.detail().props.round.key, 2);
  page.rounds()[0].props.onSelect();
  assert.equal(page.detail().props.round.key, 1);
});
test('importing another document resets the prior query', () => {
  const page = loaded(); page.filter('alpha');
  page.complete(page.import(), trajectory('next'));
  assert.equal(page.nodes((node) => node.type === 'input' && node.props.type === 'search')[0].props.value, '');
  assert.deepEqual(keys(page), [0, 1, 2]);
});
test('switching an embedded subagent resets filtering for its own rounds', () => {
  const doc = trajectory(); doc.subagent_trajectories = [trajectory('child')];
  const page = loaded(doc); page.filter('alpha');
  const graph = page.nodes((node) => node.props?.root && node.props.onSelect)[0];
  graph.props.onSelect(graph.props.root.children[0]);
  page.nodes(() => false); // flush document-change effect before checking the next render
  assert.equal(page.nodes((node) => node.type === 'input' && node.props.type === 'search')[0].props.value, '');
  assert.deepEqual(keys(page), [0, 1, 2]);
});

test('importing another document starts at its root instead of an old URL node', () => {
  const previous = trajectory('previous');
  previous.subagent_trajectories = [trajectory('child')];
  const page = loaded(previous);
  page.visitParams('type=session&id=previous&highlight_call_id=old-call');
  let graph = page.nodes((node) => node.props?.root && node.props.onSelect)[0];
  graph.props.onSelect(graph.props.root.children[0]);
  page.nodes(() => false);
  graph = page.nodes((node) => node.props?.root && node.props.onSelect)[0];
  assert.deepEqual(graph.props.selectedPath, ['child']);
  assert.equal(page.params().get('node'), 'child');

  const imported = trajectory('imported');
  imported.steps[2].message = 'new root message';
  imported.subagent_trajectories = [trajectory('child')];
  imported.subagent_trajectories[0].steps[2].message = 'new child message';
  page.complete(page.import(), imported);
  page.nodes(() => false);
  graph = page.nodes((node) => node.props?.root && node.props.onSelect)[0];
  assert.equal(page.session(), 'imported');
  assert.deepEqual(Array.from(graph.props.selectedPath), [], 'the previous document node must not select the imported child');
  assert.equal(page.params().get('node'), null, 'import success clears the old document node');
  assert.equal(page.queryId(), 'imported', 'URL cleanup must not replace the newly imported input ID');
  assert.equal(page.params().get('id'), 'previous', 'the established local-import URL identity is unchanged');
  assert.equal(page.params().get('highlight_call_id'), 'old-call', 'unrelated existing parameters are unchanged');
  assert.equal(page.rounds().find((node) => node.props.round.key === 1).props.round.steps[1].message,
    'new root message');
});

test('a rejected import keeps the existing node selection and URL', () => {
  const previous = trajectory('previous');
  previous.subagent_trajectories = [trajectory('child')];
  const page = loaded(previous);
  let graph = page.nodes((node) => node.props?.root && node.props.onSelect)[0];
  graph.props.onSelect(graph.props.root.children[0]);
  page.nodes(() => false);
  page.complete(page.import(), { schema_version: 'ATIF-v1.7', steps: [null] });
  page.nodes(() => false);
  graph = page.nodes((node) => node.props?.root && node.props.onSelect)[0];
  assert.equal(page.session(), 'previous');
  assert.equal(page.error(), 'atif.malformedDocument');
  assert.deepEqual(graph.props.selectedPath, ['child']);
  assert.equal(page.params().get('node'), 'child');
});

test('a successful import without the former child still selects its root', () => {
  const previous = trajectory('previous');
  previous.subagent_trajectories = [trajectory('child')];
  const page = loaded(previous);
  let graph = page.nodes((node) => node.props?.root && node.props.onSelect)[0];
  graph.props.onSelect(graph.props.root.children[0]);
  page.nodes(() => false);
  const imported = trajectory('imported');
  imported.subagent_trajectories = [trajectory('other-child')];
  page.complete(page.import(), imported);
  page.nodes(() => false);
  graph = page.nodes((node) => node.props?.root && node.props.onSelect)[0];
  assert.deepEqual(Array.from(graph.props.selectedPath), []);
  assert.equal(page.params().get('node'), null);
});

test('URL navigation still selects an embedded child after a successful import', () => {
  const doc = trajectory('imported');
  doc.subagent_trajectories = [trajectory('child')];
  const page = loaded(doc);
  page.visitParams('type=session&id=imported&node=child');
  page.nodes(() => false);
  let graph = page.nodes((node) => node.props?.root && node.props.onSelect)[0];
  assert.deepEqual(graph.props.selectedPath, ['child']);
  page.visitParams('type=session&id=imported');
  page.nodes(() => false);
  graph = page.nodes((node) => node.props?.root && node.props.onSelect)[0];
  assert.deepEqual(Array.from(graph.props.selectedPath), []);
});
test('a filtered raw JSON download retains the whole original root and subagent document', async () => {
  const doc = trajectory(); doc.subagent_trajectories = [trajectory('child')];
  const page = loaded(doc); page.filter('alpha');
  page.nodes((node) => node.type === 'button' && node.props.children === 'atif.downloadJson')[0].props.onClick();
  assert.deepEqual(JSON.parse(await page.downloads[0].text()), doc);
});
test('empty trajectories keep the existing no-step state', () => {
  const page = loaded({ schema_version: 'ATIF-v1.7', session_id: 'empty', steps: [] });
  assert.equal(page.nodes((node) => node.type === 'input' && node.props.type === 'search').length, 0);
  assert.equal(page.nodes((node) => node.props?.children === 'atif.noStepData').length, 1);
});

const { roundMatchesText } = require(process.env.AGENTSIGHT_TRAJECTORY_FILTER_BUILD);
for (const field of ['tool_calls', 'observation']) {
  test(`search keeps working after a malformed ${field} import is rejected`, () => {
    const doc = trajectory();
    doc.steps[2][field] = field === 'tool_calls' ? { bad: true } : { results: { bad: true } };
    const page = loaded();
    page.complete(page.import(), doc);
    assert.equal(page.error(), 'atif.malformedDocument');
    assert.equal(page.session(), 'root');
    assert.equal(roundMatchesText(doc.steps, 'alpha'), true);
    assert.equal(roundMatchesText(doc.steps, 'never matches'), false);
    page.filter('alpha');
    assert.deepEqual(keys(page), [1]);
    page.filter('never matches');
    assert.deepEqual(keys(page), []);
  });
}
test('pure matcher includes optional content, literal punctuation and frozen data', () => {
  const steps = trajectory().steps;
  const before = JSON.stringify(steps);
  for (const step of steps) Object.freeze(step);
  Object.freeze(steps);
  for (const query of ['preamble', '[literal]', 'readfile', '文件.py', 'ObservationNeedle']) {
    assert.equal(roundMatchesText(steps, query), true, query);
  }
  assert.equal(roundMatchesText(steps, '.*'), false);
  assert.equal(roundMatchesText([], '   '), true);
  assert.equal(roundMatchesText([], 'query'), false);
  assert.equal(roundMatchesText([{ step_id: 1, source: 'agent', tool_calls: [{ function_name: 'tool' }] }], 'missing'), false);
  assert.equal(roundMatchesText([{ step_id: 1, source: 'agent', observation: { results: [{ content: null }] } }], 'null'), false);
  assert.equal(roundMatchesText([{ step_id: 1, source: 'agent', tool_calls: [{ function_name: 'tool', arguments: null }] }], 'null'), true);
  assert.equal(roundMatchesText([{ step_id: 1, source: 'agent', reasoning_content: 42 }], '42'), false);
  assert.equal(JSON.stringify(steps), before);
});
