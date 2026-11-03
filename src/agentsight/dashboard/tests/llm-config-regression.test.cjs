const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { runInNewContext } = require('node:vm');
const test = require('node:test');

// Drive the compiled form and its effects, with configuration I/O mocked.
// Model changes must be read through the same input that a user sees.
async function configForm(config) {
  const states = [];
  const dependencies = [];
  const pendingEffects = [];
  let stateIndex = 0;
  let effectIndex = 0;
  let submitted;
  const formRef = { current: null };
  const exports = {};
  const t = (key) => key;
  runInNewContext(readFileSync(process.env.AGENTSIGHT_LLM_CONFIG_BUILD, 'utf8'), {
    exports,
    setTimeout: () => 0,
    require(name) {
      if (name === 'react') return {
        forwardRef: (render) => () => render({}, formRef),
        useImperativeHandle: (ref, createHandle) => { ref.current = createHandle(); },
        useState(initial) {
          const index = stateIndex++;
          if (!(index in states)) states[index] = initial;
          return [states[index], (value) => { states[index] = typeof value === 'function' ? value(states[index]) : value; }];
        },
        useEffect(callback, deps) {
          const index = effectIndex++;
          if (!dependencies[index] || deps.some((dep, i) => !Object.is(dep, dependencies[index][i]))) pendingEffects.push(callback);
          dependencies[index] = deps;
        },
      };
      if (name === 'react/jsx-runtime') return {
        jsx: (type, props) => ({ type, props }),
        jsxs: (type, props) => ({ type, props }),
      };
      if (name === '../i18n') return { useI18n: () => ({ t }) };
      if (name === '../utils/apiClient') return {
        fetchOptimizeConfig: async () => config,
        saveOptimizeConfig: async (body) => { submitted = body; return { ...config, ...body }; },
      };
      throw new Error(`Unexpected LlmConfigForm dependency: ${name}`);
    },
  });
  function render() {
    stateIndex = 0;
    effectIndex = 0;
    return exports.LlmConfigForm();
  }
  function nodes(node, predicate) {
    if (!node || typeof node !== 'object') return [];
    return [...(predicate(node) ? [node] : []),
      ...[node.props?.children].flat(Infinity).flatMap((child) => nodes(child, predicate))];
  }
  async function flush() {
    render();
    while (pendingEffects.length) {
      for (const callback of pendingEffects.splice(0)) callback();
      await new Promise(setImmediate);
      render();
    }
  }
  const selects = () => nodes(render(), (node) => node.type === 'select');
  const modelInput = () => nodes(render(), (node) => node.type === 'input'
    && ['opt.llm.model.placeholder', 'opt.llm.model.customPlaceholder'].includes(node.props.placeholder))[0];
  await flush();
  return {
    model: () => modelInput()?.props.value,
    input(value) { modelInput().props.onChange({ target: { value } }); },
    async provider(value) { selects()[0].props.onChange({ target: { value } }); await flush(); },
    preset(value) { selects()[1].props.onChange({ target: { value } }); },
    async save() {
      render();
      await formRef.current.save();
      return submitted;
    },
  };
}

test('editing a loaded custom endpoint submits its visible model', async () => {
  const form = await configForm({ base_url: 'https://custom.example/v1', model: 'original-model' });
  assert.equal(form.model(), 'original-model');
  form.input('replacement-model');
  assert.equal(form.model(), 'replacement-model');
  assert.equal((await form.save()).model, 'replacement-model');
});

test('switching a preset custom model to a custom provider preserves the visible edit', async () => {
  const form = await configForm({ base_url: 'https://api.openai.com/v1', model: 'gpt-4o' });
  form.preset('__custom__');
  form.input('custom-preset-model');
  await form.provider('custom');
  assert.equal(form.model(), 'custom-preset-model');
  assert.equal((await form.save()).model, 'custom-preset-model');
});

test('switching between preset providers preserves an unknown custom model edit', async () => {
  const form = await configForm({ base_url: 'https://api.openai.com/v1', model: 'external-original' });
  form.input('external-replacement');
  await form.provider('deepseek');
  assert.equal(form.model(), 'external-replacement');
  assert.equal((await form.save()).model, 'external-replacement');
});

test('preset providers still submit their own custom input', async () => {
  const form = await configForm({ base_url: 'https://api.openai.com/v1', model: 'gpt-4o' });
  form.preset('__custom__');
  form.input('custom-for-preset');
  assert.equal((await form.save()).model, 'custom-for-preset');
});

test('a custom model matching the next provider switches back to its known selection', async () => {
  const form = await configForm({ base_url: 'https://custom.example/v1', model: 'old-model' });
  form.input('deepseek-chat');
  await form.provider('deepseek');
  assert.equal(form.model(), undefined, 'a known model uses the preset select');
  form.preset('deepseek-reasoner');
  const saved = await form.save();
  assert.equal(saved.model, 'deepseek-reasoner');
  assert.equal(saved.base_url, 'https://api.deepseek.com/v1');
});
