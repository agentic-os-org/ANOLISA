const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { runInNewContext } = require('node:vm');
const test = require('node:test');

function deferred() {
  let resolve, reject;
  const promise = new Promise((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

// Run the production form and effect cleanup with controlled configuration I/O.
async function configForm(load, save) {
  const states = [], dependencies = [], cleanups = [], effects = [];
  let stateIndex = 0, effectIndex = 0, writes = 0, fetches = 0, locale = 'en';
  let submitted;
  const formRef = { current: null };
  const exports = {};
  const translate = (language) => (key, params) => `${language}:${key}${params?.msg ? `:${params.msg}` : ''}`;
  let t = translate(locale);
  runInNewContext(readFileSync(process.env.AGENTSIGHT_LLM_CONFIG_BUILD, 'utf8'), {
    exports, Error, setTimeout: () => 0,
    require(name) {
      if (name === 'react') return {
        forwardRef: (render) => () => render({}, formRef),
        useImperativeHandle: (ref, createHandle) => { ref.current = createHandle(); },
        useState(initial) {
          const index = stateIndex++;
          if (!(index in states)) states[index] = initial;
          return [states[index], (value) => {
            writes++;
            states[index] = typeof value === 'function' ? value(states[index]) : value;
          }];
        },
        useEffect(callback, deps) {
          const index = effectIndex++;
          if (!dependencies[index] || deps.some((dep, i) => !Object.is(dep, dependencies[index][i]))) {
            effects.push(() => { cleanups[index]?.(); cleanups[index] = callback(); });
          }
          dependencies[index] = deps;
        },
      };
      if (name === 'react/jsx-runtime') return {
        jsx: (type, props) => ({ type, props }), jsxs: (type, props) => ({ type, props }),
      };
      if (name === '../i18n') return { useI18n: () => ({ t }) };
      if (name === '../utils/apiClient') return {
        fetchOptimizeConfig: async () => { fetches++; return typeof load === 'function' ? load() : load; },
        saveOptimizeConfig: async (body) => {
          submitted = body;
          return save ? save(body) : { base_url: body.base_url, model: body.model };
        },
      };
      throw new Error(`Unexpected LlmConfigForm dependency: ${name}`);
    },
  });
  function render() { stateIndex = 0; effectIndex = 0; return exports.LlmConfigForm(); }
  function nodes(node, predicate) {
    if (!node || typeof node !== 'object') return [];
    return [...(predicate(node) ? [node] : []),
      ...[node.props?.children].flat(Infinity).flatMap((child) => nodes(child, predicate))];
  }
  async function flush() {
    render();
    do {
      for (const effect of effects.splice(0)) effect();
      await new Promise(setImmediate);
      render();
    } while (effects.length);
  }
  const inputs = () => nodes(render(), (node) => node.type === 'input');
  const field = (kind) => inputs().find((node) => kind === 'base' ? node.props.placeholder === 'https://your-api-endpoint/v1'
    : kind === 'key' ? node.props.autoComplete === 'off'
    : kind === 'timeout' ? node.props.type === 'number'
    : node.props.placeholder?.includes('opt.llm.model.'));
  await flush();
  return {
    fetches: () => fetches, writes: () => writes, flush,
    unmount() { for (const cleanup of cleanups) cleanup?.(); },
    async locale(language) { locale = language; t = translate(locale); await flush(); },
    value: (kind) => field(kind)?.props.value,
    edit(kind, value) { field(kind).props.onChange({ target: { value } }); },
    preset(value) { nodes(render(), (node) => node.type === 'select')[1].props.onChange({ target: { value } }); },
    error: () => nodes(render(), (node) => node.props?.className?.includes('bg-red-50'))[0]?.props.children,
    loading: () => inputs().length === 0,
    async save() {
      render();
      await formRef.current.save();
      return submitted;
    },
  };
}

test('language changes retain every editable draft without refetching', async () => {
  const form = await configForm({ base_url: 'https://custom.example/v1', model: 'original', search_timeout_secs: 5 });
  for (const [kind, value] of [['base', 'https://draft.example/v1'], ['model', 'draft-model'], ['timeout', '19'], ['key', 'draft-key']]) form.edit(kind, value);
  await form.locale('zh');
  for (const [kind, value] of [['base', 'https://draft.example/v1'], ['model', 'draft-model'], ['timeout', '19'], ['key', 'draft-key']]) assert.equal(form.value(kind), value);
  assert.equal(form.fetches(), 1);
});

test('language changes during the initial load keep one owned request', async () => {
  const request = deferred();
  const form = await configForm(() => request.promise);
  await form.locale('zh');
  assert.equal(form.fetches(), 1);
  assert.equal(form.loading(), true);
  request.resolve({ base_url: 'https://custom.example/v1', model: 'loaded' });
  await form.flush();
  assert.equal(form.value('model'), 'loaded');
  assert.equal(form.loading(), false);
});

test('load errors use the current locale without another request', async () => {
  const form = await configForm(() => Promise.reject(new Error('offline')));
  assert.equal(form.error(), 'en:opt.llm.loadFailed:offline');
  await form.locale('zh');
  assert.equal(form.error(), 'zh:opt.llm.loadFailed:offline');
  assert.equal(form.fetches(), 1);
});

test('cleanup prevents late successful and failed loads from writing form state', async () => {
  for (const success of [true, false]) {
    const request = deferred();
    const form = await configForm(() => request.promise);
    form.unmount();
    const writes = form.writes();
    if (success) request.resolve({ base_url: 'https://custom.example/v1', model: 'late' });
    else request.reject(new Error('late failure'));
    await new Promise(setImmediate);
    assert.equal(form.writes(), writes);
  }
});

test('a preset model draft remains the saved value after changing language', async () => {
  const form = await configForm({ base_url: 'https://api.openai.com/v1', model: 'gpt-4o' });
  form.preset('gpt-4.1');
  form.edit('timeout', '11');
  await form.locale('zh');
  const saved = await form.save();
  assert.equal(saved.model, 'gpt-4.1');
  assert.equal(saved.search_timeout_secs, 11);
});

test('validation and save failures also translate when the locale changes', async () => {
  const form = await configForm({ base_url: 'https://api.openai.com/v1', model: 'gpt-4o' }, () => Promise.reject(new Error('unavailable')));
  form.edit('timeout', '0');
  await form.save();
  await form.locale('zh');
  assert.equal(form.error(), 'zh:opt.llm.semanticSearchTimeout.invalid');
  form.edit('timeout', '5');
  await form.save();
  assert.equal(form.error(), 'zh:opt.llm.saveFailed:unavailable');
  await form.locale('en');
  assert.equal(form.error(), 'en:opt.llm.saveFailed:unavailable');
  assert.equal(form.fetches(), 1);
});
