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
  const selection = target == null ? profile.adapter : `${profile.adapter}/${target}`;
  const args = [
    "config", "show", "--config", configFile, "--sdk-target", selection,
  ];
  const { stdout } = await execute(executable, args, {
    timeout: 5000,
    maxBuffer: 9 * 1024 * 1024,
    env: environment,
  });
  const document = JSON.parse(stdout);
  if (
    typeof document.target !== "string" || !document.target ||
    (target != null && target !== document.target)
  )
    throw new AwError("sdk_target_required");
  if (
    typeof document.input_enabled !== "boolean" ||
    typeof document.target_requires_service !== "boolean" ||
    (document.observe_session_start !== undefined &&
      typeof document.observe_session_start !== "boolean")
  )
    throw new AwError("sdk_configuration_invalid");
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
    target: document.target,
    endpoint,
    inputEnabled: document.input_enabled,
    ...(document.observe_session_start ? { sessionStartEnabled: true } : {}),
    availability:
      requiresService || document.target_requires_service
        ? "required"
        : "native_fallback",
  };
}
