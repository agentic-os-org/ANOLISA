/** Pi's last-mile declarations for application-owned SDK workers. */
import { readFileSync } from "node:fs";
import { validateRoutes } from "./bridge.mjs";

// A Hook contract identity, not a package version: deployment descriptors are
// compiled offline and the same profile covers every admitted Pi release.
export const piProfile = Object.freeze({
  adapter: "pi",
  version: "worker-hooks/v1",
  entrypoint: "sdk",
});

// Managed launch reads the same declaration, so both entry modes admit the
// same verified releases and extending acceptance needs no code change.
const { strict_output_releases: releases } = JSON.parse(
  readFileSync(new URL("./capabilities.json", import.meta.url), "utf8"),
);
const release = (text) => {
  const match =
    typeof text === "string" &&
    /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.exec(text);
  return match ? match.slice(1).map(Number) : null;
};
const compare = (left, right) =>
  left[0] - right[0] || left[1] - right[1] || left[2] - right[2];
const minimumRelease = release(releases?.minimum);
const maximumRelease = release(releases?.maximum);
if (!minimumRelease || !maximumRelease || compare(minimumRelease, maximumRelease) > 0)
  throw new Error("AW Pi capability declaration has invalid strict-output bounds");

/** Strict output needs a matching published SDK/Core pair inside the verified range. */
export function strictOutputRelease({ version, coreVersion }) {
  const installed = release(version);
  return (
    installed !== null &&
    version === coreVersion &&
    compare(installed, minimumRelease) >= 0 &&
    compare(installed, maximumRelease) <= 0
  );
}

/**
 * Bind a loaded worker to the capabilities its native seams actually provide.
 * Unsupported effects stay out of the ceiling, so Service admission rejects any
 * policy that needs them instead of the worker silently dropping a Hook.
 */
export function workerBinding({ worker, inputEnabled, strictOutput }) {
  const events = {
    "session.start": ["observe"],
    "turn.stop": ["observe"],
    "session.end": ["observe"],
    "tool.before": ["observe", "block", "replace_input"],
    "tool.after": ["observe", "replace_result"],
  };
  if (inputEnabled) {
    // Input capture wraps the runWorker factory; a run-only worker has no seam.
    if (typeof worker.run === "function")
      throw new Error("AW Pi input.submit requires the runWorker factory");
    events["input.submit"] = ["observe", "block"];
  }
  const capabilities = {
    ...piProfile,
    events,
    ...(strictOutput
      ? { failure_actions: { "tool.after": ["withhold_result"] } }
      : {}),
  };
  return {
    capabilities,
    startup: {
      resident_policy_reload: true,
      strict_output_supported: strictOutput,
      hooks: [],
    },
    install(routes) {
      validateRoutes({
        hooks: routes.hooks,
        replacement_step: routes.events?.["tool.after"]?.replacement_step,
      });
      return routes.hooks.every((hook) => Object.hasOwn(events, hook.event));
    },
  };
}
