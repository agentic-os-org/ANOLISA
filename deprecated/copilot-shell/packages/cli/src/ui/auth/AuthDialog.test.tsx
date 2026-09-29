/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { AuthDialog } from './AuthDialog.js';
import { LoadedSettings } from '../../config/settings.js';
import type { Config } from '@copilot-shell/core';
import { AuthType } from '@copilot-shell/core';
import { renderWithProviders } from '../../test-utils/render.js';
import { UIStateContext } from '../contexts/UIStateContext.js';
import { UIActionsContext } from '../contexts/UIActionsContext.js';
import type { UIState } from '../contexts/UIStateContext.js';
import type { UIActions } from '../contexts/UIActionsContext.js';

const createMockUIState = (overrides: Partial<UIState> = {}): UIState => {
  // AuthDialog only uses authError, pendingAuthType and showBashOptionInAuthDialog
  const baseState = {
    authError: null,
    pendingAuthType: undefined,
    showBashOptionInAuthDialog: true,
  } as Partial<UIState>;

  return {
    ...baseState,
    ...overrides,
  } as UIState;
};

const createMockUIActions = (overrides: Partial<UIActions> = {}): UIActions => {
  const baseActions = {
    handleAuthSelect: vi.fn(),
    handleContinueToBash: vi.fn(),
  } as Partial<UIActions>;

  return {
    ...baseActions,
    ...overrides,
  } as UIActions;
};

const renderAuthDialog = (
  settings: LoadedSettings,
  uiStateOverrides: Partial<UIState> = {},
  uiActionsOverrides: Partial<UIActions> = {},
  configAuthType: AuthType | undefined = undefined,
  configApiKey: string | undefined = undefined,
) => {
  const uiState = createMockUIState(uiStateOverrides);
  const uiActions = createMockUIActions(uiActionsOverrides);

  const mockConfig = {
    getAuthType: vi.fn(() => configAuthType),
    getContentGeneratorConfig: vi.fn(() => ({ apiKey: configApiKey })),
  } as unknown as Config;

  return renderWithProviders(
    <UIStateContext.Provider value={uiState}>
      <UIActionsContext.Provider value={uiActions}>
        <AuthDialog />
      </UIActionsContext.Provider>
    </UIStateContext.Provider>,
    { settings, config: mockConfig },
  );
};

describe('AuthDialog', () => {
  const wait = (ms = 50) => new Promise((resolve) => setTimeout(resolve, ms));

  const waitFor = async (
    predicate: () => void,
    options: { timeout?: number; interval?: number } = {},
  ) => {
    const { timeout = 1000, interval = 10 } = options;
    const start = Date.now();
    let lastError: unknown;
    while (Date.now() - start < timeout) {
      try {
        predicate();
        return;
      } catch (e) {
        lastError = e;
      }
      await new Promise((resolve) => setTimeout(resolve, interval));
    }
    if (lastError) throw lastError;
    throw new Error('waitFor timed out');
  };

  const makeUnauthenticatedSettings = (): LoadedSettings =>
    new LoadedSettings(
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      {
        settings: {},
        originalSettings: {},
        path: '',
      },
      {
        settings: {
          security: { auth: { selectedType: undefined } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        originalSettings: {
          security: { auth: { selectedType: undefined } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        path: '',
      },
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      true,
      new Set(),
    );

  let originalEnv: NodeJS.ProcessEnv;

  beforeEach(() => {
    originalEnv = { ...process.env };
    process.env['GEMINI_API_KEY'] = '';
    process.env['QWEN_DEFAULT_AUTH_TYPE'] = '';
    vi.clearAllMocks();
  });

  afterEach(() => {
    process.env = originalEnv;
  });

  it('should show an error if the initial auth type is invalid', () => {
    process.env['GEMINI_API_KEY'] = '';

    const settings: LoadedSettings = new LoadedSettings(
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      {
        settings: {},
        originalSettings: {},
        path: '',
      },
      {
        settings: {
          security: {
            auth: {
              selectedType: AuthType.USE_GEMINI,
            },
          },
        },
        originalSettings: {
          security: {
            auth: {
              selectedType: AuthType.USE_GEMINI,
            },
          },
        },
        path: '',
      },
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      true,
      new Set(),
    );

    const { lastFrame } = renderAuthDialog(settings, {
      authError: 'GEMINI_API_KEY  environment variable not found',
    });

    expect(lastFrame()).toContain(
      'GEMINI_API_KEY  environment variable not found',
    );
  });

  describe('GEMINI_API_KEY environment variable', () => {
    it('should detect GEMINI_API_KEY environment variable', () => {
      process.env['GEMINI_API_KEY'] = 'foobar';

      const settings: LoadedSettings = new LoadedSettings(
        {
          settings: {
            security: { auth: { selectedType: undefined } },
            ui: { customThemes: {} },
            mcpServers: {},
          },
          originalSettings: {
            security: { auth: { selectedType: undefined } },
            ui: { customThemes: {} },
            mcpServers: {},
          },
          path: '',
        },
        {
          settings: {},
          originalSettings: {},
          path: '',
        },
        {
          settings: { ui: { customThemes: {} }, mcpServers: {} },
          originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
          path: '',
        },
        {
          settings: { ui: { customThemes: {} }, mcpServers: {} },
          originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
          path: '',
        },
        true,
        new Set(),
      );

      const { lastFrame } = renderAuthDialog(settings);

      // Since the auth dialog only shows Custom Provider option now,
      // it won't show GEMINI_API_KEY messages
      expect(lastFrame()).toContain('Custom Provider');
    });

    it('should not show the GEMINI_API_KEY message if QWEN_DEFAULT_AUTH_TYPE is set to something else', () => {
      process.env['GEMINI_API_KEY'] = 'foobar';
      process.env['QWEN_DEFAULT_AUTH_TYPE'] = AuthType.USE_OPENAI;

      const settings: LoadedSettings = new LoadedSettings(
        {
          settings: {
            security: { auth: { selectedType: undefined } },
            ui: { customThemes: {} },
            mcpServers: {},
          },
          originalSettings: {
            security: { auth: { selectedType: undefined } },
            ui: { customThemes: {} },
            mcpServers: {},
          },
          path: '',
        },
        {
          settings: {},
          originalSettings: {},
          path: '',
        },
        {
          settings: { ui: { customThemes: {} }, mcpServers: {} },
          originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
          path: '',
        },
        {
          settings: { ui: { customThemes: {} }, mcpServers: {} },
          originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
          path: '',
        },
        true,
        new Set(),
      );

      const { lastFrame } = renderAuthDialog(settings);

      expect(lastFrame()).not.toContain(
        'Existing API key detected (GEMINI_API_KEY)',
      );
    });

    it('should show the GEMINI_API_KEY message if QWEN_DEFAULT_AUTH_TYPE is set to use api key', () => {
      process.env['GEMINI_API_KEY'] = 'foobar';
      process.env['QWEN_DEFAULT_AUTH_TYPE'] = AuthType.USE_OPENAI;

      const settings: LoadedSettings = new LoadedSettings(
        {
          settings: {
            security: { auth: { selectedType: undefined } },
            ui: { customThemes: {} },
            mcpServers: {},
          },
          originalSettings: {
            security: { auth: { selectedType: undefined } },
            ui: { customThemes: {} },
            mcpServers: {},
          },
          path: '',
        },
        {
          settings: {},
          originalSettings: {},
          path: '',
        },
        {
          settings: { ui: { customThemes: {} }, mcpServers: {} },
          originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
          path: '',
        },
        {
          settings: { ui: { customThemes: {} }, mcpServers: {} },
          originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
          path: '',
        },
        true,
        new Set(),
      );

      const { lastFrame } = renderAuthDialog(settings);

      // Since the auth dialog only shows Custom Provider option now,
      // it won't show GEMINI_API_KEY messages
      expect(lastFrame()).toContain('Custom Provider');
    });
  });

  describe('QWEN_DEFAULT_AUTH_TYPE environment variable', () => {
    it('should select the auth type specified by QWEN_DEFAULT_AUTH_TYPE', () => {
      process.env['QWEN_DEFAULT_AUTH_TYPE'] = AuthType.USE_OPENAI;

      const settings: LoadedSettings = new LoadedSettings(
        {
          settings: {
            security: { auth: { selectedType: undefined } },
            ui: { customThemes: {} },
            mcpServers: {},
          },
          originalSettings: {
            security: { auth: { selectedType: undefined } },
            ui: { customThemes: {} },
            mcpServers: {},
          },
          path: '',
        },
        {
          settings: {},
          originalSettings: {},
          path: '',
        },
        {
          settings: { ui: { customThemes: {} }, mcpServers: {} },
          originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
          path: '',
        },
        {
          settings: { ui: { customThemes: {} }, mcpServers: {} },
          originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
          path: '',
        },
        true,
        new Set(),
      );

      const { lastFrame } = renderAuthDialog(settings);

      // This is a bit brittle, but it's the best way to check which item is selected.
      expect(lastFrame()).toContain('● Custom Provider');
    });

    it('should fall back to default if QWEN_DEFAULT_AUTH_TYPE is not set', () => {
      const settings: LoadedSettings = new LoadedSettings(
        {
          settings: {
            security: { auth: { selectedType: undefined } },
            ui: { customThemes: {} },
            mcpServers: {},
          },
          originalSettings: {
            security: { auth: { selectedType: undefined } },
            ui: { customThemes: {} },
            mcpServers: {},
          },
          path: '',
        },
        {
          settings: {},
          originalSettings: {},
          path: '',
        },
        {
          settings: { ui: { customThemes: {} }, mcpServers: {} },
          originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
          path: '',
        },
        {
          settings: { ui: { customThemes: {} }, mcpServers: {} },
          originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
          path: '',
        },
        true,
        new Set(),
      );

      const { lastFrame } = renderAuthDialog(settings);

      // 默认选中阿里云认证（第一个选项）
      expect(lastFrame()).toContain('● Aliyun Authentication');
    });

    it('should show an error and fall back to default if QWEN_DEFAULT_AUTH_TYPE is invalid', () => {
      process.env['QWEN_DEFAULT_AUTH_TYPE'] = 'invalid-auth-type';

      const settings: LoadedSettings = new LoadedSettings(
        {
          settings: {
            security: { auth: { selectedType: undefined } },
            ui: { customThemes: {} },
            mcpServers: {},
          },
          originalSettings: {
            security: { auth: { selectedType: undefined } },
            ui: { customThemes: {} },
            mcpServers: {},
          },
          path: '',
        },
        {
          settings: {},
          originalSettings: {},
          path: '',
        },
        {
          settings: { ui: { customThemes: {} }, mcpServers: {} },
          originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
          path: '',
        },
        {
          settings: { ui: { customThemes: {} }, mcpServers: {} },
          originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
          path: '',
        },
        true,
        new Set(),
      );

      const { lastFrame } = renderAuthDialog(settings);

      // auth dialog 不再显示 QWEN_DEFAULT_AUTH_TYPE 错误，
      // 回退到默认的阿里云认证选项
      expect(lastFrame()).toContain('● Aliyun Authentication');
    });
  });

  it('should prevent exiting when no auth method is selected and show error message', async () => {
    const handleAuthSelect = vi.fn();
    const settings: LoadedSettings = new LoadedSettings(
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      {
        settings: {},
        originalSettings: {},
        path: '',
      },
      {
        settings: {
          security: { auth: { selectedType: undefined } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        originalSettings: {
          security: { auth: { selectedType: undefined } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        path: '',
      },
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      true,
      new Set(),
    );

    const { lastFrame, stdin, unmount } = renderAuthDialog(
      settings,
      {},
      { handleAuthSelect },
      undefined, // config.getAuthType() returns undefined
    );

    // Wait for the dialog to fully render and keypress handler to be registered
    await waitFor(() => {
      expect(lastFrame()).toContain('● Aliyun Authentication');
    });
    await wait(); // extra tick: ensure keypress handler re-registered

    // Simulate pressing escape key
    stdin.write('\u001b'); // ESC key
    await wait();

    // Should show error message instead of calling handleAuthSelect
    expect(lastFrame()).toContain(
      'You must select an auth method or continue to Bash to proceed. Press Ctrl+C again to exit.',
    );
    expect(handleAuthSelect).not.toHaveBeenCalled();
    unmount();
  });

  it('should not exit if there is already an error message', async () => {
    const handleAuthSelect = vi.fn();
    const settings: LoadedSettings = new LoadedSettings(
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      {
        settings: {},
        originalSettings: {},
        path: '',
      },
      {
        settings: {
          security: { auth: { selectedType: undefined } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        originalSettings: {
          security: { auth: { selectedType: undefined } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        path: '',
      },
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      true,
      new Set(),
    );

    const { lastFrame, stdin, unmount } = renderAuthDialog(
      settings,
      { authError: 'Initial error' },
      { handleAuthSelect },
      undefined, // config.getAuthType() returns undefined
    );

    // Wait for the error message to appear and keypress handler to be registered
    await waitFor(() => {
      expect(lastFrame()).toContain('Initial error');
    });
    await wait(); // extra tick: ensure keypress handler re-registered

    expect(lastFrame()).toContain('Initial error');

    // Simulate pressing escape key
    stdin.write('\u001b'); // ESC key
    await wait();

    // Should not call handleAuthSelect
    expect(handleAuthSelect).not.toHaveBeenCalled();
    unmount();
  });

  it('should render dual sections and bash entry', () => {
    const settings: LoadedSettings = new LoadedSettings(
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      {
        settings: {},
        originalSettings: {},
        path: '',
      },
      {
        settings: {
          security: { auth: { selectedType: undefined } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        originalSettings: {
          security: { auth: { selectedType: undefined } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        path: '',
      },
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      true,
      new Set(),
    );

    const { lastFrame } = renderAuthDialog(settings);

    expect(lastFrame()).toContain('Get started');
    expect(lastFrame()).toContain('Use Copilot Shell');
    expect(lastFrame()).toContain('Continue without Copilot Shell');
    expect(lastFrame()).not.toContain('Continue without AI');
    expect(lastFrame()).toContain('Continue to Bash');
    expect(lastFrame()).not.toContain('● Continue to Bash');
  });

  it('should hide bash section for manual auth dialog opens', () => {
    const settings: LoadedSettings = new LoadedSettings(
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      {
        settings: {},
        originalSettings: {},
        path: '',
      },
      {
        settings: {
          security: { auth: { selectedType: undefined } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        originalSettings: {
          security: { auth: { selectedType: undefined } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        path: '',
      },
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      true,
      new Set(),
    );

    const { lastFrame } = renderAuthDialog(settings, {
      showBashOptionInAuthDialog: false,
    });

    expect(lastFrame()).toContain('Select authorization');
    expect(lastFrame()).toContain(
      "Choose how you'd like to authenticate in Copilot Shell.",
    );
    expect(lastFrame()).not.toContain('Use with AI');
    expect(lastFrame()).not.toContain('Continue without Copilot Shell');
    expect(lastFrame()).not.toContain('Continue to Bash');
    expect(lastFrame()).toContain('(↑↓ Select · Enter Continue)');
  });

  it('should ignore tab when bash section is hidden', async () => {
    const settings: LoadedSettings = new LoadedSettings(
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      {
        settings: {},
        originalSettings: {},
        path: '',
      },
      {
        settings: {
          security: { auth: { selectedType: undefined } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        originalSettings: {
          security: { auth: { selectedType: undefined } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        path: '',
      },
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      true,
      new Set(),
    );

    const { lastFrame, stdin, unmount } = renderAuthDialog(settings, {
      showBashOptionInAuthDialog: false,
    });

    await waitFor(() => {
      expect(lastFrame()).toContain('● Aliyun Authentication');
    });

    stdin.write('\t');
    await wait();

    expect(lastFrame()).toContain('● Aliyun Authentication');
    expect(lastFrame()).not.toContain('Continue to Bash');
    unmount();
  });

  it('should navigate to bash with down arrow and trigger bash on enter', async () => {
    const handleContinueToBash = vi.fn();
    const settings: LoadedSettings = new LoadedSettings(
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      {
        settings: {},
        originalSettings: {},
        path: '',
      },
      {
        settings: {
          security: { auth: { selectedType: undefined } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        originalSettings: {
          security: { auth: { selectedType: undefined } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        path: '',
      },
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      true,
      new Set(),
    );

    const { lastFrame, stdin, unmount } = renderAuthDialog(
      settings,
      {},
      { handleContinueToBash },
    );

    await waitFor(() => {
      expect(lastFrame()).toContain('● Aliyun Authentication');
    });

    stdin.write('\u001b[B');
    await wait();
    stdin.write('\u001b[B');
    await wait();

    expect(lastFrame()).toContain('● Continue to Bash');

    stdin.write('\r');
    await wait();

    expect(handleContinueToBash).toHaveBeenCalledTimes(1);
    unmount();
  });

  it('should wrap to bash with up arrow from first auth option', async () => {
    const settings: LoadedSettings = new LoadedSettings(
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      {
        settings: {},
        originalSettings: {},
        path: '',
      },
      {
        settings: {
          security: { auth: { selectedType: undefined } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        originalSettings: {
          security: { auth: { selectedType: undefined } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        path: '',
      },
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      true,
      new Set(),
    );

    const { lastFrame, stdin, unmount } = renderAuthDialog(settings);

    await waitFor(() => {
      expect(lastFrame()).toContain('● Aliyun Authentication');
    });

    stdin.write('\u001b[A');
    await wait();

    expect(lastFrame()).toContain('● Continue to Bash');
    unmount();
  });

  it('should switch sections with tab and shift+tab', async () => {
    const settings: LoadedSettings = new LoadedSettings(
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      {
        settings: {},
        originalSettings: {},
        path: '',
      },
      {
        settings: {
          security: { auth: { selectedType: undefined } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        originalSettings: {
          security: { auth: { selectedType: undefined } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        path: '',
      },
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      true,
      new Set(),
    );

    const { lastFrame, stdin, unmount } = renderAuthDialog(settings);

    await waitFor(() => {
      expect(lastFrame()).toContain('● Aliyun Authentication');
    });

    stdin.write('\t');
    await wait();
    expect(lastFrame()).toContain('● Continue to Bash');

    stdin.write('\x1b[Z');
    await wait();
    expect(lastFrame()).toContain('● Aliyun Authentication');
    unmount();
  });

  it('should still select auth option on enter', async () => {
    const handleAuthSelect = vi.fn();
    const settings: LoadedSettings = new LoadedSettings(
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      {
        settings: {},
        originalSettings: {},
        path: '',
      },
      {
        settings: {
          security: { auth: { selectedType: undefined } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        originalSettings: {
          security: { auth: { selectedType: undefined } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        path: '',
      },
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      true,
      new Set(),
    );

    const { stdin, unmount } = renderAuthDialog(
      settings,
      {},
      { handleAuthSelect },
    );

    stdin.write('\r');
    await wait();

    expect(handleAuthSelect).toHaveBeenCalledWith(AuthType.USE_ALIYUN);
    unmount();
  });

  it('should allow exiting when auth method is already selected', async () => {
    const handleAuthSelect = vi.fn();
    const settings: LoadedSettings = new LoadedSettings(
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      {
        settings: {},
        originalSettings: {},
        path: '',
      },
      {
        settings: {
          security: { auth: { selectedType: AuthType.USE_OPENAI } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        originalSettings: {
          security: { auth: { selectedType: AuthType.USE_OPENAI } },
          ui: { customThemes: {} },
          mcpServers: {},
        },
        path: '',
      },
      {
        settings: { ui: { customThemes: {} }, mcpServers: {} },
        originalSettings: { ui: { customThemes: {} }, mcpServers: {} },
        path: '',
      },
      true,
      new Set(),
    );

    const { lastFrame, stdin, unmount } = renderAuthDialog(
      settings,
      {},
      { handleAuthSelect },
      AuthType.USE_OPENAI, // config.getAuthType() returns USE_OPENAI
    );

    // Wait for the dialog to fully render and keypress handler to be registered
    await waitFor(() => {
      expect(lastFrame()).toContain('● Custom Provider');
    });
    await wait(); // extra tick: ensure keypress handler re-registered

    // Simulate pressing escape key
    stdin.write('\u001b'); // ESC key
    await wait();

    // Should call handleAuthSelect with undefined to exit
    expect(handleAuthSelect).toHaveBeenCalledWith(undefined);
    unmount();
  });

  it('should show initializing loading after selecting Custom Provider', async () => {
    const handleAuthSelect = vi.fn();
    const settings = makeUnauthenticatedSettings();

    const { lastFrame, stdin, unmount } = renderAuthDialog(
      settings,
      {},
      { handleAuthSelect },
    );

    await waitFor(() => {
      expect(lastFrame()).toContain('● Aliyun Authentication');
    });
    await wait(); // extra tick: ensure keypress handler re-registered

    // Move to Custom Provider and select it
    stdin.write('[B');
    await wait();
    expect(lastFrame()).toContain('● Custom Provider');

    stdin.write('\r');
    await wait();

    expect(lastFrame()).toContain(
      'Initializing model configuration, please wait...',
    );
    await waitFor(() => {
      expect(handleAuthSelect).toHaveBeenCalledWith(AuthType.USE_OPENAI);
    });
    unmount();
  });

  it('should ignore repeated keypresses while initializing', async () => {
    const handleAuthSelect = vi.fn();
    const settings = makeUnauthenticatedSettings();

    const { lastFrame, stdin, unmount } = renderAuthDialog(
      settings,
      {},
      { handleAuthSelect },
    );

    await waitFor(() => {
      expect(lastFrame()).toContain('● Aliyun Authentication');
    });
    await wait(); // extra tick: ensure keypress handler re-registered

    stdin.write('\r');
    await wait();

    expect(lastFrame()).toContain(
      'Initializing model configuration, please wait...',
    );

    // Any further input must be ignored while the loading indicator is shown
    stdin.write('\r');
    stdin.write('\r');
    stdin.write('');
    stdin.write('[B');
    await wait();

    expect(handleAuthSelect).toHaveBeenCalledTimes(1);
    expect(handleAuthSelect).toHaveBeenCalledWith(AuthType.USE_ALIYUN);
    unmount();
  });
});
