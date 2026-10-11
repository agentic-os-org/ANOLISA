const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { runInNewContext } = require('node:vm');
const test = require('node:test');

const model = {};
runInNewContext(readFileSync(process.env.AGENTSIGHT_SESSION_MODEL_BUILD, 'utf8'), {
  exports: model, require: () => ({}),
});
const merge = (ebpf, logs) => JSON.parse(JSON.stringify(model.mergeSessions(ebpf, logs)));
const captured = (id, extra = {}) => ({
  session_id: id, agent_name: 'capture', model: 'ebpf-model', conversation_count: 3,
  total_input_tokens: 20, total_output_tokens: 10, first_user_query: 'capture first',
  last_user_query: 'capture last', last_seen_ns: 1_000_000, ...extra,
});
const logged = (id, extra = {}) => ({
  session_id: id, agent_name: 'log-agent', model_name: 'log-model', project: '/project',
  num_steps: 7, total_prompt_tokens: 40, total_completion_tokens: 30,
  collected_at_ns: 2_000_000, first_user_message: 'log first', last_user_message: 'log last',
  is_subagent: false, ...extra,
});

test('pure model preserves source precedence and does not mutate frozen input', () => {
  const ebpf = Object.freeze([Object.freeze(captured('same'))]);
  const logs = Object.freeze([Object.freeze(logged('same'))]);
  assert.deepEqual(merge(ebpf, logs), [{
    session_id: 'same', sources: ['ebpf', 'log'], agent_name: 'capture', project: '/project',
    model: 'ebpf-model', count: 3, input_tokens: 20, output_tokens: 10,
    first_message: 'log first', last_message: 'log last', last_active_ms: 2, subagent_count: 0,
  }]);
  assert.equal(ebpf[0].model, 'ebpf-model');
  assert.equal(logs[0].session_id, 'same');
});

test('Codex rollout UUID aliases retain the captured row identity', () => {
  const id = '00000000-0000-0000-0000-000000000001';
  const result = merge([captured(id)], [logged(`rollout-2026-10-01-${id}`)]);
  assert.equal(result.length, 1);
  assert.equal(result[0].session_id, id);
  assert.deepEqual(result[0].sources, ['ebpf', 'log']);
});

test('log parents hide children regardless of arrival order and orphan children remain', () => {
  const child = logged('parent:subagent:child', { is_subagent: true });
  for (const logs of [[child, logged('parent')], [logged('parent'), child]]) {
    const result = merge([], logs);
    assert.equal(result.length, 1);
    assert.equal(result[0].session_id, 'parent');
    assert.equal(result[0].subagent_count, 1);
  }
  assert.equal(merge([], [child])[0].session_id, child.session_id);
  assert.equal(merge([captured('parent')], [child]).length, 1);
});

test('last activity precedence, invalid timestamp fallback and stable ties are unchanged', () => {
  const result = merge([captured('older'), captured('tie')], [
    logged('fallback', { end_time: 'invalid', collected_at_ns: 4_000_000 }),
    logged('dated', { end_time: '2026-01-01T00:00:00Z' }),
    logged('unknown', { collected_at_ns: 0 }),
    logged('older', { collected_at_ns: 0 }),
  ]);
  assert.deepEqual(result.map((row) => row.session_id), ['dated', 'fallback', 'older', 'tie', 'unknown']);
  assert.equal(result[2].last_active_ms, 1);
  assert.equal(result[4].last_active_ms, null);
});

test('missing captured display values fall back to logs while nullable previews keep capture', () => {
  const result = merge([captured('same', { agent_name: '', model: null })], [logged('same', {
    first_user_message: null, last_user_message: null,
  })]);
  assert.equal(result[0].agent_name, 'log-agent');
  assert.equal(result[0].model, 'log-model');
  assert.equal(result[0].first_message, 'capture first');
  assert.equal(result[0].last_message, 'capture last');
  assert.deepEqual(merge(null, null), []);
});

test('parent lookup reads a large orphan window linearly', () => {
  const length = 200;
  let rowReads = 0;
  const logs = new Proxy(Array.from({ length }, (_, i) => logged(`missing-${i}:subagent:c`, {
    is_subagent: true,
  })), {
    get(target, key, receiver) {
      if (typeof key === 'string' && /^\d+$/.test(key)) rowReads += 1;
      return Reflect.get(target, key, receiver);
    },
  });
  assert.equal(merge([], logs).length, length);
  assert.ok(rowReads <= length * 6, `read ${rowReads} rows for ${length} inputs`);
});

test('an eBPF-only parent hides and counts its aliased log-side children', () => {
  // A Codex parent captured only by eBPF is keyed by the bare trailing UUID
  // while its log-collected children carry the full rollout stem in their
  // composite ids. The parent IS present, so the children must fold into it
  // (no stray orphan rows) and its badge must show their count.
  const uuid = '00000000-0000-0000-0000-000000000123';
  const stem = `rollout-2026-10-02T09-30-00-${uuid}`;
  const children = [
    logged(`${stem}:subagent:c1`, { is_subagent: true }),
    logged(`${stem}:subagent:c2`, { is_subagent: true }),
  ];
  const result = merge([captured(uuid)], children);
  assert.equal(result.length, 1, 'both children must fold into the aliased parent');
  assert.equal(result[0].session_id, uuid);
  assert.deepEqual(result[0].sources, ['ebpf']);
  assert.equal(result[0].subagent_count, 2, 'the eBPF-only parent shows its child count');
});

test('an aliased parent present in both sources counts its children once', () => {
  // Both sources hold the parent (eBPF bare UUID, log rollout stem) plus one
  // child: the alias tallies and the merged-branch carry-over must agree, not
  // double-count.
  const uuid = '00000000-0000-0000-0000-000000000456';
  const stem = `rollout-2026-10-03T11-00-00-${uuid}`;
  const result = merge([captured(uuid)], [
    logged(stem),
    logged(`${stem}:subagent:c1`, { is_subagent: true }),
  ]);
  assert.equal(result.length, 1);
  assert.deepEqual(result[0].sources, ['ebpf', 'log']);
  assert.equal(result[0].subagent_count, 1);
});

test('an aliased orphan child without any parent row stays visible', () => {
  // The alias resolution must not swallow genuine orphans: no parent row in
  // either source means the child stays reachable as its own row.
  const uuid = '00000000-0000-0000-0000-000000000789';
  const stem = `rollout-2026-10-04T12-00-00-${uuid}`;
  const child = logged(`${stem}:subagent:c1`, { is_subagent: true });
  const result = merge([], [child]);
  assert.equal(result.length, 1);
  assert.equal(result[0].session_id, child.session_id);
});

test('the production page keeps its public model export and consumes the same function', () => {
  const page = {};
  runInNewContext(readFileSync(process.env.AGENTSIGHT_SESSION_PAGE_BUILD, 'utf8'), {
    exports: page,
    require: (id) => id === '../utils/sessionModel' ? model : {},
  });
  assert.equal(page.mergeSessions, model.mergeSessions);
});
