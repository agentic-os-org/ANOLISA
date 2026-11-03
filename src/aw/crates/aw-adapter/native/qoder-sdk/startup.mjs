/** One-shot prewarm through the same admitted Query factory used for ordinary turns. */
export async function prewarm(createQuery, { options = {}, initializeTimeoutMs = 60000 } = {}) {
  if (!Number.isFinite(initializeTimeoutMs) || initializeTimeoutMs <= 0)
    throw new TypeError("initializeTimeoutMs must be a positive finite number");
  let activate;
  const prompt = new Promise(resolve => { activate = resolve; });
  const source = async function* () {
    const value = await prompt;
    if (typeof value === "string") yield { type: "user",
      message: { role: "user", content: [{ type: "text", text: value }] }, parent_tool_use_id: null };
    else if (value !== undefined) yield* value;
  };
  let session;
  const lifecycle = { initializeTimeoutMs, discard: () => activate(undefined) };
  try {
    session = await createQuery({ prompt: source(), options }, lifecycle);
  } catch (error) {
    activate(undefined);
    throw error;
  }
  let used = false, closed = false, closing;
  const close = () => {
    if (!closing) {
      closed = true;
      activate(undefined);
      closing = Promise.resolve().then(() => session.close());
      closing.catch(() => {});
    }
    return closing;
  };
  return Object.freeze({
    query(value) {
      if (closed) throw new Error("WarmQuery is closed");
      if (used) throw new Error("WarmQuery can only be queried once");
      lifecycle.check?.();
      used = true;
      activate(value);
      return session;
    },
    close,
    [Symbol.asyncDispose]: close,
  });
}
