/** Native configuration keeps SDK behavior; policy observes the public Hook boundary. */
const policyCallbacks = new WeakSet();

export function nativeConfiguration(config) {
  return config.resident === true && !config.legacy_command_consumer &&
    !config.native_policy?.declarative_plugins;
}

export function policyCallback(callback) {
  policyCallbacks.add(callback);
  return callback;
}

export function isPolicyCallback(callback) { return policyCallbacks.has(callback); }
