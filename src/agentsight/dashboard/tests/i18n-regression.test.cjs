const assert = require('node:assert/strict');
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
