/** Configuration-driven public Hooks with task admission at the first protected boundary. */
import { loadConfiguration } from "../../../typescript/src/configuration.mjs";
import { Instance, AwError } from "../../../typescript/src/client.mjs";
import { Admission } from "../../../typescript/src/admission.mjs";
import {
  qoderOptions,
  qoderProfile,
  qoderCapabilities,
} from "../../../../crates/aw-adapter/native/qoder-sdk/sdk.mjs";
export { qoderOptions };

const hookEnvironment = (environment) => {
  const copy = { ...environment };
  delete copy.QODER_PERSONAL_ACCESS_TOKEN;
  return copy;
};

/** Select wiring from a trusted descriptor or AW's offline configuration parser. */
export async function createQoderBridge({
  configFile,
  descriptorFile,
  target,
  query,
  availability = "native_fallback",
  executable = "aw",
  attachTimeoutMs = 5000,
  cwd = process.cwd(),
  environment = process.env,
}) {
  if (typeof query !== "function")
    throw new AwError("qoder_bridge_configuration_required");
  if (
    !Number.isInteger(attachTimeoutMs) ||
    attachTimeoutMs < 100 ||
    attachTimeoutMs > 60000
  )
    throw new AwError("invalid_attach_timeout");
  const clean = hookEnvironment(environment);
  const {
    inputEnabled,
    sessionStartEnabled,
    availability: configuredAvailability,
    target: selected,
    endpoint,
  } = await loadConfiguration({
    configFile,
    descriptorFile,
    profile: qoderProfile,
    target,
    executable,
    environment: clean,
  });
  target = selected;
  const capabilities = qoderCapabilities(inputEnabled, sessionStartEnabled);
  const supported = Object.keys(capabilities.events);
  if (
    availability === "native_fallback" &&
    configuredAvailability === "required"
  )
    throw new AwError("sdk_required_policy_conflict");
  const pending = new Set();
  let closed = false;
  const instance = new Instance({
    endpoint,
    target,
    cwd,
    environment: clean,
    availability,
    reconnectMs: 100,
    capabilities,
    install: (routes) =>
      routes.hooks.every(
        (hook) => supported.includes(hook.event) && !hook.skill_check,
      ),
  });
  // Preinstall the routes before Query creation; registration stays deployment-owned.
  instance.listen();
  return {
    get state() {
      return instance.state;
    },
    get lastError() {
      return instance.lastError;
    },
    query(input) {
      if (closed) throw new AwError("instance_closed");
      const options = input.options ?? {};
      const lazy = new Admission(instance, {
        events: supported,
        timeoutMs: attachTimeoutMs,
        cwd: options.cwd ?? cwd,
        environment: hookEnvironment({ ...clean, ...options.env }),
      });
      const integration = qoderOptions(lazy, options);
      let session;
      try {
        session = integration.bind(
          query({ ...input, options: integration.options }),
        );
      } catch (error) {
        void lazy.close();
        throw error;
      }
      let closing;
      const close = () => {
        closing ??= (async () => {
          // Cancel admission first; stop native callbacks before releasing their Scope.
          lazy.cancel();
          try {
            await session.close();
          } finally {
            try {
              await lazy.close();
            } finally {
              pending.delete(close);
            }
          }
        })();
        return closing;
      };
      pending.add(close);
      return new Proxy(session, {
        get(object, key) {
          if (key === "close") return close;
          if (key === Symbol.asyncIterator)
            return () => {
              const iterator = (async function* () {
                try {
                  for await (const message of object) yield message;
                  if (integration.failure) throw integration.failure;
                } finally {
                  await close();
                }
              })();
              return {
                next: (...args) => iterator.next(...args),
                async return(value) {
                  try {
                    return await iterator.return(value);
                  } finally {
                    await close();
                  }
                },
                async throw(error) {
                  try {
                    return await iterator.throw(error);
                  } finally {
                    await close();
                  }
                },
                [Symbol.asyncIterator]() {
                  return this;
                },
              };
            };
          const value = Reflect.get(object, key, object);
          return typeof value === "function" ? value.bind(object) : value;
        },
      });
    },
    async close() {
      closed = true;
      try {
        const results = await Promise.allSettled(
          [...pending].map((close) => close()),
        );
        const failure = results.find((result) => result.status === "rejected");
        if (failure) throw failure.reason;
      } finally {
        await instance.close();
      }
    },
  };
}
