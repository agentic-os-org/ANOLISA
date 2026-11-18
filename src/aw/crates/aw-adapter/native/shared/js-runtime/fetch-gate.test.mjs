import assert from 'node:assert/strict';
import test from 'node:test';
import { checkedFetch, installFetchGate } from './fetch-gate.mjs';

test('checked bytes and headers survive mutation while policy awaits', async () => {
  let release;
  const pause = new Promise(resolve => { release = resolve; });
  let sent;
  const fetch = checkedFetch(async (url, init) => { sent = { url, init }; },
    () => async body => { await pause; return body; });
  const init = { body: 'safe', headers: { test: 'original' } };
  const pending = fetch('https://model.test/messages', init);
  init.body = 'secret';
  init.headers.test = 'changed';
  release();
  await pending;
  assert.equal(sent.init.body, 'safe');
  assert.equal(sent.init.headers.get('test'), 'original');
});

test('policy errors, unsupported bodies and cancellation never send', async () => {
  let sends = 0;
  const send = () => { sends++; };
  const failing = checkedFetch(send, () => () => { throw Error('policy failed'); });
  await assert.rejects(failing('https://model.test', { body: 'secret' }));
  await assert.rejects(failing('https://model.test', { body: new Uint8Array(4) }));
  await assert.rejects(failing(new Request('https://model.test'), { body: 'secret' }));
  const controller = new AbortController();
  const cancelled = checkedFetch(send, () => async () => { controller.abort(); return 'safe'; });
  await assert.rejects(cancelled('https://model.test', { body: 'secret', signal: controller.signal }),
    error => error === controller.signal.reason && error.name === 'AbortError');
  for (const before of [false, true]) {
    const custom = new AbortController();
    const reason = { reason: 'caller cancelled' };
    let checks = 0;
    const fetch = checkedFetch(send, () => async () => { checks++; custom.abort(reason); return 'safe'; });
    if (before) custom.abort(reason);
    await assert.rejects(fetch('https://model.test', { body: 'secret', signal: custom.signal }),
      error => error === reason);
    assert.equal(checks, before ? 0 : 1);
  }
  assert.equal(sends, 0);
});

test('global and pooled runtime sends share the gate and retain native instances', async () => {
  const sent = [];
  class NativeSession {
    get fetch() { return async (_url, init) => { sent.push([this, init.body]); }; }
  }
  const runtime = { fetch: async (_url, init) => { sent.push(['global', init.body]); },
    Bun: { FetchSession: NativeSession } };
  installFetchGate(() => async () => 'checked', runtime);
  const session = new runtime.Bun.FetchSession();
  assert.ok(session instanceof NativeSession);
  await runtime.fetch('https://model.test', { body: 'secret' });
  await session.fetch('https://model.test', { body: 'secret' });
  assert.deepEqual(sent, [['global', 'checked'], [session, 'checked']]);
  assert.throws(() => { runtime.fetch = () => {}; }, TypeError);
  assert.throws(() => { runtime.Bun.FetchSession = NativeSession; }, TypeError);
});
