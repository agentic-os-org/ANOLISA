/** Share the SDK Scope and its route checks between the main and sync transport threads. */
import { setTimeout as delay } from "node:timers/promises";
import { Instance, AwError } from "./client.mjs";
import {
  LanguageSource,
  SourceDenied,
  SourceResultWithheld,
  confirmSourceRoutes,
} from "./sources.mjs";

export async function openProfileConnection(
  { capabilities, connection, session, cooperative, waitForAttach },
  signal,
) {
  const instance = new Instance({
    ...connection,
    capabilities,
    install: (routes) => confirmSourceRoutes(capabilities, routes),
  });
  const source = new LanguageSource(instance, { ownsInstance: true });
  const wait = AbortSignal.any([signal, AbortSignal.timeout(session.timeoutMs)]);
  try {
    if (cooperative) {
      source.listen();
      if (waitForAttach) {
        while (instance.state !== "ready") {
          wait.throwIfAborted();
          if (instance.lastError && !instance.lastError.unavailable)
            throw instance.lastError;
          await delay(10, undefined, { signal: wait });
        }
      }
      return {
        source,
        async acquire(signal) {
          const active = source.session(session);
          try {
            await active.prepare(signal);
            return active;
          } catch (error) {
            await active.close();
            throw error;
          }
        },
      };
    }
    const active = source.session(session);
    while (instance.state !== "ready") {
      wait.throwIfAborted();
      await source.connect();
      if (instance.state === "ready" || instance.lastError?.unavailable)
        break;
      await delay(10, undefined, { signal: wait });
    }
    await active.prepare(signal);
    return { source, session: active };
  } catch (error) {
    try {
      await source.close();
    } catch (cause) {
      throw new AwError("source_cleanup_unconfirmed", { cause });
    }
    throw error;
  }
}

export function packProfileError(error) {
  return {
    code: typeof error?.code === "string" ? error.code : undefined,
    name: error?.name,
    message: error?.message,
    event: error?.event,
    sessionId: error?.sessionId,
    callId: error?.callId,
    turnId: error?.turnId,
    toolName: error?.toolName,
    toolExecuted: error?.toolExecuted,
    unavailable: error?.unavailable,
  };
}

export function unpackProfileError(value) {
  if (value.name === "SourceDenied")
    return new SourceDenied(value.event, value);
  if (value.name === "SourceResultWithheld")
    return new SourceResultWithheld(value.event, value);
  const error = value.code
    ? new AwError(value.code, {
        transport: value.unavailable,
        message: value.message,
      })
    : new Error(value.message);
  if (!value.code && value.name) error.name = value.name;
  if (value.toolExecuted) error.toolExecuted = true;
  return error;
}
