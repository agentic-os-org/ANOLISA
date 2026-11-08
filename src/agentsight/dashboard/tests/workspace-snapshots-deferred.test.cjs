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

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

const settle = () => new Promise(resolve => setTimeout(resolve, 0));

function createDriver() {
  const slots = [];
  const driver = {
    render(callback) {
      driver.cursor = 0;
      driver.effects = [];
      return { value: callback(), effects: driver.effects };
    },
    useState(initial) {
      const index = driver.cursor++;
      const slot = slots[index] ??= {
        value: typeof initial === 'function' ? initial() : initial,
      };
      slot.setter ??= update => {
        slot.value = typeof update === 'function' ? update(slot.value) : update;
      };
      return [slot.value, slot.setter];
    },
    useRef(initial) {
      const index = driver.cursor++;
      const slot = slots[index] ??= { value: { current: initial } };
      return slot.value;
    },
    useEffect(callback) { driver.effects.push(callback); },
    useCallback(callback) { return callback; },
    useMemo(callback) { return callback(); },
  };
  return driver;
}

function loadModule(relativePath, moduleStubs, driver) {
  const module = { exports: {} };
  const react = {
    __esModule: true,
    Fragment: Symbol.for('react.fragment'),
    createElement(type, props, ...children) {
      return { type, props: { ...(props ?? {}), children: children.length === 1 ? children[0] : children } };
    },
    useState: driver.useState,
    useRef: driver.useRef,
    useEffect: driver.useEffect,
    useCallback: driver.useCallback,
    useMemo: driver.useMemo,
  };
  react.default = react;
  const requireStub = name => {
    if (name === 'react') return react;
    if (moduleStubs[name]) return moduleStubs[name];
    throw new Error(`unexpected require from ${relativePath}: ${name}`);
  };
  new Function('require', 'module', 'exports', transpile(relativePath))(requireStub, module, module.exports);
  return module.exports;
}

function listResponse(marker, nextCursor = null) {
  return {
    document_digest: `digest-${marker}`,
    query_time: `query-${marker}`,
    workspace: { ws_id: `ws-${marker}`, path: `/workspace/${marker}` },
    evidence: null,
    groups: [],
    next_cursor: nextCursor,
    anomalies: [],
  };
}

function snapshot(id) {
  return {
    id,
    workspace: '/workspace',
    meta: { created_at: '2026-01-02T03:04:05Z', message: null, pinned: false, missing: false, parent_id: null },
  };
}

function group(stepIndex, ids) {
  return {
    step_index: stepIndex,
    step_id: stepIndex + 10,
    interval_start: '2026-01-02T03:00:00Z',
    interval_end: '2026-01-02T04:00:00Z',
    snapshots: ids.map(snapshot),
  };
}

function hookProbe(initialTarget) {
  const calls = [];
  const driver = createDriver();
  const hook = loadModule('src/utils/useWorkspaceSnapshots.ts', {
    './apiClient': {
      fetchWorkspaceSnapshots(target, options) {
        const pending = deferred();
        calls.push({ target, options, ...pending });
        return pending.promise;
      },
    },
  }, driver);
  let target = initialTarget;
  let cleanup;
  return {
    calls,
    render() {
      const rendered = driver.render(() => hook.useWorkspaceSnapshots(target));
      return rendered;
    },
    setTarget(next) { target = next; },
    cleanup() { cleanup?.(); },
    runEffect(rendered) {
      cleanup?.();
      cleanup = rendered.effects[0]();
    },
  };
}

const targetA = { type: 'session', id: 'session-a', trajectoryPath: [] };
const targetB = { type: 'session', id: 'session-b', trajectoryPath: ['child-b'] };

test('snapshot list ignores imported documents and drops a superseded target response', async () => {
  const probe = hookProbe(null);
  let rendered = probe.render();
  probe.runEffect(rendered);
  assert.equal(probe.calls.length, 0, 'an imported/null target must not query the server');
  assert.equal(rendered.value.loading, false);

  probe.setTarget(targetA);
  rendered = probe.render();
  probe.runEffect(rendered);
  assert.equal(probe.calls.length, 1);
  probe.setTarget(targetB);
  rendered = probe.render();
  probe.runEffect(rendered);
  assert.equal(probe.calls.length, 2);
  assert.equal(probe.calls[0].options.signal.aborted, true);

  probe.calls[1].resolve(listResponse('b'));
  await settle();
  probe.calls[0].resolve(listResponse('a'));
  await settle();
  rendered = probe.render();
  assert.equal(rendered.value.data.workspace.ws_id, 'ws-b');
  assert.equal(rendered.value.error, null);
  assert.equal(rendered.value.loading, false);
  probe.cleanup();
});

test('snapshot refresh invalidates the older fixed-time page', async () => {
  const probe = hookProbe(targetA);
  let rendered = probe.render();
  probe.runEffect(rendered);
  rendered.value.refresh();
  rendered = probe.render();
  probe.runEffect(rendered);
  assert.equal(probe.calls.length, 2);

  probe.calls[1].resolve(listResponse('fresh'));
  await settle();
  probe.calls[0].resolve(listResponse('stale'));
  await settle();
  rendered = probe.render();
  assert.equal(rendered.value.data.query_time, 'query-fresh');
  assert.equal(rendered.value.loading, false);
  probe.cleanup();
});

test('snapshot pagination merges split Step groups and de-duplicates entries', async () => {
  const probe = hookProbe(targetA);
  let rendered = probe.render();
  probe.runEffect(rendered);
  const repeated = {
    code: 'workspace_verification_failed',
    message: 'candidate failed',
    timestamp: '2026-01-01T10:00:00Z',
  };
  const first = {
    ...listResponse('a', 'cursor-1'),
    groups: [group(2, ['snapshot-2', 'snapshot-1'])],
    anomalies: [repeated, { code: 'first', message: 'first page' }],
  };
  probe.calls[0].resolve(first);
  await settle();
  rendered = probe.render();
  rendered.value.loadMore();
  assert.equal(probe.calls.length, 2);
  assert.equal(probe.calls[1].options.cursor, 'cursor-1');

  probe.calls[1].resolve({
    ...listResponse('a'),
    groups: [group(2, ['snapshot-1', 'snapshot-0']), group(5, ['snapshot-old'])],
    anomalies: [
      repeated,
      { code: 'second', message: 'second page' },
      { ...repeated, timestamp: '2026-01-01T11:00:00Z' },
    ],
  });
  await settle();
  rendered = probe.render();
  assert.deepEqual(rendered.value.data.groups.map(item => item.step_index), [2, 5]);
  assert.deepEqual(rendered.value.data.groups[0].snapshots.map(item => item.id), [
    'snapshot-2', 'snapshot-1', 'snapshot-0',
  ]);
  assert.deepEqual(rendered.value.data.anomalies.map(item => item.code), [
    'workspace_verification_failed', 'first', 'second', 'workspace_verification_failed',
  ]);
  assert.deepEqual(rendered.value.data.anomalies.map(item => item.timestamp), [
    '2026-01-01T10:00:00Z', undefined, undefined, '2026-01-01T11:00:00Z',
  ]);
  assert.equal(rendered.value.data.next_cursor, null);
  assert.equal(rendered.value.loadingMore, false);
  probe.cleanup();
});

function detailResponse(id) {
  return {
    workspace: { ws_id: 'ws-1', path: '/workspace' },
    snapshot: snapshot(id),
    step_index: 2,
    query_time: 'query-1',
    target_to_current: [{ path: `${id}.txt`, change_type: 'Modified', detail: null }],
    current_to_target: [{ path: `${id}.txt`, change_type: 'Modified', detail: null }],
    rollback_enabled: false,
    rollback_disabled_reason: 'disabled',
  };
}

function findAll(node, predicate, found = []) {
  if (node == null || typeof node !== 'object') return found;
  if (Array.isArray(node)) {
    for (const child of node) findAll(child, predicate, found);
    return found;
  }
  if (predicate(node)) found.push(node);
  findAll(node.props?.children, predicate, found);
  return found;
}

function text(node) {
  if (node == null || typeof node === 'boolean') return '';
  if (typeof node === 'string' || typeof node === 'number') return String(node);
  if (Array.isArray(node)) return node.map(text).join('');
  return text(node.props?.children);
}

test('snapshot detail keeps the newest response and leaves rollback disabled', async () => {
  const calls = [];
  const driver = createDriver();
  const component = loadModule('src/components/WorkspaceSnapshotDialog.tsx', {
    '../i18n': { useI18n: () => ({ t: key => key }), useLocaleTag: () => 'en-US' },
    '../utils/apiClient': {
      ApiRequestError: class ApiRequestError extends Error {},
      fetchWorkspaceSnapshotDetail(request, signal) {
        const pending = deferred();
        calls.push({ request, signal, ...pending });
        return pending.promise;
      },
    },
  }, driver);
  const base = {
    session_id: 'session-a',
    trajectory_path: '[]',
    document_digest: 'digest-1',
    query_time: 'query-1',
    workspace: { ws_id: 'ws-1', path: '/workspace' },
    step_index: 2,
  };
  let request = { ...base, snapshot_id: 'snapshot-a' };
  let rendered = driver.render(() => component.WorkspaceSnapshotDialog({ request, onClose() {} }));
  let cleanup = rendered.effects[0]();
  request = { ...base, snapshot_id: 'snapshot-b' };
  rendered = driver.render(() => component.WorkspaceSnapshotDialog({ request, onClose() {} }));
  cleanup();
  cleanup = rendered.effects[0]();
  assert.equal(calls[0].signal.aborted, true);

  calls[1].resolve(detailResponse('snapshot-b'));
  await settle();
  calls[0].resolve(detailResponse('snapshot-a'));
  await settle();
  rendered = driver.render(() => component.WorkspaceSnapshotDialog({ request, onClose() {} }));
  assert.equal(text(rendered.value).includes('snapshot-a'), false);
  assert.equal(text(rendered.value).includes('snapshot-b'), true);
  const diffs = findAll(rendered.value, node => Array.isArray(node.props?.entries));
  assert.deepEqual(diffs.map(node => node.props.entries[0].path), ['snapshot-b.txt', 'snapshot-b.txt']);
  const rollback = findAll(rendered.value, node => node.type === 'button' && text(node) === 'atif.confirmRollback')[0];
  assert.ok(rollback);
  assert.equal(rollback.props.disabled, true);
  assert.equal(rollback.props.onClick, undefined);
  cleanup();
});
