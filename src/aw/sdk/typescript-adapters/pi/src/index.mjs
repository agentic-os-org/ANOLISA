/** Application-owned Pi worker using the common SDK Instance, Task and configuration. */
import { realpath } from "node:fs/promises";
import { extname, isAbsolute } from "node:path";
import { AwError, Instance } from "../../../typescript/src/client.mjs";
import { loadConfiguration } from "../../../typescript/src/configuration.mjs";
import { TaskScope } from "../../../typescript/src/task-scope.mjs";
import {
  packages,
  loadWorker,
} from "../../../../crates/aw-adapter/native/pi/entrypoint.mjs";
import { runWorker } from "../../../../crates/aw-adapter/native/pi/resident.mjs";
import {
  piProfile,
  strictOutputRelease,
  workerBinding,
} from "../../../../crates/aw-adapter/native/pi/sdk.mjs";

/** Run an existing Pi worker factory and publish its cooperative attach listener. */
export async function runPiWorker({
  application,
  args = [],
  configFile,
  descriptorFile,
  target,
  executable = "aw",
  attachTimeoutMs = 5000,
  automatic = false,
}) {
  if (
    typeof application !== "string" ||
    !isAbsolute(application) ||
    extname(application) !== ".mjs"
  )
    throw new AwError("pi_application_required");
  if (
    !Number.isInteger(attachTimeoutMs) ||
    attachTimeoutMs < 100 ||
    attachTimeoutMs > 60000
  )
    throw new AwError("invalid_attach_timeout");
  if (typeof automatic !== "boolean")
    throw new AwError("invalid_automatic_option");

  application = await realpath(application);
  // Resolve wiring before importing the business factory and Pi packages.
  const configured = await loadConfiguration({
    configFile,
    descriptorFile,
    target,
    executable,
    profile: piProfile,
    requiresService: true,
  });
  const installed = packages(application);
  const strictOutput = strictOutputRelease(installed);
  const { worker, sdk, core } = await loadWorker(
    application,
    args,
    installed,
    strictOutput,
  );
  const binding = workerBinding({
    worker,
    inputEnabled: configured.inputEnabled,
    strictOutput,
  });
  const instance = new Instance({
    endpoint: configured.endpoint,
    target: configured.target,
    availability: configured.availability,
    capabilities: binding.capabilities,
    // Give Task release and event cleanup the same bound as the native bridge.
    timeoutMs: 6000,
    reconnectMs: 100,
    install: binding.install,
  });
  let workerError;
  let failed = false;
  try {
    await runWorker(worker, sdk, core, args, {
      startup: binding.startup,
      createScope: () => new TaskScope(instance, { timeoutMs: attachTimeoutMs }),
      ready: () => instance.listen({ automatic }),
    });
  } catch (error) {
    failed = true;
    workerError = error;
  } finally {
    try {
      await instance.close();
    } catch (closeError) {
      if (!failed) throw closeError;
      throw new AggregateError(
        [workerError, closeError],
        workerError?.message ?? "Pi worker and AW shutdown failed",
        { cause: workerError },
      );
    }
  }
  if (failed) throw workerError;
}
