const assert = require('node:assert/strict');
const test = require('node:test');

const {
  containmentTargetCandidates,
  defaultContainmentTargetPid,
  enforcementSupportsContainment,
  enforcementSupportsMode,
  enforcementViolationTotal,
  fetchContainmentPlan,
  fetchLatencyMetrics,
  fetchSecurityCase,
  fetchSecurityStatus,
  fetchStorageStatus,
  reviewSecurityCase,
  semanticSearchSessions,
} = require(process.env.AGENTSIGHT_API_CLIENT_BUILD);
const {
  applySemanticRanking,
} = require(process.env.AGENTSIGHT_SEMANTIC_FILTER_BUILD);
const {
  containmentLifecyclePresentation,
} = require(process.env.AGENTSIGHT_CONTAINMENT_LIFECYCLE_BUILD);
const {
  formatNs,
  formatNsPadded,
  formatMsCompact,
  formatNsCompact,
} = require(process.env.AGENTSIGHT_DATETIME_BUILD);
const {
  fmtTime,
  securityDetailRows,
} = require(process.env.AGENTSIGHT_SECURITY_UTILS_BUILD);
const {
  SAME_PLACE,
  fixLocusDiverges,
  fixLocusLabel,
} = require(process.env.AGENTSIGHT_ACCURACY_ATTRIBUTION_BUILD);

function enforcementHealth(alternatePidRetarget) {
  return {
    ready: true,
    backend: 'mock',
    capabilities: {
      credential_observe: true,
      credential_audit: true,
      credential_enforce: true,
      policy_handoff: true,
      alternate_pid_retarget: alternatePidRetarget,
      test_development: true,
    },
    message: null,
  };
}

function containmentPlan() {
  return {
    case_id: 'case-1',
    source_path: '/root/secret',
    original_target: {
      agent_id: 'agent-1',
      root_pid: 42,
      process_start_time: 101,
      display_name: 'stale agent',
    },
    original_target_valid: false,
    candidates: [{
      agent_id: 'agent-1',
      root_pid: 77,
      process_start_time: 202,
      display_name: 'live agent',
    }],
    default_duration_secs: 900,
    min_duration_secs: 60,
    max_duration_secs: 3600,
    existing_action: null,
  };
}

function securityCaseDetail() {
  return {
    case_id: 'case-1',
    policy_id: 'credential-exfiltration',
    policy_revision: 1,
    agent_id: 'agent-1',
    severity: 'high',
    risk_score: 99,
    status: 'open',
    blocked: false,
    opened_at_ns: 1,
    updated_at_ns: 1,
    summary: 'fixture',
    evidence: [],
    containment: null,
  };
}

test('fetchSecurityCase rejects a non-2xx state envelope before returning it', async () => {
  global.fetch = async () => new Response(JSON.stringify({
    state: 'missing',
    data: { case_id: 'case-1' },
  }), { status: 404, statusText: 'Not Found' });

  await assert.rejects(
    () => fetchSecurityCase('case-1'),
    (error) => error?.status === 404 && error?.code === 'security_api_error',
  );
});

test('fetchSecurityStatus preserves a non-2xx availability state envelope', async () => {
  global.fetch = async () => new Response(JSON.stringify({
    state: 'daemon_unreachable',
    data: { error: 'socket unavailable' },
    meta: { source: 'agentsight' },
  }), { status: 503, statusText: 'Service Unavailable' });

  const response = await fetchSecurityStatus();

  assert.equal(response.state, 'daemon_unreachable');
  assert.deepEqual(response.data, { error: 'socket unavailable' });
});

test('fetchStorageStatus preserves schema 2 maintenance and partial inventory fields', async () => {
  let requestedUrl = null;
  const policy = {
    retention_days: 30,
    size_limit_bytes: 500,
    cleanup_trigger_bytes: 500,
    cleanup_target_bytes: 400,
    check_interval: 60,
    check_interval_unit: 'seconds',
    enforced_by: 'serve',
  };
  const maintenance = {
    scheduled: true,
    worker_running: true,
    worker_heartbeat_unix_ms: 1_700_000_030_000,
    last_attempt_unix_ms: 1_700_000_000_000,
    last_success_unix_ms: 1_699_999_000_000,
    last_result: 'lock_busy',
    consecutive_failures: 2,
    next_run_unix_ms: 1_700_000_060_000,
  };
  const payload = {
    schema_version: 2,
    observed_at_unix_ms: 42,
    stores: [
      {
        id: 'reuse',
        availability: 'present',
        size: {
          database_bytes: 100,
          wal_bytes: 20,
          shm_bytes: 10,
          freelist_bytes: 30,
          physical_bytes: 130,
          logical_bytes: 100,
        },
        policy,
        coverage: 'partial',
        size_state: 'within_policy',
        maintenance,
      },
      {
        id: 'causal',
        availability: 'missing',
        size: null,
        policy,
        coverage: 'partial',
        size_state: 'unknown',
        maintenance: {
          ...maintenance,
          worker_running: false,
          worker_heartbeat_unix_ms: null,
          last_attempt_unix_ms: null,
          last_success_unix_ms: null,
          last_result: null,
          next_run_unix_ms: null,
        },
      },
      {
        id: 'enforcement',
        availability: 'missing',
        size: null,
        policy,
        coverage: 'partial',
        size_state: 'unknown',
        maintenance: {
          ...maintenance,
          scheduled: false,
          worker_running: false,
          worker_heartbeat_unix_ms: null,
        },
      },
    ],
  };
  global.fetch = async (url, init) => {
    requestedUrl = String(url);
    assert.equal(init.credentials, 'same-origin');
    return new Response(JSON.stringify(payload), { status: 200 });
  };

  const response = await fetchStorageStatus();

  assert.equal(new URL(requestedUrl).pathname, '/api/storage/status');
  assert.equal(response.schema_version, 2);
  assert.deepEqual(response.stores.map((store) => store.id), ['reuse', 'causal', 'enforcement']);
  assert.equal(response.stores[0].coverage, 'partial');
  assert.equal(response.stores[1].coverage, 'partial');
  assert.equal(response.stores[2].coverage, 'partial');
  assert.deepEqual(response.stores[0].maintenance, maintenance);
  assert.equal(response.stores[1].maintenance.next_run_unix_ms, null);
});

test('fetchLatencyMetrics forwards ranges and preserves nullable percentile data', async () => {
  let requestedUrl = null;
  global.fetch = async (url) => {
    requestedUrl = String(url);
    return new Response(JSON.stringify([{
      agent_name: 'claude',
      call_count: 3,
      streaming_call_count: 2,
      ttft_ms: { p50: 10, p95: 20, p99: 30 },
      tps_tokens_per_second: { p50: 40, p95: 50, p99: 60 },
      tpot_ms_per_token: null,
      e2e_latency_ms: { p50: 100, p95: 200, p99: 300 },
    }]), { status: 200 });
  };

  const response = await fetchLatencyMetrics(1_000_000_000, 2_000_000_000, 'claude');
  const url = new URL(requestedUrl);

  assert.equal(url.pathname, '/api/metrics/latency');
  assert.equal(url.searchParams.get('start_ns'), '1000000000');
  assert.equal(url.searchParams.get('end_ns'), '2000000000');
  assert.equal(url.searchParams.get('agent_name'), 'claude');
  assert.deepEqual(response[0].ttft_ms, { p50: 10, p95: 20, p99: 30 });
  assert.deepEqual(response[0].tps_tokens_per_second, { p50: 40, p95: 50, p99: 60 });
  assert.deepEqual(response[0].e2e_latency_ms, { p50: 100, p95: 200, p99: 300 });
  assert.equal(response[0].tpot_ms_per_token, null);
});

test('fetchLatencyMetrics omits agent_name when no filter is provided', async () => {
  let requestedUrl = null;
  global.fetch = async (url) => {
    requestedUrl = String(url);
    return new Response('[]', { status: 200 });
  };

  await fetchLatencyMetrics(3_000_000_000, 4_000_000_000);
  const url = new URL(requestedUrl);

  assert.equal(url.searchParams.get('start_ns'), '3000000000');
  assert.equal(url.searchParams.get('end_ns'), '4000000000');
  assert.equal(url.searchParams.has('agent_name'), false);
});

test('semanticSearchSessions posts the query and candidates and returns ranked results', async () => {
  let capturedUrl = '';
  let capturedBody = null;
  global.fetch = async (url, init) => {
    capturedUrl = String(url);
    capturedBody = JSON.parse(init.body);
    return new Response(JSON.stringify({
      results: [
        { session_id: 'sess-1', relevance: 'high', reason: 'mentions OOM' },
        { session_id: 'sess-2', relevance: 'medium', reason: 'performance tuning' },
      ],
    }), { status: 200, headers: { 'Content-Type': 'application/json' } });
  };

  const response = await semanticSearchSessions({
    query: 'OOM',
    candidates: [
      { session_id: 'sess-1', first_message: 'help with memory', last_message: null, project: 'web' },
    ],
  });

  assert.ok(capturedUrl.endsWith('/api/sessions/search'), capturedUrl);
  assert.equal(capturedBody.query, 'OOM');
  assert.equal(capturedBody.candidates.length, 1);
  assert.equal(response.results[0].relevance, 'high');
  assert.equal(response.results[0].session_id, 'sess-1');
});

test('semanticSearchSessions rejects a non-2xx response so the caller can degrade silently', async () => {
  global.fetch = async () => new Response('service unavailable', { status: 503 });
  await assert.rejects(
    () => semanticSearchSessions({
      query: 'performance',
      candidates: [{ session_id: 'sess-1', first_message: 'slow query', last_message: null, project: null }],
    }),
    /\/api\/sessions\/search -> 503/,
  );
});

test('applySemanticRanking keeps the full list while the LLM is loading', () => {
  // Regression for the review finding: during the LLM round-trip the table
  // must not flash "no matching sessions" when ranked results are empty.
  const base = Array.from({ length: 6 }, (_, i) => ({ session_id: `s-${i}` }));
  const result = applySemanticRanking(base, 'OOM', {}, true);
  assert.equal(result.length, 6);
});

test('applySemanticRanking ranks high relevance first and ignores unknown ids', () => {
  const base = [{ session_id: 'a' }, { session_id: 'b' }, { session_id: 'c' }];
  const matches = {
    b: { relevance: 'high', reason: 'x' },
    a: { relevance: 'medium', reason: 'y' },
    z: { relevance: 'high', reason: 'not in base' },
  };
  const result = applySemanticRanking(base, 'query', matches, false);
  assert.deepEqual(result.map((s) => s.session_id), ['b', 'a']);
});

test('applySemanticRanking shows all candidates when the list is tiny', () => {
  const base = [{ session_id: 'a' }];
  assert.equal(applySemanticRanking(base, 'query', {}, false).length, 1);
});

test('applySemanticRanking returns the base list for an empty search', () => {
  const base = [{ session_id: 'a' }];
  assert.equal(applySemanticRanking(base, '   ', {}, false), base);
});

test('fetchSecurityCase accepts a valid system-audit detail response', async () => {
  global.fetch = async () => new Response(JSON.stringify({
    state: 'ok',
    data: securityCaseDetail(),
  }), { status: 200 });

  const response = await fetchSecurityCase('case-1');

  assert.equal(response.data.case_id, 'case-1');
  assert.deepEqual(response.data.evidence, []);
});

test('fetchContainmentPlan rejects a non-2xx state envelope', async () => {
  global.fetch = async () => new Response(JSON.stringify({
    state: 'missing',
    data: { case_id: 'case-1' },
  }), { status: 404, statusText: 'Not Found' });

  await assert.rejects(
    () => fetchContainmentPlan('case-1'),
    (error) => error?.status === 404 && error?.code === 'security_api_error',
  );
});
test('fetchSecurityCase rejects a successful response whose detail shape is malformed', async () => {
  global.fetch = async () => new Response(JSON.stringify({
    state: 'ok',
    data: { ...securityCaseDetail(), evidence: 'not-an-array' },
  }), { status: 200 });

  await assert.rejects(
    () => fetchSecurityCase('case-1'),
    (error) => error?.status === 200 && error?.code === 'malformed_security_case',
  );
});

test('reviewSecurityCase redirects unauthenticated requests to login', async () => {
  global.window = { location: { hash: '#/audit' } };
  global.fetch = async () => new Response('', {
    status: 401,
    statusText: 'Unauthorized',
  });

  await assert.rejects(
    () => reviewSecurityCase('case-1', 'confirmed'),
    /Authentication required/,
  );
  assert.equal(global.window.location.hash, '#/login');
});

test('enforcement capabilities fail closed while the backend is not ready', () => {
  const health = {
    ready: false,
    backend: 'mock',
    capabilities: {
      credential_observe: true,
      credential_audit: true,
      credential_enforce: true,
      policy_handoff: true,
      alternate_pid_retarget: true,
      test_development: true,
    },
    message: 'private socket /run/agentsight/enforcer.sock is unavailable',
  };

  assert.equal(enforcementSupportsMode(health, 'enforce'), false);
  assert.equal(enforcementSupportsContainment(health), false);
});

test('audit-only backends report all observed violations instead of blocked-only rows', () => {
  const health = {
    ready: true,
    backend: 'actplane',
    capabilities: {
      credential_observe: true,
      credential_audit: true,
      credential_enforce: false,
      policy_handoff: false,
      alternate_pid_retarget: false,
      test_development: false,
    },
    message: null,
  };
  const violations = [{ blocked: false }, { blocked: false }, { blocked: true }];

  assert.equal(enforcementViolationTotal(violations, health), 3);
});

test('alternate PID candidates require an explicit backend capability', () => {
  const plan = containmentPlan();

  assert.deepEqual(containmentTargetCandidates(plan, enforcementHealth(false)), []);
  assert.equal(defaultContainmentTargetPid(plan, enforcementHealth(false)), null);
  assert.deepEqual(
    containmentTargetCandidates(plan, enforcementHealth(true)).map((target) => target.root_pid),
    [77],
  );
  assert.equal(defaultContainmentTargetPid(plan, enforcementHealth(true)), 77);

  plan.original_target_valid = true;
  assert.deepEqual(
    containmentTargetCandidates(plan, enforcementHealth(false)).map((target) => target.root_pid),
    [42],
  );
  assert.equal(defaultContainmentTargetPid(plan, enforcementHealth(false)), 42);

  plan.original_target_valid = false;
  plan.candidates.push({
    agent_id: 'agent-1',
    root_pid: 88,
    process_start_time: 303,
    display_name: 'second live agent',
  });
  assert.equal(defaultContainmentTargetPid(plan, enforcementHealth(true)), null);
});

test('terminal containment lifecycle overrides historical blocked time', () => {
  const presentation = containmentLifecyclePresentation({
    lifecycle_state: 'expired',
    blocked_at_ns: 10,
  });

  assert.equal(presentation.labelKey, 'cont.lifecycle.expired.label');
});

// 2026-08-17T08:00:00Z expressed in nanoseconds.
const SAMPLE_NS = 1_786_608_000_000_000_000;

test('datetime helpers honor the requested locale', () => {
  // The exact rendering depends on the host timezone, so assert on
  // locale-sensitive differences rather than a fixed string.
  assert.notEqual(formatNs(SAMPLE_NS, 'en-US'), formatNs(SAMPLE_NS, 'zh-CN'));
  assert.match(formatNsPadded(SAMPLE_NS, 'zh-CN'), /\d{4}\/\d{2}\/\d{2} \d{2}:\d{2}:\d{2}/);
  assert.equal(
    formatNsCompact(SAMPLE_NS, 'zh-CN'),
    formatMsCompact(SAMPLE_NS / 1_000_000, 'zh-CN'),
  );
});

test('formatNsCompact renders a dash for missing timestamps', () => {
  assert.equal(formatNsCompact(null, 'en-US'), '—');
  assert.equal(formatNsCompact(0, 'zh-CN'), '—');
});

test('fmtTime formats via the caller-provided locale', () => {
  const event = { timestamp_ns: SAMPLE_NS };
  assert.equal(fmtTime(event, 'zh-CN'), formatMsCompact(SAMPLE_NS / 1_000_000, 'zh-CN'));
  assert.equal(fmtTime(event, 'en-US'), formatMsCompact(SAMPLE_NS / 1_000_000, 'en-US'));
  assert.equal(fmtTime({}, 'en-US'), '-');
});

test('securityDetailRows emits stable ids and message keys', () => {
  const rows = securityDetailRows({
    verdict: 'deny',
    reason: 'policy matched',
    nested: { error_message: 'boom' },
  });

  assert.deepEqual(
    rows.map((row) => ({ id: row.id, labelKey: row.labelKey })),
    [
      { id: 'verdict', labelKey: 'sec.detail.verdict' },
      { id: 'error', labelKey: 'sec.detail.error' },
      { id: 'reason', labelKey: 'sec.detail.reason' },
    ],
  );
  assert.equal(rows.find((row) => row.id === 'verdict').value, 'deny');
  assert.equal(rows.find((row) => row.id === 'error').value, 'boom');
});

// The API serializes `FixLocus::None` as the Chinese sentinel '无'. Translating
// that literal in SAME_PLACE would make every Env/Input issue look divergent.
test('fix locus comparison uses protocol values, not translated labels', () => {
  assert.equal(SAME_PLACE.Env, '无');
  assert.equal(SAME_PLACE.Input, '无');

  assert.equal(fixLocusDiverges('Env', '无'), false);
  assert.equal(fixLocusDiverges('Input', '无'), false);
  assert.equal(fixLocusDiverges('Env', 'Skill'), true);
  assert.equal(fixLocusDiverges('Skill', 'Skill'), false);
  // Orchestration has no in-place fix, so any locus counts as divergent.
  assert.equal(fixLocusDiverges('Orchestration', 'Skill'), true);
});

test('fixLocusLabel translates only the sentinel value', () => {
  const t = (key) => (key === 'opt.accuracy.fixLocusNone' ? 'None' : `??${key}`);
  assert.equal(fixLocusLabel('无', t), 'None');
  assert.equal(fixLocusLabel('Skill', t), 'Skill');
  assert.equal(fixLocusLabel('Context-policy', t), 'Context-policy');
});
