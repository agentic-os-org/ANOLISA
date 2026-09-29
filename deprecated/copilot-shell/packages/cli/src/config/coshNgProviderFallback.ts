/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import { AuthType, loadCoshNgAuth } from '@copilot-shell/core';
import type { Settings } from './settingsSchema.js';

/**
 * Maps the authentication capability cosh-ng last used (see `loadCoshNgAuth()`
 * in core, which owns reading and validating `config.toml`) onto this CLI's
 * settings shape.
 *
 * The result is meant to be merged as the lowest-precedence settings layer, so
 * that `settings.json`, CLI flags and environment variables all still win. It
 * is never written back: for the OpenAI-compatible case the credential lives
 * only in the returned in-memory settings, and for the Aliyun case the AK/SK
 * is not copied here at all - `loadAliyunCredentials()` reads it from
 * `config.toml` on demand.
 *
 * Returns `undefined` when there is nothing usable to inherit, leaving the
 * existing `/auth` flow untouched.
 *
 * @param configDir Directory holding cosh-ng's `config.toml`
 *   (normally `~/.copilot-shell`).
 */
export function loadCoshNgProviderFallback(
  configDir?: string,
): Settings | undefined {
  const coshNgAuth = loadCoshNgAuth(configDir);
  if (!coshNgAuth) {
    return undefined;
  }

  // cosh-ng may not record a model; each auth type has its own default here, so
  // leave `model.name` alone rather than inventing one.
  const model = coshNgAuth.model;
  const modelSettings = model ? { model: { name: model } } : {};

  if (coshNgAuth.kind === 'openai') {
    return {
      security: {
        auth: {
          selectedType: AuthType.USE_OPENAI,
          apiKey: coshNgAuth.apiKey,
          baseUrl: coshNgAuth.baseUrl,
          ...(model ? { openaiModel: model } : {}),
        },
      },
      ...modelSettings,
    } as Settings;
  }

  return {
    security: {
      auth: {
        selectedType: AuthType.USE_ALIYUN,
        ...(model ? { aliyunModel: model } : {}),
      },
    },
    ...modelSettings,
  } as Settings;
}
