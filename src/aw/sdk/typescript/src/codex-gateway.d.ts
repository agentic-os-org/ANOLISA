/** Operator-reviewed native hashes from one installation of the three AW command Hooks. */
export type CodexHookPins = Record<"user_prompt_submit" | "pre_tool_use" | "post_tool_use", string>;

/**
 * Render native trust entries for the same definitions at group zero in a fresh home.
 * Pass the canonical physical home path (resolve symlinks with `realpath` first)
 * so the generated key matches Codex's physical `hooks.json` lookup.
 */
export function reviewedHookTrust(home: string, pins: CodexHookPins): string;

/** Append the five local Responses provider settings to the configured Codex target. */
export function gatewayArgs(
  port: number,
  options: { provider: string; name: string; envKey: string },
): string[];

/** Forward only requests with supported result shapes and completed credential checks. */
export function forwardCodexResponses<T>(
  request: Record<string, unknown>,
  redactText: (text: string) => string,
  forward: (request: Record<string, unknown>, counts: { scanned: number; changed: number }) => T,
): T;
