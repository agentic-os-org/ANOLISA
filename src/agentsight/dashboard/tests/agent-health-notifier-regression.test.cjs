const assert = require('node:assert/strict');
const { join } = require('node:path');
const test = require('node:test');

// Regression tests for AgentHealthNotifier stale health polls (#5545).
//
// The notifier polls agent health every ten seconds. Responses may resolve
// out of order, and an effect cleanup (unmount / StrictMode remount) can
// happen while a poll is in flight. Tests below execute the REAL component
// (transpiled from src/components/AgentHealthNotifier.tsx with the
// dashboard's babel toolchain) against a minimal hooks driver and manually
// resolved health responses, with the interval/timer globals captured so no
// real timer ever fires.
//
// Reproduction from the report: resolve the second request with a healthy
// PID, then resolve the first request with offline+has_crash for the same
// PID — a crash toast must not appear after the newer healthy snapshot.
// An older healthy response must also not clear the deduplication marker a
// newer hung notice installed, or the next identical hung snapshot repeats
// its toast.

const babel = require('@babel/core');

function transpile(relativePath) {
  const out = babel.transformFileSync(join(process.cwd(), relativePath), {
    presets: [
      ['@babel/preset-env', { targets: { node: 'current' } }],
      ['@babel/preset-typescript', { isTSX: true, allExtensions: true }],
      ['@babel/preset-react', { runtime: 'classic' }],
    ],
  });
  return out.code;
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

// Captured before any test stubs the timer globals; settle() must always
// use the real timer or the awaits below would hang under captureTimers().
const REAL_SET_TIMEOUT = setTimeout;
const settle = () => new Promise((resolve) => REAL_SET_TIMEOUT(resolve, 0));

function createHooksDriver() {
  const slots = [];          // useState/useRef storage, indexed by hook order
  const driver = {
    slots,
    render(Component, props = {}) {
      driver._cursor = 0;
      driver._effects = [];
      driver._callbacks = [];
      const element = Component(props);
      return { element, effects: driver._effects, callbacks: driver._callbacks };
    },
    useState(initial) {
      const slot = slots[driver._cursor] ?? {
        value: typeof initial === 'function' ? initial() : initial,
      };
      slots[driver._cursor] = slot;
      if (!slot.setter) {
        slot.setter = (update) => {
          slot.value = typeof update === 'function'
            ? update(slot.value)
            : update;
        };
      }
      driver._cursor += 1;
      return [slot.value, slot.setter];
    },
    useRef(initial) {
      const slot = slots[driver._cursor] ?? { value: { current: initial } };
      slots[driver._cursor] = slot;
      driver._cursor += 1;
      return slot.value;
    },
    useEffect(fn) {
      driver._effects.push(fn);
    },
    useCallback(fn) {
      driver._callbacks.push(fn);
      return fn;
    },
    useMemo(factory) {
      return factory();
    },
  };
  return driver;
}

// Captures interval callbacks and toast-dismiss timeouts instead of letting
// real timers fire; the tests decide when a poll "tick" happens.
function captureTimers() {
  const captured = { intervals: [], clears: 0, timeouts: 0 };
  const originals = {
    setInterval: global.setInterval,
    clearInterval: global.clearInterval,
    setTimeout: global.setTimeout,
  };
  global.setInterval = (fn, ms) => {
    captured.intervals.push({ fn, ms });
    return { captured: true };
  };
  global.clearInterval = () => {
    captured.clears += 1;
  };
  global.setTimeout = (fn, ms) => {
    captured.timeouts += 1;
    return { captured: true };
  };
  captured.restore = () => {
    global.setInterval = originals.setInterval;
    global.clearInterval = originals.clearInterval;
    global.setTimeout = originals.setTimeout;
  };
  return captured;
}

function loadNotifierModule(driver, healthCalls) {
  const code = transpile('src/components/AgentHealthNotifier.tsx');
  const module = { exports: {} };
  const hooks = {
    useState: driver.useState,
    useRef: driver.useRef,
    useEffect: driver.useEffect,
    useCallback: driver.useCallback,
    useMemo: driver.useMemo,
  };
  const reactStub = {
    __esModule: true,
    default: { createElement: (type, props, ...children) => ({ type, props, children }), ...hooks },
    createElement: (type, props, ...children) => ({ type, props, children }),
    ...hooks,
  };
  const requireStub = (name) => {
    if (name === 'react') return reactStub;
    if (name === '../i18n') {
      return {
        useI18n: () => ({
          t: (key, params) => ({ key, params }),
        }),
      };
    }
    if (name === '../utils/apiClient') {
      return {
        fetchAgentProcessHealth: (...args) => {
          const d = deferred();
          healthCalls.push({ args, ...d });
          return d.promise;
        },
      };
    }
    throw new Error(`unexpected require from AgentHealthNotifier.tsx: ${name}`);
  };
  const fn = new Function('require', 'module', 'exports', code);
  fn(requireStub, module, module.exports);
  return module.exports;
}

// Mounts the notifier: runs the mounted effect (initial poll + interval
// registration) with timer globals captured.
function mountNotifier() {
  const timers = captureTimers();
  const driver = createHooksDriver();
  const healthCalls = [];
  const module = loadNotifierModule(driver, healthCalls);
  const Notifier = module.AgentHealthNotifier;
  assert.equal(typeof Notifier, 'function', 'AgentHealthNotifier must be a component');
  const rendered = driver.render(Notifier);
  // Run the mounted effect: it starts the initial poll and registers the
  // interval whose captured callback drives later ticks.
  const mountedEffect = rendered.effects[rendered.effects.length - 1];
  const cleanup = mountedEffect();
  assert.equal(healthCalls.length, 1, 'mount must start the first poll');
  assert.equal(timers.intervals.length, 1, 'mount must register the poll interval');
  assert.equal(timers.intervals[0].ms, 10_000);
  const toasts = () => driver.slots[0].value;
  const notified = () => {
    const slot = driver.slots.find((s) => s && s.value && s.value.current instanceof Set);
    assert.ok(slot, 'notifier must keep a deduplication Set ref');
    return slot.value.current;
  };
  const tick = () => {
    timers.intervals[0].fn();
    return healthCalls[healthCalls.length - 1];
  };
  return { timers, driver, healthCalls, cleanup, toasts, notified, tick };
}

const agent = (pid, status, has_crash = false, agent_name = 'agent') => ({
  pid,
  status,
  has_crash,
  agent_name,
});

test('a stale offline response must not raise a crash toast after a newer healthy snapshot', async () => {
  const ctx = mountNotifier();

  // A tick starts a newer poll while the mount poll is still pending.
  const newer = ctx.tick();
  assert.equal(ctx.healthCalls.length, 2);

  // The NEWER response lands first: PID 10 is healthy.
  newer.resolve({ agents: [agent(10, 'running')] });
  await settle();
  await settle();

  // The OLDER response lands last and reports the same PID offline+crashed.
  ctx.healthCalls[0].resolve({ agents: [agent(10, 'offline', true)] });
  await settle();
  await settle();

  assert.deepEqual(ctx.toasts(), [], 'an obsolete response must not raise a crash toast');
  assert.equal(ctx.notified().has(10), false, 'an obsolete response must not mark the PID notified');
  ctx.timers.restore();
});

test('an older healthy response must not clear a newer hung notice marker', async () => {
  const ctx = mountNotifier();

  // Newer poll: PID 5 is hung — one hung toast, marker installed.
  const newer = ctx.tick();
  newer.resolve({ agents: [agent(5, 'hung')] });
  await settle();
  await settle();
  assert.equal(ctx.toasts().length, 1, 'the hung snapshot must notify once');
  assert.equal(ctx.notified().has(-5), true);

  // Older mount poll lands last with a healthy snapshot: it must not clear
  // the marker while the anomaly is still active.
  ctx.healthCalls[0].resolve({ agents: [agent(5, 'running')] });
  await settle();
  await settle();

  // The next identical hung snapshot is the SAME active anomaly: no repeat.
  const again = ctx.tick();
  again.resolve({ agents: [agent(5, 'hung')] });
  await settle();
  await settle();

  assert.equal(ctx.toasts().length, 1, 'an active anomaly must not repeat its toast');
  ctx.timers.restore();
});

test('effect cleanup must invalidate a pending health response', async () => {
  const ctx = mountNotifier();

  // Unmount (or StrictMode remount) while the mount poll is in flight.
  ctx.cleanup();
  assert.equal(ctx.timers.clears, 1, 'cleanup must clear the poll interval');

  ctx.healthCalls[0].resolve({ agents: [agent(7, 'offline', true)] });
  await settle();
  await settle();

  assert.deepEqual(ctx.toasts(), [], 'a response landing after cleanup must not toast');
  assert.equal(ctx.notified().has(7), false, 'a response landing after cleanup must not mark notified');
  ctx.timers.restore();
});

test('recovery clears the marker so a later recurrence notifies again', async () => {
  const ctx = mountNotifier();

  ctx.healthCalls[0].resolve({ agents: [agent(9, 'hung')] });
  await settle();
  await settle();
  assert.equal(ctx.toasts().length, 1);

  // The newest snapshot reports recovery: the marker may be cleared.
  const recovered = ctx.tick();
  recovered.resolve({ agents: [agent(9, 'running')] });
  await settle();
  await settle();
  assert.equal(ctx.notified().has(-9), false, 'recovery must clear the hung marker');

  // The anomaly recurs: it is a NEW episode and must notify again.
  const recurred = ctx.tick();
  recurred.resolve({ agents: [agent(9, 'hung')] });
  await settle();
  await settle();

  assert.equal(ctx.toasts().length, 2, 'a recurrence after recovery must notify again');
  ctx.timers.restore();
});

test('repeated snapshots of one active anomaly notify exactly once', async () => {
  const ctx = mountNotifier();

  ctx.healthCalls[0].resolve({ agents: [agent(3, 'hung')] });
  await settle();
  await settle();
  const first = ctx.tick();
  first.resolve({ agents: [agent(3, 'hung')] });
  await settle();
  await settle();
  const second = ctx.tick();
  second.resolve({ agents: [agent(3, 'hung')] });
  await settle();
  await settle();

  assert.equal(ctx.toasts().length, 1, 'one active anomaly keeps exactly one notice');
  ctx.timers.restore();
});

test('a disappeared process releases its marker and re-notifies on return', async () => {
  const ctx = mountNotifier();

  ctx.healthCalls[0].resolve({ agents: [agent(12, 'offline', true)] });
  await settle();
  await settle();
  assert.equal(ctx.toasts().length, 1);

  const gone = ctx.tick();
  gone.resolve({ agents: [] });
  await settle();
  await settle();
  assert.equal(ctx.notified().has(12), false, 'a disappeared PID must release its marker');

  const back = ctx.tick();
  back.resolve({ agents: [agent(12, 'offline', true)] });
  await settle();
  await settle();

  assert.equal(ctx.toasts().length, 2, 'a returning crash must notify again');
  ctx.timers.restore();
});

test('a failed poll is silent and keeps active anomaly markers', async () => {
  const ctx = mountNotifier();

  ctx.healthCalls[0].resolve({ agents: [agent(4, 'hung')] });
  await settle();
  await settle();
  assert.equal(ctx.toasts().length, 1);

  const failing = ctx.tick();
  failing.reject(new Error('health endpoint down'));
  await settle();
  await settle();
  assert.equal(ctx.toasts().length, 1, 'a failed poll must be silent');

  const still = ctx.tick();
  still.resolve({ agents: [agent(4, 'hung')] });
  await settle();
  await settle();

  assert.equal(ctx.toasts().length, 1, 'the failed poll must not have cleared the marker');
  ctx.timers.restore();
});
