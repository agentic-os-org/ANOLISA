/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi, afterEach } from 'vitest';
import { renderHook, act } from '@testing-library/react';
import type { Config } from '@copilot-shell/core';
import { AuthType } from '@copilot-shell/core';
import type { LoadedSettings } from '../../config/settings.js';
import { useAuthCommand } from './useAuth.js';
import {
  maybeRunKtunerFirstRunCheck,
  isKtunerAvailable,
  hasPromptedConsent,
  markConsentPrompted,
  hasNotifiedUnavailable,
  markNotifiedUnavailable,
} from '../../utils/ktunerFirstRun.js';

vi.mock('../hooks/useQwenAuth.js', () => ({
  useQwenAuth: () => ({
    qwenAuthState: undefined,
    cancelQwenAuth: vi.fn(),
  }),
}));

vi.mock('../../config/modelProvidersScope.js', () => ({
  getPersistScopeForModelSelection: () => 'user',
}));

// Mock the ktuner first-run helper so successful-auth tests never spawn a real
// external binary or write a global sentinel (kongche #2).
vi.mock('../../utils/ktunerFirstRun.js', () => ({
  maybeRunKtunerFirstRunCheck: vi.fn(),
  isKtunerAvailable: vi.fn(() => false),
  hasPromptedConsent: vi.fn(() => false),
  markConsentPrompted: vi.fn(),
  hasNotifiedUnavailable: vi.fn(() => false),
  markNotifiedUnavailable: vi.fn(),
}));

describe('useAuthCommand', () => {
  const createMockSettings = (): LoadedSettings =>
    ({
      merged: {
        security: {
          auth: {},
        },
        model: {},
      },
      setValue: vi.fn(),
      isTrusted: false,
      user: { settings: {} },
      workspace: { settings: {} },
    }) as unknown as LoadedSettings;

  const createMockConfig = (): Config =>
    ({
      getAuthType: vi.fn(() => undefined),
      getModelsConfig: vi.fn(() => ({})),
      refreshAuth: vi.fn(),
      getContentGenerator: vi.fn(() => undefined),
      getContentGeneratorConfig: vi.fn(() => undefined),
      updateCredentials: vi.fn(),
      getUsageStatisticsEnabled: vi.fn(() => false),
    }) as unknown as Config;

  it('restores bash option after canceling OpenAI auth when startup allows bash', async () => {
    const settings = createMockSettings();
    const config = createMockConfig();
    const addItem = vi.fn();

    const { result } = renderHook(() =>
      useAuthCommand(settings, config, addItem, true),
    );

    await act(async () => {
      await result.current.handleAuthSelect(AuthType.USE_OPENAI);
    });

    expect(result.current.showBashOptionInAuthDialog).toBe(false);
    expect(result.current.isAuthenticating).toBe(true);

    act(() => {
      result.current.cancelAuthentication();
    });

    expect(result.current.isAuthenticating).toBe(false);
    expect(result.current.isAuthDialogOpen).toBe(true);
    expect(result.current.showBashOptionInAuthDialog).toBe(true);
  });

  it('keeps bash option hidden after cancel when startup does not allow bash', async () => {
    const settings = createMockSettings();
    const config = createMockConfig();
    const addItem = vi.fn();

    const { result } = renderHook(() =>
      useAuthCommand(settings, config, addItem, false),
    );

    await act(async () => {
      await result.current.handleAuthSelect(AuthType.USE_OPENAI);
    });

    act(() => {
      result.current.cancelAuthentication();
    });

    expect(result.current.isAuthDialogOpen).toBe(true);
    expect(result.current.showBashOptionInAuthDialog).toBe(false);
  });

  it('should persist effective model to model.name and security.auth.openaiModel', async () => {
    const settings = createMockSettings();
    const config = createMockConfig();
    const addItem = vi.fn();
    vi.mocked(config.refreshAuth).mockResolvedValue(undefined);
    vi.mocked(config.getContentGeneratorConfig).mockReturnValue({
      model: 'my-model',
    } as ReturnType<Config['getContentGeneratorConfig']>);

    const { result } = renderHook(() =>
      useAuthCommand(settings, config, addItem, false),
    );

    // Step 1: set pendingAuthType (simulates user selecting OpenAI in AuthDialog)
    await act(async () => {
      await result.current.handleAuthSelect(AuthType.USE_OPENAI);
    });

    // Step 2: submit credentials (simulates OpenAIKeyPrompt submission)
    await act(async () => {
      await result.current.handleAuthSelect(AuthType.USE_OPENAI, {
        apiKey: 'sk-test',
        baseUrl: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
        model: 'my-model',
      });
    });

    const calls = vi.mocked(settings.setValue).mock.calls;
    const modelNameCall = calls.find(([, key]) => key === 'model.name');
    const openaiModelCall = calls.find(
      ([, key]) => key === 'security.auth.openaiModel',
    );
    const openaiModelsCall = calls.find(
      ([, key]) => key === 'security.auth.openaiModels',
    );
    expect(modelNameCall).toBeDefined();
    expect(modelNameCall![2]).toBe('my-model');
    expect(openaiModelCall).toBeDefined();
    expect(openaiModelCall![2]).toBe('my-model');
    expect(openaiModelsCall).toBeDefined();
    expect(openaiModelsCall![2]).toEqual(['my-model']);
  });

  it('should persist validated fallback model over submitted model', async () => {
    const settings = createMockSettings();
    settings.merged.security!.auth!.openaiModels = [
      'qwen3.5-plus',
      'qwen3-coder-plus',
    ];
    const config = createMockConfig();
    const addItem = vi.fn();
    vi.mocked(config.refreshAuth).mockResolvedValue(undefined);
    vi.mocked(config.getContentGeneratorConfig).mockReturnValue({
      model: 'qwen3-coder-plus',
    } as ReturnType<Config['getContentGeneratorConfig']>);

    const { result } = renderHook(() =>
      useAuthCommand(settings, config, addItem, false),
    );

    await act(async () => {
      await result.current.handleAuthSelect(AuthType.USE_OPENAI);
    });

    await act(async () => {
      await result.current.handleAuthSelect(AuthType.USE_OPENAI, {
        apiKey: 'sk-test',
        baseUrl: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
        model: 'my-model',
      });
    });

    const calls = vi.mocked(settings.setValue).mock.calls;
    const modelNameCall = calls.find(([, key]) => key === 'model.name');
    const openaiModelCall = calls.find(
      ([, key]) => key === 'security.auth.openaiModel',
    );
    const openaiModelsCall = calls.find(
      ([, key]) => key === 'security.auth.openaiModels',
    );
    expect(modelNameCall).toBeDefined();
    expect(modelNameCall![2]).toBe('qwen3-coder-plus');
    expect(openaiModelCall).toBeDefined();
    expect(openaiModelCall![2]).toBe('qwen3-coder-plus');
    expect(openaiModelsCall).toBeDefined();
    expect(openaiModelsCall![2]).toEqual(['qwen3-coder-plus', 'qwen3.5-plus']);
  });

  it('should set authError and keep isAuthenticating for OpenAI on failure', async () => {
    const settings = createMockSettings();
    const config = createMockConfig();
    const addItem = vi.fn();
    vi.mocked(config.refreshAuth).mockRejectedValue(
      new Error('Invalid API key'),
    );

    const { result } = renderHook(() =>
      useAuthCommand(settings, config, addItem, false),
    );

    // Step 1: set pendingAuthType
    await act(async () => {
      await result.current.handleAuthSelect(AuthType.USE_OPENAI);
    });

    // Step 2: submit credentials that will fail
    await act(async () => {
      await result.current.handleAuthSelect(AuthType.USE_OPENAI, {
        apiKey: 'sk-bad',
        baseUrl: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
        model: 'test-model',
      });
    });

    expect(result.current.authError).toBeTruthy();
    expect(result.current.isAuthenticating).toBe(true);

    const errorItem = addItem.mock.calls.find(([item]) => {
      const historyItem = item as Omit<import('../types.js').HistoryItem, 'id'>;
      return historyItem.type === 'error';
    });
    expect(errorItem).toBeDefined();
  });

  it('refreshes the static area exactly once after the initial login', async () => {
    const settings = createMockSettings();
    const config = createMockConfig();
    const addItem = vi.fn();
    const refreshStatic = vi.fn();
    vi.mocked(config.refreshAuth).mockResolvedValue(undefined);
    vi.mocked(config.getContentGeneratorConfig).mockReturnValue({
      model: 'my-model',
    } as ReturnType<Config['getContentGeneratorConfig']>);

    const { result } = renderHook(() =>
      useAuthCommand(settings, config, addItem, false, refreshStatic),
    );

    await act(async () => {
      await result.current.handleAuthSelect(AuthType.USE_OPENAI);
    });
    await act(async () => {
      await result.current.handleAuthSelect(AuthType.USE_OPENAI, {
        apiKey: 'sk-test',
        baseUrl: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
        model: 'my-model',
      });
    });

    expect(refreshStatic).toHaveBeenCalledTimes(1);

    // A subsequent re-authentication (e.g. user switches provider) must not
    // clear the screen again, otherwise the session scrollback would be lost.
    await act(async () => {
      await result.current.handleAuthSelect(AuthType.USE_OPENAI);
    });
    await act(async () => {
      await result.current.handleAuthSelect(AuthType.USE_OPENAI, {
        apiKey: 'sk-test-2',
        baseUrl: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
        model: 'my-model',
      });
    });

    expect(refreshStatic).toHaveBeenCalledTimes(1);
  });

  it('does not refresh the static area when the session was already authenticated', async () => {
    const settings = createMockSettings();
    const config = createMockConfig();
    vi.mocked(config.getAuthType).mockReturnValue(AuthType.USE_OPENAI);
    vi.mocked(config.refreshAuth).mockResolvedValue(undefined);
    vi.mocked(config.getContentGeneratorConfig).mockReturnValue({
      model: 'my-model',
    } as ReturnType<Config['getContentGeneratorConfig']>);
    const addItem = vi.fn();
    const refreshStatic = vi.fn();

    const { result } = renderHook(() =>
      useAuthCommand(settings, config, addItem, false, refreshStatic),
    );

    await act(async () => {
      await result.current.handleAuthSelect(AuthType.USE_OPENAI);
    });
    await act(async () => {
      await result.current.handleAuthSelect(AuthType.USE_OPENAI, {
        apiKey: 'sk-test',
        baseUrl: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
        model: 'my-model',
      });
    });

    expect(refreshStatic).not.toHaveBeenCalled();
  });

  it('does not refresh the static area when user chose Continue to Bash then later authenticates', async () => {
    const settings = createMockSettings();
    const config = createMockConfig();
    vi.mocked(config.refreshAuth).mockResolvedValue(undefined);
    vi.mocked(config.getContentGeneratorConfig).mockReturnValue({
      model: 'my-model',
    } as ReturnType<Config['getContentGeneratorConfig']>);
    const addItem = vi.fn();
    const refreshStatic = vi.fn();

    const { result } = renderHook(() =>
      useAuthCommand(settings, config, addItem, true, refreshStatic),
    );

    // User skips auth and continues to bash
    act(() => {
      result.current.handleContinueToBash();
    });

    // Later the user authenticates via /auth
    await act(async () => {
      await result.current.handleAuthSelect(AuthType.USE_OPENAI);
    });
    await act(async () => {
      await result.current.handleAuthSelect(AuthType.USE_OPENAI, {
        apiKey: 'sk-test',
        baseUrl: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
        model: 'my-model',
      });
    });

    expect(refreshStatic).not.toHaveBeenCalled();
  });

  // --- ktuner tri-state gate (general.ktunerCheck: ask | enabled | disabled) ---

  const authAndSucceed = async (
    settings: LoadedSettings,
    addItem: ReturnType<typeof vi.fn>,
  ) => {
    const config = createMockConfig();
    vi.mocked(config.refreshAuth).mockResolvedValue(undefined);
    vi.mocked(config.getContentGeneratorConfig).mockReturnValue({
      model: 'test',
    } as ReturnType<Config['getContentGeneratorConfig']>);
    const { result } = renderHook(() =>
      useAuthCommand(settings, config, addItem, false),
    );
    await act(async () => {
      await result.current.handleAuthSelect(AuthType.USE_OPENAI);
    });
    await act(async () => {
      await result.current.handleAuthSelect(AuthType.USE_OPENAI, {
        apiKey: 'sk-test',
        baseUrl: 'https://example.com/v1',
        model: 'test',
      });
    });
  };

  const setMode = (settings: LoadedSettings, mode?: string) => {
    (settings.merged as Record<string, unknown>)['general'] = mode
      ? { ktunerCheck: mode }
      : {};
  };
  const ktunerItemText = (addItem: ReturnType<typeof vi.fn>): string =>
    addItem.mock.calls
      .map(([item]) => (item as { text?: string }).text ?? '')
      .find((txt) => txt.includes('ktuner')) ?? '';

  let savedPlatform: PropertyDescriptor | undefined;
  const resetKtunerMocks = () => {
    vi.mocked(maybeRunKtunerFirstRunCheck).mockClear();
    vi.mocked(markConsentPrompted).mockClear();
    vi.mocked(markNotifiedUnavailable).mockClear();
    vi.mocked(isKtunerAvailable).mockReturnValue(false);
    vi.mocked(hasPromptedConsent).mockReturnValue(false);
    vi.mocked(hasNotifiedUnavailable).mockReturnValue(false);
    if (!savedPlatform) {
      savedPlatform = Object.getOwnPropertyDescriptor(process, 'platform');
    }
    Object.defineProperty(process, 'platform', {
      value: 'linux',
      configurable: true,
    });
  };
  afterEach(() => {
    if (savedPlatform) {
      Object.defineProperty(process, 'platform', savedPlatform);
      savedPlatform = undefined;
    }
  });

  it('disabled: neither runs the check nor shows a hint', async () => {
    resetKtunerMocks();
    vi.mocked(isKtunerAvailable).mockReturnValue(true);
    const settings = createMockSettings();
    setMode(settings, 'disabled');
    const addItem = vi.fn();
    await authAndSucceed(settings, addItem);
    expect(maybeRunKtunerFirstRunCheck).not.toHaveBeenCalled();
    expect(ktunerItemText(addItem)).toBe('');
  });

  it('enabled + available: runs the read-only check', async () => {
    resetKtunerMocks();
    vi.mocked(isKtunerAvailable).mockReturnValue(true);
    const settings = createMockSettings();
    setMode(settings, 'enabled');
    const addItem = vi.fn();
    await authAndSucceed(settings, addItem);
    expect(maybeRunKtunerFirstRunCheck).toHaveBeenCalledTimes(1);
    expect(markConsentPrompted).not.toHaveBeenCalled();
  });

  it('enabled + unavailable: surfaces a one-time notice (own marker)', async () => {
    resetKtunerMocks();
    vi.mocked(isKtunerAvailable).mockReturnValue(false);
    vi.mocked(hasNotifiedUnavailable).mockReturnValue(false);
    const settings = createMockSettings();
    setMode(settings, 'enabled');
    const addItem = vi.fn();
    await authAndSucceed(settings, addItem);
    expect(maybeRunKtunerFirstRunCheck).not.toHaveBeenCalled();
    expect(ktunerItemText(addItem)).toContain('no trusted ktuner binary');
    expect(markNotifiedUnavailable).toHaveBeenCalledTimes(1);
    expect(markConsentPrompted).not.toHaveBeenCalled();
  });

  it('ask + available + not prompted: shows the hint', async () => {
    resetKtunerMocks();
    vi.mocked(isKtunerAvailable).mockReturnValue(true);
    vi.mocked(hasPromptedConsent).mockReturnValue(false);
    const settings = createMockSettings();
    setMode(settings, undefined);
    const addItem = vi.fn();
    await authAndSucceed(settings, addItem);
    expect(maybeRunKtunerFirstRunCheck).not.toHaveBeenCalled();
    expect(ktunerItemText(addItem)).toContain('/ktuner enable');
    expect(markConsentPrompted).toHaveBeenCalledTimes(1);
  });

  it('ask + available + already prompted: stays silent', async () => {
    resetKtunerMocks();
    vi.mocked(isKtunerAvailable).mockReturnValue(true);
    vi.mocked(hasPromptedConsent).mockReturnValue(true);
    const settings = createMockSettings();
    setMode(settings, 'ask');
    const addItem = vi.fn();
    await authAndSucceed(settings, addItem);
    expect(maybeRunKtunerFirstRunCheck).not.toHaveBeenCalled();
    expect(ktunerItemText(addItem)).toBe('');
  });

  it('ask + unavailable: stays silent', async () => {
    resetKtunerMocks();
    vi.mocked(isKtunerAvailable).mockReturnValue(false);
    const settings = createMockSettings();
    setMode(settings, 'ask');
    const addItem = vi.fn();
    await authAndSucceed(settings, addItem);
    expect(maybeRunKtunerFirstRunCheck).not.toHaveBeenCalled();
    expect(ktunerItemText(addItem)).toBe('');
  });

  it('non-Linux: the whole ktuner gate is skipped', async () => {
    resetKtunerMocks();
    Object.defineProperty(process, 'platform', {
      value: 'darwin',
      configurable: true,
    });
    vi.mocked(isKtunerAvailable).mockReturnValue(true);
    const settings = createMockSettings();
    setMode(settings, 'enabled');
    const addItem = vi.fn();
    await authAndSucceed(settings, addItem);
    expect(maybeRunKtunerFirstRunCheck).not.toHaveBeenCalled();
    expect(ktunerItemText(addItem)).toBe('');
  });
});
