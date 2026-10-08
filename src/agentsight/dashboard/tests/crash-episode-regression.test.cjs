const assert = require('node:assert/strict');
const { join } = require('node:path');
const babel = require('@babel/core');

const compiled = babel.transformFileSync(join(__dirname, '../src/components/AgentHealthNotifier.tsx'), {
  presets: [
    [require.resolve('@babel/preset-env'), { targets: { node: 'current' } }],
    [require.resolve('@babel/preset-typescript'), { isTSX: true, allExtensions: true }],
    [require.resolve('@babel/preset-react'), { runtime: 'automatic' }],
  ],
}).code;
const { runInNewContext } = require('node:vm');
const test = require('node:test');

const settle = () => new Promise(setImmediate);
const agent = (status, has_crash = false) => ({ pid: 10, agent_name: 'Agent', status, has_crash });

function notifier() {
  const states = [];
  const refs = [];
  const requests = [];
  const intervals = [];
  let stateIndex = 0;
  let refIndex = 0;
  let effects = [];
  const exports = {};
  runInNewContext(compiled, {
    exports,
    setTimeout: () => 0,
    setInterval(callback, ms) { assert.equal(ms, 10_000); intervals.push(callback); return intervals.length; },
    clearInterval() {},
    require(name) {
      if (name === 'react') return {
        useState(initial) {
          const index = stateIndex++;
          if (!(index in states)) states[index] = initial;
          return [states[index], (value) => { states[index] = typeof value === 'function' ? value(states[index]) : value; }];
        },
        useRef(initial) {
          const index = refIndex++;
          return refs[index] ??= { current: initial };
        },
        useEffect: (callback) => effects.push(callback),
        useCallback: (callback) => callback,
      };
      if (name === 'react/jsx-runtime') return {
        jsx: (type, props) => ({ type, props }),
        jsxs: (type, props) => ({ type, props }),
      };
      if (name === '../i18n') return { useI18n: () => ({ t: (key) => key }) };
      if (name === '../utils/apiClient') return {
        fetchAgentProcessHealth(options) {
          assert.equal(options.includeClients, true);
          return new Promise((resolve, reject) => requests.push({ resolve, reject }));
        },
      };
      throw new Error(`Unexpected AgentHealthNotifier dependency: ${name}`);
    },
  });
  function render() {
    stateIndex = 0;
    refIndex = 0;
    effects = [];
    return exports.AgentHealthNotifier();
  }
  return {
    mount() {
      render();
      const cleanup = effects[0]();
      return { cleanup, poll: intervals[intervals.length - 1] };
    },
    requests,
    messages: () => Array.from(render().props.children, (node) => node.props.children),
    async respond(index, agents) { requests[index].resolve({ agents }); await settle(); },
  };
}


async function snapshots(values) {
  const view = notifier();
  const effect = view.mount();
  for (const [index, agents] of values.entries()) {
    if (index) effect.poll();
    await view.respond(index, agents);
  }
  return view.messages();
}

for (const recovery of ['healthy', 'unhealthy', 'unknown', 'no_port', 'hung']) {
  test(`observed ${recovery} PID reuse rearms a later crash`, async () => {
    const messages = await snapshots([
      [agent('offline', true)], [agent('offline', true)],
      [agent(recovery)], [agent('offline', true)], [agent('offline', true)],
    ]);
    assert.equal(messages.filter(message => message === 'comp.agentHealth.crashToast').length, 2);
  });
}

test('continuous crash snapshots retain one notice', async () => {
  assert.deepEqual(await snapshots(Array.from({ length: 4 }, () => [agent('offline', true)])), ['comp.agentHealth.crashToast']);
});

test('disappearance still starts a later crash episode', async () => {
  assert.equal((await snapshots([[agent('offline', true)], [], [agent('offline', true)]])).length, 2);
});

test('normal exit and healthy snapshots do not raise a crash', async () => {
  assert.deepEqual(await snapshots([[agent('healthy')], [agent('offline')], [agent('unknown')]]), []);
});

test('hung recovery retains its existing independent deduplication', async () => {
  assert.deepEqual(await snapshots([[agent('hung')], [agent('hung')], [agent('healthy')], [agent('hung')]]), ['comp.agentHealth.hungToast', 'comp.agentHealth.hungToast']);
});

test('one recovered PID does not rearm another active crash', async () => {
  const second = { ...agent('offline', true), pid: 20 };
  const messages = await snapshots([[agent('offline', true), second], [agent('healthy'), second], [agent('offline', true), second]]);
  assert.equal(messages.length, 3);
});
