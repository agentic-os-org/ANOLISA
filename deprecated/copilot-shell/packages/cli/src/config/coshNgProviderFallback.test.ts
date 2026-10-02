/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, beforeEach, vi } from 'vitest';
import { AuthType, loadCoshNgAuth } from '@copilot-shell/core';
import { loadCoshNgProviderFallback } from './coshNgProviderFallback.js';

// Reading and validating config.toml lives in core and is covered by
// coshNgAuth.test.ts; here we only pin down the settings mapping.
vi.mock('@copilot-shell/core', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@copilot-shell/core')>()),
  loadCoshNgAuth: vi.fn(),
}));

describe('loadCoshNgProviderFallback', () => {
  beforeEach(() => {
    vi.mocked(loadCoshNgAuth).mockReset();
  });

  it('maps an OpenAI-compatible provider onto OpenAI auth settings', () => {
    vi.mocked(loadCoshNgAuth).mockReturnValue({
      kind: 'openai',
      apiKey: 'sk-from-cosh-ng',
      baseUrl: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
      model: 'qwen3-235b-a22b',
    });

    expect(loadCoshNgProviderFallback('/config/dir')).toEqual({
      security: {
        auth: {
          selectedType: AuthType.USE_OPENAI,
          apiKey: 'sk-from-cosh-ng',
          baseUrl: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
          openaiModel: 'qwen3-235b-a22b',
        },
      },
      model: { name: 'qwen3-235b-a22b' },
    });
    expect(loadCoshNgAuth).toHaveBeenCalledWith('/config/dir');
  });

  it('maps an Aliyun provider onto Aliyun auth without copying credentials', () => {
    vi.mocked(loadCoshNgAuth).mockReturnValue({
      kind: 'aliyun',
      accessKeyId: 'LTAI-test',
      accessKeySecret: 'secret-test',
      model: 'qwen3.7-plus',
    });

    const fallback = loadCoshNgProviderFallback();

    // The AK/SK stays in config.toml; loadAliyunCredentials() reads it there.
    expect(fallback).toEqual({
      security: {
        auth: {
          selectedType: AuthType.USE_ALIYUN,
          aliyunModel: 'qwen3.7-plus',
        },
      },
      model: { name: 'qwen3.7-plus' },
    });
    expect(JSON.stringify(fallback)).not.toContain('secret-test');
  });

  it('leaves the model alone when cosh-ng recorded none', () => {
    vi.mocked(loadCoshNgAuth).mockReturnValue({
      kind: 'openai',
      apiKey: 'sk-from-cosh-ng',
      baseUrl: 'https://example.com/v1',
    });

    expect(loadCoshNgProviderFallback()).toEqual({
      security: {
        auth: {
          selectedType: AuthType.USE_OPENAI,
          apiKey: 'sk-from-cosh-ng',
          baseUrl: 'https://example.com/v1',
        },
      },
    });
  });

  it('returns undefined when there is nothing to inherit', () => {
    vi.mocked(loadCoshNgAuth).mockReturnValue(undefined);

    expect(loadCoshNgProviderFallback()).toBeUndefined();
  });
});
