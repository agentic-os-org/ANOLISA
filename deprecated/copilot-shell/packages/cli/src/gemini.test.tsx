/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import {
  describe,
  it,
  expect,
  vi,
  beforeEach,
  afterEach,
  type MockInstance,
} from 'vitest';
import {
  main,
  setupUnhandledRejectionHandler,
  validateDnsResolutionOrder,
  startInteractiveUI,
} from './gemini.js';
import { type LoadedSettings } from './config/settings.js';
import { appEvents, AppEvent } from './utils/events.js';
import type { Config } from '@copilot-shell/core';
import { OutputFormat } from '@copilot-shell/core';

// Custom error to identify mock process.exit calls
class MockProcessExitError extends Error {
  constructor(readonly code?: string | number | null | undefined) {
    super('PROCESS_EXIT_MOCKED');
    this.name = 'MockProcessExitError';
  }
}

// Mock dependencies
vi.mock('./config/settings.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./config/settings.js')>();
  return {
    ...actual,
    loadSettings: vi.fn(),
  };
});

vi.mock('./config/config.js', () => ({
  loadCliConfig: vi.fn().mockResolvedValue({
    getQuestion: vi.fn(() => ''),
    isInteractive: () => false,
  } as unknown as Config),
  parseArguments: vi.fn().mockResolvedValue({ promptInteractive: undefined }),
  isDebugMode: vi.fn(() => false),
}));

vi.mock('read-package-up', () => ({
  readPackageUp: vi.fn().mockResolvedValue({
    packageJson: { name: 'test-pkg', version: 'test-version' },
    path: '/fake/path/package.json',
  }),
}));

vi.mock('update-notifier', () => ({
  default: vi.fn(() => ({
    notify: vi.fn(),
  })),
}));

vi.mock('./utils/events.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./utils/events.js')>();
  return {
    ...actual,
    appEvents: {
      emit: vi.fn(),
      once: vi.fn(),
    },
  };
});

vi.mock('./ui/hooks/useKittyKeyboardProtocol.js', () => ({
  useKittyKeyboardProtocol: vi.fn(() => ({
    supported: false,
    enabled: false,
    checking: false,
  })),
}));

vi.mock('./utils/relaunch.js', () => ({
  relaunchAppInChildProcess: vi.fn(),
}));

vi.mock('./core/initializer.js', () => ({
  initializeApp: vi.fn().mockResolvedValue({
    authError: null,
    themeError: null,
    shouldOpenAuthDialog: false,
    geminiMdFileCount: 0,
  }),
}));

describe('gemini.tsx main function', () => {
  let initialUnhandledRejectionListeners: NodeJS.UnhandledRejectionListener[] =
    [];

  beforeEach(() => {
    initialUnhandledRejectionListeners =
      process.listeners('unhandledRejection');
  });

  afterEach(() => {
    const currentListeners = process.listeners('unhandledRejection');
    const addedListener = currentListeners.find(
      (listener) => !initialUnhandledRejectionListeners.includes(listener),
    );

    if (addedListener) {
      process.removeListener('unhandledRejection', addedListener);
    }
    vi.restoreAllMocks();
  });

  describe('-c <command> POSIX shell passthrough (login shell / scp compatibility)', () => {
    let originalArgv: string[];
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    let processExitSpy: any;
    let spawnSyncMock: ReturnType<typeof vi.fn>;

    beforeEach(async () => {
      originalArgv = process.argv;
      processExitSpy = vi.spyOn(process, 'exit').mockImplementation((code) => {
        throw new MockProcessExitError(code);
      });

      // Mock child_process.spawnSync via dynamic import stub
      spawnSyncMock = vi.fn().mockReturnValue({ status: 0 });
      vi.doMock('node:child_process', () => ({
        spawnSync: spawnSyncMock,
        spawn: vi.fn(),
      }));
    });

    afterEach(() => {
      process.argv = originalArgv;
      processExitSpy.mockRestore();
      vi.doUnmock('node:child_process');
    });

    it('should exec bash and exit when -c <command> is provided (e.g. scp)', async () => {
      process.argv = ['node', 'cli.js', '-c', 'echo hello'];
      try {
        await main();
      } catch (e) {
        if (!(e instanceof MockProcessExitError)) throw e;
        expect(e.code).toBe(0);
      }
      expect(spawnSyncMock).toHaveBeenCalledWith(
        '/bin/bash',
        ['-c', 'echo hello'],
        expect.objectContaining({ stdio: 'inherit' }),
      );
    });

    it('should exec bash and forward non-zero exit code', async () => {
      spawnSyncMock.mockReturnValue({ status: 1 });
      process.argv = ['node', 'cli.js', '-c', 'exit 1'];
      try {
        await main();
      } catch (e) {
        if (!(e instanceof MockProcessExitError)) throw e;
        expect(e.code).toBe(1);
      }
      expect(spawnSyncMock).toHaveBeenCalledWith(
        '/bin/bash',
        ['-c', 'exit 1'],
        expect.objectContaining({ stdio: 'inherit' }),
      );
    });

    it('should fall back to status 1 when spawnSync returns null status', async () => {
      spawnSyncMock.mockReturnValue({ status: null });
      process.argv = ['node', 'cli.js', '-c', 'some-command'];
      try {
        await main();
      } catch (e) {
        if (!(e instanceof MockProcessExitError)) throw e;
        expect(e.code).toBe(1);
      }
    });

    it('should NOT intercept -c when used alone (--continue / -k boolean flag)', async () => {
      // -c with no following argument → treated as boolean --continue, not shell passthrough
      process.argv = ['node', 'cli.js', '-c'];
      // main() should proceed past the intercept and reach loadSettings/parseArguments
      const { loadSettings } = await import('./config/settings.js');
      vi.mocked(loadSettings).mockReturnValue({
        errors: [],
        merged: { advanced: {}, security: { auth: {} }, ui: {} },
        setValue: vi.fn(),
        forScope: () => ({ settings: {}, originalSettings: {}, path: '' }),
      } as never);
      try {
        await main();
      } catch {
        // may throw for other reasons (auth, etc.) — that's fine
      }
      // spawnSync should NOT have been called
      expect(spawnSyncMock).not.toHaveBeenCalled();
    });

    it('should NOT intercept when next arg starts with - (another flag)', async () => {
      process.argv = ['node', 'cli.js', '-c', '-p', 'hello'];
      const { loadSettings } = await import('./config/settings.js');
      vi.mocked(loadSettings).mockReturnValue({
        errors: [],
        merged: { advanced: {}, security: { auth: {} }, ui: {} },
        setValue: vi.fn(),
        forScope: () => ({ settings: {}, originalSettings: {}, path: '' }),
      } as never);
      try {
        await main();
      } catch {
        // may throw for other reasons — that's fine
      }
      expect(spawnSyncMock).not.toHaveBeenCalled();
    });
  });

  it('verifies that we dont load the config before relaunchAppInChildProcess', async () => {
    const processExitSpy = vi
      .spyOn(process, 'exit')
      .mockImplementation((code) => {
        throw new MockProcessExitError(code);
      });
    const { relaunchAppInChildProcess } = await import('./utils/relaunch.js');
    const { loadCliConfig, parseArguments } =
      await import('./config/config.js');
    const { loadSettings } = await import('./config/settings.js');

    const callOrder: string[] = [];
    vi.mocked(relaunchAppInChildProcess).mockImplementation(async () => {
      callOrder.push('relaunch');
    });
    vi.mocked(loadCliConfig).mockImplementation(async () => {
      callOrder.push('loadCliConfig');
      return {
        isInteractive: () => false,
        getQuestion: () => '',
        getDebugMode: () => false,
        getListExtensions: () => false,
        getMcpServers: () => ({}),
        initialize: vi.fn(),
        getIdeMode: () => false,
        getExperimentalZedIntegration: () => false,
        getScreenReader: () => false,
        getGeminiMdFileCount: () => 0,
        getProjectRoot: () => '/',
        getOutputFormat: () => OutputFormat.TEXT,
      } as unknown as Config;
    });
    vi.mocked(parseArguments).mockResolvedValue({
      promptInteractive: undefined,
    } as never);
    vi.mocked(loadSettings).mockReturnValue({
      errors: [],
      merged: {
        advanced: { autoConfigureMemory: true },
        security: { auth: {} },
        ui: {},
      },
      setValue: vi.fn(),
      forScope: () => ({ settings: {}, originalSettings: {}, path: '' }),
    } as never);
    try {
      await main();
    } catch (e) {
      // Mocked process exit throws an error.
      if (!(e instanceof MockProcessExitError)) throw e;
    }

    // It is critical that we call relaunch before loadCliConfig to avoid
    // loading config in the outer process when we are going to relaunch.
    // By ensuring we don't load the config we also ensure we don't trigger any
    // operations that might require loading the config such as such as
    // initializing mcp servers.
    expect(callOrder).toEqual(['relaunch', 'loadCliConfig']);
    processExitSpy.mockRestore();
  });

  it('should log unhandled promise rejections and open debug console on first error', async () => {
    const processExitSpy = vi
      .spyOn(process, 'exit')
      .mockImplementation((code) => {
        throw new MockProcessExitError(code);
      });
    const appEventsMock = vi.mocked(appEvents);
    const rejectionError = new Error('Test unhandled rejection');

    setupUnhandledRejectionHandler();
    // Simulate an unhandled rejection.
    // We are not using Promise.reject here as vitest will catch it.
    // Instead we will dispatch the event manually.
    process.emit('unhandledRejection', rejectionError, Promise.resolve());

    // We need to wait for the rejection handler to be called.
    await new Promise(process.nextTick);

    expect(appEventsMock.emit).toHaveBeenCalledWith(AppEvent.OpenDebugConsole);
    expect(appEventsMock.emit).toHaveBeenCalledWith(
      AppEvent.LogError,
      expect.stringContaining('Unhandled Promise Rejection'),
    );
    expect(appEventsMock.emit).toHaveBeenCalledWith(
      AppEvent.LogError,
      expect.stringContaining('Please file a bug report using the /bug tool.'),
    );

    // Simulate a second rejection
    const secondRejectionError = new Error('Second test unhandled rejection');
    process.emit('unhandledRejection', secondRejectionError, Promise.resolve());
    await new Promise(process.nextTick);

    // Ensure emit was only called once for OpenDebugConsole
    const openDebugConsoleCalls = appEventsMock.emit.mock.calls.filter(
      (call) => call[0] === AppEvent.OpenDebugConsole,
    );
    expect(openDebugConsoleCalls.length).toBe(1);

    // Avoid the process.exit error from being thrown.
    processExitSpy.mockRestore();
  });

  it('invokes runNonInteractiveStreamJson and performs cleanup in stream-json mode', async () => {
    const originalIsTTY = Object.getOwnPropertyDescriptor(
      process.stdin,
      'isTTY',
    );
    const originalIsRaw = Object.getOwnPropertyDescriptor(
      process.stdin,
      'isRaw',
    );
    Object.defineProperty(process.stdin, 'isTTY', {
      value: true,
      configurable: true,
    });
    Object.defineProperty(process.stdin, 'isRaw', {
      value: false,
      configurable: true,
    });

    const processExitSpy = vi
      .spyOn(process, 'exit')
      .mockImplementation((code) => {
        throw new MockProcessExitError(code);
      });

    const { loadCliConfig, parseArguments } =
      await import('./config/config.js');
    const { loadSettings } = await import('./config/settings.js');
    const cleanupModule = await import('./utils/cleanup.js');
    const validatorModule = await import('./validateNonInterActiveAuth.js');
    const streamJsonModule = await import('./nonInteractive/session.js');
    const initializerModule = await import('./core/initializer.js');
    const startupWarningsModule = await import('./utils/startupWarnings.js');
    const userStartupWarningsModule =
      await import('./utils/userStartupWarnings.js');

    vi.mocked(cleanupModule.cleanupCheckpoints).mockResolvedValue(undefined);
    vi.mocked(cleanupModule.registerCleanup).mockImplementation(() => {});
    const runExitCleanupMock = vi.mocked(cleanupModule.runExitCleanup);
    runExitCleanupMock.mockResolvedValue(undefined);
    vi.spyOn(initializerModule, 'initializeApp').mockResolvedValue({
      authError: null,
      themeError: null,
      shouldOpenAuthDialog: false,
      geminiMdFileCount: 0,
    });
    vi.spyOn(startupWarningsModule, 'getStartupWarnings').mockResolvedValue([]);
    vi.spyOn(
      userStartupWarningsModule,
      'getUserStartupWarnings',
    ).mockResolvedValue([]);

    const validatedConfig = { validated: true } as unknown as Config;
    const validateAuthSpy = vi
      .spyOn(validatorModule, 'validateNonInteractiveAuth')
      .mockResolvedValue(validatedConfig);
    const runStreamJsonSpy = vi
      .spyOn(streamJsonModule, 'runNonInteractiveStreamJson')
      .mockResolvedValue(undefined);

    vi.mocked(loadSettings).mockReturnValue({
      errors: [],
      merged: {
        advanced: {},
        security: { auth: {} },
        ui: {},
      },
      setValue: vi.fn(),
      forScope: () => ({ settings: {}, originalSettings: {}, path: '' }),
    } as never);

    vi.mocked(parseArguments).mockResolvedValue({
      extensions: [],
    } as never);

    const configStub = {
      isInteractive: () => false,
      getQuestion: () => '  hello stream  ',
      getDebugMode: () => false,
      getListExtensions: () => false,
      getMcpServers: () => ({}),
      initialize: vi.fn().mockResolvedValue(undefined),
      getIdeMode: () => false,
      getExperimentalZedIntegration: () => false,
      getScreenReader: () => false,
      getGeminiMdFileCount: () => 0,
      getProjectRoot: () => '/',
      getInputFormat: () => 'stream-json',
      getContentGeneratorConfig: () => ({ authType: 'test-auth' }),
    } as unknown as Config;

    vi.mocked(loadCliConfig).mockResolvedValue(configStub);

    try {
      await main();
    } catch (error) {
      if (!(error instanceof MockProcessExitError)) {
        throw error;
      }
    } finally {
      processExitSpy.mockRestore();
      if (originalIsTTY) {
        Object.defineProperty(process.stdin, 'isTTY', originalIsTTY);
      } else {
        delete (process.stdin as { isTTY?: unknown }).isTTY;
      }
      if (originalIsRaw) {
        Object.defineProperty(process.stdin, 'isRaw', originalIsRaw);
      } else {
        delete (process.stdin as { isRaw?: unknown }).isRaw;
      }
    }

    expect(runStreamJsonSpy).toHaveBeenCalledTimes(1);
    const [configArg, inputArg] = runStreamJsonSpy.mock.calls[0];
    expect(configArg).toBe(validatedConfig);
    expect(inputArg).toBe('hello stream');

    expect(validateAuthSpy).toHaveBeenCalledWith(
      undefined,
      configStub,
      expect.any(Object),
    );
    expect(runExitCleanupMock).toHaveBeenCalledTimes(1);
  });
});

describe('gemini.tsx main function kitty protocol', () => {
  let originalEnvNoRelaunch: string | undefined;
  let setRawModeSpy: MockInstance<
    (mode: boolean) => NodeJS.ReadStream & { fd: 0 }
  >;

  beforeEach(() => {
    // Set no relaunch in tests since process spawning causing issues in tests
    originalEnvNoRelaunch = process.env['QWEN_CODE_NO_RELAUNCH'];
    process.env['QWEN_CODE_NO_RELAUNCH'] = 'true';

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    if (!(process.stdin as any).setRawMode) {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (process.stdin as any).setRawMode = vi.fn();
    }
    setRawModeSpy = vi.spyOn(process.stdin, 'setRawMode');

    Object.defineProperty(process.stdin, 'isTTY', {
      value: true,
      configurable: true,
    });
    Object.defineProperty(process.stdin, 'isRaw', {
      value: false,
      configurable: true,
    });
  });

  afterEach(() => {
    // Restore original env variables
    if (originalEnvNoRelaunch !== undefined) {
      process.env['QWEN_CODE_NO_RELAUNCH'] = originalEnvNoRelaunch;
    } else {
      delete process.env['QWEN_CODE_NO_RELAUNCH'];
    }
  });

  it('should call setRawMode when isInteractive is true', async () => {
    // Note: Kitty protocol detection has been disabled to avoid terminal
    // compatibility issues with login shells. This test now only verifies
    // that setRawMode is called for interactive mode.
    const { loadCliConfig, parseArguments } =
      await import('./config/config.js');
    const { loadSettings } = await import('./config/settings.js');
    vi.mocked(loadCliConfig).mockResolvedValue({
      isInteractive: () => true,
      getQuestion: () => '',
      getDebugMode: () => false,
      getListExtensions: () => false,
      getMcpServers: () => ({}),
      initialize: vi.fn(),
      getIdeMode: () => false,
      getExperimentalZedIntegration: () => false,
      getScreenReader: () => false,
      getGeminiMdFileCount: () => 0,
      getResumedSessionData: () => undefined,
    } as unknown as Config);
    vi.mocked(loadSettings).mockReturnValue({
      errors: [],
      merged: {
        advanced: {},
        security: { auth: {} },
        ui: {},
      },
      setValue: vi.fn(),
      forScope: () => ({ settings: {}, originalSettings: {}, path: '' }),
    } as never);
    vi.mocked(parseArguments).mockResolvedValue({
      model: undefined,
      debug: undefined,
      prompt: undefined,
      promptInteractive: undefined,
      query: undefined,
      allFiles: undefined,
      yolo: undefined,
      approvalMode: undefined,
      telemetry: undefined,
      checkpointing: undefined,
      telemetryTarget: undefined,
      telemetryOtlpEndpoint: undefined,
      telemetryOtlpProtocol: undefined,
      telemetryLogPrompts: undefined,
      telemetryOutfile: undefined,
      allowedMcpServerNames: undefined,
      allowedTools: undefined,
      acp: undefined,
      experimentalAcp: undefined,
      extensions: undefined,
      listExtensions: undefined,
      openaiLogging: undefined,
      openaiApiKey: undefined,
      openaiBaseUrl: undefined,
      openaiLoggingDir: undefined,
      proxy: undefined,
      includeDirectories: undefined,
      tavilyApiKey: undefined,
      googleApiKey: undefined,
      googleSearchEngineId: undefined,
      webSearchDefault: undefined,
      screenReader: undefined,
      vlmSwitchMode: undefined,
      useSmartEdit: undefined,
      inputFormat: undefined,
      outputFormat: undefined,
      includePartialMessages: undefined,
      continue: undefined,
      resume: undefined,
      coreTools: undefined,
      excludeTools: undefined,
      authType: undefined,
      maxSessionTurns: undefined,
      experimentalLsp: undefined,
      experimentalHooks: undefined,
      channel: undefined,
      chatRecording: undefined,
    });

    await main();

    expect(setRawModeSpy).toHaveBeenCalledWith(true);
  });
});

describe('validateDnsResolutionOrder', () => {
  let consoleWarnSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    consoleWarnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
  });

  afterEach(() => {
    consoleWarnSpy.mockRestore();
  });

  it('should return "ipv4first" when the input is "ipv4first"', () => {
    expect(validateDnsResolutionOrder('ipv4first')).toBe('ipv4first');
    expect(consoleWarnSpy).not.toHaveBeenCalled();
  });

  it('should return "verbatim" when the input is "verbatim"', () => {
    expect(validateDnsResolutionOrder('verbatim')).toBe('verbatim');
    expect(consoleWarnSpy).not.toHaveBeenCalled();
  });

  it('should return the default "ipv4first" when the input is undefined', () => {
    expect(validateDnsResolutionOrder(undefined)).toBe('ipv4first');
    expect(consoleWarnSpy).not.toHaveBeenCalled();
  });

  it('should return the default "ipv4first" and log a warning for an invalid string', () => {
    expect(validateDnsResolutionOrder('invalid-value')).toBe('ipv4first');
    expect(consoleWarnSpy).toHaveBeenCalledWith(
      'Invalid value for dnsResolutionOrder in settings: "invalid-value". Using default "ipv4first".',
    );
  });
});

type ReactElementLike = {
  type?: unknown;
  props?: {
    children?: unknown;
    initialPromptCount?: number;
    onPromptCountChange?: (promptCount: number) => void;
  };
};

function isReactElementLike(value: unknown): value is ReactElementLike {
  return typeof value === 'object' && value !== null && 'props' in value;
}

function renderAppWrapper(element: unknown): unknown {
  if (!isReactElementLike(element) || typeof element.type !== 'function') {
    return element;
  }
  return element.type(element.props ?? {});
}

function findSessionStatsProviderProps(
  element: unknown,
): ReactElementLike['props'] | undefined {
  if (Array.isArray(element)) {
    for (const child of element) {
      const result = findSessionStatsProviderProps(child);
      if (result) {
        return result;
      }
    }
    return undefined;
  }

  if (!isReactElementLike(element)) {
    return undefined;
  }

  if (
    typeof element.props?.initialPromptCount === 'number' &&
    typeof element.props?.onPromptCountChange === 'function'
  ) {
    return element.props;
  }

  return findSessionStatsProviderProps(element.props?.children);
}

describe('startInteractiveUI', () => {
  // Mock dependencies
  const mockConfig = {
    getSessionId: () => 'session-1',
    getResumedSessionData: () => undefined,
    getProjectRoot: () => '/root',
    getScreenReader: () => false,
  } as Config;
  const mockSettings = {
    merged: {
      ui: {
        hideWindowTitle: false,
      },
    },
  } as LoadedSettings;
  const mockStartupWarnings = ['warning1'];
  const mockWorkspaceRoot = '/root';

  vi.mock('./utils/version.js', () => ({
    getCliVersion: vi.fn(() => Promise.resolve('1.0.0')),
  }));

  // Note: Kitty protocol detection has been disabled.
  // This mock is no longer needed but kept for compatibility.

  vi.mock('./ui/utils/updateCheck.js', () => ({
    checkForUpdates: vi.fn(() => Promise.resolve(null)),
  }));

  vi.mock('./utils/cleanup.js', () => ({
    cleanupCheckpoints: vi.fn(() => Promise.resolve()),
    registerCleanup: vi.fn(),
    runExitCleanup: vi.fn(() => Promise.resolve()),
  }));

  vi.mock('ink', () => ({
    render: vi.fn().mockReturnValue({ unmount: vi.fn() }),
  }));

  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('should render the UI with proper React context and exitOnCtrlC disabled', async () => {
    const { render } = await import('ink');
    const renderSpy = vi.mocked(render);

    const mockInitializationResult = {
      authError: null,
      themeError: null,
      shouldOpenAuthDialog: false,
      geminiMdFileCount: 0,
    };

    await startInteractiveUI(
      mockConfig,
      mockSettings,
      mockStartupWarnings,
      mockWorkspaceRoot,
      mockInitializationResult,
    );

    // Verify render was called with correct options
    expect(renderSpy).toHaveBeenCalledTimes(1);
    const [reactElement, options] = renderSpy.mock.calls[0];

    // Verify render options
    expect(options).toEqual({
      exitOnCtrlC: false,
      isScreenReaderEnabled: false,
    });

    // Verify React element structure is valid (but don't deep dive into JSX internals)
    expect(reactElement).toBeDefined();
  });

  it('should carry prompt count into remounted UI after shell suspend', async () => {
    const { render } = await import('ink');
    const renderSpy = vi.mocked(render);

    const mockInitializationResult = {
      authError: null,
      themeError: null,
      shouldOpenAuthDialog: false,
      geminiMdFileCount: 0,
    };

    await startInteractiveUI(
      mockConfig,
      mockSettings,
      mockStartupWarnings,
      mockWorkspaceRoot,
      mockInitializationResult,
    );

    const [reactElement] = renderSpy.mock.calls[0];
    const firstProviderProps = findSessionStatsProviderProps(
      renderAppWrapper(reactElement),
    );
    expect(firstProviderProps?.initialPromptCount).toBe(0);

    firstProviderProps?.onPromptCountChange?.(2);

    const remountedProviderProps = findSessionStatsProviderProps(
      renderAppWrapper(reactElement),
    );
    expect(remountedProviderProps?.initialPromptCount).toBe(2);
  });

  it('should perform all startup tasks in correct order', async () => {
    const { getCliVersion } = await import('./utils/version.js');
    const { checkForUpdates } = await import('./ui/utils/updateCheck.js');
    const { registerCleanup } = await import('./utils/cleanup.js');

    const mockInitializationResult = {
      authError: null,
      themeError: null,
      shouldOpenAuthDialog: false,
      geminiMdFileCount: 0,
    };

    await startInteractiveUI(
      mockConfig,
      mockSettings,
      mockStartupWarnings,
      mockWorkspaceRoot,
      mockInitializationResult,
    );

    // Verify all startup tasks were called
    expect(getCliVersion).toHaveBeenCalledTimes(1);
    expect(registerCleanup).toHaveBeenCalledTimes(1);

    // Verify cleanup handler is registered with unmount function
    const cleanupFn = vi.mocked(registerCleanup).mock.calls[0][0];
    expect(typeof cleanupFn).toBe('function');

    // checkForUpdates should be called asynchronously (not waited for)
    // We need a small delay to let it execute
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(checkForUpdates).toHaveBeenCalledTimes(1);
  });

  it('should not check for updates when update nag is disabled', async () => {
    const { checkForUpdates } = await import('./ui/utils/updateCheck.js');

    const mockInitializationResult = {
      authError: null,
      themeError: null,
      shouldOpenAuthDialog: false,
      geminiMdFileCount: 0,
    };

    const settingsWithUpdateNagDisabled = {
      merged: {
        general: {
          disableUpdateNag: true,
        },
        ui: {
          hideWindowTitle: false,
        },
      },
    } as LoadedSettings;

    await startInteractiveUI(
      mockConfig,
      settingsWithUpdateNagDisabled,
      mockStartupWarnings,
      mockWorkspaceRoot,
      mockInitializationResult,
    );

    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(checkForUpdates).not.toHaveBeenCalled();
  });
});
