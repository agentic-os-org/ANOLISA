const assert = require('node:assert/strict');
const { join } = require('node:path');
const test = require('node:test');
const babel = require('@babel/core');

function transpile(relativePath) {
  return babel.transformFileSync(join(process.cwd(), relativePath), {
    presets: [
      ['@babel/preset-env', { targets: { node: 'current' } }],
      ['@babel/preset-typescript', { isTSX: true, allExtensions: true }],
      ['@babel/preset-react', { runtime: 'classic' }],
    ],
  }).code;
}

function loadWsCkpt(apiFetch) {
  const module = { exports: {} };
  new Function('require', 'module', 'exports', transpile('src/utils/wsCkpt.ts'))(
    (name) => {
      assert.equal(name, './apiClient');
      return { API_BASE: 'http://api.test', apiFetch };
    },
    module,
    module.exports,
  );
  return module.exports;
}

const snapshot = (id, createdAt, workspace = 'ws-1') => ({
  id, workspace, meta: { created_at: createdAt, message: id },
});
const atif = { schema_version: 'ATIF-v1.7', steps: [{ step_id: 1, message: 'one' }] };
const scope = { ws_id: 'ws-1', path: '/workspace', snapshot_count: 2 };

test('snapshot pages share a cancellation signal and merge before attachment', async () => {
  const calls = [];
  const pages = [
    {
      trajectory: atif, scope, association: { revision: 'r1', as_of: 't1' },
      step_snapshots: {}, completeness: { complete: false, next_cursor: 'cursor 1' },
    },
    {
      trajectory: atif, scope, association: { revision: 'r1', as_of: 't1' },
      step_snapshots: { '1': [snapshot('later', '2026-01-02'), snapshot('earlier', '2026-01-01')] },
      completeness: { complete: true, next_cursor: null },
    },
  ];
  const signal = new AbortController().signal;
  const api = loadWsCkpt(async (url, init) => {
    assert.equal(init.signal, signal);
    calls.push(url);
    return pages.shift();
  });

  const result = await api.fetchWorkspaceSnapshots('session', 'session / 1', signal);

  assert.deepEqual(calls, [
    'http://api.test/api/ws-ckpt/session/session%20%2F%201/snapshots',
    'http://api.test/api/ws-ckpt/session/session%20%2F%201/snapshots?cursor=cursor%201',
  ]);
  assert.deepEqual(result.step_snapshots['1'].map((row) => row.id), ['earlier', 'later']);
  assert.equal(result.completeness.complete, true);
});

test('snapshot page merge rejects a changed association revision or timestamp', async () => {
  for (const association of [{ revision: 'r2', as_of: 't1' }, { revision: 'r1', as_of: 't2' }]) {
    const pages = [
      { trajectory: atif, scope, association: { revision: 'r1', as_of: 't1' }, step_snapshots: {}, completeness: { next_cursor: 'c' } },
      { trajectory: atif, scope, association, step_snapshots: {}, completeness: { next_cursor: null } },
    ];
    const api = loadWsCkpt(async () => pages.shift());
    await assert.rejects(api.fetchWorkspaceSnapshots('conversation', 'c1'), /Snapshot page changed/);
  }
});

test('snapshot association is accepted only for an exact Step document match', () => {
  const api = loadWsCkpt(async () => {});
  const batch = { trajectory: atif, scope, step_snapshots: {} };
  assert.equal(api.snapshotsMatchDocument(batch, { ...atif, steps: [...atif.steps] }), true);
  assert.equal(api.snapshotsMatchDocument(batch, { ...atif, steps: [{ ...atif.steps[0], message: 'edited' }] }), false);
  assert.equal(api.snapshotsMatchDocument(batch, { ...atif, steps: [] }), false);
});

test('snapshot association accepts raw ATIF Steps after the Rust serde round trip', () => {
  const api = loadWsCkpt(async () => {});
  // This is the raw collected form. The snapshot endpoint parses it with
  // agentsight-atif and serializes it again before returning the trajectory.
  const raw = {
    schema_version: 'ATIF-v1.7',
    steps: [{
      source: 'agent', step_id: 1, reasoning_effort: null,
      tool_calls: [{ function_name: 'terminal', tool_call_id: 'call-1' }],
      observation: { results: [{ content: { z: 2, a: 1 }, source_call_id: null }] },
      extra: { provider: { z: true, a: false } },
    }],
  };
  const reserialized = {
    schema_version: 'ATIF-v1.7',
    steps: [{
      step_id: 1, source: 'agent', message: '',
      tool_calls: [{ tool_call_id: 'call-1', function_name: 'terminal', arguments: null }],
      observation: { results: [{ content: { a: 1, z: 2 } }] },
      extra: { provider: { a: false, z: true } },
    }],
  };
  assert.equal(api.snapshotsMatchDocument({ trajectory: reserialized, scope, step_snapshots: {} }, raw), true);
});

test('multimodal message conversion preserves the backend ContentPart key and array order', () => {
  const api = loadWsCkpt(async () => {});
  const parts = [
    { type: 'text', text: 'hello' },
    { type: 'image_url', image_url: { url: 'https://example.invalid/test.png', detail: 'low' } },
  ];
  const raw = { ...atif, steps: [{ step_id: 1, source: 'agent', message: parts }] };
  // agentsight-atif de_message uses serde_json with preserve_order enabled.
  const message = '[{"type":"text","text":"hello"},{"type":"image_url","image_url":{"url":"https://example.invalid/test.png","detail":"low"}}]';
  const batch = { trajectory: { ...raw, steps: [{ ...raw.steps[0], message }] }, scope, step_snapshots: {} };
  assert.equal(api.snapshotsMatchDocument(batch, raw), true);
  assert.equal(api.snapshotsMatchDocument(batch, {
    ...raw, steps: [{ ...raw.steps[0], message: [{ type: 'text', text: 'changed' }, parts[1]] }],
  }), false, 'different text must not match');
  assert.equal(api.snapshotsMatchDocument(batch, {
    ...raw, steps: [{ ...raw.steps[0], message: [...parts].reverse() }],
  }), false, 'ContentPart array order must remain significant');
  assert.equal(api.snapshotsMatchDocument(batch, {
    ...raw, steps: [{ ...raw.steps[0], message: message.replace('hello', 'changed') }],
  }), false, 'serialized message strings must remain exact');
});

test('snapshot association still rejects meaningful Step changes after normalization', () => {
  const api = loadWsCkpt(async () => {});
  const step = {
    step_id: 1, source: 'agent', message: 'ran command', timestamp: '2026-11-02T00:00:00Z',
    tool_calls: [{ tool_call_id: 'call-1', function_name: 'terminal', arguments: { command: 'pwd' } }],
    observation: { results: [{ content: { stdout: '/workspace' } }] },
  };
  const batch = { trajectory: { schema_version: 'ATIF-v1.7', steps: [step] }, scope, step_snapshots: {} };
  assert.equal(api.snapshotsMatchDocument(batch, {
    ...batch.trajectory,
    steps: [{ ...step, tool_calls: [{ ...step.tool_calls[0], arguments: { command: 'ls' } }] }],
  }), false, 'tool arguments must remain exact');
  assert.equal(api.snapshotsMatchDocument(batch, {
    ...batch.trajectory,
    steps: [{ ...step, observation: { results: [{ content: { stdout: '/tmp' } }] } }],
  }), false, 'result content must remain exact');
  assert.equal(api.snapshotsMatchDocument(batch, {
    ...batch.trajectory,
    steps: [{ ...step, timestamp: '2026-11-02T00:00:01Z' }],
  }), false, 'timestamps must remain exact');
});

test('diff, prepare, execute, and result API calls use the required targets and bodies', async () => {
  const calls = [];
  const api = loadWsCkpt(async (url, init) => {
    calls.push({ url, init });
    return {};
  });
  const target = { workspace: 'ws-1', snapshot_id: 'snap-1' };

  await api.fetchSnapshotDiff(target);
  await api.prepareSnapshotRestore(target, restorePreview);
  await api.executeSnapshotRestore('confirm-1');
  await api.fetchSnapshotRestoreResult('confirm-1');

  assert.deepEqual(calls.map(({ url }) => url), [
    'http://api.test/api/ws-ckpt/diff',
    'http://api.test/api/ws-ckpt/restore/prepare',
    'http://api.test/api/ws-ckpt/restore/execute',
    'http://api.test/api/ws-ckpt/restore/result',
  ]);
  assert.ok(calls.every(({ init }) => init.method === 'POST' && init.headers['Content-Type'] === 'application/json'));
  assert.deepEqual(calls.map(({ init }) => JSON.parse(init.body)), [
    target,
    { target, writers_stopped: true, expected_preview: restorePreview },
    { confirmation_token: 'confirm-1' },
    { confirmation_token: 'confirm-1' },
  ]);
});

test('snapshot 401 lets the viewer check workspace availability before prompting for login', async () => {
  const { apiFetch } = require(process.env.AGENTSIGHT_API_CLIENT_BUILD);
  const previousWindow = global.window;
  const previousFetch = global.fetch;
  global.window = { location: { hash: '#/atif?type=session&id=kept' } };
  let headStatus = 204;
  global.fetch = async (url, init) => {
    if (init?.method !== 'HEAD') return new Response(null, { status: 401 });
    assert.equal(url, 'http://api.test/api/ws-ckpt/session/kept/snapshots');
    return new Response(null, { status: headStatus });
  };
  try {
    const api = loadWsCkpt(apiFetch);
    await assert.rejects(api.fetchWorkspaceSnapshots('session', 'kept'), (error) => error?.status === 401);
    assert.equal(global.window.location.hash, '#/atif?type=session&id=kept',
      'the initial 401 alone must not replace the viewer route');
    assert.equal(await api.canUnlockWorkspaceSnapshots('session', 'kept'), true);
    for (headStatus of [404, 401, 500]) {
      assert.equal(await api.canUnlockWorkspaceSnapshots('session', 'kept'), false);
    }

    await assert.rejects(apiFetch('http://api.test/api/private'), /Authentication required/);
    assert.equal(global.window.location.hash, '#/login', 'ordinary API calls retain the default auth redirect');
  } finally {
    global.window = previousWindow;
    global.fetch = previousFetch;
  }
});

function createHooksDriver() {
  const slots = [];
  return {
    slots,
    render(Component, props) {
      this.cursor = 0;
      this.effects = [];
      const element = Component(props);
      return { element, effects: this.effects };
    },
    useState(initial) {
      const slot = slots[this.cursor] ?? { value: typeof initial === 'function' ? initial() : initial };
      slots[this.cursor] = slot;
      slot.setter ??= (next) => { slot.value = typeof next === 'function' ? next(slot.value) : next; };
      this.cursor += 1;
      return [slot.value, slot.setter];
    },
    useRef(initial) {
      const slot = slots[this.cursor] ?? { value: { current: initial } };
      slots[this.cursor] = slot;
      this.cursor += 1;
      return slot.value;
    },
    useEffect(fn) { this.effects.push(fn); },
  };
}

class ApiRequestError extends Error {
  constructor(status, body) { super(`HTTP ${status}`); this.status = status; this.body = body; }
}

function loadDialog(api) {
  const driver = createHooksDriver();
  const code = transpile('src/components/WorkspaceSnapshotDialog.tsx');
  const module = { exports: {} };
  const react = {
    __esModule: true,
    default: {
      createElement: (type, props, ...children) => ({ type, props: { ...props, children } }),
      useState: (...args) => driver.useState(...args),
      useRef: (...args) => driver.useRef(...args),
      useEffect: (...args) => driver.useEffect(...args),
    },
    createElement: (type, props, ...children) => ({ type, props: { ...props, children } }),
    useState: (...args) => driver.useState(...args),
    useRef: (...args) => driver.useRef(...args),
    useEffect: (...args) => driver.useEffect(...args),
  };
  const documentStub = { activeElement: null, body: { style: { overflow: '' } } };
  new Function('require', 'module', 'exports', 'document', code)((name) => {
    if (name === 'react') return react;
    if (name === '../i18n') return { useI18n: () => ({ t: (key, params = {}) => {
      const label = Object.entries(params).reduce((s, [k, v]) => s.replaceAll(`{${k}}`, String(v)), key);
      return Object.keys(params).length ? `${label} ${JSON.stringify(params)}` : label;
    } }), useLocaleTag: () => 'en-US' };
    if (name === '../utils/apiClient') return { ApiRequestError };
    if (name === '../utils/wsCkpt') return api;
    throw new Error(`Unexpected dialog dependency: ${name}`);
  }, module, module.exports, documentStub);
  return { Component: module.exports.WorkspaceSnapshotDialog, driver };
}

const targetSelection = { snapshot: snapshot('snap-1', '2026-01-02'), scope, restoreEnabled: true };
const changeList = [
  { path: 'src/main.rs', change_type: 'Modified', detail: 'content differs' },
  { path: 'draft.txt', change_type: 'Deleted', detail: null },
];
const restorePreview = { generation: Array(32).fill(7), diff_digest: Array(32).fill(6) };
const diff = {
  changes: changeList, scope, snapshot_id: 'snap-1', compared_at: '2026-01-03T00:00:00Z',
  restore_preview: restorePreview,
};
const confirmation = { confirmation_token: 'token-1', operation_id: 'op-1' };

function walk(node, output = []) {
  if (node == null || typeof node === 'boolean') return output;
  if (Array.isArray(node)) { for (const child of node) walk(child, output); return output; }
  if (typeof node !== 'object') return output;
  output.push(node);
  walk(node.props?.children, output);
  return output;
}
const textOf = (node) => walk(node).flatMap((item) => {
  const children = item.props?.children ?? [];
  return (Array.isArray(children) ? children : [children]).filter((v) => typeof v === 'string' || typeof v === 'number').map(String);
});
const find = (tree, predicate) => walk(tree).find(predicate);
const button = (tree, label) => find(tree, (node) => node.type === 'button' && textOf(node).includes(label));
const settle = () => new Promise(setImmediate);

async function mountedDialog(api, selection = targetSelection, restoreBlocked = false, callbacks = {}) {
  const { Component, driver } = loadDialog(api);
  const render = (open = true) => driver.render(Component, {
    selection, open, restoreBlocked, onClose() {}, onRestoreAttentionChange() {}, ...callbacks,
  });
  const initial = render();
  const effects = initial.effects.map((effect) => effect());
  await settle();
  return { driver, render, effects };
}

test('restore requires the writers-stopped checkbox and renders the expected workspace changes', async () => {
  const calls = { diff: [], prepare: [], execute: [] };
  const api = {
    fetchSnapshotDiff: (...args) => { calls.diff.push(args); return Promise.resolve(diff); },
    prepareSnapshotRestore: (...args) => { calls.prepare.push(args); return Promise.resolve(confirmation); },
    executeSnapshotRestore: (...args) => { calls.execute.push(args); return Promise.resolve({ status: 'succeeded' }); },
    fetchSnapshotRestoreResult: async () => ({ evidence: null }),
  };
  const mounted = await mountedDialog(api);
  await Promise.all(mounted.effects);
  const tree = mounted.render().element;
  assert.equal(find(tree, (node) => node.type === 'li' && textOf(node).includes('src/main.rs')) !== undefined, true);
  assert.ok(textOf(tree).includes('snapshot.wholeWorkspace'));
  const footer = find(tree, (node) => node.type === 'footer');
  assert.deepEqual(walk(footer).filter(node => node.type === 'button').map(textOf), [['snapshot.close'], ['snapshot.confirm']]);
  assert.equal(find(footer, (node) => node.type === 'input'), undefined);
  assert.equal(textOf(footer).some(text => text.includes('snapshot.fileCount')), false);
  assert.equal(button(tree, 'snapshot.confirm').props.disabled, true);
  assert.equal(calls.prepare.length, 0);
  assert.equal(calls.execute.length, 0);

  const checkbox = find(tree, (node) => node.type === 'input' && node.props.type === 'checkbox');
  checkbox.props.onChange({ target: { checked: true } });
  const checkedTree = mounted.render().element;
  button(checkedTree, 'snapshot.confirm').props.onClick();
  await settle();
  assert.deepEqual(calls.prepare, [[{ workspace: 'ws-1', snapshot_id: 'snap-1' }, restorePreview]]);
  assert.deepEqual(calls.execute, [['token-1']]);
});

test('large diffs render 20 files per page with footer controls and refresh resets the page', async () => {
  const changes = Array.from({ length: 20_001 }, (_, index) => ({
    path: `file-${index}.txt`, change_type: 'Added', detail: null,
  }));
  let fetches = 0;
  const mounted = await mountedDialog({
    fetchSnapshotDiff: async () => { fetches += 1; return { ...diff, changes }; },
  });
  const rows = (tree) => walk(tree).filter((node) => node.type === 'li');
  let tree = mounted.render().element;
  assert.equal(rows(tree).length, 20);
  assert.ok(textOf(rows(tree)[0]).includes('file-0.txt'));
  assert.ok(textOf(rows(tree)[19]).includes('file-19.txt'));
  const footer = find(tree, (node) => node.type === 'footer');
  assert.ok(textOf(footer).includes('snapshot.fileCount {"n":20001}'));
  assert.ok(textOf(footer).includes('common.page {"cur":1,"total":1001}'));
  assert.equal(button(footer, 'common.prev').props.disabled, true);
  find(footer, (node) => node.type === 'input').props.onChange({ target: { checked: true } });
  button(footer, 'common.next').props.onClick();

  tree = mounted.render().element;
  assert.equal(rows(tree).length, 20);
  assert.ok(textOf(rows(tree)[0]).includes('file-20.txt'));
  assert.ok(textOf(rows(tree)[19]).includes('file-39.txt'));
  assert.equal(button(tree, 'snapshot.confirm').props.disabled, false);
  assert.equal(fetches, 1);
  button(tree, 'snapshot.refresh').props.onClick();
  await settle();

  tree = mounted.render().element;
  assert.ok(textOf(rows(tree)[0]).includes('file-0.txt'));
  assert.equal(button(tree, 'snapshot.confirm').props.disabled, true);
  assert.equal(fetches, 2);
});

test('stale prepare refusal clears the diff and requires refresh before another confirmation', async () => {
  const calls = { diff: 0, prepare: 0, execute: 0 };
  const api = {
    fetchSnapshotDiff: async () => { calls.diff += 1; return diff; },
    prepareSnapshotRestore: async () => { calls.prepare += 1; throw new ApiRequestError(409, { error: 'workspace changed' }); },
    executeSnapshotRestore: async () => { calls.execute += 1; return { status: 'succeeded' }; },
    fetchSnapshotRestoreResult: async () => ({ evidence: null }),
  };
  const mounted = await mountedDialog(api);
  await Promise.all(mounted.effects);
  let tree = mounted.render().element;
  find(tree, (node) => node.type === 'input' && node.props.type === 'checkbox').props.onChange({ target: { checked: true } });
  tree = mounted.render().element;
  button(tree, 'snapshot.confirm').props.onClick();
  await settle();
  tree = mounted.render().element;
  assert.ok(textOf(tree).includes('snapshot.stalePreview'));
  assert.equal(button(tree, 'snapshot.confirm').props.disabled, true);
  assert.equal(find(tree, (node) => node.type === 'input' && node.props.type === 'checkbox'), undefined,
    'the stopped-writers confirmation is removed with the stale diff');
  assert.equal(calls.execute, 0);

  button(tree, 'snapshot.refresh').props.onClick();
  await settle();
  tree = mounted.render().element;
  assert.equal(calls.diff, 2, 'a fresh diff is fetched after the stale preparation response');
  assert.equal(button(tree, 'snapshot.confirm').props.disabled, true, 'writers must be checked again after refresh');
});

for (const [label, failure] of [
  ['transport error', new Error('connection reset after request write')],
  ['HTTP 502', new ApiRequestError(502, { error: 'upstream response was lost' })],
  ['HTTP 500', new ApiRequestError(500, { error: 'proxy could not confirm dispatch' })],
]) {
  test(`unknown ${label} keeps the operation queryable across close and reopen`, async () => {
    const calls = { execute: 0, result: 0, close: 0, pending: [] };
    const { Component, driver } = loadDialog({
      fetchSnapshotDiff: async () => diff,
      prepareSnapshotRestore: async () => confirmation,
      executeSnapshotRestore: async () => { calls.execute += 1; throw failure; },
      fetchSnapshotRestoreResult: async (token, signal) => {
        calls.result += 1;
        assert.equal(token, 'token-1');
        assert.ok(signal instanceof AbortSignal);
        return { evidence: { outcome: { Unknown: { reason: 'still running' } } } };
      },
    });
    const render = (open = true) => driver.render(Component, {
      selection: targetSelection, open, onClose() { calls.close += 1; },
      onRestoreAttentionChange(value) { calls.pending.push(value); },
    });
    const initial = render();
    initial.effects.forEach((effect) => effect());
    await settle();
    let tree = render().element;
    find(tree, (node) => node.type === 'input' && node.props.type === 'checkbox').props.onChange({ target: { checked: true } });
    tree = render().element;
    button(tree, 'snapshot.confirm').props.onClick();
    await settle();
    tree = render().element;
    assert.ok(textOf(tree).includes('snapshot.unknown'));
    assert.ok(JSON.stringify(tree).includes('op-1'), 'the operation identifier is displayed');
    assert.deepEqual(calls.pending, ['pending']);
    assert.equal(button(tree, 'snapshot.confirm').props.disabled, true);
    assert.equal(button(tree, 'snapshot.cancelRestore').props.disabled, true);
    const stopChecking = render().effects.at(-1)();
    await settle();
    assert.equal(calls.result, 1, 'the first lookup starts while the dialog is still open');
    const close = find(tree, (node) => node.type === 'button' && node.props['aria-label'] === 'snapshot.close');
    close.props.onClick();
    stopChecking();
    assert.equal(calls.close, 1);
    assert.equal(render(false).element, null);
    const reopened = render(true);
    tree = reopened.element;
    assert.ok(JSON.stringify(tree).includes('op-1'), 'closing hides the dialog without discarding the operation');
    const stopReopenedCheck = reopened.effects.at(-1)();
    await settle();
    tree = render().element;
    assert.ok(textOf(tree).includes('snapshot.checkingResult'));
    assert.equal(button(tree, 'snapshot.confirm').props.disabled, true);
    assert.equal(button(tree, 'snapshot.cancelRestore').props.disabled, true);
    assert.equal(calls.execute, 1, 'reopening a pending operation must never execute it again');
    assert.equal(calls.result, 2);
    assert.deepEqual(calls.pending, ['pending']);
    stopReopenedCheck();
  });
}

test('automatic result lookup updates the open dialog without another rollback', async () => {
  const calls = { prepare: 0, execute: 0, result: 0 };
  const mounted = await mountedDialog({
    fetchSnapshotDiff: async () => diff,
    prepareSnapshotRestore: async () => { calls.prepare += 1; return confirmation; },
    executeSnapshotRestore: async () => { calls.execute += 1; return { status: 'unknown' }; },
    fetchSnapshotRestoreResult: async (token) => {
      assert.equal(token, confirmation.confirmation_token);
      calls.result += 1;
      return { evidence: { outcome: { Succeeded: {} } } };
    },
  });
  const tree = () => mounted.render().element;
  find(tree(), (node) => node.type === 'input').props.onChange({ target: { checked: true } });
  button(tree(), 'snapshot.confirm').props.onClick();
  await settle();
  mounted.render().effects.at(-1)();
  await settle();
  const finished = tree();
  assert.ok(textOf(finished).includes('snapshot.querySucceeded'));
  assert.equal(button(finished, 'snapshot.confirm'), undefined);
  assert.equal(button(finished, 'snapshot.cancelRestore'), undefined);
  assert.deepEqual(calls, { prepare: 1, execute: 1, result: 1 });
});

test('known-no-effect refusal retains a failure until the same snapshot is successfully retried', async () => {
  const calls = { diff: 0, prepare: [], execute: [], attention: [] };
  const refreshedChanges = [{ path: 'new.txt', change_type: 'Added', detail: null }];
  const refreshedPreview = { ...restorePreview, diff_digest: Array(32).fill(8) };
  const mounted = await mountedDialog({
    fetchSnapshotDiff: async () => ++calls.diff === 1 ? diff : {
      ...diff, changes: refreshedChanges, restore_preview: refreshedPreview,
    },
    prepareSnapshotRestore: async (target, preview) => {
      assert.deepEqual(target, { workspace: 'ws-1', snapshot_id: 'snap-1' });
      calls.prepare.push(preview);
      return { ...confirmation, confirmation_token: `token-${calls.prepare.length}` };
    },
    executeSnapshotRestore: async (token) => {
      calls.execute.push(token);
      if (calls.execute.length === 1) throw new ApiRequestError(502, { error: 'writer still active', effect: 'known_no_effect' });
      return { status: 'succeeded' };
    },
    fetchSnapshotRestoreResult: async () => { assert.fail('a known refusal must not require outcome lookup'); },
  }, targetSelection, false, { onRestoreAttentionChange: status => calls.attention.push(status) });
  let tree = mounted.render().element;
  find(tree, node => node.type === 'input').props.onChange({ target: { checked: true } });
  button(mounted.render().element, 'snapshot.confirm').props.onClick();
  await settle();
  tree = mounted.render().element;
  assert.equal(button(tree, 'snapshot.cancelRestore').props.disabled, false);
  assert.ok(textOf(tree).includes('snapshot.restoreFailed'));
  assert.equal(button(tree, 'snapshot.refresh').props.disabled, false);
  assert.equal(button(tree, 'snapshot.confirm').props.disabled, true);
  button(tree, 'snapshot.refresh').props.onClick();
  await settle();
  tree = mounted.render().element;
  assert.equal(button(tree, 'snapshot.confirm').props.disabled, true, 'refresh does not acknowledge stopped writers');
  find(tree, node => node.type === 'input').props.onChange({ target: { checked: true } });
  button(mounted.render().element, 'snapshot.confirm').props.onClick();
  await settle();
  assert.deepEqual(calls.prepare, [restorePreview, refreshedPreview]);
  assert.deepEqual(calls.execute, ['token-1', 'token-2']);
  assert.deepEqual(calls.attention, ['pending', 'failed', 'pending', null]);
  assert.ok(textOf(mounted.render().element).includes('snapshot.succeeded'));
});

test('cancelling a rejected rollback clears its failure without issuing another request', async () => {
  const calls = { execute: 0, close: 0, attention: [] };
  const mounted = await mountedDialog({
    fetchSnapshotDiff: async () => diff,
    prepareSnapshotRestore: async () => confirmation,
    executeSnapshotRestore: async () => {
      calls.execute += 1;
      throw new ApiRequestError(502, { error: 'writer still active', effect: 'known_no_effect' });
    },
  }, targetSelection, false, {
    onClose() { calls.close += 1; },
    onRestoreAttentionChange(status) { calls.attention.push(status); },
  });
  const tree = () => mounted.render().element;
  find(tree(), node => node.type === 'input').props.onChange({ target: { checked: true } });
  button(tree(), 'snapshot.confirm').props.onClick();
  await settle();
  button(tree(), 'snapshot.cancelRestore').props.onClick();
  assert.deepEqual(calls, { execute: 1, close: 1, attention: ['pending', 'failed', null] });
  assert.equal(button(tree(), 'snapshot.cancelRestore'), undefined);
  assert.ok(!textOf(tree()).includes('snapshot.restoreFailed'));
});
test('read-only server shows the diff but disables restore controls', async () => {
  const mounted = await mountedDialog({
    fetchSnapshotDiff: async () => ({ ...diff, restore_preview: undefined }),
    prepareSnapshotRestore: async () => { throw new Error('must not prepare'); },
    executeSnapshotRestore: async () => { throw new Error('must not execute'); },
    fetchSnapshotRestoreResult: async () => ({ evidence: null }),
  }, { ...targetSelection, restoreEnabled: false });
  await Promise.all(mounted.effects);
  const tree = mounted.render().element;
  assert.ok(textOf(tree).includes('snapshot.disabled'));
  assert.equal(find(tree, (node) => node.type === 'input' && node.props.type === 'checkbox'), undefined);
  assert.equal(button(tree, 'snapshot.confirm').props.disabled, true);
});

test('another pending rollback leaves this snapshot readable and blocks preparation', async () => {
  const mounted = await mountedDialog({
    fetchSnapshotDiff: async () => diff,
    prepareSnapshotRestore: async () => { assert.fail('must resolve the pending rollback first'); },
  }, targetSelection, true);
  const tree = mounted.render().element;
  assert.ok(textOf(tree).includes('src/main.rs'));
  assert.ok(textOf(tree).includes('snapshot.restoreBlocked'));
  assert.equal(button(tree, 'snapshot.refresh').props.disabled, false);
  assert.equal(button(tree, 'snapshot.confirm').props.disabled, true);
  assert.equal(find(tree, (node) => node.type === 'input'), undefined);
});
