/** The synchronous JS surface blocks the application thread while AW RPC runs here. */
import { parentPort, workerData } from "node:worker_threads";
import {
  openProfileConnection,
  packProfileError,
} from "./profile-connection.mjs";

const startup = new AbortController();
const opening = openProfileConnection(workerData, startup.signal);
opening.catch(() => {});
const requests = new Map();
const status = (session) => session && {
  sessionId: session.sessionId,
  routes: session.task?.routes,
  fallback: session.fallback,
};
parentPort.on("message", async ({ operation, args, port, wait }) => {
  const cancel = new AbortController();
  port.on("message", () => cancel.abort());
  try {
    let value;
    if (operation === "close") {
      startup.abort();
      const binding = await opening.catch((error) => {
        if (error.code === "source_cleanup_unconfirmed") throw error;
      });
      await binding?.source.close(...args);
    } else {
      const { session, acquire } = await opening;
      if (operation === "prepare") {
        value = status(session);
      } else if (operation === "acquire") {
        const active = await acquire(cancel.signal);
        requests.set(args[0], active);
        value = status(active);
      } else if (operation === "release") {
        const active = requests.get(args[0]);
        requests.delete(args[0]);
        await active?.close();
      } else {
        const active = session ?? requests.get(args.shift());
        const options = { ...args.at(-1), signal: cancel.signal };
        value = await active[operation](...args.slice(0, -1), options);
      }
    }
    port.postMessage({ value });
  } catch (error) {
    port.postMessage({ error: packProfileError(error) });
  } finally {
    if (wait) {
      Atomics.store(wait, 0, 1);
      Atomics.notify(wait, 0);
    }
    port.close();
  }
});
