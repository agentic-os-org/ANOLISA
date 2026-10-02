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
