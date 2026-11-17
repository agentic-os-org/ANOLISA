/** Gate an owned request snapshot immediately before the runtime sends it. */
export function checkedFetch(send, check) {
  return async function (input, init) {
    const url = input instanceof Request ? input.url : String(input);
    // The semantic profile selects requests before any asynchronous work.
    const gate = check(url, init, input);
    if (!gate) return send.call(this, input, init);
    if (input instanceof Request || !init || typeof init.body !== 'string') {
      throw Error('AW final gate requires a serialized request body');
    }
    const snapshot = { ...init, headers: new Headers(init.headers), body: init.body };
    snapshot.signal?.throwIfAborted();
    const body = await gate(snapshot.body, snapshot.signal);
    snapshot.signal?.throwIfAborted();
    if (typeof body !== 'string') {
      throw Error('AW final gate did not produce a live request');
    }
    // Only immutable checked bytes cross the runtime boundary after the await.
    return send.call(this, url, { ...snapshot, body });
  };
}

/** Cover both global fetch and Bun's independently pooled fetch sessions. */
export function installFetchGate(check, runtime = globalThis) {
  const wrapped = checkedFetch(runtime.fetch, check);
  Object.defineProperty(runtime, 'fetch', { value: wrapped, writable: false, configurable: false });
  const NativeSession = runtime.Bun?.FetchSession;
  if (NativeSession) {
    class CheckedSession extends NativeSession {
      constructor(...args) {
        super(...args);
        Object.defineProperty(this, 'fetch', { value: checkedFetch(this.fetch, check) });
      }
    }
    Object.defineProperty(runtime.Bun, 'FetchSession', { value: CheckedSession, writable: false });
  }
}
