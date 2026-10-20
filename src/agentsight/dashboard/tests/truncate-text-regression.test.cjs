const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const test = require('node:test');

const { truncateText } = require(process.env.AGENTSIGHT_TRUNCATE_TEXT_BUILD);

const endsWithLoneHighSurrogate = (s) => {
  if (s.length === 0) return false;
  const last = s.charCodeAt(s.length - 1);
  return last >= 0xd800 && last <= 0xdbff;
};

test('short text is returned unchanged', () => {
  assert.equal(truncateText('hello', 10), 'hello');
  assert.equal(truncateText('hello', 5), 'hello');
  assert.equal(truncateText('', 5), '');
});

test('long text is clipped at the budget with an ellipsis', () => {
  assert.equal(truncateText('abcdefgh', 5), 'abcde\u2026');
});

test('a cut landing inside a surrogate pair drops the lone high surrogate', () => {
  // The #6738 repro shape: 299 ASCII code units, then an astral character.
  const text = 'a'.repeat(299) + '\u{1f431}' + ' tail';
  const naive = text.slice(0, 300);
  assert.ok(endsWithLoneHighSurrogate(naive), 'the naive slice must split the pair (the bug)');
  const clipped = truncateText(text, 300);
  assert.ok(
    !endsWithLoneHighSurrogate(clipped.slice(0, -1)),
    'no lone surrogate may sit before the ellipsis',
  );
  assert.equal(clipped, 'a'.repeat(299) + '\u2026');
});

test('a complete surrogate pair inside the budget is kept whole', () => {
  assert.equal(truncateText('\u{1f600}\u{1f601}', 4), '\u{1f600}\u{1f601}');
  assert.equal(truncateText('\u{1f600}\u{1f601}', 3), '\u{1f600}\u2026');
});

// The helper is only a fix while the content-bearing sites actually use it:
// pin that the raw code-unit clips are gone from the three components.
test('the content-bearing truncation sites route through the helper', () => {
  const sites = {
    'src/pages/AtifViewerPage.tsx': ["text.slice(0, TEXT_THRESHOLD)"],
    'src/components/CausalAttributionPanel.tsx': [".slice(0, 13)", ".slice(0, 200)"],
    'src/components/SubagentGraph.tsx': ['node.label.slice(0, 20)'],
  };
  for (const [file, banned] of Object.entries(sites)) {
    const src = readFileSync(join(__dirname, '..', file), 'utf8');
    for (const needle of banned) {
      assert.ok(!src.includes(needle), `${file} still clips content with raw ${needle}`);
    }
    assert.ok(src.includes('truncateText'), `${file} must use truncateText`);
  }
});
