/** The synchronous JS surface blocks the application thread while AW RPC runs here. */
import { parentPort, workerData } from "node:worker_threads";
import {
  openProfileConnection,
  packProfileError,
} from "./profile-connection.mjs";

const startup = new AbortController();
const opening = openProfileConnection(workerData, startup.signal);
opening.catch(() => {});
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
      const { session } = await opening;
      if (operation === "prepare") {
        value = {
          sessionId: session.sessionId,
          routes: session.task?.routes,
          fallback: session.fallback,
        };
      } else {
        const options = { ...args.at(-1), signal: cancel.signal };
        value = await session[operation](...args.slice(0, -1), options);
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
