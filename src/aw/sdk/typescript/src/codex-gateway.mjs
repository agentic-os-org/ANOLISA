/** Controller-side integration for a local Codex 0.166.0 Responses gateway. */
import { isAbsolute, join } from "node:path";

const reviewedEvents = ["user_prompt_submit", "pre_tool_use", "post_tool_use"];

/**
 * Render pre-reviewed hashes for the first AW handler in each new private home.
 * Pass the canonical home path (via `realpath` when `CODEX_HOME` is reached
 * through a symlink), because Codex looks up trust keys by the physical
 * `hooks.json` path.
 */
export function reviewedHookTrust(home, pins) {
  if (!isAbsolute(home)) throw new Error("Codex home must be absolute");
  return reviewedEvents.map(event => {
    const pin = pins[event];
    if (typeof pin !== "string" || !/^sha256:[a-f0-9]{64}$/.test(pin))
      throw new Error(`Missing reviewed Codex Hook hash for ${event}`);
    const key = JSON.stringify(`${join(home, "hooks.json")}:${event}:0:0`);
    return `\n[hooks.state.${key}]\ntrusted_hash = "${pin}"\n`;
  }).join("");
}

const reservedEnv = new Set([
  "HOME", "PATH", "SHELL", "USER", "LOGNAME", "PWD", "OLDPWD", "TMPDIR", "ENV",
  "IFS", "SHELLOPTS", "BASHOPTS", "LANG", "TERM", "SHLVL", "HTTP_PROXY",
  "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY",
]);
const reservedPrefix = /^(?:AW_|BASH_|CODEX_|DYLD_|GIT_|LC_|LD_|NODE_|PYTHON|SSH_|SSL_|XDG_)/;

/** Build the five native settings for the provider selected in the deployment. */
export function gatewayArgs(port, options) {
  if (!Number.isInteger(port) || port < 1 || port > 65535)
    throw new Error("Invalid Codex gateway port");
  const { provider, name, envKey } = options;
  if (typeof provider !== "string" || !/^[a-z][a-z0-9_-]{0,63}$/.test(provider))
    throw new Error("Invalid Codex gateway provider ID");
  if (typeof name !== "string" || !name.length || /[\p{Cc}\p{Cs}"']/u.test(name))
    throw new Error("Invalid Codex gateway provider name");
  if (typeof envKey !== "string" || !/^[A-Z][A-Z0-9_]{0,63}$/.test(envKey)
      || reservedEnv.has(envKey) || reservedPrefix.test(envKey))
    throw new Error("Invalid Codex gateway environment key");
  const prefix = `model_providers.${provider}`;
  return [
    `${prefix}.name=${JSON.stringify(name)}`,
    `${prefix}.base_url="http://127.0.0.1:${port}/v1"`,
    `${prefix}.env_key="${envKey}"`,
    `${prefix}.wire_api="responses"`,
    `${prefix}.supports_websockets=false`,
  ].flatMap(setting => ["-c", setting]);
}

const nonResults = new Set(["message", "reasoning", "function_call", "custom_tool_call"]);

/**
 * Call the gateway's downstream sender only after every ordinary tool result in
 * this request passed the application's live credential scrubber. Both protocol
 * conversion and retries must consume the supplied payload. Counts are safe to log.
 */
export function forwardCodexResponses(request, redactText, forward) {
  if (!Array.isArray(request?.input) || request.previous_response_id != null
      || request.conversation != null || typeof redactText !== "function")
    throw new Error("Codex credential gate requires explicit Responses input");

  let scanned = 0;
  let changed = 0;
  const input = request.input.map(item => {
    if (nonResults.has(item?.type)) return item;
    if (!["function_call_output", "custom_tool_call_output"].includes(item?.type)
        || typeof item.output !== "string"
        || Object.keys(item).some(key => !["type", "id", "call_id", "output", "status"].includes(key)))
      throw new Error("Unsupported Codex tool result at credential gate");
    let output;
    try {
      output = redactText(item.output);
    } catch {
      throw new Error("Codex credential scrubber unavailable");
    }
    if (typeof output !== "string") throw new Error("Invalid Codex credential scrubber reply");
    scanned++;
    if (output !== item.output) changed++;
    return { ...item, output };
  });
  return forward({ ...request, input }, { scanned, changed });
}
