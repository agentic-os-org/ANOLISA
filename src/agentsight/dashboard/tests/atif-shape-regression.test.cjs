const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { dirname, join } = require('node:path');
const { runInNewContext } = require('node:vm');
const test = require('node:test');

const step = (extra = {}) => ({ step_id: 1, source: 'agent', message: 'message', ...extra });
const document = (extra = {}) => ({ schema_version: 'ATIF-v1.7', session_id: 'fixture', steps: [step()], ...extra });

function viewer(locale = 'en-US') {
  const states = [], refs = [], readers = [], requests = [], downloads = [];
  let stateIndex = 0, refIndex = 0, child = false;
  const react = {
    useState(initial) {
      if (child) return [typeof initial === 'function' ? initial() : initial, () => {}];
      const index = stateIndex++;
      if (!(index in states)) states[index] = typeof initial === 'function' ? initial() : initial;
      return [states[index], (value) => { states[index] = typeof value === 'function' ? value(states[index]) : value; }];
    },
    useRef(initial) { return refs[refIndex++] ??= { current: initial }; },
    useCallback: (callback) => callback,
    useMemo: (callback) => callback(),
    useEffect: () => {},
    createContext: () => ({}),
  };
  const output = dirname(process.env.AGENTSIGHT_ATIF_PAGE_BUILD);
  const tree = require(join(output, '../utils/trajectoryTree.js'));
  const roundModel = require(join(output, '../utils/roundModel.js'));
  let messages;
  const t = (key, params = {}) => Object.entries(params).reduce(
    (value, [name, replacement]) => value.replaceAll(`{${name}}`, String(replacement)),
    messages[locale][key],
  );
  const modules = {};
  function load(file, extra = '') {
    const exports = {};
    runInNewContext(readFileSync(file, 'utf8') + extra, {
      exports, URLSearchParams, Blob,
      URL: { createObjectURL(blob) { downloads.push(blob); return 'blob:private'; }, revokeObjectURL() {} },
      document: { createElement: () => ({ click() {} }) },
      FileReader: class { constructor() { readers.push(this); } readAsText() {} },
      require(name) {
        if (name === 'react') return react;
        if (name === 'react/jsx-runtime') return { jsx: (type, props) => ({ type, props }), jsxs: (type, props) => ({ type, props }) };
        if (name === 'react-router-dom') return { useSearchParams: () => [new URLSearchParams(), () => {}] };
        if (name === '../i18n') return { useI18n: () => ({ t }), useLocaleTag: () => locale };
        if (name === '../utils/trajectoryTree') return tree;
        if (name === '../utils/roundModel') return roundModel;
        if (name === '../utils/savings') return require(join(output, '../utils/savings.js'));
        if (name === '../utils/trajectoryTextFilter') return require(join(output, '../utils/trajectoryTextFilter.js'));
        if (name === '../utils/apiClient') return {
          fetchAtifBySession: (...args) => new Promise((resolve, reject) => requests.push({ kind: 'session', args, resolve, reject })),
          fetchTrajectoryAtif: (...args) => new Promise((resolve, reject) => requests.push({ kind: 'collected', args, resolve, reject })),
          fetchAtifByConversation: (...args) => new Promise((resolve, reject) => requests.push({ kind: 'conversation', args, resolve, reject })),
          fetchSessionSavings: async () => ({ items: [], total_compounded_saved: 0 }),
        };
        if (modules[name]) return modules[name];
        if (name === '../components/CausalAttributionPanel') return { CausalAttributionPanel: () => null };
        throw new Error(`Unexpected ATIF dependency ${name}`);
      },
    });
    return exports;
  }
  messages = load(join(output, '../i18n.js')).messages;
  modules['../components/SubagentGraph'] = load(join(output, '../components/SubagentGraph.js'));
  const page = load(process.env.AGENTSIGHT_ATIF_PAGE_BUILD, '\nexports.parts = { isAtifDocument, StepCard };');
  function render() { stateIndex = 0; refIndex = 0; return page.AtifViewerPage(); }
  function nodes(node, predicate) {
    if (!node || typeof node !== 'object') return [];
    return [...(predicate(node) ? [node] : []), ...[node.props?.children].flat(Infinity).flatMap((value) => nodes(value, predicate))];
  }
  function renderChildren(node) {
    if (node == null || typeof node !== 'object') return node;
    if (Array.isArray(node)) return node.map(renderChildren);
    if (typeof node.type === 'function') return renderChildren(node.type(node.props));
    return { ...node, props: { ...node.props, children: renderChildren(node.props?.children) } };
  }
  return {
    requests, downloads, parts: page.parts, t,
    messageText(value) {
      const card = page.parts.StepCard({ step: value, expandedSections: new Set(), onToggleSection() {} });
      return nodes(card, node => node.type?.name === 'ExpandableText')[0]?.props.text;
    },
    import(data) {
      const input = nodes(render(), (node) => node.type === 'input' && node.props.type === 'file')[0];
      input.props.onChange({ target: { files: [{ name: 'fixture.json' }], value: 'fixture.json' } });
      readers.at(-1).onload({ target: { result: JSON.stringify(data) } });
    },
    load(kind = 'session') {
      if (kind === 'conversation') nodes(render(), (node) => node.type === 'button' && node.props.children === t('atif.byConversation'))[0].props.onClick();
      nodes(render(), (node) => node.type === 'input' && node.props.type === 'text')[0].props.onChange({ target: { value: 'fixture' } });
      nodes(render(), (node) => node.type === 'button' && node.props.children === t('atif.load'))[0].props.onClick();
    },
    error() { return nodes(render(), (node) => node.props?.className === 'bg-red-50 border border-red-200 rounded-xl p-4 text-red-600 text-sm')[0]?.props.children.at(-1); },
    session() { return nodes(render(), (node) => node.type === 'span' && node.props.className === 'text-xs text-gray-400 font-mono truncate')[0]?.props.children; },
    renderAll() { const element = render(); child = true; try { return renderChildren(element); } finally { child = false; } },
    renderStep(value) {
      child = true;
      try { return renderChildren(page.parts.StepCard({ step: value, expandedSections: new Set(['1-toolcalls', '1-observation', '1-reasoning']), onToggleSection() {} })); }
      finally { child = false; }
    },
    download() { nodes(render(), (node) => node.type === 'button' && node.props.children === t('atif.downloadJson'))[0].props.onClick(); },
  };
}

const malformed = [
  null, [], { schema_version: 7 },
  document({ steps: [null] }), document({ steps: {} }),
  document({ session_id: 7 }), document({ trajectory_id: {} }), document({ notes: [] }),
  document({ agent: { name: 7, version: '1' } }),
  document({ agent: { name: 'agent', version: {} } }),
  document({ agent: { name: 'agent', version: '1', model_name: [] } }),
  document({ agent: { name: 'agent', version: '1', tool_definitions: {} } }),
  document({ steps: [step({ step_id: '1' })] }), document({ steps: [step({ source: {} })] }),
  document({ steps: [step({ message: {} })] }), document({ steps: [step({ reasoning_content: [] })] }),
  document({ steps: [step({ timestamp: {} })] }), document({ steps: [step({ model_name: [] })] }),
  document({ steps: [step({ tool_calls: [null] })] }), document({ steps: [step({ tool_calls: {} })] }),
  document({ steps: [step({ tool_calls: [{ tool_call_id: 1, function_name: 'tool' }] })] }),
  document({ steps: [step({ tool_calls: [{ tool_call_id: 'call', function_name: {} }] })] }),
  document({ steps: [step({ observation: [] })] }),
  document({ steps: [step({ observation: { results: [null] } })] }),
  document({ steps: [step({ observation: { results: {} } })] }),
  document({ steps: [step({ observation: { results: [{ source_call_id: [] }] } })] }),
  document({ steps: [step({ observation: { results: [{ subagent_trajectory_ref: [null] }] } })] }),
  document({ steps: [step({ observation: { results: [{ subagent_trajectory_ref: [{ trajectory_id: [] }] }] } })] }),
  document({ steps: [step({ metrics: { prompt_tokens: '3' } })] }),
  document({ final_metrics: { total_steps: {} } }), document({ final_metrics: [] }),
  document({ subagent_trajectories: [document({ steps: [null] })] }), document({ subagent_trajectories: [null] }),
  ...[
    [null], [{}], [{ type: 'text', text: 3 }],
    [{ type: 'image', source: null }],
    [{ type: 'image', source: { media_type: 'image/png', path: 3 } }],
    [{ type: 'image', source: { media_type: 'text/plain', path: 'image.txt' } }],
    [{ type: 'image', source: { media_type: ['image/png'], path: 'image.png' } }],
    [{ type: 'text', text: 'text', source: { media_type: 'image/png', path: 'image.png' } }],
    [{ type: 'image', text: 'unexpected text', source: { media_type: 'image/png', path: 'image.png' } }],
    [{ type: 'unknown' }],
  ].map(message => document({ steps: [step({ message })] })),
];

const contentParts = [
  { type: 'text', text: 'What color is the square?' },
  { type: 'image', source: { media_type: 'image/png', path: 'images/square.png' } },
];

test('ATIF v1.6/v1.7 text and image message parts render and export unchanged', async () => {
  const messages = [contentParts, [contentParts[0]], [], ...['image/jpeg', 'image/png', 'image/gif', 'image/webp'].map(media_type => [
    { type: 'image', source: { media_type, path: 'images/fixture' } },
  ])];
  const cases = ['ATIF-v1.6', 'ATIF-v1.7'].flatMap(schema_version => messages.map(message => ({ schema_version, message })));
  for (const { schema_version, message } of cases) {
    const data = document({
      schema_version,
      agent: { name: 'fixture-agent', version: '1' },
      steps: [step({ source: 'user', message })],
      subagent_trajectories: [document({ trajectory_id: 'child', steps: [step({ message })] })],
    });
    const page = viewer();
    assert.equal(page.parts.isAtifDocument(data), true);
    page.import(data);
    assert.equal(page.error(), undefined);
    assert.equal(page.session(), 'fixture');
    const rendered = JSON.stringify(page.renderAll());
    if (message.length > 0) assert.ok(rendered.includes(message[0].text ?? message[0].source.path));
    assert.equal(typeof page.messageText(data.steps[0]), 'string');
    assert.deepEqual(JSON.parse(page.messageText(data.steps[0])), message);
    const { roundMatchesText } = require(join(dirname(process.env.AGENTSIGHT_ATIF_PAGE_BUILD), '../utils/trajectoryTextFilter.js'));
    for (const part of message) {
      assert.equal(roundMatchesText(data.steps, part.text ?? part.source.path), true);
    }
    page.download();
    assert.deepEqual(JSON.parse(await page.downloads[0].text()), data);
  }
});

test('valid content-part conversation and session responses remain viewable', async () => {
  for (const kind of ['conversation', 'session']) {
    const data = document({ steps: [step({ source: 'user', message: contentParts })] });
    const page = viewer();
    page.load(kind);
    page.requests[0].resolve(data);
    await new Promise(setImmediate);
    assert.equal(page.session(), 'fixture');
    assert.equal(page.error(), undefined);
    assert.doesNotThrow(() => page.renderAll());
  }
});

for (const [index, data] of malformed.entries()) {
  test(`consumed malformed shape ${index} cannot replace a valid import`, () => {
    const page = viewer();
    page.import(document({ session_id: 'kept' }));
    page.import(data);
    assert.equal(page.session(), 'kept', `malformed fixture ${index} replaced the document`);
    assert.ok(page.error(), `malformed fixture ${index} did not report an error`);
  });
}

test('optional nullable fields and arbitrary JSON payloads remain supported and export unchanged', async () => {
  const data = document({ agent: null, notes: null, trajectory_id: null, final_metrics: null, extra: [1, 'extension'], steps: [step({
    timestamp: null, model_name: null, reasoning_content: null, metrics: null,
    tool_calls: [{ tool_call_id: 'call', function_name: 'tool', arguments: [null, false, { nested: 'value' }] }],
    observation: { results: [{ source_call_id: null, content: { output: [1, null] }, subagent_trajectory_ref: null }] },
  })], subagent_trajectories: null });
  const page = viewer();
  page.import(data);
  assert.equal(page.session(), 'fixture');
  assert.equal(page.error(), undefined);
  assert.doesNotThrow(() => page.renderStep(data.steps[0]));
  page.download();
  assert.deepEqual(JSON.parse(await page.downloads[0].text()), data);
  for (const value of [document({ steps: null }), document({ agent: { name: 'agent' } }), { schema_version: 'ATIF-v1.7' }]) assert.equal(page.parts.isAtifDocument(value), true);
});

test('omitted default tool arguments render through the real step and tool components', () => {
  const page = viewer();
  const value = step({ tool_calls: [{ tool_call_id: 'call', function_name: 'tool' }] });
  page.import(document({ steps: [value] }));
  assert.equal(page.session(), 'fixture');
  assert.doesNotThrow(() => page.renderStep(value));
});

test('malformed conversation responses become localized load errors', async () => {
  for (const locale of ['en-US', 'zh-CN']) {
    const page = viewer(locale);
    page.load('conversation');
    page.requests[0].resolve(document({ steps: [null] }));
    await new Promise(setImmediate);
    assert.equal(page.session(), undefined);
    assert.equal(page.error(), page.t('atif.malformedDocument'));
    assert.doesNotThrow(() => page.renderAll());
  }
});

test('a malformed export falls back to a valid collected document and validates the fallback', async () => {
  for (const valid of [true, false]) {
    const page = viewer();
    page.load();
    page.requests[0].resolve(document({ steps: [null] }));
    await new Promise(setImmediate);
    assert.equal(page.requests[1]?.kind, 'collected');
    page.requests[1].resolve(valid ? document() : document({ steps: [step({ message: {} })] }));
    await new Promise(setImmediate);
    assert.equal(page.session(), valid ? 'fixture' : undefined);
    assert.equal(page.error(), valid ? undefined : page.t('atif.malformedCollected', { id: 'fixture' }));
  }
});

test('embedded document identities survive validation and render in the actual graph', () => {
  const data = document({ subagent_trajectories: [document({ session_id: 'child', trajectory_id: 'child/id', agent: { name: 'agentsight-opt:child', version: '1' }, steps: [] })] });
  const page = viewer();
  assert.equal(page.parts.isAtifDocument(data), true);
  page.import(data);
  assert.doesNotThrow(() => page.renderAll());
  assert.ok(JSON.stringify(page.renderAll()).includes('child'), 'the actual graph renders the embedded child');
  assert.equal(data.subagent_trajectories[0].trajectory_id, 'child/id');
});
