/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type { Config } from '@copilot-shell/core';
import {
  InputFormat,
  logUserPrompt,
  migrateConfigDirIfNeeded,
} from '@copilot-shell/core';
import { render } from 'ink';
import { spawn } from 'node:child_process';
import dns from 'node:dns';
import os from 'node:os';
import { basename } from 'node:path';
import v8 from 'node:v8';
import React from 'react';
import * as cliConfig from './config/config.js';
import { loadCliConfig, parseArguments } from './config/config.js';
import type { DnsResolutionOrder, LoadedSettings } from './config/settings.js';
import { getSettingsWarnings, loadSettings } from './config/settings.js';
import {
  initializeApp,
  type InitializationResult,
} from './core/initializer.js';
import { runNonInteractive } from './nonInteractiveCli.js';
import { runNonInteractiveStreamJson } from './nonInteractive/session.js';
import { AppContainer } from './ui/AppContainer.js';
import { setMaxSizedBoxDebugging } from './ui/components/shared/MaxSizedBox.js';
import { KeypressProvider } from './ui/contexts/KeypressContext.js';
import {
  getPromptCountFromSessionData,
  SessionStatsProvider,
} from './ui/contexts/SessionContext.js';
import { SettingsContext } from './ui/contexts/SettingsContext.js';
import { VimModeProvider } from './ui/contexts/VimModeContext.js';
import { useKittyKeyboardProtocol } from './ui/hooks/useKittyKeyboardProtocol.js';
import { themeManager } from './ui/themes/theme-manager.js';
import { ConsolePatcher } from './ui/utils/ConsolePatcher.js';
import { checkForUpdates } from './ui/utils/updateCheck.js';
import {
  cleanupCheckpoints,
  registerCleanup,
  runExitCleanup,
} from './utils/cleanup.js';
import { AppEvent, appEvents } from './utils/events.js';
import { handleAutoUpdate } from './utils/handleAutoUpdate.js';
import { readStdin } from './utils/readStdin.js';
import { relaunchAppInChildProcess } from './utils/relaunch.js';
import { getStartupWarnings } from './utils/startupWarnings.js';
import { getUserStartupWarnings } from './utils/userStartupWarnings.js';
import { getCliVersion } from './utils/version.js';
import { computeWindowTitle } from './utils/windowTitle.js';
import { validateNonInteractiveAuth } from './validateNonInterActiveAuth.js';
import { showResumeSessionPicker } from './ui/components/StandaloneSessionPicker.js';
import { initializeLlmOutputLanguage } from './utils/languageUtils.js';
import { initializeI18n, t, type SupportedLanguage } from './i18n/index.js';
import {
  getAndMarkUnshownFeatureTips,
  type FeatureTip,
} from './utils/featureTips.js';
import { stopAutoMemory } from './core/autoMemoryLifecycle.js';

export function validateDnsResolutionOrder(
  order: string | undefined,
): DnsResolutionOrder {
  const defaultValue: DnsResolutionOrder = 'ipv4first';
  if (order === undefined) {
    return defaultValue;
  }
  if (order === 'ipv4first' || order === 'verbatim') {
    return order;
  }
  // We don't want to throw here, just warn and use the default.
  console.warn(
    `Invalid value for dnsResolutionOrder in settings: "${order}". Using default "${defaultValue}".`,
  );
  return defaultValue;
}

function getNodeMemoryArgs(isDebugMode: boolean): string[] {
  const totalMemoryMB = os.totalmem() / (1024 * 1024);
  const heapStats = v8.getHeapStatistics();
  const currentMaxOldSpaceSizeMb = Math.floor(
    heapStats.heap_size_limit / 1024 / 1024,
  );

  // Set target to 50% of total memory
  const targetMaxOldSpaceSizeInMB = Math.floor(totalMemoryMB * 0.5);
  if (isDebugMode) {
    console.debug(
      `Current heap size ${currentMaxOldSpaceSizeMb.toFixed(2)} MB`,
    );
  }

  if (process.env['QWEN_CODE_NO_RELAUNCH']) {
    return [];
  }

  if (targetMaxOldSpaceSizeInMB > currentMaxOldSpaceSizeMb) {
    if (isDebugMode) {
      console.debug(
        `Need to relaunch with more memory: ${targetMaxOldSpaceSizeInMB.toFixed(2)} MB`,
      );
    }
    return [`--max-old-space-size=${targetMaxOldSpaceSizeInMB}`];
  }

  return [];
}

import { runAcpAgent } from './acp-integration/acpAgent.js';

export function setupUnhandledRejectionHandler() {
  let unhandledRejectionOccurred = false;
  process.on('unhandledRejection', (reason, _promise) => {
    const errorMessage = `=========================================
This is an unexpected error. Please file a bug report using the /bug tool.
CRITICAL: Unhandled Promise Rejection!
=========================================
Reason: ${reason}${
      reason instanceof Error && reason.stack
        ? `
Stack trace:
${reason.stack}`
        : ''
    }`;
    appEvents.emit(AppEvent.LogError, errorMessage);
    if (!unhandledRejectionOccurred) {
      unhandledRejectionOccurred = true;
      appEvents.emit(AppEvent.OpenDebugConsole);
    }
  });
}

export async function startInteractiveUI(
  config: Config,
  settings: LoadedSettings,
  startupWarnings: string[],
  workspaceRoot: string = process.cwd(),
  initializationResult: InitializationResult,
  featureTips: FeatureTip[] = [],
) {
  const version = await getCliVersion();
  setWindowTitle(basename(workspaceRoot), settings);
  let interactivePromptCount = getPromptCountFromSessionData(
    config.getResumedSessionData(),
  );

  // Create wrapper component to use hooks inside render
  const AppWrapper = () => {
    const kittyProtocolStatus = useKittyKeyboardProtocol();
    const nodeMajorVersion = parseInt(process.versions.node.split('.')[0], 10);
    return (
      <SettingsContext.Provider value={settings}>
        <KeypressProvider
          kittyProtocolEnabled={kittyProtocolStatus.enabled}
          config={config}
          debugKeystrokeLogging={settings.merged.general?.debugKeystrokeLogging}
          pasteWorkaround={
            process.platform === 'win32' || nodeMajorVersion < 20
          }
        >
          <SessionStatsProvider
            sessionId={config.getSessionId()}
            initialPromptCount={interactivePromptCount}
            onPromptCountChange={(promptCount) => {
              interactivePromptCount = promptCount;
            }}
          >
            <VimModeProvider settings={settings}>
              <AppContainer
                config={config}
                settings={settings}
                startupWarnings={startupWarnings}
                featureTips={featureTips}
                version={version}
                initializationResult={initializationResult}
              />
            </VimModeProvider>
          </SessionStatsProvider>
        </KeypressProvider>
      </SettingsContext.Provider>
    );
  };

  let instance = render(
    process.env['DEBUG'] ? (
      <React.StrictMode>
        <AppWrapper />
      </React.StrictMode>
    ) : (
      <AppWrapper />
    ),
    {
      exitOnCtrlC: false,
      isScreenReaderEnabled: config.getScreenReader(),
    },
  );

  // /bash command: temporarily unmount TUI, spawn interactive shell, then remount
  const handleSpawnShell = async (shell: string) => {
    // Step 1: unmount TUI
    instance.unmount();

    // Step 2: restore terminal mode
    const wasRaw = process.stdin.isRaw;
    if (wasRaw && process.stdin.isTTY) {
      process.stdin.setRawMode(false);
    }

    // Step 3: print hint
    process.stdout.write(
      `\n\x1b[33m[Copilot Shell] ${t("Entering interactive shell. Type 'exit' or press Ctrl+D to return.")}\x1b[0m\n\n`,
    );

    // Step 4: spawn interactive shell, fully inheriting the parent terminal
    // Split shell string to separate executable from any extra args (e.g. 'bash --login')
    const [shellExe, ...shellExtraArgs] = shell.split(/\s+/).filter(Boolean);
    await new Promise<void>((resolve) => {
      const child = spawn(shellExe, shellExtraArgs, {
        stdio: 'inherit',
        env: {
          ...process.env,
          COSH_BASH_SESSION: '1',
        },
      });
      child.on('exit', () => resolve());
      child.on('error', (err) => {
        process.stderr.write(
          `\n[Copilot Shell] ${t('Failed to spawn shell: {{error}}', { error: err.message })}\n`,
        );
        resolve();
      });
    });

    // Step 5: print restore hint
    process.stdout.write(
      `\n\x1b[33m[Copilot Shell] ${t('Returned from shell. Restoring TUI...')}\x1b[0m\n`,
    );

    // Step 6: restore rawMode if it was set before
    if (wasRaw && process.stdin.isTTY) {
      process.stdin.setRawMode(true);
    }

    // Step 7: re-render TUI directly (do NOT call startInteractiveUI again
    // to avoid config.initialize() being called twice)
    const newInstance = render(
      process.env['DEBUG'] ? (
        <React.StrictMode>
          <AppWrapper />
        </React.StrictMode>
      ) : (
        <AppWrapper />
      ),
      {
        exitOnCtrlC: false,
        isScreenReaderEnabled: config.getScreenReader(),
      },
    );

    // Re-register cleanup and event listener for the new instance
    registerCleanup(() => newInstance.unmount());
    appEvents.once(AppEvent.SpawnShell, async (nextShell: string) => {
      // Patch: replace `instance` reference with `newInstance` for the next cycle
      instance = newInstance;
      await handleSpawnShell(nextShell);
    });
  };

  appEvents.once(AppEvent.SpawnShell, handleSpawnShell);

  if (!settings.merged.general?.disableUpdateNag) {
    checkForUpdates()
      .then((info) => {
        handleAutoUpdate(info, settings, config.getProjectRoot());
      })
      .catch((err) => {
        // Silently ignore update check errors.
        if (config.getDebugMode()) {
          console.error('Update check failed:', err);
        }
      });
  }

  registerCleanup(() => instance.unmount());
}

export async function main() {
  setupUnhandledRejectionHandler();

  // Handle POSIX shell `-c <command>` convention **before** yargs parses argv.
  // SSH-based tools (scp, rsync, git-over-ssh, sftp) invoke the login shell as:
  //   cosh -c "scp -t /remote/path"
  // We detect this by checking that -c is present AND the next argument is a
  // non-empty string that does not start with '-' (which would indicate another
  // flag rather than a shell command).
  // Our own `-k` (--continue) is now a boolean flag that takes no value, so
  // there is no conflict.
  const rawArgs = process.argv.slice(2);
  const dashCIndex = rawArgs.indexOf('-c');
  if (dashCIndex !== -1) {
    const nextArg = rawArgs[dashCIndex + 1];
    if (
      typeof nextArg === 'string' &&
      nextArg.length > 0 &&
      !nextArg.startsWith('-')
    ) {
      const { spawnSync } = await import('node:child_process');
      const result = spawnSync('/bin/bash', ['-c', nextArg], {
        stdio: 'inherit',
        env: process.env,
      });
      process.exit(result.status ?? 1);
    }
  }

  // Detect nested co session: running inside a shell spawned by /bash from
  // another co session. Show a soft warning and let the user decide.
  if (process.env['COSH_BASH_SESSION'] && process.stdin.isTTY) {
    // i18n is not yet initialized at this point; initialize early using the
    // env variable so the prompt is shown in the user's configured language.
    await initializeI18n(
      (process.env['QWEN_CODE_LANG'] as SupportedLanguage | 'auto') ?? 'auto',
    );
    const tag = '\x1b[33m[Copilot Shell]\x1b[0m';
    process.stdout.write(
      `\n${tag} ${t('Nested session detected: you are attempting to start a new session. The new session will not contain the original context.')}\n`,
    );
    process.stdout.write(
      `${tag} ${t('Press Ctrl+C to exit, then type exit to return to the original session.')}\n`,
    );
    process.stdout.write(
      `${tag} ${t('Press Enter to start a new session.')}\n\n`,
    );
    await new Promise<void>((resolve) => {
      const onData = (chunk: Buffer) => {
        const key = chunk[0];
        // Enter (0x0d or 0x0a) → start new session; Ctrl-C (0x03) → go back
        if (key === 0x03) {
          process.stdout.write('\n');
          process.stdin.removeListener('data', onData);
          process.stdin.setRawMode?.(false);
          process.exit(0);
        }
        process.stdin.removeListener('data', onData);
        process.stdin.setRawMode?.(false);
        resolve();
      };
      process.stdin.setRawMode?.(true);
      process.stdin.resume();
      process.stdin.once('data', onData);
    });
  }

  // Migrate config directory from ~/.copilot to ~/.copilot-shell BEFORE loading
  // settings, so that loadSettings() can read from the new path immediately.
  // Pass the warning through an env var so the child process (after relaunch) can show it.
  const migrationWarning = await migrateConfigDirIfNeeded();
  if (migrationWarning) {
    process.env['COSH_MIGRATION_WARNING'] = migrationWarning;
  }

  const settings = loadSettings();
  await cleanupCheckpoints();

  let argv = await parseArguments();

  // Check for invalid input combinations early to prevent crashes
  if (argv.promptInteractive && !process.stdin.isTTY) {
    console.error(
      'Error: The --prompt-interactive flag cannot be used when input is piped from stdin.',
    );
    process.exit(1);
  }

  const isDebugMode = cliConfig.isDebugMode(argv);

  dns.setDefaultResultOrder(
    validateDnsResolutionOrder(settings.merged.advanced?.dnsResolutionOrder),
  );

  // Load custom themes from settings
  themeManager.loadCustomThemes(settings.merged.ui?.customThemes);

  if (settings.merged.ui?.theme) {
    if (!themeManager.setActiveTheme(settings.merged.ui?.theme)) {
      // If the theme is not found during initial load, log a warning and continue.
      // The useThemeCommand hook in AppContainer.tsx will handle opening the dialog.
      console.warn(`Warning: Theme "${settings.merged.ui?.theme}" not found.`);
    }
  }

  {
    const memoryArgs = settings.merged.advanced?.autoConfigureMemory
      ? getNodeMemoryArgs(isDebugMode)
      : [];
    // Relaunch app so we always have a child process that can be internally
    // restarted if needed.
    await relaunchAppInChildProcess(memoryArgs, []);
  }

  // Handle --resume without a session ID by showing the session picker
  if (argv.resume === '') {
    const selectedSessionId = await showResumeSessionPicker();
    if (!selectedSessionId) {
      // User cancelled or no sessions available
      process.exit(0);
    }

    // Update argv with the selected session ID
    argv = { ...argv, resume: selectedSessionId };
  }

  // We are now past the logic handling potentially launching a child process
  // to run Gemini CLI. It is now safe to perform expensive initialization that
  // may have side effects.

  // Initialize output language file before config loads to ensure it's included in context
  initializeLlmOutputLanguage(settings.merged.general?.outputLanguage);

  {
    const configWarnings: string[] = [];
    const config = await loadCliConfig(
      settings.merged,
      argv,
      process.cwd(),
      argv.extensions,
      configWarnings,
    );
    registerCleanup(() => config.shutdown());
    registerCleanup(() => stopAutoMemory());

    // FIXME: list extensions after the config initialize
    // if (config.getListExtensions()) {
    //   console.log('Installed extensions:');
    //   for (const extension of extensions) {
    //     console.log(`- ${extension.config.name}`);
    //   }
    //   process.exit(0);
    // }

    // Setup unified ConsolePatcher based on interactive mode
    const isInteractive = config.isInteractive();
    const consolePatcher = new ConsolePatcher({
      stderr: isInteractive,
      debugMode: isDebugMode,
    });
    consolePatcher.patch();
    registerCleanup(consolePatcher.cleanup);

    const wasRaw = process.stdin.isRaw;
    // Note: Kitty protocol detection has been disabled to avoid terminal
    // compatibility issues with login shells (DA1 response echo issue).
    // cosh will use standard terminal input mode.
    if (config.isInteractive() && !wasRaw && process.stdin.isTTY) {
      // Set raw mode for interactive UI
      process.stdin.setRawMode(true);

      // This cleanup isn't strictly needed but may help in certain situations.
      process.on('SIGTERM', () => {
        process.stdin.setRawMode(wasRaw);
      });
      process.on('SIGINT', () => {
        process.stdin.setRawMode(wasRaw);
      });
    }

    setMaxSizedBoxDebugging(isDebugMode);

    // Check input format early to determine initialization flow
    const inputFormat =
      typeof config.getInputFormat === 'function'
        ? config.getInputFormat()
        : InputFormat.TEXT;

    // For stream-json mode, defer config.initialize() until after the initialize control request
    // For other modes, initialize normally
    let initializationResult: InitializationResult | undefined;
    if (inputFormat !== InputFormat.STREAM_JSON) {
      initializationResult = await initializeApp(config, settings);
    }

    if (config.getExperimentalZedIntegration()) {
      return runAcpAgent(config, settings, argv);
    }

    let input = config.getQuestion();
    // In the child process, migrationWarning from migrateConfigDirIfNeeded() is null
    // (migration already done by parent). Read it from the env var set by parent instead.
    const effectiveMigrationWarning =
      migrationWarning ?? process.env['COSH_MIGRATION_WARNING'] ?? null;
    delete process.env['COSH_MIGRATION_WARNING'];
    const startupWarnings = [
      ...new Set([
        ...configWarnings.map((w) => t(w)),
        ...(effectiveMigrationWarning ? [effectiveMigrationWarning] : []),
        ...(await getStartupWarnings()),
        ...(await getUserStartupWarnings({
          workspaceRoot: process.cwd(),
          useRipgrep: settings.merged.tools?.useRipgrep ?? true,
          useBuiltinRipgrep: settings.merged.tools?.useBuiltinRipgrep ?? true,
        })),
        ...getSettingsWarnings(settings),
      ]),
    ];

    // Render UI, passing necessary config values. Check that there is no command line question.
    if (config.isInteractive()) {
      // Drain any buffered stdin input that accumulated during startup.
      // In login shell scenarios, the system or SSH infrastructure may write
      // diagnostic commands (e.g. shell-type probes) to the TTY before the
      // interactive UI is ready. Between setRawMode(true) and the moment the
      // KeypressProvider attaches its listener, the kernel TTY buffer may hold
      // those bytes. Flushing here prevents them from appearing in the input box.
      if (process.stdin.isTTY) {
        process.stdin.resume();
        // Give the event loop one tick so any pending bytes arrive, then drain.
        await new Promise<void>((resolve) => {
          setImmediate(() => {
            while (process.stdin.read() !== null) {
              // Discard buffered input accumulated during startup
            }
            resolve();
          });
        });
        process.stdin.pause();
      }

      // Load one-time feature tips for first-time users
      let featureTips: FeatureTip[] = [];
      if (!settings.merged.ui?.hideFeatureTipBanner) {
        try {
          featureTips = await getAndMarkUnshownFeatureTips();
        } catch {
          // Non-critical — skip if loading fails
        }
      }

      await startInteractiveUI(
        config,
        settings,
        startupWarnings,
        process.cwd(),
        initializationResult!,
        featureTips,
      );
      return;
    }

    // For non-stream-json mode, initialize config here
    if (inputFormat !== InputFormat.STREAM_JSON) {
      await config.initialize();
    }

    // Print config warnings to stderr for non-interactive mode (no TUI available)
    if (configWarnings.length > 0) {
      for (const w of configWarnings) {
        process.stderr.write(`\x1b[33m${t(w)}\x1b[0m\n`);
      }
    }

    // Only read stdin if NOT in stream-json mode
    // In stream-json mode, stdin is used for protocol messages (control requests, etc.)
    // and should be consumed by StreamJsonInputReader instead
    if (inputFormat !== InputFormat.STREAM_JSON && !process.stdin.isTTY) {
      const stdinData = await readStdin();
      if (stdinData) {
        input = `${stdinData}\n\n${input}`;
      }
    }

    const nonInteractiveConfig = await validateNonInteractiveAuth(
      settings.merged.security?.auth?.useExternal,
      config,
      settings,
    );

    const prompt_id = Math.random().toString(16).slice(2);

    if (inputFormat === InputFormat.STREAM_JSON) {
      const trimmedInput = (input ?? '').trim();

      await runNonInteractiveStreamJson(
        nonInteractiveConfig,
        trimmedInput.length > 0 ? trimmedInput : '',
      );
      await runExitCleanup();
      process.exit(0);
    }

    if (!input) {
      console.error(
        `No input provided via stdin. Input can be provided by piping data into co/copilot or using the --prompt option.`,
      );
      process.exit(1);
    }

    logUserPrompt(config, {
      'event.name': 'user_prompt',
      'event.timestamp': new Date().toISOString(),
      prompt: input,
      prompt_id,
      auth_type: config.getContentGeneratorConfig()?.authType,
      prompt_length: input.length,
    });

    if (config.getDebugMode()) {
      console.log('Session ID: %s', config.getSessionId());
    }

    await runNonInteractive(nonInteractiveConfig, settings, input, prompt_id);
    // Call cleanup before process.exit, which causes cleanup to not run
    await runExitCleanup();
    process.exit(0);
  }
}

function setWindowTitle(title: string, settings: LoadedSettings) {
  if (!settings.merged.ui?.hideWindowTitle) {
    const windowTitle = computeWindowTitle(title);
    process.stdout.write(`\x1b]2;${windowTitle}\x07`);

    process.on('exit', () => {
      process.stdout.write(`\x1b]2;\x07`);
    });
  }
}
