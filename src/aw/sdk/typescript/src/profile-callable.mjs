/** Callable guards and live forwarding for runtime-resolved profile exports. */
export const guards = new WeakMap();

// A bound guard still dispatches through the original receiver and arguments.
// Save the corresponding native binding for ordinary method assignment/restore.
export function bindGuard(receiver, ...args) {
  const fn = Function.prototype.bind.call(this, receiver, ...args);
  const guard = guards.get(this);
  if (!guard?.target) return fn;
  const previous = guard.binding;
  const original =
    previous?.original ?? (guard.live ? guard.live() : guard.native);
  const parameters = previous
    ? [...previous.parameters, ...args]
    : [receiver, ...args];
  const bound = callableProxy(fn, () => fn);
  guards.set(bound, {
    target: guard.target,
    native: Function.prototype.bind.call(
      previous ? guard.native : original,
      receiver,
      ...args,
    ),
    binding: { original, parameters },
  });
  return bound;
}

// A live export may change which keys exist. Its proxy must remain extensible,
// and descriptors on that proxy must allow replacement with the next export.
function liveProperties(current) {
  return {
    has: (_, key) => Reflect.has(current(), key),
    ownKeys: () => Reflect.ownKeys(current()),
    getPrototypeOf: () => Reflect.getPrototypeOf(current()),
    getOwnPropertyDescriptor(_, key) {
      const descriptor = Reflect.getOwnPropertyDescriptor(current(), key);
      return descriptor && { ...descriptor, configurable: true };
    },
    defineProperty(_, key, descriptor) {
      return (
        descriptor.configurable !== false &&
        Reflect.defineProperty(current(), key, descriptor)
      );
    },
    deleteProperty: (_, key) => Reflect.deleteProperty(current(), key),
    setPrototypeOf: (_, prototype) =>
      Reflect.setPrototypeOf(current(), prototype),
    preventExtensions: () => false,
  };
}

// [[Construct]] is set at proxy creation. Probe it without invoking SDK code
// or reading its prototype (which may itself be an accessor).
function callableShell(fn) {
  try {
    new new Proxy(fn, { construct: () => ({}) })();
    return function () {}.bind(null);
  } catch {
    return () => {};
  }
}

function callableProxy(
  proxyTarget,
  getCurrent,
  call = (receiver, args, newTarget) =>
    newTarget
      ? Reflect.construct(getCurrent(), args, newTarget)
      : Reflect.apply(getCurrent(), receiver, args),
) {
  const virtual = proxyTarget !== getCurrent();
  const methods = new WeakMap();
  const nativeReceiver = (receiver) =>
    receiver === forward ? getCurrent() : receiver;
  const methodHandler = {
    apply(fn, receiver, args) {
      return Reflect.apply(fn, nativeReceiver(receiver), args);
    },
  };
  const forward = new Proxy(proxyTarget, {
    ...(virtual ? liveProperties(getCurrent) : {}),
    get(_fn, property, receiver) {
      const value = Reflect.get(
        getCurrent(),
        property,
        nativeReceiver(receiver),
      );
      if (typeof value !== "function") return value;
      const own = Object.getOwnPropertyDescriptor(proxyTarget, property);
      if (own && !own.configurable && own.writable === false) return own.value;
      if (value === Function.prototype.bind && guards.get(receiver)?.target)
        return bindGuard;
      // Function invocation helpers must still enter this proxy's apply trap.
      if (
        value === Function.prototype.call ||
        value === Function.prototype.apply ||
        value === Function.prototype.bind
      )
        return value;
      // Ordinary method calls need the native class for private static fields.
      // An extracted method still takes its receiver from the caller.
      if (!methods.has(value))
        methods.set(value, new Proxy(value, methodHandler));
      return methods.get(value);
    },
    set(_fn, property, value, receiver) {
      return Reflect.set(
        getCurrent(),
        property,
        value,
        nativeReceiver(receiver),
      );
    },
    apply(_fn, receiver, args) {
      return call(receiver, args);
    },
    construct(_fn, args, newTarget) {
      return call(undefined, args, nativeReceiver(newTarget));
    },
  });
  return forward;
}

export function wrap(fn, target, invoke, live) {
  const getCurrent = live ?? (() => fn);
  // A proxy over the native function must return a locked own method verbatim.
  const virtual =
    live ||
    Reflect.ownKeys(fn).some((key) => {
      const property = Object.getOwnPropertyDescriptor(fn, key);
      return (
        property?.configurable === false &&
        property.writable === false &&
        typeof property.value === "function"
      );
    });
  const wrapped = callableProxy(
    virtual ? callableShell(fn) : fn,
    getCurrent,
    (receiver, args, newTarget) =>
      invoke(target, fn, receiver, args, getCurrent, newTarget),
  );
  guards.set(wrapped, { native: fn, target, live });
  return wrapped;
}

export function forwardExport(namespace, key) {
  // A restored live binding can later become a constructor.
  return callableProxy(function () {}.bind(null), () => namespace[key]);
}
