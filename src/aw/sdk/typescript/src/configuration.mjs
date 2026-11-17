/** Share AW's authoritative offline parser across optional framework bindings. */
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { isAbsolute, join } from "node:path";
import { AwError } from "./transport.mjs";

const execute = promisify(execFile);
export async function loadConfiguration({
  configFile,
  descriptorFile,
  profile,
  requiresService = false,
  target,
  executable = "aw",
  environment,
}) {
  if (typeof configFile !== "string" || !isAbsolute(configFile))
    throw new AwError("sdk_configuration_required");
  if (descriptorFile !== undefined) {
    const { loadDescriptor } = await import("./deployment.mjs");
    return loadDescriptor({
      configFile,
      descriptorFile,
      target,
      profile,
      requiresService,
    });
  }
  const { stdout } = await execute(
    executable,
    ["config", "show", "--config", configFile],
    { timeout: 5000, maxBuffer: 9 * 1024 * 1024, env: environment },
  );
  const document = JSON.parse(stdout).spec;
  const candidates = Object.entries(document.agents).filter(
    ([, agent]) => agent.adapter === profile.adapter,
  );
  target ??= candidates.length === 1 ? candidates[0][0] : undefined;
  if (!candidates.some(([name]) => name === target))
    throw new AwError("sdk_target_required");
  const daemon = document.daemon;
  const endpoint =
    daemon.endpoint !== "auto"
      ? daemon.endpoint
      : daemon.state_dir !== "auto"
        ? join(daemon.state_dir, "aw.sock")
        : undefined;
  if (!endpoint || !isAbsolute(endpoint))
    throw new AwError("sdk_explicit_service_path_required");
  return {
    target,
    endpoint,
    inputEnabled: document.events["input.submit"]?.enabled === true,
    availability:
      requiresService ||
      Object.values(document.events).some((event) => event.enabled && event.required)
        ? "required"
        : "native_fallback",
  };
}
