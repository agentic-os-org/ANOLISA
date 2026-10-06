const assert = require('node:assert/strict');
const { join } = require('node:path');
const test = require('node:test');

// Pins the subagent badge on merged Codex sessions: mergeSessions tallies
// children of log-collected parents by the composite parent key. A Codex
// session observed through both eBPF and logs merges the collected
// `rollout-<ts>-<uuid>` parent into the eBPF row keyed by the bare UUID,
// but the eBPF row's subagent_count was initialized from the *other* parent
// key and stayed 0, hiding the collected children from the badge (#5547).
//
// Since the session merge model was extracted from the React page
// (utils/sessionModel.ts), these tests transpile that pure module with the
// dashboard's babel toolchain — same approach as before the extraction, now
// against the model seam itself — and drive the exported mergeSessions
// against plain SessionSummary / TrajectorySummary fixtures. The page keeps
// re-exporting the same function for production callers.

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

function loadMergeSessions() {
  // sessionModel.ts is a pure module: its only import is a type-only import
  // from apiClient, which the typescript preset strips, so the transpiled
  // code performs no runtime requires.
  const module = { exports: {} };
  new Function('require', 'module', 'exports', transpile('src/utils/sessionModel.ts'))(
    (name) => {
      throw new Error(`unexpected require from src/utils/sessionModel.ts: ${name}`);
    },
    module,
    module.exports,
  );
  assert.equal(typeof module.exports.mergeSessions, 'function', 'mergeSessions must be exported');
  return module.exports.mergeSessions;
}

const UUID = 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee';
const ROLLOUT = `rollout-2026-10-04T10-00-00-${UUID}`;

function ebpfSession(sessionId, overrides = {}) {
  return {
    session_id: sessionId,
    conversation_count: 7,
    first_seen_ns: 1_000_000_000,
    last_seen_ns: 9_000_000_000,
    total_input_tokens: 100,
    total_output_tokens: 50,
    model: 'gpt-5',
    agent_name: 'codex',
    first_user_query: null,
    last_user_query: null,
    ...overrides,
  };
}

function trajectory(sessionId, overrides = {}) {
  return {
    session_id: sessionId,
    schema_version: '1.7',
    agent_name: 'codex',
    model_name: null,
    num_steps: 12,
    total_prompt_tokens: null,
    total_completion_tokens: null,
    start_time: null,
    end_time: null,
    first_user_message: 'fix the bug',
    last_user_message: 'done',
    project: 'anolisa',
    source: 'codex',
    is_subagent: false,
    collected_at_ns: 8_000_000_000,
    ...overrides,
  };
}

function childOf(parentId, name) {
  return trajectory(`${parentId}:subagent:${name}`, {
    is_subagent: true,
    num_steps: 2,
    first_user_message: null,
    last_user_message: null,
    collected_at_ns: 8_500_000_000,
  });
}

test('an aliased Codex parent carries its child count into the merged row', () => {
  const mergeSessions = loadMergeSessions();
  const merged = mergeSessions(
    [ebpfSession(UUID)],
    [trajectory(ROLLOUT), childOf(ROLLOUT, 'child-1'), childOf(ROLLOUT, 'child-2')],
  );
  assert.equal(merged.length, 1, 'the aliased parent must merge into one row');
  const row = merged[0];
  assert.equal(row.session_id, UUID, 'the merged row keeps the eBPF identity');
  assert.deepEqual(row.sources, ['ebpf', 'log']);
  assert.equal(row.subagent_count, 2, 'the collected children must appear in the badge');
  // eBPF token/count ownership and the log-side previews stay unchanged.
  assert.equal(row.count, 7);
  assert.equal(row.input_tokens, 100);
  assert.equal(row.output_tokens, 50);
  assert.equal(row.first_message, 'fix the bug');
  assert.equal(row.last_message, 'done');
  assert.equal(row.project, 'anolisa');
});

test('an exact-ID parent keeps its child count without double-counting', () => {
  const mergeSessions = loadMergeSessions();
  const merged = mergeSessions(
    [ebpfSession('sess-1')],
    [trajectory('sess-1'), childOf('sess-1', 'a'), childOf('sess-1', 'b')],
  );
  assert.equal(merged.length, 1);
  const row = merged[0];
  assert.deepEqual(row.sources, ['ebpf', 'log']);
  assert.equal(row.subagent_count, 2, 'same-ID children must not be counted twice');
  assert.equal(row.count, 7, 'eBPF conversation count stays the owner of count');
});

test('source-order permutations of the same inputs yield the same merge', () => {
  const mergeSessions = loadMergeSessions();
  const ebpf = [ebpfSession(UUID)];
  const parent = trajectory(ROLLOUT);
  const children = [childOf(ROLLOUT, 'child-1'), childOf(ROLLOUT, 'child-2')];

  const byKey = (rows) => Object.fromEntries(rows.map((r) => [r.session_id, r]));
  const canonical = byKey(mergeSessions(ebpf, [parent, ...children]));
  // Children before their parent inside the log array - mergeSessions must
  // be insensitive to the order rows arrive in from either endpoint.
  const permuted = byKey(mergeSessions(ebpf, [...children.slice().reverse(), parent]));
  const permuted2 = byKey(mergeSessions(ebpf, [children[1], parent, children[0]]));

  for (const rows of [permuted, permuted2]) {
    assert.deepEqual(Object.keys(rows), Object.keys(canonical));
    assert.equal(rows[UUID].subagent_count, canonical[UUID].subagent_count);
    assert.equal(rows[UUID].subagent_count, 2);
    assert.deepEqual(rows[UUID].sources, canonical[UUID].sources);
  }
});

test('orphaned children without any parent stay visible as their own rows', () => {
  const mergeSessions = loadMergeSessions();
  const orphan = childOf('rollout-2026-10-04T11-00-00-ffffffff-1111-2222-3333-444444444444', 'lost');
  const merged = mergeSessions([], [orphan]);
  assert.equal(merged.length, 1, 'an orphaned subagent must keep its own row');
  assert.equal(merged[0].session_id, orphan.session_id);
  assert.deepEqual(merged[0].sources, ['log']);
  assert.equal(merged[0].subagent_count, 0);
});

test('a log-only parent keeps reporting its own child count', () => {
  const mergeSessions = loadMergeSessions();
  const merged = mergeSessions(
    [],
    [trajectory(ROLLOUT), childOf(ROLLOUT, 'only-child')],
  );
  assert.equal(merged.length, 1);
  assert.equal(merged[0].session_id, ROLLOUT);
  assert.equal(merged[0].subagent_count, 1);
  assert.equal(merged[0].count, 12, 'the log side owns num_steps without eBPF rows');
});
