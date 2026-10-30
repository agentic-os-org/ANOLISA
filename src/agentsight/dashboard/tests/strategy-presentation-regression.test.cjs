const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { runInNewContext } = require('node:vm');
const test = require('node:test');
const { OPTIMIZATION_STRATEGIES } = require(process.env.AGENTSIGHT_STRATEGIES_BUILD);

function component(file, name, locale = 'en') {
  const exports = {};
  const t = (key, params) => `${locale}:${key}${params?.strategy ? `:${params.strategy}` : ''}`;
  runInNewContext(readFileSync(file, 'utf8') + `\nexports.fixture = ${name};`, {
    exports, setTimeout, clearTimeout,
    require(id) {
      if (id === 'react') return { useState: (value) => [value, () => {}] };
      if (id === 'react/jsx-runtime') return { jsx: (type, props) => ({ type, props }), jsxs: (type, props) => ({ type, props }) };
      if (id === '../utils/optimizationStrategies') return { OPTIMIZATION_STRATEGIES };
      if (id === '../i18n') return { useI18n: () => ({ t }), useLocaleTag: () => locale === 'zh' ? 'zh-CN' : 'en-US' };
      if (id === 'recharts' || id === 'react-router-dom' || id.startsWith('../components/') || id.startsWith('../utils/')) return {};
      throw new Error(`Unexpected component dependency: ${id}`);
    },
  });
  return exports.fixture;
}

function nodes(node) {
  if (!node || typeof node !== 'object') return [];
  return [node, ...[node.props?.children].flat(Infinity).flatMap(nodes)];
}

function badges(strategy, locale) {
  const item = { id: 'call', category: 'tool_output', strategy, strategy_label: 'server label', title: 'output', before_tokens: 100, after_tokens: 60, compounded_saved: 40 };
  const atif = component(process.env.AGENTSIGHT_ATIF_PAGE_BUILD, 'ToolCallItem', locale)({ tc: { tool_call_id: 'call', function_name: 'fixture', arguments: {} }, savingsMap: new Map([['call', item]]) });
  const savings = component(process.env.AGENTSIGHT_SAVINGS_PAGE_BUILD, 'OptimizationTableRow', locale)({ item });
  return { atif: nodes(atif), savings: nodes(savings) };
}

for (const strategy of ['compress-schema', 'compress-response', 'rewrite-command', 'compress-toon']) {
  test(`${strategy} shares translated labels and badge styles across both production views`, () => {
    const config = OPTIMIZATION_STRATEGIES[strategy];
    for (const locale of ['en', 'zh']) {
      const result = badges(strategy, locale);
      const label = `${locale}:${config.labelKey}`;
      assert.ok(result.atif.some((node) => node.props?.children === `${locale}:atif.optimizedTokens:${label}` && node.props.className.includes(config.color) && node.props.className.includes(config.bg)));
      assert.ok(result.savings.some((node) => [node.props?.children].flat().includes(label) && node.props.className.includes(config.color) && node.props.className.includes(config.bg)));
      assert.ok(result.savings.some((node) => [node.props?.children].flat().includes(`${locale}:${config.tooltipKey}`)));
    }
  });
}

test('all existing chart colors and translated tooltip keys remain available', () => {
  assert.deepEqual(Object.values(OPTIMIZATION_STRATEGIES).map((config) => config.pie), ['#3b82f6', '#8b5cf6', '#f59e0b', '#14b8a6']);
  assert.ok(Object.values(OPTIMIZATION_STRATEGIES).every((config) => config.tooltipKey.startsWith('ts.')));
});

test('unknown strategies retain each view’s server-label and neutral-style fallback', () => {
  const result = badges('future-strategy', 'en');
  assert.equal(OPTIMIZATION_STRATEGIES['future-strategy'], undefined);
  assert.ok(result.atif.some((node) => node.props?.children === 'en:atif.optimizedTokens:server label' && node.props.className.includes('bg-gray-100') && node.props.className.includes('text-gray-700')));
  assert.ok(result.savings.some((node) => [node.props?.children].flat().includes('server label') && node.props.className.includes('bg-gray-100') && node.props.className.includes('text-gray-700')));
});
