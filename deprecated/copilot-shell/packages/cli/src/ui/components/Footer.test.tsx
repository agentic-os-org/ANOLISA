/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

// Move all mocks to the top
import { render } from 'ink-testing-library';
import { Text } from 'ink';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { Footer } from './Footer.js';
import * as useTerminalSize from '../hooks/useTerminalSize.js';
import * as useStatusLineModule from '../hooks/useStatusLine.js';
import { type UIState, UIStateContext } from '../contexts/UIStateContext.js';
import { ConfigContext } from '../contexts/ConfigContext.js';
import { VimModeProvider } from '../contexts/VimModeContext.js';
import { SettingsContext } from '../contexts/SettingsContext.js'; // Import the correct context
import { CompactModeProvider } from '../contexts/CompactModeContext.js'; // 添加CompactModeProvider
import type { LoadedSettings } from '../../config/settings.js';

vi.mock('../hooks/useTerminalSize.js');
const useTerminalSizeMock = vi.mocked(useTerminalSize.useTerminalSize);

vi.mock('../hooks/useStatusLine.js');
const useStatusLineMock = vi.mocked(useStatusLineModule.useStatusLine);

// Mock all the sub-components that Footer uses
vi.mock('./AutoAcceptIndicator.js', () => ({
  AutoAcceptIndicator: () => <Text>AutoAcceptIndicator</Text>,
}));

vi.mock('./ShellModeIndicator.js', () => ({
  ShellModeIndicator: () => <Text>ShellModeIndicator</Text>,
}));

vi.mock('./ContextUsageDisplay.js', () => ({
  ContextUsageDisplay: ({
    promptTokenCount,
    terminalWidth,
    contextWindowSize,
  }: {
    promptTokenCount: number;
    terminalWidth: number;
    contextWindowSize: number;
  }) => {
    const percentage = ((promptTokenCount / contextWindowSize) * 100).toFixed(
      1,
    );
    const label = terminalWidth < 100 ? '% used' : '% context used';
    return (
      <Text color="gray">
        {percentage}
        {label}
      </Text>
    );
  },
}));

vi.mock('./ConsoleSummaryDisplay.js', () => ({
  ConsoleSummaryDisplay: ({ errorCount }: { errorCount: number }) =>
    (() => {
      if (errorCount > 0) {
        return <Text>{`Errors: ${errorCount}`}</Text>;
      }
      return <Text />;
    })(),
}));

// Mock i18n module
vi.mock('../../i18n/index.js', () => ({
  t: (key: string) => key,
}));

const defaultProps = {
  model: 'gemini-pro',
};

const createMockConfig = (overrides = {}) => ({
  getModel: vi.fn(() => defaultProps.model),
  getDebugMode: vi.fn(() => false),
  getContentGeneratorConfig: vi.fn(() => ({ contextWindowSize: 131072 })),
  getMcpServers: vi.fn(() => ({})),
  getBlockedMcpServers: vi.fn(() => []),
  getProjectRoot: vi.fn(() => '/test/project'),
  ...overrides,
});

const createMockUIState = (overrides: Partial<UIState> = {}): UIState =>
  ({
    sessionStats: {
      lastPromptTokenCount: 0, // Set to 0 to avoid context usage display
      sessionId: 'test-session',
      metrics: {
        models: {},
        tools: {
          totalCalls: 0,
          totalSuccess: 0,
          totalFail: 0,
          totalDurationMs: 0,
          totalDecisions: { accept: 0, reject: 0, modify: 0, auto_accept: 0 },
          byName: {},
        },
        files: { totalLinesAdded: 0, totalLinesRemoved: 0 },
      },
    },
    currentModel: 'gemini-pro',
    branchName: undefined,
    geminiMdFileCount: 0,
    contextFileNames: [],
    showToolDescriptions: false,
    ideContextState: undefined,
    errorCount: 0,
    showErrorDetails: false,
    showAutoAcceptIndicator: undefined,
    ctrlCPressedOnce: false,
    ctrlDPressedOnce: false,
    showEscapePrompt: false,
    shellModeActive: false,
    // Adding missing attributes
    filteredConsoleMessages: [],
    constrainHeight: true,
    availableTerminalHeight: 24,
    mainAreaWidth: 80,
    staticAreaMaxItemHeight: 10,
    staticExtraHeight: 0,
    dialogsVisible: false,
    pendingHistoryItems: [],
    nightly: false,
    terminalWidth: 80,
    terminalHeight: 24,
    mainControlsRef: { current: null },
    currentIDE: null,
    showIdeRestartPrompt: false,
    ideTrustRestartReason: 'folder-trust',
    isRestarting: false,
    extensionsUpdateState: new Map(),
    activePtyId: undefined,
    embeddedShellFocused: false,
    isVisionSwitchDialogOpen: false,
    showWelcomeBackDialog: false,
    welcomeBackInfo: null,
    welcomeBackChoice: null,
    isSubagentCreateDialogOpen: false,
    isAgentsManagerDialogOpen: false,
    isFeedbackDialogOpen: false,
    isThemeDialogOpen: false,
    isAuthenticating: false,
    isConfigInitialized: true,
    authError: null,
    isAuthDialogOpen: false,
    showBashOptionInAuthDialog: false,
    pendingAuthType: undefined,
    editorError: null,
    isEditorDialogOpen: false,
    debugMessage: '',
    quittingMessages: null,
    isSettingsDialogOpen: false,
    isModelDialogOpen: false,
    isPermissionsDialogOpen: false,
    isApprovalModeDialogOpen: false,
    isResumeDialogOpen: false,
    slashCommands: [],
    pendingSlashCommandHistoryItems: [],
    commandContext: { cwd: '/test' },
    shellConfirmationRequest: null,
    confirmationRequest: null,
    confirmUpdateExtensionRequests: [],
    settingInputRequests: [],
    pluginChoiceRequests: [],
    loopDetectionConfirmationRequest: null,
    userPromptConfirmationRequest: null,
    sandboxBypassRequest: null,
    streamingState: 'idle',
    initError: null,
    pendingGeminiHistoryItems: [],
    thought: null,
    userMessages: [],
    buffer: { text: '' },
    inputWidth: 80,
    suggestionsWidth: 20,
    isInputActive: false,
    shouldShowIdePrompt: false,
    shouldShowCommandMigrationNudge: false,
    commandMigrationTomlFiles: [],
    isFolderTrustDialogOpen: false,
    isTrustedFolder: undefined,
    elapsedTime: 0,
    currentLoadingPhrase: '',
    historyRemountKey: 0,
    messageQueue: [],
    currentIDEInfo: null,
    updateInfo: null,
    history: [],
    historyManager: {
      history: [],
      currentIndex: 0,
      setCurrentIndex: vi.fn(),
      goToNext: vi.fn(),
      goToPrevious: vi.fn(),
      getCurrentItem: vi.fn(),
    },
    ...overrides,
  }) as UIState;

const createMockSettings = (): LoadedSettings =>
  ({
    merged: {
      general: {
        vimMode: false,
      },
    },
    raw: {},
    defaults: {},
    schema: {},
    errors: [],
  }) as LoadedSettings;

const renderWithWidth = (width: number, uiState: UIState) => {
  useTerminalSizeMock.mockReturnValue({ columns: width, rows: 24 });
  const mockSettings = createMockSettings();
  return render(
    <SettingsContext.Provider value={mockSettings}>
      <ConfigContext.Provider value={createMockConfig() as never}>
        <VimModeProvider settings={mockSettings}>
          <CompactModeProvider
            value={{
              compactMode: false,
              setCompactMode: vi.fn(),
              frozenSnapshot: null,
              setFrozenSnapshot: vi.fn(),
            }}
          >
            <UIStateContext.Provider value={uiState}>
              <Footer />
            </UIStateContext.Provider>
          </CompactModeProvider>
        </VimModeProvider>
      </ConfigContext.Provider>
    </SettingsContext.Provider>,
  );
};

describe('<Footer />', () => {
  beforeEach(() => {
    // Reset all mocks before each test
    vi.clearAllMocks();
    // Mock status line to return empty array
    useStatusLineMock.mockReturnValue({ lines: [] });
  });

  afterEach(() => {
    vi.resetModules(); // This ensures the module is reloaded with original mock
  });

  it('renders the component', () => {
    const { lastFrame } = renderWithWidth(120, createMockUIState());
    expect(lastFrame()).toBeDefined();
  });

  it('does not display the working directory or branch name', () => {
    const { lastFrame } = renderWithWidth(120, createMockUIState());
    expect(lastFrame()).not.toMatch(/\(.*\*\)/);
  });

  it('displays the context percentage', () => {
    const { lastFrame } = renderWithWidth(
      120,
      createMockUIState({
        sessionStats: {
          lastPromptTokenCount: 131, // 小数值，产生0.1%
          sessionId: 'test-session',
          metrics: {
            models: {},
            tools: {
              totalCalls: 0,
              totalSuccess: 0,
              totalFail: 0,
              totalDurationMs: 0,
              totalDecisions: {
                accept: 0,
                reject: 0,
                modify: 0,
                auto_accept: 0,
              },
            },
            files: { totalLinesAdded: 0, totalLinesRemoved: 0 },
          },
        },
      }),
    );
    expect(lastFrame()).toMatch(/0\.1% context used/);
  });

  it('displays the abbreviated context percentage on narrow terminal', () => {
    const { lastFrame } = renderWithWidth(
      99,
      createMockUIState({
        sessionStats: {
          lastPromptTokenCount: 131, // 小数值，产生0.1%
          sessionId: 'test-session',
          metrics: {
            models: {},
            tools: {
              totalCalls: 0,
              totalSuccess: 0,
              totalFail: 0,
              totalDurationMs: 0,
              totalDecisions: {
                accept: 0,
                reject: 0,
                modify: 0,
                auto_accept: 0,
              },
            },
            files: { totalLinesAdded: 0, totalLinesRemoved: 0 },
          },
        },
      }),
    );
    expect(lastFrame()).toMatch(/0\.1%/);
  });

  it('shows esc interrupt hint during tool confirmation', () => {
    useStatusLineMock.mockReturnValue({ lines: null });
    const uiState = createMockUIState({
      streamingState: 'waiting_for_confirmation',
    });
    const { lastFrame } = renderWithWidth(200, uiState);
    expect(lastFrame()).toContain('Ctrl+C to exit');
    expect(lastFrame()).toContain('/bash for shell');
    expect(lastFrame()).toContain('? for shortcuts');
    expect(lastFrame()).toContain('Press Esc');
    expect(lastFrame()).toContain('interrupt');
  });

  it('does not show esc interrupt hint when idle', () => {
    useStatusLineMock.mockReturnValue({ lines: null });
    const uiState = createMockUIState({
      streamingState: 'idle',
    });
    const { lastFrame } = renderWithWidth(120, uiState);
    expect(lastFrame()).toContain('Ctrl+C to exit');
    expect(lastFrame()).toContain('/bash for shell');
    expect(lastFrame()).toContain('? for shortcuts');
    expect(lastFrame()).not.toContain('Press Esc to interrupt');
  });

  describe('footer rendering (golden snapshots)', () => {
    it('renders complete footer on wide terminal', () => {
      const uiState = createMockUIState({
        sessionStats: {
          lastPromptTokenCount: 131, // 小数值，产生0.1%
          sessionId: 'test-session',
          metrics: {
            models: {},
            tools: {
              totalCalls: 0,
              totalSuccess: 0,
              totalFail: 0,
              totalDurationMs: 0,
              totalDecisions: {
                accept: 0,
                reject: 0,
                modify: 0,
                auto_accept: 0,
              },
              byName: {},
            },
            files: { totalLinesAdded: 0, totalLinesRemoved: 0 },
          },
        },
      });
      const { lastFrame } = renderWithWidth(120, uiState);
      expect(lastFrame()).toMatchSnapshot('complete-footer-wide');
    });

    it('renders complete footer on narrow terminal', () => {
      const uiState = createMockUIState({
        sessionStats: {
          lastPromptTokenCount: 131, // 小数值，产生0.1%
          sessionId: 'test-session',
          metrics: {
            models: {},
            tools: {
              totalCalls: 0,
              totalSuccess: 0,
              totalFail: 0,
              totalDurationMs: 0,
              totalDecisions: {
                accept: 0,
                reject: 0,
                modify: 0,
                auto_accept: 0,
              },
              byName: {},
            },
            files: { totalLinesAdded: 0, totalLinesRemoved: 0 },
          },
        },
      });
      const { lastFrame } = renderWithWidth(79, uiState);
      expect(lastFrame()).toMatchSnapshot('complete-footer-narrow');
    });
  });
});
