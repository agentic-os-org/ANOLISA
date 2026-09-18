/**
 * Configuration resolution for the agent-memory OpenClaw plugin.
 *
 * Reads plugin config, falls back to env vars, then to OS defaults.
 * Validation rules are kept in lock-step with the Rust crate
 * (`src/agent-memory/src/ns/mod.rs::validate_user_id`,
 * `src/agent-memory/src/config.rs`, including the `Profile::tool_visible`
 * gate the child enforces on the profile this module forwards) so that a
 * value accepted here is also one the subprocess can honor — failures in
 * the deep child are harder to diagnose than failures at plugin boot.
 */

import type { OpenClawPluginApi } from "openclaw/plugin-sdk/plugin-entry";
import { randomBytes } from "node:crypto";
import fs from "node:fs";
import path from "node:path";

/** Profiles this adapter can run the agent-memory child under.
 *
 *  `expert` is deliberately absent: the child honors it by hiding the Tier B
 *  tools this plugin's memory contract is made of (see `resolveProfile`).
 *  `openclaw.plugin.json`'s `configSchema.properties.profile.enum` must list
 *  exactly these values — `tests/unit/manifest-config-schema-test.ts` fails
 *  on drift in either direction. */
export const SUPPORTED_PROFILES = ["basic", "advanced"] as const;

export type AgentMemoryProfile = (typeof SUPPORTED_PROFILES)[number];

export type AgentMemoryConfig = {
  binaryPath: string;
  userId: string;
  profile: AgentMemoryProfile;
  maxReadBytes: number;
  maxWriteBytes: number;
  /** Session id pinned for this client's lifetime. Forwarded as
   *  `MEMORY_SESSION_ID` env on every spawn so respawns reuse the
   *  same session directory and `mem_promote` can find prior scratch. */
  sessionId: string;
  /** Base directory for per-session scratch + log. Forwarded as
   *  `MEMORY_SESSION_DIR` env. Defaults to `/run/anolisa/sessions`
   *  (the spec ships a tmpfiles.d snippet that creates it at 0700). */
  sessionDir: string;
};

const DEFAULT_PROFILE: AgentMemoryProfile = "advanced";
const DEFAULT_MAX_READ_BYTES = 1_048_576;
const DEFAULT_MAX_WRITE_BYTES = 16 * 1_048_576;

// Spec ships /usr/lib/tmpfiles.d/anolisa-memory.conf which creates this
// at 0700 at boot. tests/dev runs can override via plugin config or
// the same env var name.
const DEFAULT_SESSION_DIR = "/run/anolisa/sessions";

/** Generate a session id whose shape matches the Rust side's
 *  `SessionId::generate()` (prefix `ses_` + a Crockford-base32-ish
 *  unique tail). The exact alphabet doesn't matter — Rust validates
 *  it via `validate_user_id`, which accepts hex digits. */
function generateSessionId(): string {
  return `ses_${randomBytes(10).toString("hex")}`;
}

// Defence in depth for *_BYTES caps. agent-memory's own runtime caps
// are configured by these env vars, so the plugin enforces an outer
// bound: 4 GiB is well above any reasonable single-tool payload and
// keeps a runaway config from triggering OOM in the subprocess.
const MAX_BYTES_HARD_CAP = 4 * 1024 * 1024 * 1024; // 4 GiB

// Mirrors Rust `ns::mod.rs::validate_user_id` — must accept exactly
// the same set so that a config that passes here also passes inside
// the subprocess.
const USER_ID_MAX_LEN = 128;

export function validateUserId(value: string): string {
  if (value.length === 0) {
    throw new Error("userId must not be empty");
  }
  // The Rust limit is `str::len() > 128`, i.e. UTF-8 *bytes* (its error
  // text says so). Counting `value.length` instead — UTF-16 code units —
  // made this mirror accept everything the subprocess then rejects: 43 CJK
  // characters are 43 units but 129 bytes, and 33 emoji are 66 units but
  // 132 bytes. The rejection is also silent where it matters. A `userId`
  // that fails `validate_user_id` is only warned about and dropped
  // (`config.rs::read_validated_user_id_env` → the OS uid), and a
  // `sessionId` that fails it is replaced by a freshly generated one
  // (`service/mod.rs` → `SessionId::generate()`), so the operator keeps a
  // working plugin pinned to the wrong namespace and loses the
  // `mem_promote` continuity that pinning a session id exists to provide.
  // Failing here instead is the contract this module already states: a
  // configuration error the operator sees at plugin boot, not one that
  // resurfaces as different behaviour in the deep child.
  const byteLength = Buffer.byteLength(value, "utf8");
  if (byteLength > USER_ID_MAX_LEN) {
    throw new Error(`userId length ${byteLength} exceeds ${USER_ID_MAX_LEN} bytes`);
  }
  if (value.includes("/") || value.includes("\\")) {
    throw new Error(`userId '${value}' contains a path separator`);
  }
  if (value.includes("..")) {
    throw new Error(`userId '${value}' contains '..'`);
  }
  // Unicode control characters: matches Rust's `char::is_control()`
  // (C0: U+0000-001F, DEL: U+007F, C1: U+0080-009F).
  for (const ch of value) {
    const cp = ch.codePointAt(0)!;
    if (cp < 0x20 || cp === 0x7f || (cp >= 0x80 && cp <= 0x9f)) {
      throw new Error(
        `userId '${value}' contains control character (codepoint=${cp.toString(16)})`,
      );
    }
  }
  return value;
}

function normalizeTrimmedString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

/** Tier B tools the child hides under `Profile::Expert`
 *  (`src/agent-memory/src/config.rs::Profile::tool_visible`) that this plugin
 *  registers for the host's memory contract. Spelled out in the rejection so
 *  the operator can tie it to the `METHOD_NOT_FOUND` the child would otherwise
 *  answer with at call time. */
export const CONTRACT_TOOLS_HIDDEN_BY_EXPERT = [
  "memory_search",
  "memory_observe",
  "memory_get_context",
] as const;

/** Resolve `profile`, rejecting the one value the child accepts but this
 *  adapter cannot honor.
 *
 *  `expert` hides Tier B in the child at both `tools/list` and `tools/call`,
 *  and three of the four tools this plugin registers are Tier B — as are the
 *  two paths that call `memory_search` behind the agent's back, the
 *  auto-recall hook and the `corpus=all` supplement. Forwarding it therefore
 *  does not give the operator "file tools only": it loads a memory slot whose
 *  search, observe and get_context tools fail every call, whose auto-recall
 *  fails on every prompt, and whose corpus supplement silently answers
 *  nothing — the only hint is an error string inside each tool result. The
 *  adapter exposes exactly one Tier A tool (`memory_get` → `mem_read`) and
 *  cannot expose the rest without breaking the host's memory contract, so
 *  `expert` stays a setting for direct MCP clients that drive Tier A
 *  themselves. Refusing at boot is this module's standing contract for a
 *  value the subprocess would honor differently than the operator expects.
 *
 *  Unrecognized values keep falling back to `DEFAULT_PROFILE`: the manifest
 *  enum already turns a typo into a host-side config error, and this resolver
 *  stays permissive for hosts that do not validate. */
function resolveProfile(value: unknown): AgentMemoryProfile {
  const s = typeof value === "string" ? value.trim().toLowerCase() : "";
  if (s === "expert") {
    throw new Error(
      `profile 'expert' cannot run the OpenClaw adapter: the agent-memory child hides ` +
        `${CONTRACT_TOOLS_HIDDEN_BY_EXPERT.join(", ")} under that profile, and those are ` +
        `three of the four tools this plugin registers for the host's memory contract ` +
        `(auto-recall and the corpus=all supplement call memory_search too). Use ` +
        `'advanced' (the default) or 'basic'; 'expert' is for direct MCP clients that ` +
        `drive the Tier A file tools themselves.`,
    );
  }
  const supported: readonly string[] = SUPPORTED_PROFILES;
  if (supported.includes(s)) {
    return s as AgentMemoryProfile;
  }
  return DEFAULT_PROFILE;
}

export function normalizePositiveInt(
  value: unknown,
  fallback: number,
  cap: number = MAX_BYTES_HARD_CAP,
): number {
  let n: number | null = null;
  if (typeof value === "number" && Number.isFinite(value) && value > 0) {
    n = Math.floor(value);
  } else if (typeof value === "string") {
    const parsed = Number.parseInt(value, 10);
    if (Number.isFinite(parsed) && parsed > 0) {
      n = parsed;
    }
  }
  if (n === null) return fallback;
  if (n > cap) {
    // Loud fallback rather than silent truncation — a config writer
    // who asks for 1 PiB is almost certainly confused, and we don't
    // want the subprocess to inherit a nonsense env.
    console.error(
      `[agent-memory] requested byte cap ${n} exceeds plugin hard cap ${cap}; using ${fallback}`,
    );
    return fallback;
  }
  return n;
}

function knownBinaryLocations(): string[] {
  const homeDir = process.env.HOME || "";
  return [
    "/usr/bin/agent-memory", // RPM (system mode, PREFIX=/usr)
    "/usr/local/bin/agent-memory", // make install (default PREFIX=/usr/local)
    `${homeDir}/.local/bin/agent-memory`, // user mode (make install PREFIX=~/.local)
  ];
}

/** Search PATH directories for agent-memory without spawning a shell. */
function searchPathEnv(): string | undefined {
  const pathEnv = process.env.PATH || "";
  for (const dir of pathEnv.split(path.delimiter)) {
    if (!dir) continue;
    const candidate = path.join(dir, "agent-memory");
    if (fs.existsSync(candidate) && isExecutable(candidate)) {
      return candidate;
    }
  }
  return undefined;
}

/** Find the agent-memory binary on the system. */
function resolveBinaryPath(explicit?: string): string {
  if (explicit) {
    if (fs.existsSync(explicit) && isExecutable(explicit)) {
      return explicit;
    }
    throw new Error(
      `agent-memory binary not found or not executable at configured path: ${explicit}`,
    );
  }

  // Search PATH without child_process.
  const pathResult = searchPathEnv();
  if (pathResult) {
    return pathResult;
  }

  // Try known locations.
  for (const loc of knownBinaryLocations()) {
    if (fs.existsSync(loc) && isExecutable(loc)) {
      return loc;
    }
  }

  throw new Error(
    "agent-memory binary not found. Install it or set the binaryPath config option.",
  );
}

function isExecutable(filePath: string): boolean {
  try {
    fs.accessSync(filePath, fs.constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

/** Resolve the user ID for the namespace mount. */
function resolveUserId(explicit?: string): string {
  if (explicit && explicit.trim()) {
    return validateUserId(explicit.trim());
  }

  // Env var override.
  const envUserId = process.env["USER_ID"]?.trim();
  if (envUserId) {
    return validateUserId(envUserId);
  }

  // OS uid (unforgeable, matches agent-memory Rust logic).
  if (process.getuid) {
    return String(process.getuid());
  }

  // Fallback for non-Linux: use USER env var (less trustworthy but functional).
  const userEnv = process.env["USER"];
  if (userEnv) {
    return validateUserId(userEnv);
  }
  return "unknown";
}

/** Resolve the session id: explicit plugin config → env override →
 *  freshly generated one stable for this plugin's lifetime. */
function resolveSessionId(explicit?: string): string {
  if (explicit) return validateUserId(explicit);
  const envSid = process.env["MEMORY_SESSION_ID"]?.trim();
  if (envSid) return validateUserId(envSid);
  return generateSessionId();
}

/** Resolve the session base dir: explicit → env → default. */
function resolveSessionDir(explicit?: string): string {
  if (explicit) return explicit;
  const envDir = process.env["MEMORY_SESSION_DIR"]?.trim();
  if (envDir) return envDir;
  return DEFAULT_SESSION_DIR;
}

/** Resolve the full plugin config with defaults.
 *
 *  Operator-supplied identifiers are resolved (and therefore validated)
 *  before the environment is probed for the agent-memory binary. A
 *  malformed `userId` / `sessionId` is a configuration error, and this
 *  module's contract — see the header note about failures at plugin boot
 *  being easier to diagnose than failures in the deep child — is that the
 *  operator sees it as one. Resolving `binaryPath` first inverted that on
 *  any host without the binary installed: the throw from
 *  `resolveBinaryPath` ("agent-memory binary not found") masked the real
 *  validation error, so `userId: "a/b"` reported a missing binary instead
 *  of a path separator. Field order in the returned object is unchanged;
 *  only evaluation order moves. */
export function resolveConfig(api: OpenClawPluginApi): AgentMemoryConfig {
  const raw = (api.pluginConfig as Record<string, unknown>) ?? {};

  const userId = resolveUserId(normalizeTrimmedString(raw.userId));
  const sessionId = resolveSessionId(normalizeTrimmedString(raw.sessionId));
  const sessionDir = resolveSessionDir(normalizeTrimmedString(raw.sessionDir));
  const profile = resolveProfile(raw.profile);
  const maxReadBytes = normalizePositiveInt(raw.maxReadBytes, DEFAULT_MAX_READ_BYTES);
  const maxWriteBytes = normalizePositiveInt(raw.maxWriteBytes, DEFAULT_MAX_WRITE_BYTES);

  return {
    binaryPath: resolveBinaryPath(normalizeTrimmedString(raw.binaryPath)),
    userId,
    profile,
    maxReadBytes,
    maxWriteBytes,
    sessionId,
    sessionDir,
  };
}
