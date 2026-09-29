/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import * as fs from 'node:fs';
import * as path from 'node:path';
import toml from '@iarna/toml';
import { Storage } from './storage.js';

/**
 * cosh-ng keeps its configuration - including the active AI provider and its
 * credentials - in `~/.copilot-shell/config.toml`, while this generation reads
 * `settings.json` plus `aliyun_creds.json` from the same directory.
 * `cosh-switch` swaps the RPM without touching either, so a user who
 * authenticated in cosh-ng and switched back arrives with no auth type at all:
 * the header renders "Unknown" and the auth wizard opens on every launch
 * (issue #1938).
 *
 * This module is the single, provider-neutral reader for that file. It answers
 * one question - "which authentication capability did cosh-ng last use, and
 * with which credentials?" - and leaves the mapping onto this CLI's settings
 * and credential stores to its callers:
 *
 * - the CLI settings loader turns the result into `security.auth.*`
 * - `loadAliyunCredentials()` uses the AK/SK variant when no native
 *   `aliyun_creds.json` exists
 *
 * Everything here is read-only. Nothing is copied to disk, so there is no
 * second place a credential can go stale, no migration version to track and no
 * half-migrated state to roll back. The scope is deliberately "capabilities
 * both generations already share": provider types with no equivalent here are
 * refused rather than approximated.
 */
export const COSH_NG_CONFIG_FILE_NAME = 'config.toml';

/**
 * cosh-ng provider types that speak the OpenAI-compatible wire protocol. These
 * map onto a single OpenAI-compatible auth type here; cosh-ng itself only
 * varies request details per type (see `profile_from_name()` in
 * crates/cosh-core/src/provider/profile.rs).
 */
const OPENAI_COMPATIBLE_PROVIDER_TYPES: ReadonlySet<string> = new Set([
  'openai',
  'openai_compat',
  'dashscope',
  'deepseek',
  'generic',
]);

/** cosh-ng's Aliyun AK/SK provider type. */
const ALIYUN_PROVIDER_TYPE = 'aliyun';

/**
 * cosh-ng's ECS RAM role flow, which mints STS credentials from the instance
 * metadata service. This generation has no equivalent, so it is never
 * converted.
 */
const ECS_RAM_ROLE_AUTH_SOURCE = 'ecs_ram_role';

/**
 * cosh-ng's own default when a provider section omits `type`.
 * See `resolve_provider()` in crates/cosh-core/src/config.rs.
 */
const DEFAULT_PROVIDER_TYPE = 'generic';

/**
 * An authentication capability that both generations support, as recorded by
 * cosh-ng. `model` is optional: it is not a credential, and every caller has
 * its own default, so a provider that omits it is still usable.
 */
export type CoshNgAuth =
  | {
      kind: 'openai';
      apiKey: string;
      baseUrl: string;
      model?: string;
    }
  | {
      kind: 'aliyun';
      accessKeyId: string;
      accessKeySecret: string;
      model?: string;
    };

/** Absolute path of cosh-ng's config file inside the shared config directory. */
export function getCoshNgConfigPath(configDir?: string): string {
  return path.join(
    configDir ?? Storage.getGlobalQwenDir(),
    COSH_NG_CONFIG_FILE_NAME,
  );
}

/**
 * Emits a diagnostic about the config file.
 *
 * No value read out of `config.toml` is ever passed in - not credentials, not
 * provider ids or type names, not parser output, not surrounding lines. A
 * provider id looks like a harmless key, but it is arbitrary user input and a
 * pasted credential is made entirely of characters an id may contain, so there
 * is no sanitizer that makes echoing one safe. Messages therefore describe
 * "the active provider" and name only fields, using this module's own
 * constants.
 */
function warn(message: string): void {
  console.warn(`[cosh-ng compat] ${message}`);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function readString(value: unknown): string | undefined {
  if (typeof value !== 'string') {
    return undefined;
  }
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

/**
 * Substitutes `${VAR}` references the way cosh-ng does: an undefined variable
 * becomes the empty string rather than staying literal, and an unterminated
 * `${` is left alone. See `expand_env_vars()` in
 * crates/cosh-core/src/config.rs.
 *
 * Substitution is single-pass - a variable whose own value contains `${` is not
 * re-expanded - which avoids the unbounded rewrite loop the Rust
 * implementation's repeated scan would allow.
 */
function expandEnvVars(value: string): string {
  return value.replace(
    /\$\{([^}]*)\}/g,
    (_match, name: string) => process.env[name] ?? '',
  );
}

/**
 * Reads a field cosh-ng expands `${VAR}` in - credentials and base URLs.
 *
 * Expansion happens before the required-field check, so a reference to an
 * undefined variable reads as absent (cosh-ng would send an empty credential)
 * and the provider is refused rather than silently half-configured.
 */
function readExpandedString(value: unknown): string | undefined {
  if (typeof value !== 'string') {
    return undefined;
  }
  return readString(expandEnvVars(value));
}

/**
 * Reads cosh-ng's active provider from `config.toml`.
 *
 * Returns `undefined` - leaving the existing `/auth` flow untouched - when the
 * file is absent, unreadable, unparseable, names a provider that has no
 * section, uses a provider type with no equivalent here, relies on the ECS RAM
 * role flow, carries a temporary STS credential, or is missing a credential the
 * provider cannot work without (including one whose `${VAR}` resolves to
 * nothing).
 *
 * @param configDir Directory holding `config.toml`, defaulting to the shared
 *   `~/.copilot-shell`.
 */
export function loadCoshNgAuth(configDir?: string): CoshNgAuth | undefined {
  const configPath = getCoshNgConfigPath(configDir);

  let content: string;
  try {
    if (!fs.existsSync(configPath)) {
      return undefined;
    }
    content = fs.readFileSync(configPath, 'utf-8');
  } catch (_error) {
    // The error message can quote the path but never the contents; keep the
    // log to the path we already know.
    warn(
      `Could not read ${configPath}; run /auth to configure authentication.`,
    );
    return undefined;
  }

  let parsed: unknown;
  try {
    parsed = toml.parse(content);
  } catch (_error) {
    // TOML parse errors quote the offending line, which is very often the
    // `api_key = "..."` one. Never surface it.
    warn(
      `Ignoring malformed ${configPath}; run /auth to configure authentication.`,
    );
    return undefined;
  }

  const ai = isRecord(parsed) ? parsed['ai'] : undefined;
  if (!isRecord(ai)) {
    return undefined;
  }

  const activeProvider = readString(ai['active_provider']);
  if (!activeProvider) {
    // cosh-ng was never authenticated either; there is nothing to inherit.
    return undefined;
  }

  const providers = isRecord(ai['providers']) ? ai['providers'] : undefined;
  const provider = providers ? providers[activeProvider] : undefined;
  if (!isRecord(provider)) {
    warn(
      `The active provider has no matching [ai.providers] section in ` +
        `${configPath}; run /auth to configure authentication.`,
    );
    return undefined;
  }

  const providerType = readString(provider['type']) ?? DEFAULT_PROVIDER_TYPE;
  // cosh-ng does not expand ${VAR} in model names; keep that behaviour.
  const model = readString(ai['active_model']) ?? readString(provider['model']);

  if (OPENAI_COMPATIBLE_PROVIDER_TYPES.has(providerType)) {
    return readOpenAiAuth(provider, configPath, model);
  }
  if (providerType === ALIYUN_PROVIDER_TYPE) {
    return readAliyunAuth(provider, configPath, model);
  }

  warn(
    `The active provider uses an unsupported provider type, which has no ` +
      `equivalent auth method here; run /auth to configure authentication.`,
  );
  return undefined;
}

function readOpenAiAuth(
  provider: Record<string, unknown>,
  configPath: string,
  model: string | undefined,
): CoshNgAuth | undefined {
  const apiKey = readExpandedString(provider['api_key']);
  const baseUrl = readExpandedString(provider['base_url']);

  const missing: string[] = [];
  if (!apiKey) missing.push('api_key');
  if (!baseUrl) missing.push('base_url');
  if (!apiKey || !baseUrl) {
    warnMissing(configPath, missing);
    return undefined;
  }

  return { kind: 'openai', apiKey, baseUrl, model };
}

function readAliyunAuth(
  provider: Record<string, unknown>,
  configPath: string,
  model: string | undefined,
): CoshNgAuth | undefined {
  if (readString(provider['auth_source']) === ECS_RAM_ROLE_AUTH_SOURCE) {
    warn(
      `The active provider authenticates through the ECS RAM role, which has ` +
        `no equivalent here; run /auth to configure authentication.`,
    );
    return undefined;
  }

  // Temporary STS credentials are deliberately not inherited. This generation
  // stores an expiry alongside the token and refreshes expired ones through the
  // ECS RAM role - writing the result back to `aliyun_creds.json`. Inheriting a
  // token would therefore either fake an expiry or turn a read-only fallback
  // into a writer, so only long-lived AK/SK pairs cross the bridge.
  //
  // The token can arrive two ways: cosh-ng reads `security_token` from the
  // provider section, and falls back to $ALIBABA_CLOUD_SECURITY_TOKEN when the
  // field is absent. Both must refuse, or a temporary AK/SK would be inherited
  // as if it were long-lived and every request would go out without its token.
  // Presence alone disqualifies - an empty value included, since cosh-ng would
  // read that as a token too.
  if (
    provider['security_token'] !== undefined ||
    process.env['ALIBABA_CLOUD_SECURITY_TOKEN'] !== undefined
  ) {
    warn(
      `The active provider uses a temporary STS credential, which is not ` +
        `inherited; run /auth to configure authentication.`,
    );
    return undefined;
  }

  const accessKeyId = readExpandedString(provider['access_key_id']);
  const accessKeySecret = readExpandedString(provider['access_key_secret']);

  const missing: string[] = [];
  if (!accessKeyId) missing.push('access_key_id');
  if (!accessKeySecret) missing.push('access_key_secret');
  if (!accessKeyId || !accessKeySecret) {
    warnMissing(configPath, missing);
    return undefined;
  }

  return { kind: 'aliyun', accessKeyId, accessKeySecret, model };
}

/**
 * Reports absent fields by this module's own field names - never by value, and
 * without distinguishing "absent" from "resolved to nothing", since the latter
 * would disclose which `${VAR}` the user referenced.
 */
function warnMissing(configPath: string, missing: string[]): void {
  warn(
    `The active provider in ${configPath} is missing ` +
      `${missing.join(', ')}; run /auth to configure authentication.`,
  );
}
