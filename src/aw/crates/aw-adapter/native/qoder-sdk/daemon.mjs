/** Daemon shutdown joins each owned Query and original credential callback. */
const records = new WeakMap();

export function credentialOptions(value) {
  if (Array.isArray(value)) return value.map(credentialOptions);
  if (value && typeof value === "object")
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, credentialOptions(item)]));
  return value;
}

function trackedCredentials(value, record) {
  const auth = credentialOptions(value);
  for (const name of ["fetchJobToken", "fetchServiceAccountToken"]) {
    if (typeof auth?.[name] !== "function") continue;
    const original = auth[name];
    auth[name] = (...args) => {
      if (record.closing) throw new Error("AW daemon credentials are closing");
      const pending = Promise.resolve().then(() => original(...args));
      record.callbacks.add(pending);
      return pending.finally(() => record.callbacks.delete(pending));
    };
  }
  return auth;
}

export async function managedDaemon(sdk, options, config, bounded,
  environment = { ...process.env, ...(options.env ?? {}) }) {
  const record = { binding: config.binding, sdk: config.sdk, site: config.sdk_site ?? "global", environment, queries: new Set(), callbacks: new Set(), closing: false };
  const auth = trackedCredentials(options.auth, record);
  const daemon = await sdk.startDaemon({ ...options, env: { ...environment }, ...(auth ? { auth } : {}) });
  if (typeof daemon.auth === "function") {
    const authenticate = daemon.auth.bind(daemon);
    daemon.auth = async payload => {
      if (record.closing) throw new Error("AW daemon credentials are closing");
      // Omitted payload keeps the SDK's held credentials; replacements must
      // share the same drain set, including callbacks from earlier credentials.
      return await authenticate(payload === undefined ? undefined : trackedCredentials(payload, record));
    };
  }
  const stop = daemon.stop.bind(daemon);
  daemon.stop = () => {
    if (!record.stopped) {
      record.closing = true;
      record.stopped = (async () => {
        const closed = await Promise.allSettled([...record.queries].map(close => close()));
        const failures = closed.filter(item => item.status === "rejected").map(item => item.reason);
        try { await bounded(Promise.allSettled([...record.callbacks])); }
        catch (error) { failures.push(error); }
        try { await stop(); }
        catch (error) { failures.push(error); }
        if (failures.length) throw new AggregateError(failures, "AW daemon cleanup failed");
      })();
      record.stopped.catch(() => {});
    }
    return record.stopped;
  };
  records.set(daemon, record);
  return daemon;
}

export function daemonOwner(daemon, config) {
  if (daemon === undefined) return undefined;
  const record = records.get(daemon);
  if (!record || record.closing || record.binding !== config.binding)
    throw new Error("AW Qoder SDK requires a live daemon created by its startDaemon factory for this binding");
  return record;
}
