const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { runInNewContext } = require('node:vm');
const test = require('node:test');

// Exercise the compiled cards through the handles used by the page's Save
// button. Delay only configuration I/O; keep state and effect dependencies.
async function card(kind) {
  const states = [], effects = [], callbacks = [], pending = [], submitted = [];
  let stateIndex, effectIndex, callbackIndex, renderCard;
  let resolveLoad, rejectLoad;
  let reads = 0;
  const loaded = new Promise((resolve, reject) => { resolveLoad = resolve; rejectLoad = reject; });
  const ref = { current: null };
  const t = (key) => key;
  const react = {
    forwardRef(render) { renderCard = () => render({}, ref); return renderCard; },
    useImperativeHandle(handle, create) { handle.current = create(); },
    useState(initial) {
      const index = stateIndex++;
      if (!(index in states)) states[index] = initial;
      return [states[index], (value) => { states[index] = typeof value === 'function' ? value(states[index]) : value; }];
    },
    useEffect(callback, deps) {
      const index = effectIndex++;
      if (!effects[index] || deps.some((dep, i) => !Object.is(dep, effects[index][i]))) pending.push(callback);
      effects[index] = deps;
    },
    useCallback(callback, deps) {
      const index = callbackIndex++;
      if (!callbacks[index] || deps.some((dep, i) => !Object.is(dep, callbacks[index].deps[i]))) {
        callbacks[index] = { callback, deps };
      }
      return callbacks[index].callback;
    },
  };
  runInNewContext(readFileSync(kind === 'storage'
    ? process.env.AGENTSIGHT_SETTINGS_PAGE_BUILD : process.env.AGENTSIGHT_LLM_CONFIG_BUILD, 'utf8'), {
    exports: {},
    require(name) {
      if (name === 'react') return react;
      if (name === 'react/jsx-runtime') return {
        jsx: (type, props) => ({ type, props }), jsxs: (type, props) => ({ type, props }),
      };
      if (name === '../i18n') return { useI18n: () => ({ t }) };
      if (name === '../components/OptimizationSettings') return { LlmConfigForm: () => null };
      if (name === '../utils/apiClient') return {
        fetchStorageStatus: () => reads++ === 0 ? loaded : Promise.resolve({ total_physical_bytes: 1024 }),
        fetchOptimizeConfig: () => loaded,
        async saveStorageLimit(value) { submitted.push(value); },
        async saveOptimizeConfig(value) { submitted.push(value); return value; },
      };
      throw new Error(`Unexpected card dependency: ${name}`);
    },
  });
  function render() {
    stateIndex = effectIndex = callbackIndex = 0;
    return renderCard();
  }
  async function settle() {
    render();
    while (pending.length) {
      for (const effect of pending.splice(0)) effect();
      await new Promise(setImmediate);
      render();
    }
    await new Promise(setImmediate);
    return render();
  }
  function nodes(node) {
    if (!node || typeof node !== 'object') return [];
    return [node, ...[node.props?.children].flat(Infinity).flatMap(nodes)];
  }
  await settle();
  return {
    submitted,
    async load(value) { resolveLoad(value); await settle(); },
    async fail() { rejectLoad('configuration unavailable'); await settle(); },
    async save() { render(); const result = await ref.current.save(); await settle(); return result; },
    input(value) {
      nodes(render()).find((node) => node.props?.id === 'storage-limit-mb')
        .props.onChange({ target: { value } });
    },
    text: () => JSON.stringify(render()),
  };
}

for (const kind of ['storage', 'llm']) {
  test(`${kind}: saving while the initial read is pending does not write defaults`, async () => {
    const form = await card(kind);
    assert.equal(await form.save(), false);
    assert.deepEqual(form.submitted, []);
    await form.load(kind === 'storage'
      ? { max_total_size_mb: 512, total_physical_bytes: 1024 }
      : { base_url: 'https://example.test/v1', model: 'existing', search_timeout_secs: 45 });
    assert.equal(await form.save(), true);
    if (kind === 'storage') assert.deepEqual(form.submitted, [512]);
    else {
      assert.equal(form.submitted[0].search_timeout_secs, 45);
      assert.equal(form.submitted[0].model, 'existing');
    }
  });

  test(`${kind}: a failed initial read remains visible and cannot be saved`, async () => {
    const form = await card(kind);
    await form.fail();
    assert.equal(await form.save(), false);
    assert.deepEqual(form.submitted, []);
    assert.match(form.text(), /loadFailed/);
  });
}

test('storage: clearing the limit is invalid, but explicitly entering zero disables it', async () => {
  const form = await card('storage');
  await form.load({ max_total_size_mb: 512, total_physical_bytes: 1024 });
  for (const invalid of ['', ' ', '8', '-1', '9.5']) {
    form.input(invalid);
    assert.equal(await form.save(), false, `invalid limit: ${JSON.stringify(invalid)}`);
  }
  assert.deepEqual(form.submitted, []);
  form.input('0');
  assert.equal(await form.save(), true);
  form.input('9');
  assert.equal(await form.save(), true);
  assert.deepEqual(form.submitted, [0, 9]);
});
