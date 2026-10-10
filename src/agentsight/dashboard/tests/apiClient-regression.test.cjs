const assert = require('node:assert/strict');
const test = require('node:test');

const {
  containmentTargetCandidates,
  defaultContainmentTargetPid,
  enforcementSupportsContainment,
  enforcementSupportsMode,
  enforcementViolationTotal,
  fetchAgentProtectionPreview,
  fetchContainmentPlan,
  fetchInterruptionStats,
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
const {
  fillModelBuckets,
  fillTokenBuckets,
} = require(process.env.AGENTSIGHT_TIMESERIES_BUCKETS_BUILD);

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

test('fetchAgentProtectionPreview rejects a body that is not a preview', async () => {
  // The macOS local viewer has no enforcement routes, so its `/api/*` catch-all
  // answers this GET with `200 []`. The caller stores `preview.source_paths`
  // (undefined) as its source list and the protection dialog then throws while
  // rendering it, which React turns into a blank page rather than an error.
  global.fetch = async () => new Response(JSON.stringify([]), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });

  await assert.rejects(
    () => fetchAgentProtectionPreview(4242),
    /protection preview is unavailable/,
  );
});

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

test('fetchInterruptionStats forwards the agent filter alongside the range', async () => {
  const requested = [];
  global.fetch = async (url) => {
    requested.push(String(url));
    return new Response(JSON.stringify([]), { status: 200 });
  };

  await fetchInterruptionStats(1000, 2000, 'claude-code');
  await fetchInterruptionStats(1000, 2000);

  const withAgent = new URL(requested[0]);
  assert.equal(withAgent.pathname, '/api/interruptions/stats');
  assert.equal(withAgent.searchParams.get('start_ns'), '1000');
  assert.equal(withAgent.searchParams.get('end_ns'), '2000');
  assert.equal(
    withAgent.searchParams.get('agent_name'),
    'claude-code',
    'the tooltip breakdown must use the same agent scope as the badge',
  );

  const withoutAgent = new URL(requested[1]);
  assert.equal(
    withoutAgent.searchParams.get('agent_name'),
    null,
    'an omitted agent must not send agent_name',
  );
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

// ─── Timeseries bucket gap-filling ─────────────────────────────────────────────

/**
 * Rebuilds the server's exact i64 bucket grid for a query window.
 *
 * The Rust side (get_token_timeseries) derives `bucket_start_ns` as
 * start_ns + idx * floor((end_ns - start_ns) / buckets) in exact 64-bit
 * integers. The dashboard, however, works in JS doubles where epoch-ns
 * values are rounded to a multiple of 256 ns, so the fill helpers must
 * tolerate that wobble when mapping a bucket back onto its index.
 */
function serverBucketGrid(startMs, endMs, bucketCount) {
  const startNs = startMs * 1_000_000; // exactly what the page sends
  const endNs = endMs * 1_000_000;
  const bucketNs = (BigInt(endNs) - BigInt(startNs)) / BigInt(bucketCount);
  const grid = [];
  for (let idx = 0; idx < bucketCount; idx += 1) {
    grid.push(Number(BigInt(startNs) + BigInt(idx) * bucketNs));
  }
  return { startNs, endNs, grid };
}

// A minute-aligned start (DateTimePicker) with an arbitrary-millisecond end
// (Date.now()) is the shape that misassigns under floor(): measured on the
// pre-fix code, roughly 283 of 300 such windows shifted buckets onto their
// left neighbour. This exact pair shifts 15 of 30 buckets.
test('fillTokenBuckets keeps every bucket on its own slot under ns rounding', () => {
  const { startNs, endNs, grid } = serverBucketGrid(1760063940000, 1760113451189, 30);
  // Marker idx + 1 so slot 0 asserts real data rather than a zero-fill.
  const data = grid.map((bucketStartNs, idx) => ({
    bucket_start_ns: bucketStartNs,
    input_tokens: idx + 1,
    output_tokens: 0,
    total_tokens: idx + 1,
  }));

  const filled = fillTokenBuckets(data, startNs, endNs, 30);

  assert.equal(filled.length, 30);
  for (let i = 0; i < 30; i += 1) {
    // The marker of bucket i must still be at slot i — not merged into i-1.
    assert.equal(filled[i].input_tokens, i + 1, `bucket ${i} misplaced`);
  }
});

test('fillModelBuckets keeps every bucket on its own slot under ns rounding', () => {
  const { startNs, endNs, grid } = serverBucketGrid(1760054640000, 1760165106879, 30);
  const data = grid.map((bucketStartNs, idx) => ({
    bucket_start_ns: bucketStartNs,
    model: idx % 2 === 0 ? 'a' : 'b',
    total_tokens: idx + 1,
  }));

  const filled = fillModelBuckets(data, startNs, endNs, 30, ['a', 'b']);

  assert.equal(filled.length, 60); // 30 slots x 2 models
  for (let i = 0; i < 30; i += 1) {
    // Slot i holds models 'a' and 'b' at 2i / 2i+1; whichever model bucket i
    // carried must keep its marker inside slot i.
    const marker = Math.max(filled[2 * i].total_tokens, filled[2 * i + 1].total_tokens);
    assert.equal(marker, i + 1, `bucket ${i} misplaced`);
  }
});

test('fillTokenBuckets zero-fills missing buckets on the server grid', () => {
  // Exact small numbers: no ns rounding involved, pure gap-filling.
  const data = [
    { bucket_start_ns: 0, input_tokens: 10, output_tokens: 4, total_tokens: 14 },
    { bucket_start_ns: 200, input_tokens: 7, output_tokens: 3, total_tokens: 10 },
  ];

  const filled = fillTokenBuckets(data, 0, 300, 30);

  assert.equal(filled.length, 30);
  assert.deepEqual(filled[0], data[0]);
  assert.deepEqual(filled[20], data[1]);
  assert.deepEqual(filled[1], {
    bucket_start_ns: 10,
    input_tokens: 0,
    output_tokens: 0,
    total_tokens: 0,
  });
});

test('fillTokenBuckets passes data through for a degenerate window', () => {
  const data = [
    { bucket_start_ns: 5, input_tokens: 1, output_tokens: 1, total_tokens: 2 },
  ];
  // Zero-width range: bucketNs floors to 0 and the data is returned as-is.
  assert.equal(fillTokenBuckets(data, 5, 5, 30), data);
});

test('formatDurationSecs never renders 60 seconds inside a minute field', () => {
  const { formatDurationSecs } = require(process.env.AGENTSIGHT_FORMAT_DURATION_BUILD);

  assert.equal(formatDurationSecs(12.34), '12.3s');
  assert.equal(formatDurationSecs(59.4), '59.4s');
  // Rounding happened after the minute split before, yielding "60.0s",
  // "1m 60s" and "59m 60s".
  assert.equal(formatDurationSecs(59.96), '1m 0s');
  assert.equal(formatDurationSecs(119.6), '2m 0s');
  assert.equal(formatDurationSecs(3599.7), '60m 0s');
  assert.equal(formatDurationSecs(125), '2m 5s');
});

// ─── richText: finding texts render only their two documented tags ──────────

// The built component emits `require('react/jsx-runtime')` (compiled with
// --jsx react-jsx, same as LoginPage), which does not resolve from the temp
// outDir — stub it like the login regression does and run the built file in a
// sandbox.
function loadRichTextBuild() {
  const { readFileSync } = require('node:fs');
  const vm = require('node:vm');
  const code = readFileSync(process.env.AGENTSIGHT_RICH_TEXT_BUILD, 'utf8');
  const module = { exports: {} };
  vm.runInNewContext(code, {
    module,
    exports: module.exports,
    require: (name) => {
      if (name === 'react/jsx-runtime') {
        return {
          jsx: (type, props) => ({ type, props }),
          jsxs: (type, props) => ({ type, props }),
        };
      }
      throw new Error(`unexpected require from richText.js: ${name}`);
    },
  });
  return module.exports;
}

test('escapeRichText neutralizes markup injected through finding texts', () => {
  const { escapeRichText } = loadRichTextBuild();

  // Payload the server can really produce: confirm_before_act interpolates
  // the raw tool command into the accuracy `detail` string.
  const toolCmdPayload = 'tool `bash` ran `rm -rf <img src=x onerror=alert(1)>`';
  const escaped = escapeRichText(toolCmdPayload);
  assert.ok(!escaped.includes('<img'), 'an injected tag must not survive as markup');
  assert.ok(escaped.includes('&lt;img src=x onerror=alert(1)&gt;'),
    'the injected tag must render as literal text');

  assert.ok(!escapeRichText('<script>alert(1)</script>').includes('<script'));
  assert.ok(!escapeRichText('<svg onload=alert(1)>').includes('<svg'));
  // Quotes and ampersands must not smuggle attributes into the allowed tags.
  assert.equal(escapeRichText('a & b "c"'), 'a &amp; b &quot;c&quot;');
});

test('escapeRichText keeps only the exact documented tags', () => {
  const { escapeRichText } = loadRichTextBuild();

  assert.equal(escapeRichText('a <code>cmd</code> b'), 'a <code>cmd</code> b');
  assert.equal(escapeRichText('<b>bold</b> stays'), '<b>bold</b> stays');
  // A tag that merely starts like an allowed one must stay escaped, and no
  // attributes may ride along on the allowed tags.
  assert.equal(escapeRichText('<codeX>'), '&lt;codeX&gt;');
  assert.ok(!escapeRichText('<code onclick=alert(1)>x</code>').includes('<code '));
  assert.equal(escapeRichText('<i>no</i>'), '&lt;i&gt;no&lt;/i&gt;');
});

test('RichText sanitizes what it injects into the DOM', () => {
  const { RichText } = loadRichTextBuild();

  const element = RichText({ children: 'x <img src=x onerror=alert(1)> <code>ok</code>' });
  const html = element.props.dangerouslySetInnerHTML.__html;
  assert.ok(!html.includes('<img'), 'the component must not inject raw tags');
  assert.ok(html.includes('<code>ok</code>'), 'the documented tag survives');
});

test('optimization findings render through the sanitizer, not raw strings', () => {
  const { readFileSync } = require('node:fs');
  const { join } = require('node:path');
  const source = readFileSync(join(process.cwd(), 'src/pages/OptimizationPage.tsx'), 'utf8');
  assert.match(source, /import \{ RichText \} from '\.\.\/utils\/richText';/,
    'the page must render finding texts through utils/richText');
  assert.doesNotMatch(source, /dangerouslySetInnerHTML=\{\{ __html: s \}\}/,
    'the page must not inject the raw finding string into the DOM');
});

const { sameMembers } = require(process.env.AGENTSIGHT_SET_SELECTION_BUILD);

test('selection shortcuts compare members, not sizes', () => {
  // Picking "select unknown" with three hand-ticked rows of other sessions
  // used to clear the selection: the sizes matched, the members did not.
  const ticked = new Set(['a', 'b', 'c']);
  assert.equal(sameMembers(ticked, new Set(['a', 'b', 'c'])), true);
  assert.equal(sameMembers(ticked, new Set(['a', 'b', 'd'])), false);
  assert.equal(sameMembers(ticked, new Set(['a', 'b'])), false);
  assert.equal(sameMembers(new Set(), new Set()), true);
  assert.equal(sameMembers(ticked, new Set()), false);
});
