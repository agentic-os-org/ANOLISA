const assert = require('node:assert/strict');
const { join } = require('node:path');
const test = require('node:test');

// Pins the optimizer's `opt:<target>` rows on the sessions page:
// mergeSessions applied its Codex trailing-UUID alias to *every* session
// id, so an optimization run rooted at `opt:<uuid>` merged into the
// target session's eBPF row when the target id was itself a UUID — the
// optimization row disappeared and its preview/project replaced the
// original session's values. The same overbroad alias also tallied the
// opt run's subagents under the bare target UUID and resolved orphan
// checks against the unrelated target row (#6584).
//
// Since the session merge model was extracted from the React page
// (utils/sessionModel.ts), these tests transpile that pure module with
// the dashboard's babel toolchain — same approach as the sibling
// subagent-count suite — and drive the exported mergeSessions against
// plain SessionSummary / TrajectorySummary fixtures.

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
  // sessionModel.ts is a pure module: its only import is a type-only
  // import from apiClient, which the typescript preset strips, so the
  // transpiled code performs no runtime requires.
  const module = { exports: {} };
  new Function('require', 'module', 'exports', transpile('src/utils/sessionModel.ts'))(
    (name) => {
      throw new Error(`unexpected require from src/utils/sessionModel.ts: ${name}`);
    },
    module,
    module.exports,
  );
  return module.exports.mergeSessions;
}

const mergeSessions = loadMergeSessions();
const merge = (ebpf, logs) => JSON.parse(JSON.stringify(mergeSessions(ebpf, logs)));

const captured = (id, extra = {}) => ({
  session_id: id, agent_name: 'qoder', model: 'target-model', conversation_count: 3,
  total_input_tokens: 200, total_output_tokens: 100, first_user_query: 'Original task',
  last_user_query: 'Original last message', last_seen_ns: 1_000_000, ...extra,
});
const logged = (id, extra = {}) => ({
  session_id: id, agent_name: 'agentsight-opt', model_name: 'judge-model',
  num_steps: 1, total_prompt_tokens: 40, total_completion_tokens: 20,
  collected_at_ns: 2_000_000, first_user_message: 'Optimization run',
  last_user_message: 'Latest dimension: accuracy', is_subagent: false, ...extra,
});

const UUID = '00000000-0000-0000-0000-000000000123';

test('an opt run over a UUID target stays its own row and keeps the target intact', () => {
  const result = merge([captured(UUID)], [logged(`opt:${UUID}`, { project: UUID })]);
  assert.equal(result.length, 2, JSON.stringify(result));
  const byId = new Map(result.map((row) => [row.session_id, row]));
  const target = byId.get(UUID);
  const run = byId.get(`opt:${UUID}`);
  assert.ok(target && run);
  assert.deepEqual(target.sources, ['ebpf']);
  assert.equal(target.agent_name, 'qoder');
  assert.equal(target.model, 'target-model');
  assert.equal(target.first_message, 'Original task');
  assert.equal(target.last_message, 'Original last message');
  assert.equal(target.project, null);
  assert.deepEqual(run.sources, ['log']);
  assert.equal(run.first_message, 'Optimization run');
  assert.equal(run.project, UUID);
});

test('an opt run subagent is tallied under the opt root, not the target', () => {
  const child = logged(`opt:${UUID}:subagent:child`, { is_subagent: true });
  const result = merge([captured(UUID)], [logged(`opt:${UUID}`), child]);
  assert.equal(result.length, 2, JSON.stringify(result));
  const byId = new Map(result.map((row) => [row.session_id, row]));
  assert.equal(byId.get(UUID).subagent_count, 0);
  assert.equal(byId.get(`opt:${UUID}`).subagent_count, 1);
});

test('an orphaned opt-run subagent survives next to the UUID-matching target', () => {
  // The opt root is absent from both sources; only the unrelated target
  // (whose id is the same UUID) is captured. The child must not be
  // resolved into that target and hidden.
  const child = logged(`opt:${UUID}:subagent:child`, { is_subagent: true });
  const result = merge([captured(UUID)], [child]);
  assert.equal(result.length, 2, JSON.stringify(result));
  const byId = new Map(result.map((row) => [row.session_id, row]));
  assert.equal(byId.get(UUID).subagent_count, 0);
  assert.ok(byId.get(`opt:${UUID}:subagent:child`));
});

test('genuine Codex rollout aliases still merge into the captured row', () => {
  const rollout = `rollout-2026-10-01T00-00-00-${UUID}`;
  const result = merge([captured(UUID)], [logged(rollout)]);
  assert.equal(result.length, 1);
  assert.equal(result[0].session_id, UUID);
  assert.deepEqual(result[0].sources, ['ebpf', 'log']);
});

test('non-UUID opt targets were never merged and stay unmerged', () => {
  const result = merge([captured('my-task')], [logged('opt:my-task')]);
  assert.equal(result.length, 2);
  const byId = new Map(result.map((row) => [row.session_id, row]));
  assert.equal(byId.get('my-task').first_message, 'Original task');
  assert.ok(byId.get('opt:my-task'));
});
