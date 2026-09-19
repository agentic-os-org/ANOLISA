const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const test = require('node:test');

const {
  ALL_CAPABILITIES,
  HEALTH_PATH,
  LOCAL_DEFAULT_PATH,
  NAV_ROUTES,
  OBSERVABILITY_PATH,
  defaultPath,
  pathAllowed,
} = require(process.env.AGENTSIGHT_NAVIGATION_BUILD);

const readSource = (relativePath) => readFileSync(join(process.cwd(), relativePath), 'utf8');

const EVERY_CAPABILITY = [...ALL_CAPABILITIES];
const without = (...dropped) => EVERY_CAPABILITY.filter((capability) => !dropped.includes(capability));

// ── Landing page (#2723) ─────────────────────────────────────────────────────

test('a fresh visit lands on the Agent dashboard', () => {
  assert.equal(HEALTH_PATH, '/health');
  assert.equal(defaultPath(EVERY_CAPABILITY), HEALTH_PATH);
  assert.equal(defaultPath(['agent_health', 'agent_observability']), HEALTH_PATH);
});

test('the landing page falls back to Agent observability, then to the nav order', () => {
  assert.equal(OBSERVABILITY_PATH, '/observability');
  assert.equal(defaultPath(without('agent_health')), OBSERVABILITY_PATH);
  assert.equal(defaultPath(['agent_observability', 'settings']), OBSERVABILITY_PATH);
  // No health and no observability: the first advertised page in nav order.
  assert.equal(defaultPath(without('agent_health', 'agent_observability')), '/sessions');
  assert.equal(defaultPath(['atif', 'skills']), '/skills');
  assert.equal(defaultPath(['settings']), '/settings');
});

test('a capability set advertising no page still gets a landing path', () => {
  assert.equal(defaultPath([]), LOCAL_DEFAULT_PATH);
});

test('the landing path is always one the capability guard accepts', () => {
  // A redirect target the guard rejects would bounce between the two forever.
  const advertised = [
    EVERY_CAPABILITY,
    without('agent_health'),
    without('agent_health', 'agent_observability'),
    ['agent_observability'],
    ['sessions'],
    ['enforcement'],
    ['settings'],
  ];
  for (const capabilities of advertised) {
    const target = defaultPath(capabilities);
    assert.ok(
      pathAllowed(target, capabilities),
      `landing path ${target} is not allowed for ${JSON.stringify(capabilities)}`,
    );
  }
});

// ── Capability guard ─────────────────────────────────────────────────────────

test('the bare root renders no page of its own', () => {
  // It only redirects, so it is reachable whatever the capabilities are; the
  // redirect target is capability-checked in its own right.
  assert.equal(pathAllowed('/', EVERY_CAPABILITY), true);
  assert.equal(pathAllowed('/', []), true);
  assert.ok(NAV_ROUTES.every((route) => route.path !== '/'));
});

test('every nav entry keeps its capability gate, sub-paths included', () => {
  for (const route of NAV_ROUTES) {
    assert.equal(pathAllowed(route.path, EVERY_CAPABILITY), true, route.path);
    assert.equal(pathAllowed(route.path, without(route.capability)), false, route.path);
    assert.equal(pathAllowed(`${route.path}/detail`, EVERY_CAPABILITY), true, route.path);
    assert.equal(pathAllowed(`${route.path}/detail`, without(route.capability)), false, route.path);
  }
});

test('a route does not claim a sibling path that shares its prefix', () => {
  // `startsWith('/audit')` used to gate '/auditorium' too. No route owns it,
  // so it falls through to the router's catch-all redirect either way.
  assert.equal(pathAllowed('/auditorium', without('system_audit')), true);
  assert.equal(pathAllowed('/settingsx', without('settings')), true);
});

test('a path no nav entry owns is left to the router catch-all', () => {
  assert.equal(pathAllowed('/nope', EVERY_CAPABILITY), true);
  assert.equal(pathAllowed('/login', EVERY_CAPABILITY), true);
});

test('the nav model covers every capability the dashboard advertises', () => {
  assert.equal(NAV_ROUTES.length, EVERY_CAPABILITY.length);
  assert.equal(new Set(NAV_ROUTES.map((route) => route.capability)).size, EVERY_CAPABILITY.length);
  assert.equal(new Set(NAV_ROUTES.map((route) => route.path)).size, NAV_ROUTES.length);
});

// ── Wiring (the pure functions above cannot see the router) ──────────────────

test('App.tsx redirects the bare root and serves observability at its own path', () => {
  const source = readSource('src/App.tsx');

  assert.match(source, /<Route path="\/" element=\{<Navigate to=\{fallbackPath\} replace \/>\} \/>/);
  assert.match(source, /<Route path=\{OBSERVABILITY_PATH\} element=\{<ConversationList \/>\} \/>/);
  assert.match(source, /<Route path=\{HEALTH_PATH\} element=\{<AgentHealthPage \/>\} \/>/);
  // The root must no longer render a page conditionally on a capability.
  assert.doesNotMatch(source, /capabilities\.includes\('agent_observability'\)/);
});

test('the offline capability default is derived from the nav model', () => {
  const source = readSource('src/App.tsx');

  assert.match(source, /const DEFAULT_CAPABILITIES: AppCapability\[\] = \[\.\.\.ALL_CAPABILITIES\];/);
});

test('NavBar renders the shared nav model instead of keeping its own copy', () => {
  const source = readSource('src/components/NavBar.tsx');

  assert.match(source, /NAV_ROUTES/);
  assert.doesNotMatch(source, /labelKey: 'nav\./);
  assert.doesNotMatch(source, /path: '\//);
});
