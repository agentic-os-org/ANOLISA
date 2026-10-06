const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const test = require('node:test');

const { resolveLocale, messages, SUPPORTED_LOCALES } = require(process.env.AGENTSIGHT_I18N_BUILD);

test('resolveLocale selects the first supported browser locale', () => {
  assert.equal(resolveLocale(null, ['fr-FR', 'zh-CN']), 'zh-CN');
  assert.equal(resolveLocale(null, ['fr-FR', 'en-GB']), 'en-US');
  assert.equal(resolveLocale(null, ['zh-HK', 'en-US']), 'zh-CN');
  assert.equal(resolveLocale(null, ['de-DE', 'fr-FR']), 'en-US');
});

test('resolveLocale keeps a supported persisted locale as the highest priority', () => {
  assert.equal(resolveLocale('zh-CN', ['en-US']), 'zh-CN');
  assert.equal(resolveLocale('en-US', ['zh-CN']), 'en-US');
});

test('resolveLocale rejects an unsupported persisted locale', () => {
  assert.equal(resolveLocale('fr-FR', ['en-US']), 'en-US');
});

test('resolveLocale skips falsy browser languages', () => {
  assert.equal(resolveLocale(null, [undefined, 'zh-CN']), 'zh-CN');
  assert.equal(resolveLocale(null, [undefined]), 'en-US');
});

// tsc already guarantees key alignment across locales; placeholder sets are
// invisible to the type system, so a missing `{n}` in one translation would
// silently leak the raw brace text into the UI.
test('every message uses identical placeholders across locales', () => {
  const placeholders = (msg) => (msg.match(/\{[a-zA-Z_]+\}/g) ?? []).sort().join(',');
  const [baseLocale, ...otherLocales] = SUPPORTED_LOCALES;
  for (const key of Object.keys(messages[baseLocale])) {
    const expected = placeholders(messages[baseLocale][key]);
    for (const locale of otherLocales) {
      assert.equal(
        placeholders(messages[locale][key]),
        expected,
        `placeholder mismatch for '${key}' between ${baseLocale} and ${locale}`,
      );
    }
  }
});

// ── Risk-conclusion rendering ────────────────────────────────────────────────
// The enforcer authors the policy DSL `because` clause in English; the mapping
// to Chinese is a display concern, so it must apply to a Chinese UI only. It
// used to run unconditionally, which printed the Chinese conclusion inside the
// English Risk-cases panel — the default locale — while every surrounding label
// stayed English.

const { translateRuleReason } = require(process.env.AGENTSIGHT_RULE_REASON_BUILD);

const ruleReason = 'credential-derived data reached an untrusted network target';

test('the risk conclusion keeps the server wording outside a Chinese locale', () => {
  assert.equal(translateRuleReason(ruleReason, 'en-US'), ruleReason);
});

test('the risk conclusion is translated for a Chinese locale', () => {
  assert.equal(translateRuleReason(ruleReason, 'zh-CN'), '凭据衍生数据访问了不可信网络目标');
});

test('an unknown or empty risk conclusion stays as the server wrote it', () => {
  assert.equal(translateRuleReason('a rule authored later', 'zh-CN'), 'a rule authored later');
  assert.equal(translateRuleReason('a rule authored later', 'en-US'), 'a rule authored later');
  assert.equal(translateRuleReason('', 'zh-CN'), '');
});

test('the audit page passes the active locale to every risk-conclusion call', () => {
  const source = readFileSync(
    join(process.cwd(), 'src/pages/SystemAuditPage.tsx'),
    'utf8',
  );
  const calls = source.match(/translateRuleReason\([^)]*\)/g) ?? [];
  assert.equal(calls.length, 2, `expected two call sites, got ${JSON.stringify(calls)}`);
  for (const call of calls) {
    assert.match(
      call,
      /, locale\)$/,
      `the active locale must decide whether '${call}' is translated`,
    );
  }
});
