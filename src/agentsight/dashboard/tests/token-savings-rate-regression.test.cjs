const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const test = require('node:test');
const babel = require('@babel/core');

// Regression: each session row printed the server's per-session
// `compounded_savings_rate` (compounded_saved / total_tokens) while the
// summary card and the column tooltip use compounded_saved / baseline_tokens,
// so the same metric disagreed across the page and the row could exceed 100 %.
// The ATIF savings card had the same defect against its own "Original /
// Actual" numbers, which it renders from the baseline fields.

function loadPageModule(relativePath) {
  const out = babel.transformFileSync(join(process.cwd(), relativePath), {
    presets: [
      ['@babel/preset-env', { targets: { node: 'current' } }],
      ['@babel/preset-typescript', { isTSX: true, allExtensions: true }],
      ['@babel/preset-react', { runtime: 'classic' }],
    ],
  });
  const module = { exports: {} };
  const fn = new Function('require', 'module', 'exports', out.code);
  fn(() => ({}), module, module.exports);
  return module.exports;
}

test('session rows render the baseline formula shared with the summary card', () => {
  const page = loadPageModule('src/utils/savings.ts');
  assert.equal(typeof page.compoundedSavingsRate, 'function',
    'the shared rate helper must be exported');

  // Server-shaped SessionSavings row: saved=30, total=100, baseline=130.
  const rate = page.compoundedSavingsRate(30, 130);
  assert.equal(rate.toFixed(1), '23.1', 'rate must be compounded_saved / baseline_tokens');
  assert.notEqual(rate.toFixed(1), '30.0', 'the server per-total rate must not be shown');
  assert.equal(page.compoundedSavingsRate(30, 0), 0, 'a zero baseline must not divide by zero');

  // The numeric helper is only half the fix: pin that the row and the summary
  // card both go through it, so the two surfaces cannot drift apart again.
  const source = readFileSync(join(process.cwd(), 'src/pages/TokenSavingsPage.tsx'), 'utf8');
  const row = source.slice(source.indexOf('const SessionRow'), source.indexOf('// ─── Main page'));
  assert.match(
    row,
    /compoundedSavingsRate\(session\.compounded_saved, session\.baseline_tokens\)/,
    'the session row must compute its rate from the baseline',
  );
  assert.doesNotMatch(row, /session\.compounded_savings_rate/,
    'the server rate divides by actual total tokens and must not drive the row');
  assert.match(
    source,
    /const savingsRate = compoundedSavingsRate\(totalCompoundedSaved, baselineTokens\);/,
    'the summary card must share the same formula',
  );
});

test('the ATIF savings card uses the baseline formula, not the server savings_rate', () => {
  const page = loadPageModule('src/utils/savings.ts');
  // The card shows original 130, actual 100, saved 30; its percentage must be
  // 30 / 130 = 23.1 %, not the server's 30 / 100 = 30 %.
  const rate = page.compoundedSavingsRate(30, 130);
  assert.equal(rate.toFixed(1), '23.1', 'ATIF card rate must be saved / original');
  assert.notEqual(rate.toFixed(1), '30.0', 'actual-token denominator must not be used');

  const source = readFileSync(join(process.cwd(), 'src/pages/AtifViewerPage.tsx'), 'utf8');
  assert.match(
    source,
    /compoundedSavingsRate\(\s*savingsDetail\.total_compounded_saved,\s*savingsDetail\.total_original_tokens,?\s*\)/,
    'the ATIF card must share the savings page baseline formula',
  );
  assert.doesNotMatch(
    source,
    /savingsDetail\.savings_rate/,
    'the server rate divides by actual tokens and must not drive the card',
  );
});
