const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const test = require('node:test');

// Pins the stale-load guard on every dashboard page whose loaders are
// re-issued automatically (filter change, tab switch, or poll): a newer
// request must be able to invalidate an older in-flight response, or the
// older response lands last and the page shows data that violates the
// active filters (the class fixed for the agent-health interruption
// section, the reuse-labels list, and the agent-protection snapshots).
//
// The dashboard has no component-test harness, so — like the navigation
// regression suite — these assertions read the page sources and check the
// guard's ordering inside each loader: the request id is taken before the
// fetch, re-checked after the await, and gates the state writes.

const readSource = (relativePath) => readFileSync(join(process.cwd(), relativePath), 'utf8');

function assertGuardOrdering(source, refName, markers, description) {
  const [before, check, after] = markers.map((marker) => source.indexOf(marker));
  assert.ok(before >= 0, `${description}: marker before the guard not found (${markers[0]})`);
  assert.ok(check >= 0, `${description}: guard check not found for ${refName}`);
  assert.ok(after >= 0, `${description}: marker after the guard not found (${markers[2]})`);
  assert.ok(
    before < check && check < after,
    `${description}: the ${refName} check must sit after the await and before the state write`,
  );
}

test('skill-metrics: the filter-driven loader drops stale responses', () => {
  const source = readSource('src/pages/SkillMetricsPage.tsx');
  assert.match(source, /const loadRequestIdRef = useRef\(0\);/);

  assertGuardOrdering(
    source,
    'loadRequestIdRef',
    [
      'const data = await fetchSkillMetrics',
      'if (requestId !== loadRequestIdRef.current) return;',
      'setReport(data);',
    ],
    'SkillMetricsPage.loadData',
  );
  // The error banner and the loading flag belong to the newest request too.
  const gated = source.match(/requestId === loadRequestIdRef\.current/g) ?? [];
  assert.ok(gated.length >= 2, 'SkillMetricsPage: catch and finally must both be gated');
});

test('agent-sessions: the range loader and its 10 s poll drop stale responses', () => {
  const source = readSource('src/pages/AgentSessionsPage.tsx');
  assert.match(source, /const loadRequestIdRef = useRef\(0\);/);

  assertGuardOrdering(
    source,
    'loadRequestIdRef',
    [
      'const [ebpf, logs] = await Promise.all',
      'if (requestId !== loadRequestIdRef.current) return;',
      'setMerged(mergeSessions(ebpf, logs));',
    ],
    'AgentSessionsPage.loadData',
  );
  const gated = source.match(/requestId === loadRequestIdRef\.current/g) ?? [];
  assert.ok(gated.length >= 2, 'AgentSessionsPage: catch and finally must both be gated');
});

test('security-observability: overview, events, and sessions loaders drop stale responses', () => {
  const source = readSource('src/pages/SecurityObservabilityPage.tsx');
  assert.match(source, /const overviewRequestIdRef = useRef\(0\);/);
  assert.match(source, /const eventsRequestIdRef = useRef\(0\);/);
  assert.match(source, /const sessionsRequestIdRef = useRef\(0\);/);

  assertGuardOrdering(
    source,
    'overviewRequestIdRef',
    [
      'const results = await Promise.allSettled',
      'if (requestId !== overviewRequestIdRef.current) return;',
      'collect(results[0]',
    ],
    'SecurityObservabilityPage.loadOverview',
  );
  assertGuardOrdering(
    source,
    'eventsRequestIdRef',
    [
      'const response = await fetchSecurityEvents',
      'if (requestId !== eventsRequestIdRef.current) return;',
      'setEvents(response);',
    ],
    'SecurityObservabilityPage.loadEvents',
  );
  assertGuardOrdering(
    source,
    'sessionsRequestIdRef',
    [
      'const response = await fetchSecuritySessions',
      'if (requestId !== sessionsRequestIdRef.current) return;',
      'setSecuritySessions(response);',
    ],
    'SecurityObservabilityPage.loadSessions',
  );
});
