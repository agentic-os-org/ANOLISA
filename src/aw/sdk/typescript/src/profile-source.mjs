/** On-demand application-owned instrumentation over aw-hook-source/v1. */
import { AsyncLocalStorage } from "node:async_hooks";
import { addAbortListener } from "node:events";
import {
  MessageChannel,
  receiveMessageOnPort,
  Worker,
} from "node:worker_threads";
import { loadDescriptor } from "./deployment.mjs";
import { lazyAsyncIterator, runProfileCall } from "./profile-call.mjs";
import {
  intersectHooks,
  parseFieldPath,
  validateProfile,
} from "./profile-eval.mjs";
import { ProfilePatches } from "./profile-patch.mjs";
import {
  openProfileConnection,
  unpackProfileError,
} from "./profile-connection.mjs";
import { SOURCE_PROTOCOL, sourceCapabilities } from "./source-profile.mjs";
import { AwError } from "./transport.mjs";

export { functionCodeSha256 } from "./profile-patch.mjs";

const profileToken = Symbol("verified instrumentation profile");
const unavailable = (cause) => new AwError("source_unavailable", { cause });
const cleanupUnconfirmed = (cause) =>
  new AwError("source_cleanup_unconfirmed", { cause });

function wireCapabilities(hooks, targets) {
  const base = sourceCapabilities(Object.keys(hooks), {
    replaceInput: hooks["tool.before"]?.effects.includes("replace_input"),
    replaceResult: hooks["tool.after"]?.effects.includes("replace_result"),
    withholdResult:
      hooks["tool.after"]?.failure_actions.includes("withhold_result"),
  });
  for (const [event, hook] of Object.entries(hooks))
    base.events[event] = base.events[event].filter((effect) =>
      hook.effects.includes(effect),
    );

  // A receiver can contain native state, private fields, or shared identities.
  // Input replacement is advertised only when it can rebuild call arguments.
  if (
    targets.some(
      (target) =>
        target.hooks["tool.before"] &&
        parseFieldPath(target.fields.input.from).root !== "arg",
    )
  )
    base.events["tool.before"] = base.events["tool.before"].filter(
      (effect) => effect !== "replace_input",
    );
  return base;
}

function timeoutValue(value, minimum = 1) {
  if (!Number.isInteger(value) || value < minimum || value > 60000)
    throw new AwError("invalid_attach_timeout");
  return value;
}

function deadline(promise, timeoutMs) {
  let timer;
  const expired = new Promise((_, reject) => {
    timer = setTimeout(() => reject(cleanupUnconfirmed()), timeoutMs);
  });
  return Promise.race([promise, expired]).finally(() => clearTimeout(timer));
}

class WorkerConnection {
  constructor(options) {
    const env = { ...process.env };
    delete env.AW_PROFILE_BOOTSTRAP;
    delete env.NODE_OPTIONS;
    this.worker = new Worker(new URL("./profile-worker.mjs", import.meta.url), {
      workerData: options,
      execArgv: [],
      env,
    });
    this.pending = new Set();
    const fail = (cause) => {
      this.error ??= unavailable(cause);
      for (const reject of this.pending) reject(this.error);
    };
    this.worker.on("error", fail);
    this.worker.on("exit", () => fail());
    this.worker.unref();
  }

  request(operation, args, signal, timeoutMs) {
    if (this.error) throw this.error;
    signal?.throwIfAborted();
    const { port1, port2 } = new MessageChannel();
    const wait =
      timeoutMs === undefined
        ? undefined
        : new Int32Array(new SharedArrayBuffer(4));
    const unwrap = (reply) => {
      if (reply.error) throw unpackProfileError(reply.error);
      return reply.value;
    };
    try {
      this.worker.postMessage({ operation, args, port: port2, wait }, [port2]);
      if (wait) {
        if (Atomics.wait(wait, 0, 0, timeoutMs) === "timed-out") {
          port1.postMessage("cancel");
          throw unavailable();
        }
        const reply = receiveMessageOnPort(port1);
        if (!reply) throw unavailable();
        return unwrap(reply.message);
      }
    } catch (error) {
      port1.close();
      port2.close();
      throw error;
    } finally {
      if (wait) port1.close();
    }
    let rejectRequest;
    const pending = new Promise((resolve, reject) => {
      rejectRequest = reject;
      this.pending.add(reject);
      port1.once("message", resolve);
    });
    const cancellation =
      signal && addAbortListener(signal, () => port1.postMessage("cancel"));
    return pending
      .then(unwrap)
      .catch((error) => {
        if (signal?.aborted && error.name === "AbortError") throw signal.reason;
        throw error;
      })
      .finally(() => {
        this.pending.delete(rejectRequest);
        cancellation?.[Symbol.dispose]();
        port1.close();
      });
  }

  async close(options) {
    try {
      await this.request("close", [options]);
    } finally {
      await this.worker.terminate();
    }
  }
}

function needsWorker(targets) {
  return targets.some(({ call }) => call === "sync" || call === "sync_iter");
}

export class ProfileSource {
  #patches;
  #availability;
  #ledger = new Set();
  #cleanupFailure;
  #phase = "probed";
  #epoch = 0;
  #binding;
  #verification;
  #activation;
  #startup = new AbortController();
  #closing;
  #failure;

  constructor(token, profile, hooks, options, patches) {
    if (token !== profileToken)
      throw new AwError("source_profile_probe_required");
    this.profile = profile;
    this.hooks = hooks;
    this.options = Object.freeze({ ...options });
    this.#availability = options.availability ?? "required";
    this.packageVersion = patches.packageVersion;
    this.#patches = patches;
    Object.freeze(this);
  }

  static async probe(profile, options = {}) {
    const document = validateProfile(profile);
    if (
      document.targets.some(
        ({ hooks, fields }) => hooks["turn.stop"] && fields.output,
      )
    )
      throw new AwError("source_field_unavailable", {
        message: "turn.stop.output",
      });
    const hooks = intersectHooks(document.targets);
    wireCapabilities(hooks, document.targets);
    if (
      !["required", "native_fallback"].includes(
        options.availability ?? "required",
      )
    )
      throw new AwError("availability_required");
    const patches = new ProfilePatches(document, options);
    await patches.probe();
    return new ProfileSource(profileToken, document, hooks, options, patches);
  }

  get state() {
    return this.#failure && !["draining", "closed"].includes(this.#phase)
      ? "poisoned"
      : this.#phase;
  }
  get activation_epoch() {
    return this.#epoch;
  }

  install() {
    if (this.#phase !== "probed")
      throw new AwError("source_profile_invalid_state");
    this.#patches.install(
      (...args) => this.#invoke(...args),
      (error) => this.#lazyFailure(error),
    );
    this.#phase = "installed";
    return this;
  }

  #lazyFailure(error) {
    this.#failure ??= error;
    throw unavailable(this.#failure);
  }

  verify() {
    if (this.#phase !== "installed" || this.#verification)
      throw new AwError("source_profile_invalid_state");
    this.#verification = this.#verifyInstalled();
    return this.#verification;
  }

  async #verifyInstalled() {
    try {
      if (this.#failure) throw unavailable(this.#failure);
      await this.#patches.verify();
      if (this.#phase !== "installed" || this.#failure)
        throw unavailable(this.#failure);
      this.#phase = "verified";
      return this;
    } catch (error) {
      if (this.#phase === "installed") {
        this.#failure ??= error;
        this.#phase = "closed";
        this.#patches.restore();
      }
      throw error;
    }
  }

  activate(options = {}) {
    if (this.#phase !== "verified")
      throw new AwError("source_profile_invalid_state");
    this.#phase = "activating";
    this.#activation = this.#open(options)
      .then(() => {
        if (this.#phase !== "activating" || this.#failure)
          throw unavailable(this.#failure);
        this.#epoch++;
        this.#phase = "active";
        return this;
      })
      .catch((error) => {
        this.#failure ??= error;
        throw error;
      });
    return this.#activation;
  }

  async #open(options) {
    const availability = this.#availability;
    const capabilities = wireCapabilities(this.hooks, this.profile.targets);
    const {
      endpoint,
      target,
      configFile,
      descriptorFile,
      cooperative = false,
      timeoutMs = 5000,
      reconnectMs,
      cwd,
      environment,
      sessionId,
    } = options;
    timeoutValue(timeoutMs, 100);
    const location = descriptorFile
      ? await loadDescriptor({
          descriptorFile,
          configFile,
          target,
          profile: {
            adapter: "sdk",
            version: SOURCE_PROTOCOL,
            entrypoint: "application",
          },
          requiresService: availability === "required",
        })
      : { endpoint, target, availability };
    if (
      typeof location.endpoint !== "string" ||
      typeof location.target !== "string"
    )
      throw new AwError("source_profile_connection_required");
    this.#startup.signal.throwIfAborted();
    this.#availability = location.availability;
    const request = {
      capabilities,
      cooperative,
      connection: {
        endpoint: location.endpoint,
        target: location.target,
        availability: location.availability,
        cwd,
        environment,
        timeoutMs,
        ...(reconnectMs === undefined ? {} : { reconnectMs }),
      },
      session: { sessionId, timeoutMs, cwd, environment },
    };
    if (needsWorker(this.profile.targets)) {
      const worker = new WorkerConnection(request);
      this.#binding = { close: (options) => worker.close(options) };
      const status = await worker.request("prepare", [], this.#startup.signal);
      const budget = Math.max(
        0,
        ...(status.routes?.hooks ?? []).map((hook) => hook.budget_ms),
      );
      Object.assign(this.#binding, {
        ...status,
        dispatch: ([operation, ...args], { signal, sync }) =>
          worker.request(
            operation,
            args,
            signal,
            sync
              ? Math.min(
                  2 ** 31 - 1,
                  budget + Math.max(1000, timeoutMs) + timeoutMs,
                )
              : undefined,
          ),
      });
    } else {
      const { source, session } = await openProfileConnection(
        request,
        this.#startup.signal,
      );
      this.#binding = {
        sessionId: session.sessionId,
        fallback: session.fallback,
        routes: session.task?.routes,
        dispatch: ([operation, ...args], { signal }) => {
          const options = { ...args.at(-1), signal };
          return session[operation](...args.slice(0, -1), options);
        },
        close: (options) => source.close(options),
      };
    }
  }

  #invoke(target, original, receiver, args, getCurrent, newTarget) {
    const call = () =>
      this.#call(target, original, receiver, args, getCurrent, newTarget);
    if (
      target.call === "async_iter" &&
      this.#phase === "active" &&
      !this.#failure &&
      !this.#binding.fallback &&
      !newTarget
    ) {
      return lazyAsyncIterator(AsyncLocalStorage.bind(call));
    }
    return call();
  }

  #call(target, original, receiver, args, getCurrent, newTarget) {
    const ended = ["draining", "closed"].includes(this.#phase);
    if (this.#failure || (ended && this.#availability !== "native_fallback"))
      throw unavailable(this.#failure);
    const fn = getCurrent();
    const callNative = (callArgs) =>
      newTarget
        ? Reflect.construct(fn, callArgs, newTarget)
        : Reflect.apply(fn, receiver, callArgs);
    if (ended) return callNative(args);
    this.#patches.verifyCurrent(target, original, fn);
    if (!this.#epoch || this.#binding.fallback) return callNative(args);
    if (newTarget && target.call !== "sync")
      throw new AwError("source_verify_call_shape");
    const { promise, resolve } = Promise.withResolvers();
    this.#ledger.add(promise);
    const release = (cleanupError) => {
      if (cleanupError) this.#cleanupFailure ??= cleanupError;
      this.#ledger.delete(promise);
      resolve();
    };
    return runProfileCall({
      target,
      receiver,
      args,
      options: this.options,
      binding: this.#binding,
      callNative,
      release,
    });
  }

  async teardown(timeoutMs = 5000) {
    timeoutValue(timeoutMs);
    if (!this.#closing) {
      this.#phase = "draining";
      this.#startup.abort(unavailable());
      let restoration;
      try {
        this.#patches.restore();
      } catch (error) {
        restoration = error;
      }
      this.#closing = (async () => {
        try {
          await this.#verification?.catch(() => {});
          await this.#activation?.catch(() => {});
          await Promise.all(this.#ledger);
          await this.#binding?.close({ timeoutMs: 60000 });
          if (this.#cleanupFailure) throw this.#cleanupFailure;
          if (restoration) throw restoration;
        } finally {
          this.#phase = "closed";
        }
      })();
    }
    try {
      await deadline(this.#closing, timeoutMs);
    } catch (error) {
      if (error.code === "source_cleanup_unconfirmed") throw error;
      throw cleanupUnconfirmed(error);
    }
  }
}

export const probe = ProfileSource.probe;
