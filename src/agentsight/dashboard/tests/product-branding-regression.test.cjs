const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const test = require('node:test');

const readSource = (relativePath) => readFileSync(join(process.cwd(), relativePath), 'utf8');

test('risk summary exposes three equal product-level cards', () => {
  const source = readSource('src/pages/RiskEnforcementPage.tsx');

  assert.doesNotMatch(source, /label="执行后端"/);
  assert.match(source, /className="grid grid-cols-1 gap-4 sm:grid-cols-3"/);
});

test('user-visible audit and enforcement UI does not expose the implementation backend', () => {
  const publicUiFiles = [
    'src/pages/RiskEnforcementPage.tsx',
    'src/pages/SystemAuditPage.tsx',
    'src/components/ContainmentDialog.tsx',
  ];

  for (const file of publicUiFiles) {
    assert.doesNotMatch(readSource(file), /ActPlane|actplane/, `${file} exposes the backend brand`);
  }
});

test('risk summary violation count follows the agent filter', () => {
  // The table renders `filteredViolations`; counting the unfiltered list in
  // the summary card made the card disagree with the visible rows and with
  // the "filtered by agent" banner.
  const source = readSource('src/pages/RiskEnforcementPage.tsx');
  assert.match(
    source,
    /const displayedViolations = enforcementViolationTotal\(filteredViolations, health\);/,
    'the blocked/audited summary must count the agent-filtered violations',
  );
  assert.doesNotMatch(
    source,
    /enforcementViolationTotal\(violations, health\)/,
    'the unfiltered list must not feed the summary card',
  );
});

test('risk summary active binding count follows the agent filter', () => {
  // The bindings table renders `filteredBindings`, and `filteredBindings`
  // degrades to the full list when no agent filter is active, so deriving the
  // summary card and the binding-limit check from it keeps the unfiltered
  // behaviour while making the card agree with the visible rows.
  const source = readSource('src/pages/RiskEnforcementPage.tsx');
  assert.match(
    source,
    /const activeBindings = filteredBindings\.filter\(\(binding\) => binding\.state === 'enforced'\);/,
    'the active-bindings summary and limit check must use the agent-filtered bindings',
  );
  assert.doesNotMatch(
    source,
    /const activeBindings = bindings\.filter\(/,
    'the unfiltered list must not feed the summary card or the binding limit check',
  );
  assert.match(
    source,
    /const filteredBindings = agentIdFilter[\s\S]*?: bindings;/,
    'the binding filter must stay a passthrough when no agent filter is active',
  );
});
