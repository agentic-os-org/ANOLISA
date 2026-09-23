/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { render, cleanup } from '@testing-library/react';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { ModelDialog } from './ModelDialog.js';
import { useKeypress } from '../hooks/useKeypress.js';
import { DescriptiveRadioButtonSelect } from './shared/DescriptiveRadioButtonSelect.js';
import { ConfigContext } from '../contexts/ConfigContext.js';
import { SettingsContext } from '../contexts/SettingsContext.js';
import type { Config } from '@copilot-shell/core';
import { AuthType } from '@copilot-shell/core';
import type { LoadedSettings } from '../../config/settings.js';
import { SettingScope } from '../../config/settings.js';
import {
  AVAILABLE_MODELS_QWEN,
  MAINLINE_CODER,
  MAINLINE_VLM,
} from '../models/availableModels.js';

vi.mock('../hooks/useKeypress.js', () => ({
  useKeypress: vi.fn(),
}));
const mockedUseKeypress = vi.mocked(useKeypress);

vi.mock('./shared/DescriptiveRadioButtonSelect.js', () => ({
  DescriptiveRadioButtonSelect: vi.fn(() => null),
}));
const mockedSelect = vi.mocked(DescriptiveRadioButtonSelect);

const renderComponent = (
  props: Partial<React.ComponentProps<typeof ModelDialog>> = {},
  contextValue: Partial<Config> | undefined = undefined,
) => {
  const defaultProps = {
    onClose: vi.fn(),
  };
  const combinedProps = { ...defaultProps, ...props };

  const mockSettings = {
    isTrusted: true,
    user: { settings: {} },
    workspace: { settings: {} },
    setValue: vi.fn(),
  } as unknown as LoadedSettings;

  const mockConfig = contextValue
    ? ({
        // --- Functions used by ModelDialog ---
        getModel: vi.fn(() => MAINLINE_CODER),
        setModel: vi.fn().mockResolvedValue(undefined),
        switchModel: vi.fn().mockResolvedValue(undefined),
        getAuthType: vi.fn(() => 'openai'),

        // --- Functions used by ClearcutLogger ---
        getUsageStatisticsEnabled: vi.fn(() => true),
        getSessionId: vi.fn(() => 'mock-session-id'),
        getDebugMode: vi.fn(() => false),
        getContentGeneratorConfig: vi.fn(() => ({
          authType: AuthType.USE_OPENAI,
          model: MAINLINE_CODER,
        })),
        getUseSmartEdit: vi.fn(() => false),
        getUseModelRouter: vi.fn(() => false),
        getProxy: vi.fn(() => undefined),

        // --- Spread test-specific overrides ---
        getAvailableModelsForAuthType: vi.fn((t: AuthType) => {
          if (t === AuthType.USE_OPENAI) {
            return AVAILABLE_MODELS_QWEN.map((m) => ({
              id: m.id,
              label: m.label,
              description: m.description,
            }));
          }
          return [];
        }),
        ...contextValue,
      } as unknown as Config)
    : undefined;

  const renderResult = render(
    <SettingsContext.Provider value={mockSettings}>
      <ConfigContext.Provider value={mockConfig}>
        <ModelDialog {...combinedProps} />
      </ConfigContext.Provider>
    </SettingsContext.Provider>,
  );

  return {
    ...renderResult,
    props: combinedProps,
    mockConfig,
    mockSettings,
  };
};

describe('<ModelDialog />', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    // Ensure env-based fallback models don't leak into this suite from the developer environment.
    delete process.env['OPENAI_MODEL'];
    delete process.env['ANTHROPIC_MODEL'];
  });

  afterEach(() => {
    cleanup();
  });

  it('renders the title and help text', () => {
    const { getByText } = renderComponent();
    expect(getByText('Select Model')).toBeDefined();
    expect(getByText('(Press Esc to close)')).toBeDefined();
  });

  it('passes all model options to DescriptiveRadioButtonSelect', () => {
    renderComponent({}, {});
    expect(mockedSelect).toHaveBeenCalledTimes(1);

    const props = mockedSelect.mock.calls[0][0];
    expect(props.items).toHaveLength(AVAILABLE_MODELS_QWEN.length);
    expect(props.items[0].value).toBe(
      `${AuthType.USE_OPENAI}::${MAINLINE_CODER}`,
    );
    expect(props.items[1].value).toBe(
      `${AuthType.USE_OPENAI}::${MAINLINE_VLM}`,
    );
    expect(props.showNumbers).toBe(true);
  });

  it('initializes with the model from ConfigContext', () => {
    const mockGetModel = vi.fn(() => MAINLINE_VLM);
    renderComponent({}, { getModel: mockGetModel });

    expect(mockGetModel).toHaveBeenCalled();
    expect(mockedSelect).toHaveBeenCalledWith(
      expect.objectContaining({
        initialIndex: 1,
      }),
      undefined,
    );
  });

  it('initializes with default coder model if context is not provided', () => {
    renderComponent({}, {});

    expect(mockedSelect).toHaveBeenCalledWith(
      expect.objectContaining({
        initialIndex: 0,
      }),
      undefined,
    );
  });

  it('initializes with default coder model if getModel returns undefined', () => {
    const mockGetModel = vi.fn(() => undefined);
    // @ts-expect-error This test validates component robustness when getModel
    // returns an unexpected undefined value.
    renderComponent({}, { getModel: mockGetModel });

    expect(mockGetModel).toHaveBeenCalled();

    // When getModel returns undefined, preferredModel falls back to MAINLINE_CODER
    // which has index 0, so initialIndex should be 0
    expect(mockedSelect).toHaveBeenCalledWith(
      expect.objectContaining({
        initialIndex: 0,
      }),
      undefined,
    );
    expect(mockedSelect).toHaveBeenCalledTimes(1);
  });

  it('calls config.switchModel and onClose when DescriptiveRadioButtonSelect.onSelect is triggered', async () => {
    const { props, mockConfig, mockSettings } = renderComponent({}, {}); // Pass empty object for contextValue

    const childOnSelect = mockedSelect.mock.calls[0][0].onSelect;
    expect(childOnSelect).toBeDefined();

    await childOnSelect(`${AuthType.USE_OPENAI}::${MAINLINE_CODER}`);

    expect(mockConfig?.switchModel).toHaveBeenCalledWith(
      AuthType.USE_OPENAI,
      MAINLINE_CODER,
      undefined,
      {
        reason: 'user_manual',
        context: 'Model switched via /model dialog',
      },
    );
    expect(mockSettings.setValue).toHaveBeenCalledWith(
      SettingScope.User,
      'model.name',
      MAINLINE_CODER,
    );
    expect(mockSettings.setValue).toHaveBeenCalledWith(
      SettingScope.User,
      'security.auth.selectedType',
      AuthType.USE_OPENAI,
    );
    expect(props.onClose).toHaveBeenCalledTimes(1);
  });

  it('calls config.switchModel and persists authType+model when selecting a different authType', async () => {
    const switchModel = vi.fn().mockResolvedValue(undefined);
    const getAuthType = vi.fn(() => AuthType.USE_GEMINI);
    const getAvailableModelsForAuthType = vi.fn((t: AuthType) => {
      if (t === AuthType.USE_GEMINI) {
        return [{ id: 'gemini-pro', label: 'Gemini Pro', authType: t }];
      }
      if (t === AuthType.USE_OPENAI) {
        return AVAILABLE_MODELS_QWEN.map((m) => ({
          id: m.id,
          label: m.label,
          authType: AuthType.USE_OPENAI,
        }));
      }
      return [];
    });

    const mockConfigWithSwitchAuthType = {
      getAuthType,
      getModel: vi.fn(() => 'gemini-pro'),
      getContentGeneratorConfig: vi.fn(() => ({
        authType: AuthType.USE_OPENAI,
        model: MAINLINE_CODER,
      })),
      // Add switchModel to the mock object (not the type)
      switchModel,
      getAvailableModelsForAuthType,
    };

    const { props, mockSettings } = renderComponent(
      {},
      // Cast to Config to bypass type checking, matching the runtime behavior
      mockConfigWithSwitchAuthType as unknown as Partial<Config>,
    );

    const childOnSelect = mockedSelect.mock.calls[0][0].onSelect;
    await childOnSelect(`${AuthType.USE_OPENAI}::${MAINLINE_CODER}`);

    expect(switchModel).toHaveBeenCalledWith(
      AuthType.USE_OPENAI,
      MAINLINE_CODER,
      undefined,
      {
        reason: 'user_manual',
        context: 'AuthType+model switched via /model dialog',
      },
    );
    expect(mockSettings.setValue).toHaveBeenCalledWith(
      SettingScope.User,
      'model.name',
      MAINLINE_CODER,
    );
    expect(mockSettings.setValue).toHaveBeenCalledWith(
      SettingScope.User,
      'security.auth.selectedType',
      AuthType.USE_OPENAI,
    );
    expect(props.onClose).toHaveBeenCalledTimes(1);
  });

  it('does not pass onHighlight to DescriptiveRadioButtonSelect', () => {
    renderComponent({}, {});

    const childOnHighlight = mockedSelect.mock.calls[0][0].onHighlight;
    expect(childOnHighlight).toBeUndefined();
  });

  it('calls onClose prop when "escape" key is pressed', () => {
    const { props } = renderComponent();

    expect(mockedUseKeypress).toHaveBeenCalled();

    const keyPressHandler = mockedUseKeypress.mock.calls[0][0];
    const options = mockedUseKeypress.mock.calls[0][1];

    expect(options).toEqual({ isActive: true });

    keyPressHandler({
      name: 'escape',
      ctrl: false,
      meta: false,
      shift: false,
      paste: false,
      sequence: '',
    });
    expect(props.onClose).toHaveBeenCalledTimes(1);

    keyPressHandler({
      name: 'a',
      ctrl: false,
      meta: false,
      shift: false,
      paste: false,
      sequence: '',
    });
    expect(props.onClose).toHaveBeenCalledTimes(1);
  });

  it('updates initialIndex when config context changes', () => {
    const mockGetModel = vi.fn(() => MAINLINE_CODER);
    const mockGetAuthType = vi.fn(() => 'openai');
    const mockGetAvailableModelsForAuthType = vi.fn((t: AuthType) => {
      if (t === AuthType.USE_OPENAI) {
        return AVAILABLE_MODELS_QWEN.map((m) => ({
          id: m.id,
          label: m.label,
          description: m.description,
          authType: AuthType.USE_OPENAI,
        }));
      }
      return [];
    });
    const mockSettings = {
      isTrusted: true,
      user: { settings: {} },
      workspace: { settings: {} },
      setValue: vi.fn(),
    } as unknown as LoadedSettings;
    const { rerender } = render(
      <SettingsContext.Provider value={mockSettings}>
        <ConfigContext.Provider
          value={
            {
              getModel: mockGetModel,
              getAuthType: mockGetAuthType,
              getAvailableModelsForAuthType: mockGetAvailableModelsForAuthType,
            } as unknown as Config
          }
        >
          <ModelDialog onClose={vi.fn()} />
        </ConfigContext.Provider>
      </SettingsContext.Provider>,
    );

    expect(mockedSelect.mock.calls[0][0].initialIndex).toBe(0);

    mockGetModel.mockReturnValue(MAINLINE_VLM);
    const newMockConfig = {
      getModel: mockGetModel,
      getAuthType: mockGetAuthType,
      getAvailableModelsForAuthType: mockGetAvailableModelsForAuthType,
    } as unknown as Config;

    rerender(
      <SettingsContext.Provider value={mockSettings}>
        <ConfigContext.Provider value={newMockConfig}>
          <ModelDialog onClose={vi.fn()} />
        </ConfigContext.Provider>
      </SettingsContext.Provider>,
    );

    // Should be called at least twice: initial render + re-render after context change
    expect(mockedSelect).toHaveBeenCalledTimes(2);
    expect(mockedSelect.mock.calls[1][0].initialIndex).toBe(1);
  });

  it('shows the /auth configured OpenAI-compatible model when registry is empty', () => {
    const mockSettings = {
      isTrusted: true,
      user: { settings: {} },
      workspace: { settings: {} },
      merged: {
        security: {
          auth: {
            openaiModel: 'qwen3-max',
          },
        },
      },
      setValue: vi.fn(),
    } as unknown as LoadedSettings;

    const mockConfig = {
      getModel: vi.fn(() => 'qwen3-max'),
      getAuthType: vi.fn(() => AuthType.USE_OPENAI),
      getAvailableModelsForAuthType: vi.fn(() => []),
      getContentGeneratorConfig: vi.fn(() => ({
        authType: AuthType.USE_OPENAI,
        model: 'qwen3-max',
      })),
      switchModel: vi.fn().mockResolvedValue(undefined),
    } as unknown as Config;

    render(
      <SettingsContext.Provider value={mockSettings}>
        <ConfigContext.Provider value={mockConfig}>
          <ModelDialog onClose={vi.fn()} />
        </ConfigContext.Provider>
      </SettingsContext.Provider>,
    );

    expect(mockedSelect).toHaveBeenCalledTimes(1);
    const items = mockedSelect.mock.calls[0][0].items;
    expect(items).toHaveLength(1);
    expect(items[0].value).toBe(`${AuthType.USE_OPENAI}::qwen3-max`);
    expect(items[0].description).toBe('Current /auth model');
  });

  it('combines registered models with the /auth configured OpenAI-compatible model', () => {
    const mockSettings = {
      isTrusted: true,
      user: { settings: {} },
      workspace: { settings: {} },
      merged: {
        security: {
          auth: {
            openaiModel: 'qwen3.6-plus',
            openaiModels: ['qwen3.6-plus', 'qwen3.5-plus'],
          },
        },
      },
      setValue: vi.fn(),
    } as unknown as LoadedSettings;

    const mockConfig = {
      getModel: vi.fn(() => 'qwen3.6-plus'),
      getAuthType: vi.fn(() => AuthType.USE_OPENAI),
      getAvailableModelsForAuthType: vi.fn((t: AuthType) => {
        if (t === AuthType.USE_OPENAI) {
          return [
            {
              id: 'qwen3-max',
              label: 'qwen3-max',
              authType: AuthType.USE_OPENAI,
            },
          ];
        }
        return [];
      }),
      getContentGeneratorConfig: vi.fn(() => ({
        authType: AuthType.USE_OPENAI,
        model: 'qwen3.6-plus',
      })),
      switchModel: vi.fn().mockResolvedValue(undefined),
    } as unknown as Config;

    render(
      <SettingsContext.Provider value={mockSettings}>
        <ConfigContext.Provider value={mockConfig}>
          <ModelDialog onClose={vi.fn()} />
        </ConfigContext.Provider>
      </SettingsContext.Provider>,
    );

    expect(mockedSelect).toHaveBeenCalledTimes(1);
    const items = mockedSelect.mock.calls[0][0].items;
    expect(items.map((item) => item.value)).toEqual([
      `${AuthType.USE_OPENAI}::qwen3-max`,
      `${AuthType.USE_OPENAI}::qwen3.6-plus`,
      `${AuthType.USE_OPENAI}::qwen3.5-plus`,
    ]);
    expect(items[1].description).toBe('Current /auth model');
    expect(items[2].description).toBe('Verified via /auth');
  });

  it('does not duplicate the /auth configured model when it is already registered', () => {
    const mockSettings = {
      isTrusted: true,
      user: { settings: {} },
      workspace: { settings: {} },
      merged: {
        security: {
          auth: {
            openaiModel: 'qwen3-max',
          },
        },
      },
      setValue: vi.fn(),
    } as unknown as LoadedSettings;

    const mockConfig = {
      getModel: vi.fn(() => 'qwen3-max'),
      getAuthType: vi.fn(() => AuthType.USE_OPENAI),
      getAvailableModelsForAuthType: vi.fn((t: AuthType) => {
        if (t === AuthType.USE_OPENAI) {
          return [
            {
              id: 'qwen3-max',
              label: 'qwen3-max',
              authType: AuthType.USE_OPENAI,
            },
          ];
        }
        return [];
      }),
      getContentGeneratorConfig: vi.fn(() => ({
        authType: AuthType.USE_OPENAI,
        model: 'qwen3-max',
      })),
      switchModel: vi.fn().mockResolvedValue(undefined),
    } as unknown as Config;

    render(
      <SettingsContext.Provider value={mockSettings}>
        <ConfigContext.Provider value={mockConfig}>
          <ModelDialog onClose={vi.fn()} />
        </ConfigContext.Provider>
      </SettingsContext.Provider>,
    );

    expect(mockedSelect).toHaveBeenCalledTimes(1);
    const items = mockedSelect.mock.calls[0][0].items;
    expect(items.map((item) => item.value)).toEqual([
      `${AuthType.USE_OPENAI}::qwen3-max`,
    ]);
  });

  it('does not overwrite the /auth configured model when selecting a registered model', async () => {
    const switchModel = vi.fn().mockResolvedValue(undefined);
    const mockSettings = {
      isTrusted: true,
      user: { settings: {} },
      workspace: { settings: {} },
      merged: {
        security: {
          auth: {
            openaiModel: 'qwen3.6-plus',
          },
        },
      },
      setValue: vi.fn(),
    } as unknown as LoadedSettings;

    const mockConfig = {
      getModel: vi.fn(() => 'qwen3-max'),
      getAuthType: vi.fn(() => AuthType.USE_OPENAI),
      getAvailableModelsForAuthType: vi.fn((t: AuthType) => {
        if (t === AuthType.USE_OPENAI) {
          return [
            {
              id: 'qwen3-max',
              label: 'qwen3-max',
              authType: AuthType.USE_OPENAI,
            },
          ];
        }
        return [];
      }),
      getContentGeneratorConfig: vi.fn(() => ({
        authType: AuthType.USE_OPENAI,
        model: 'qwen3-max',
      })),
      getUsageStatisticsEnabled: vi.fn(() => false),
      getSessionId: vi.fn(() => 'mock-session-id'),
      getDebugMode: vi.fn(() => false),
      getProxy: vi.fn(() => undefined),
      switchModel,
    } as unknown as Config;

    render(
      <SettingsContext.Provider value={mockSettings}>
        <ConfigContext.Provider value={mockConfig}>
          <ModelDialog onClose={vi.fn()} />
        </ConfigContext.Provider>
      </SettingsContext.Provider>,
    );

    const childOnSelect = mockedSelect.mock.calls[0][0].onSelect;
    await childOnSelect(`${AuthType.USE_OPENAI}::qwen3-max`);

    expect(mockSettings.setValue).not.toHaveBeenCalledWith(
      expect.anything(),
      'security.auth.openaiModel',
      expect.anything(),
    );
  });

  it('selecting an /auth configured model does not call startNewSession or resetChat', async () => {
    const startNewSession = vi.fn();
    const resetChat = vi.fn();
    const switchModel = vi.fn().mockResolvedValue(undefined);

    const mockSettings = {
      isTrusted: true,
      user: { settings: {} },
      workspace: { settings: {} },
      merged: {
        security: {
          auth: {
            openaiModel: 'qwen3.6-plus',
          },
        },
      },
      setValue: vi.fn(),
    } as unknown as LoadedSettings;

    const mockConfig = {
      getModel: vi.fn(() => 'qwen3.6-plus'),
      getAuthType: vi.fn(() => AuthType.USE_OPENAI),
      getAvailableModelsForAuthType: vi.fn(() => []),
      getContentGeneratorConfig: vi.fn(() => ({
        authType: AuthType.USE_OPENAI,
        model: 'qwen3.6-plus',
      })),
      getUsageStatisticsEnabled: vi.fn(() => false),
      getSessionId: vi.fn(() => 'mock-session-id'),
      getDebugMode: vi.fn(() => false),
      getProxy: vi.fn(() => undefined),
      switchModel,
      startNewSession,
      resetChat,
    } as unknown as Config;

    render(
      <SettingsContext.Provider value={mockSettings}>
        <ConfigContext.Provider value={mockConfig}>
          <ModelDialog onClose={vi.fn()} />
        </ConfigContext.Provider>
      </SettingsContext.Provider>,
    );

    const childOnSelect = mockedSelect.mock.calls[0][0].onSelect;
    await childOnSelect(`${AuthType.USE_OPENAI}::qwen3.6-plus`);

    expect(switchModel).toHaveBeenCalledWith(
      AuthType.USE_OPENAI,
      'qwen3.6-plus',
      undefined,
      expect.objectContaining({ reason: 'user_manual' }),
    );
    expect(startNewSession).not.toHaveBeenCalled();
    expect(resetChat).not.toHaveBeenCalled();
  });

  it('shows only the /auth configured model for non-registered OpenAI-compatible providers', () => {
    const mockSettings = {
      isTrusted: true,
      user: { settings: {} },
      workspace: { settings: {} },
      merged: {
        security: {
          auth: {
            openaiModel: 'gpt-4o',
            baseUrl: 'https://api.openai.com/v1',
          },
        },
      },
      setValue: vi.fn(),
    } as unknown as LoadedSettings;

    const mockConfig = {
      getModel: vi.fn(() => 'gpt-4o'),
      getAuthType: vi.fn(() => AuthType.USE_OPENAI),
      getAvailableModelsForAuthType: vi.fn(() => []),
      getContentGeneratorConfig: vi.fn(() => ({
        authType: AuthType.USE_OPENAI,
        model: 'gpt-4o',
        baseUrl: 'https://api.openai.com/v1',
      })),
      switchModel: vi.fn().mockResolvedValue(undefined),
    } as unknown as Config;

    render(
      <SettingsContext.Provider value={mockSettings}>
        <ConfigContext.Provider value={mockConfig}>
          <ModelDialog onClose={vi.fn()} />
        </ConfigContext.Provider>
      </SettingsContext.Provider>,
    );

    expect(mockedSelect).toHaveBeenCalledTimes(1);
    const items = mockedSelect.mock.calls[0][0].items;
    expect(items.length).toBe(1);
    expect(items[0].value).toBe(`${AuthType.USE_OPENAI}::gpt-4o`);
  });
});
