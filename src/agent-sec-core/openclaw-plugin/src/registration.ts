import type { SecurityCapability } from "./types.js";
import { envFlagEnabled } from "./utils.js";

const CAPABILITY_ENABLED_ENV: Record<string, string> = {
  "scan-code": "CODE_SCANNER_HOOK_ENABLED",
  "pii-scan-user-input": "PII_CHECKER_HOOK_ENABLED",
  "skill-ledger": "SKILL_LEDGER_HOOK_ENABLED",
};

export function isCapabilityEnabled(
  capability: SecurityCapability,
  config: Record<string, any>,
): boolean {
  const enabledEnv = CAPABILITY_ENABLED_ENV[capability.id];
  if (enabledEnv) {
    const rawEnabled = process.env[enabledEnv]?.trim().toLowerCase();
    if (rawEnabled === "true" || rawEnabled === "false") {
      return envFlagEnabled(enabledEnv, true);
    }
  }
  const capabilityConfig = config[capability.id] ?? {};
  if (typeof capabilityConfig.enabled === "boolean") {
    return capabilityConfig.enabled;
  }
  return true;
}
