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

  // The agent-list effect re-fires on every time-range change; its cleanup
  // must drop responses from superseded ranges.
  assert.match(source, /fetchAgentNames\(startNs, endNs\)[\s\S]{0,200}if \(!cancelled\) setAgents\(names\);/,
    'SkillMetricsPage: the agent-list write must be gated on the effect cleanup flag');
  assert.match(source, /return \(\) => \{\s*cancelled = true;\s*\};/,
    'SkillMetricsPage: the agent-list effect must cancel in-flight responses on cleanup');
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

  // `securitySessions` is written by both loadOverview's batch and
  // loadSessions, so the two writers must share the sessions token: the
  // overview batch takes the shared token before its fetches and gates the
  // sessions write (and the selectedSessionId follow-up) on it.
  const overviewTakesSharedToken = source.indexOf('const sessionsRequestId = ++sessionsRequestIdRef.current;');
  const overviewBatchFetch = source.indexOf('const results = await Promise.allSettled');
  const guardedSessionsWrite = source.indexOf('if (sessionsRequestId === sessionsRequestIdRef.current)');
  assert.ok(
    overviewTakesSharedToken >= 0 && overviewTakesSharedToken < overviewBatchFetch,
    'loadOverview must take the shared sessions token before issuing its batch',
  );
  assert.ok(
    guardedSessionsWrite > overviewBatchFetch,
    'loadOverview must gate its securitySessions write on the shared sessions token',
  );

  // Event detail races between rapid clicks; only the newest click may write.
  assert.match(source, /const eventDetailRequestIdRef = useRef\(0\);/);
  assertGuardOrdering(
    source,
    'eventDetailRequestIdRef',
    [
      'const response = await fetchSecurityEvent(eventId);',
      'if (requestId !== eventDetailRequestIdRef.current) return;',
      'setEventDetail(response);',
    ],
    'SecurityObservabilityPage.loadEventDetail',
  );
  const gatedDetail = source.match(/requestId === eventDetailRequestIdRef\.current/g) ?? [];
  assert.ok(gatedDetail.length >= 2, 'SecurityObservabilityPage: detail catch and finally must both be gated');
});

test('conversation-list: query and agent-name loaders drop stale responses', () => {
  const source = readSource('src/pages/ConversationList.tsx');
  assert.match(source, /const loadRequestIdRef = useRef\(0\);/);
  assertGuardOrdering(
    source,
    'loadRequestIdRef',
    [
      'const [sessData, tsData, intData, iStats, iSessionCounts, iConvCounts, savingsResp] = await Promise.all',
      'if (requestId !== loadRequestIdRef.current) return { ok: true, requestId };',
      'setSessions(sessData);',
    ],
    'ConversationList.runQuery',
  );

  assert.match(source, /const agentNamesRequestIdRef = useRef\(0\);/);
  assertGuardOrdering(
    source,
    'agentNamesRequestIdRef',
    [
      'const names = await fetchAgentNames',
      'if (requestId !== agentNamesRequestIdRef.current) return;',
      'setAgentNames(names);',
    ],
    'ConversationList.loadAgentNames',
  );

  // The trace sub-table re-fetches whenever the expanded session or the time
  // range changes. Its render branch keys on `error`, so a transient failure
  // used to pin the error panel even after a later fetch returned rows, and an
  // older response could land after a newer one.
  assert.match(
    source,
    /setError\(null\);[\s\S]{0,400}fetchTraces\(sessionId, startNs, endNs\)/,
    'ConversationList.TraceSubTable: the previous failure must be cleared before the next fetch',
  );
  assert.match(
    source,
    /fetchTraces\(sessionId, startNs, endNs\)[\s\S]{0,400}if \(!cancelled\) setTraces\(rows\);/,
    'ConversationList.TraceSubTable: the trace write must be gated on the effect cleanup flag',
  );
  assert.match(
    source,
    /fetchTraces\(sessionId, startNs, endNs\)[\s\S]{0,600}if \(!cancelled\) setLoading\(false\);/,
    'ConversationList.TraceSubTable: the loading flag must belong to the newest request',
  );

  // The agent filter matches the dropdown option against the row's label. The
  // option list comes from `/api/agent-names`, whose SQL groups by
  // `agent_name COLLATE NOCASE`, while the session rows render
  // `COALESCE(agent_name, process_name)` — so the same agent can appear as
  // `Qoder` in one and `qoder` in the other, exactly the split
  // AgentSessionsPage documents. An exact `===` comparison then emptied the
  // table for the only option the dropdown offered.
  assert.match(
    source,
    /data\.filter\(\(s\) => \(s\.agent_name \?\? ''\)\.toLowerCase\(\) === agent\.toLowerCase\(\)\)/,
    'ConversationList.runQuery: the agent filter must compare case-insensitively',
  );
  assert.ok(
    !/data\.filter\(\(s\) => s\.agent_name === agent\)/.test(source),
    'ConversationList.runQuery: the exact-match agent filter must be gone',
  );
});

test('atif-viewer: the document loader drops stale responses', () => {
  const source = readSource('src/pages/AtifViewerPage.tsx');
  assert.match(source, /const loadRequestIdRef = useRef\(0\);/);
  assertGuardOrdering(
    source,
    'loadRequestIdRef',
    [
      'data = await loadSessionDoc(i.trim(), t);',
      'if (requestId !== loadRequestIdRef.current) return;',
      'setDoc(data);',
    ],
    'AtifViewerPage.handleLoad',
  );
  // The savings fetch resolves after the load; its write must be gated too.
  assert.match(
    source,
    /if \(requestId === loadRequestIdRef\.current\) setSavingsDetail\(detail\);/,
    'AtifViewerPage: the savings write must be gated on the load request id',
  );

  // Local FileReader imports share the load identity: taking the id before
  // the read invalidates in-flight network loads, and both reader
  // completions are gated so an invalidated read cannot replace the
  // document, surface an obsolete error or finish another load's state.
  const importStart = source.indexOf('const handleFileImport = useCallback(');
  assert.ok(importStart >= 0, 'AtifViewerPage.handleFileImport must exist');
  const importBody = source.slice(importStart, source.indexOf('}, [t]);', importStart));
  const takesId = importBody.indexOf('const requestId = ++loadRequestIdRef.current;');
  const startsRead = importBody.indexOf('reader.readAsText(file);');
  assert.ok(
    takesId >= 0 && takesId < startsRead,
    'AtifViewerPage.handleFileImport: the import must take the load id before reading',
  );
  const gatedCompletions = importBody.match(/requestId !== loadRequestIdRef\.current\) return;/g) ?? [];
  assert.equal(gatedCompletions.length, 2,
    'AtifViewerPage.handleFileImport: onload and onerror must both be gated');
  assert.match(
    importBody,
    /onerror[\s\S]{0,200}setLoading\(false\);/,
    'AtifViewerPage.handleFileImport: a terminal read failure must release loading',
  );
});

test('optimization: a dimension result must not land in another session', () => {
  const source = readSource('src/pages/OptimizationPage.tsx');
  assert.match(source, /const activeSessionRef = useRef\(sessionId\);/);

  const run = source.indexOf('const runDimensions = useCallback(');
  assert.ok(run >= 0, 'runDimensions must exist');
  const body = source.slice(run, source.indexOf('[sessionId, handleDimError, t],', run));
  // Every dimension write and failure handler must consult the guard: the
  // requests run for tens of seconds while the route param can change.
  assert.match(
    body,
    /if \(isCurrent\(\)\) apply\(data\);/,
    'OptimizationPage: dimension results must be gated on the active session',
  );
  assert.match(
    body,
    /if \(!isCurrent\(\)\) return;\s*handleDimError\(e\);/,
    'OptimizationPage: dimension failures must be gated on the active session',
  );
  const guarded = body.match(/forSession<[A-Za-z]+>\(/g) ?? [];
  assert.equal(guarded.length, 6, 'all six dimensions must use the gated setter');
});

test('system-audit: load-more must not append a page from a superseded list', () => {
  const source = readSource('src/pages/SystemAuditPage.tsx');
  const loadMore = source.indexOf('const loadMoreEvents = async () => {');
  assert.ok(loadMore >= 0, 'the load-more handler must exist');
  const body = source.slice(loadMore, source.indexOf('\n  };', loadMore));

  const versionTaken = body.indexOf('const version = loadRequestVersion.current;');
  const fetchAt = body.indexOf('await fetchAuditEvents(');
  const check = body.indexOf('if (loadRequestVersion.current !== version) return;');
  const append = body.indexOf('setEvents((prev) => [...prev, ...result.data.items]);');

  assert.ok(
    versionTaken >= 0 && versionTaken < fetchAt,
    'SystemAuditPage.loadMoreEvents: the page must be bound to the current list version',
  );
  assert.ok(
    check > fetchAt && check < append,
    'SystemAuditPage.loadMoreEvents: the version must be re-checked between the await and the append',
  );
});
