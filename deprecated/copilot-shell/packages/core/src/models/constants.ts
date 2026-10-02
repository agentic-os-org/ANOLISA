/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

type AuthType = import('../core/contentGenerator.js').AuthType;
type ContentGeneratorConfig =
  import('../core/contentGenerator.js').ContentGeneratorConfig;

/**
 * Field keys for model-scoped generation config.
 *
 * Kept in a small standalone module to avoid circular deps. The `import('...')`
 * usage is type-only and does not emit runtime imports.
 */
export const MODEL_GENERATION_CONFIG_FIELDS = [
  'samplingParams',
  'timeout',
  'maxRetries',
  'disableCacheControl',
  'schemaCompliance',
  'reasoning',
  'contextWindowSize',
  'customHeaders',
  'extra_body',
] as const satisfies ReadonlyArray<keyof ContentGeneratorConfig>;

/**
 * Credential-related fields that are part of ContentGeneratorConfig
 * but not ModelGenerationConfig.
 */
export const CREDENTIAL_FIELDS = [
  'model',
  'apiKey',
  'apiKeyEnvKey',
  'baseUrl',
] as const satisfies ReadonlyArray<keyof ContentGeneratorConfig>;

/**
 * All provider-sourced fields that need to be tracked for source attribution
 * and cleared when switching from provider to manual credentials.
 */
export const PROVIDER_SOURCED_FIELDS = [
  ...CREDENTIAL_FIELDS,
  ...MODEL_GENERATION_CONFIG_FIELDS,
] as const;

/**
 * Environment variable mappings per authType.
 */
export interface AuthEnvMapping {
  apiKey: string[];
  baseUrl: string[];
  model: string[];
}

export const AUTH_ENV_MAPPINGS = {
  openai: {
    apiKey: ['OPENAI_API_KEY'],
    baseUrl: ['OPENAI_BASE_URL'],
    model: ['OPENAI_MODEL', 'QWEN_MODEL'],
  },
  anthropic: {
    apiKey: ['ANTHROPIC_API_KEY'],
    baseUrl: ['ANTHROPIC_BASE_URL'],
    model: ['ANTHROPIC_MODEL'],
  },
  gemini: {
    apiKey: ['GEMINI_API_KEY'],
    baseUrl: [],
    model: ['GEMINI_MODEL'],
  },
  'vertex-ai': {
    apiKey: ['GOOGLE_API_KEY'],
    baseUrl: [],
    model: ['GOOGLE_MODEL'],
  },
  aliyun: {
    apiKey: [],
    baseUrl: [],
    model: [],
  },
} as const satisfies Record<AuthType, AuthEnvMapping>;

export const DEFAULT_MODELS = {
  openai: 'qwen3-coder-plus',
} as Partial<Record<AuthType, string>>;
