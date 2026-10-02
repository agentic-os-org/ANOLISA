/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import {
  useCallback,
  useMemo,
  useEffect,
  useRef,
  useState,
  useContext,
} from 'react';
import { type PartListUnion } from '@google/genai';
import type { UseHistoryManagerReturn } from './useHistoryManager.js';
import {
  type Logger,
  type Config,
  GitService,
  logSlashCommand,
  makeSlashCommandEvent,
  SlashCommandStatus,
  ToolConfirmationOutcome,
  IdeClient,
} from '@copilot-shell/core';
import { useSessionStats } from '../contexts/SessionContext.js';
import { AppContext } from '../contexts/AppContext.js';
import type {
  Message,
  HistoryItemWithoutId,
  SlashCommandProcessorResult,
  HistoryItem,
  ConfirmationRequest,
  HookMultiSelectRequest,
} from '../types.js';
import { MessageType } from '../types.js';
import type { LoadedSettings } from '../../config/settings.js';
import { type CommandContext, type SlashCommand } from '../commands/types.js';
import { CommandService } from '../../services/CommandService.js';
import { BuiltinCommandLoader } from '../../services/BuiltinCommandLoader.js';
import { FileCommandLoader } from '../../services/FileCommandLoader.js';
import { McpPromptLoader } from '../../services/McpPromptLoader.js';
import { AppEvent, appEvents } from '../../utils/events.js';
import { parseSlashCommand } from '../../utils/commands.js';
import { isSlashCommand } from '../utils/commandUtils.js';
import {
  type ExtensionUpdateAction,
  type ExtensionUpdateStatus,
} from '../state/extensions.js';
import { useKeypress } from './useKeypress.js';
import { t } from '../../i18n/index.js';

type SerializableHistoryItem = Record<string, unknown>;

function serializeHistoryItemForRecording(
  item: Omit<HistoryItem, 'id'>,
): SerializableHistoryItem {
  const clone: SerializableHistoryItem = { ...item };
  if ('timestamp' in clone && clone['timestamp'] instanceof Date) {
    clone['timestamp'] = clone['timestamp'].toISOString();
  }
  return clone;
}

const SLASH_COMMANDS_SKIP_RECORDING = new Set([
  'quit',
  'exit',
  'clear',
  'reset',
  'new',
  'resume',
]);

interface SlashCommandProcessorActions {
  openAuthDialog: () => void;
  openThemeDialog: () => void;
  openEditorDialog: () => void;
  openSettingsDialog: () => void;
  openModelDialog: () => void;
  openPermissionsDialog: () => void;
  openApprovalModeDialog: () => void;
  openResumeDialog: () => void;
  quit: (messages: HistoryItem[]) => void;
  setDebugMessage: (message: string) => void;
  dispatchExtensionStateUpdate: (action: ExtensionUpdateAction) => void;
  addConfirmUpdateExtensionRequest: (request: ConfirmationRequest) => void;
  openSubagentCreateDialog: () => void;
  openAgentsManagerDialog: () => void;
  openSkillsDialog: () => void;
}

/**
 * Hook to define and process slash commands (e.g., /help, /clear).
 */
export const useSlashCommandProcessor = (
  config: Config | null,
  settings: LoadedSettings,
  addItem: UseHistoryManagerReturn['addItem'],
  clearItems: UseHistoryManagerReturn['clearItems'],
  loadHistory: UseHistoryManagerReturn['loadHistory'],
  refreshStatic: () => void,
  toggleVimEnabled: () => Promise<boolean>,
  isProcessing: boolean,
  setIsProcessing: (isProcessing: boolean) => void,
  setGeminiMdFileCount: (count: number) => void,
  actions: SlashCommandProcessorActions,
  extensionsUpdateState: Map<string, ExtensionUpdateStatus>,
  isConfigInitialized: boolean,
  logger: Logger | null,
) => {
  const { stats: sessionStats, startNewSession } = useSessionStats();
  const appContext = useContext(AppContext);
  const dismissWarning = useMemo(
    () => appContext?.dismissWarning ?? ((_: string) => {}),
    [appContext?.dismissWarning],
  );
  const [commands, setCommands] = useState<readonly SlashCommand[]>([]);
  const [reloadTrigger, setReloadTrigger] = useState(0);

  const reloadCommands = useCallback(() => {
    setReloadTrigger((v) => v + 1);
  }, []);
  const [shellConfirmationRequest, setShellConfirmationRequest] =
    useState<null | {
      commands: string[];
      onConfirm: (
        outcome: ToolConfirmationOutcome,
        approvedCommands?: string[],
      ) => void;
    }>(null);
  const [confirmationRequest, setConfirmationRequest] = useState<null | {
    prompt: React.ReactNode;
    onConfirm: (confirmed: boolean) => void;
  }>(null);
  const [hookMultiSelectRequest, setHookMultiSelectRequest] =
    useState<HookMultiSelectRequest | null>(null);

  const [sessionShellAllowlist, setSessionShellAllowlist] = useState(
    new Set<string>(),
  );
  const gitService = useMemo(() => {
    if (!config?.getProjectRoot()) {
      return;
    }
    return new GitService(config.getProjectRoot(), config.storage);
  }, [config]);

  const [pendingItem, setPendingItem] = useState<HistoryItemWithoutId | null>(
    null,
  );

  // AbortController for cancelling async slash commands via ESC
  const abortControllerRef = useRef<AbortController | null>(null);

  const cancelSlashCommand = useCallback(() => {
    if (!abortControllerRef.current) {
      return;
    }
    abortControllerRef.current.abort();
    addItem(
      {
        type: MessageType.INFO,
        text: t('Command cancelled.'),
      },
      Date.now(),
    );
    setPendingItem(null);
    setIsProcessing(false);
  }, [addItem, setPendingItem, setIsProcessing]);

  useKeypress(
    (key) => {
      if (key.name === 'escape') {
        cancelSlashCommand();
      }
    },
    { isActive: isProcessing },
  );

  const pendingHistoryItems = useMemo(() => {
    const items: HistoryItemWithoutId[] = [];
    if (pendingItem != null) {
      items.push(pendingItem);
    }
    return items;
  }, [pendingItem]);

  const addMessage = useCallback(
    (message: Message) => {
      // Convert Message to HistoryItemWithoutId
      let historyItemContent: HistoryItemWithoutId;
      if (message.type === MessageType.ABOUT) {
        historyItemContent = {
          type: 'about',
          systemInfo: message.systemInfo,
        };
      } else if (message.type === MessageType.HELP) {
        historyItemContent = {
          type: 'help',
          timestamp: message.timestamp,
        };
      } else if (message.type === MessageType.STATS) {
        historyItemContent = {
          type: 'stats',
          duration: message.duration,
        };
      } else if (message.type === MessageType.MODEL_STATS) {
        historyItemContent = {
          type: 'model_stats',
        };
      } else if (message.type === MessageType.TOOL_STATS) {
        historyItemContent = {
          type: 'tool_stats',
        };
      } else if (message.type === MessageType.QUIT) {
        historyItemContent = {
          type: 'quit',
          duration: message.duration,
        };
      } else if (message.type === MessageType.COMPRESSION) {
        historyItemContent = {
          type: 'compression',
          compression: message.compression,
        };
      } else if (message.type === MessageType.SUMMARY) {
        historyItemContent = {
          type: 'summary',
          summary: message.summary,
        };
      } else {
        historyItemContent = {
          type: message.type,
          text: message.content,
        };
      }
      addItem(historyItemContent, message.timestamp.getTime());
    },
    [addItem],
  );
  const commandContext = useMemo(
    (): CommandContext => ({
      services: {
        config,
        settings,
        git: gitService,
        logger,
      },
      ui: {
        addItem,
        clear: () => {
          clearItems();
          console.clear();
          refreshStatic();
        },
        loadHistory,
        setDebugMessage: actions.setDebugMessage,
        pendingItem,
        setPendingItem,
        toggleVimEnabled,
        setGeminiMdFileCount,
        reloadCommands,
        extensionsUpdateState,
        dispatchExtensionStateUpdate: actions.dispatchExtensionStateUpdate,
        addConfirmUpdateExtensionRequest:
          actions.addConfirmUpdateExtensionRequest,
        dismissStartupWarning: dismissWarning,
      },
      session: {
        stats: sessionStats,
        sessionShellAllowlist,
        startNewSession,
      },
    }),
    [
      config,
      settings,
      gitService,
      logger,
      loadHistory,
      addItem,
      clearItems,
      refreshStatic,
      sessionStats,
      startNewSession,
      actions,
      pendingItem,
      setPendingItem,
      toggleVimEnabled,
      sessionShellAllowlist,
      setGeminiMdFileCount,
      reloadCommands,
      extensionsUpdateState,
      dismissWarning,
    ],
  );

  useEffect(() => {
    if (!config) {
      return;
    }

    const listener = () => {
      reloadCommands();
    };

    (async () => {
      const ideClient = await IdeClient.getInstance();
      ideClient.addStatusChangeListener(listener);
    })();

    return () => {
      (async () => {
        const ideClient = await IdeClient.getInstance();
        ideClient.removeStatusChangeListener(listener);
      })();
    };
  }, [config, reloadCommands]);

  useEffect(() => {
    const controller = new AbortController();
    const load = async () => {
      const loaders = [
        new McpPromptLoader(config),
        new BuiltinCommandLoader(config),
        new FileCommandLoader(config),
      ];
      const commandService = await CommandService.create(
        loaders,
        controller.signal,
      );
      setCommands(commandService.getCommands());
    };

    load();

    return () => {
      controller.abort();
    };
  }, [config, reloadTrigger, isConfigInitialized]);

  const handleSlashCommand = useCallback(
    async (
      rawQuery: PartListUnion,
      oneTimeShellAllowlist?: Set<string>,
      overwriteConfirmed?: boolean,
    ): Promise<SlashCommandProcessorResult | false> => {
      if (typeof rawQuery !== 'string') {
        return false;
      }

      const trimmed = rawQuery.trim();
      // Only proceed if the input is a '?' query or a recognised '/' command.
      if (!trimmed.startsWith('?') && !isSlashCommand(trimmed, commands)) {
        return false;
      }

      const recordedItems: Array<Omit<HistoryItem, 'id'>> = [];
      const recordItem = (item: Omit<HistoryItem, 'id'>) => {
        recordedItems.push(item);
      };
      const addItemWithRecording: UseHistoryManagerReturn['addItem'] = (
        item,
        timestamp,
      ) => {
        recordItem(item);
        return addItem(item, timestamp);
      };

      setIsProcessing(true);

      // Create a new AbortController for this command execution
      const abortController = new AbortController();
      abortControllerRef.current = abortController;

      const userMessageTimestamp = Date.now();
      addItemWithRecording(
        { type: MessageType.USER, text: trimmed },
        userMessageTimestamp,
      );

      let hasError = false;
      const {
        commandToExecute,
        args,
        canonicalPath: resolvedCommandPath,
      } = parseSlashCommand(trimmed, commands);

      const subcommand =
        resolvedCommandPath.length > 1
          ? resolvedCommandPath.slice(1).join(' ')
          : undefined;

      try {
        if (commandToExecute) {
          if (commandToExecute.action) {
            const fullCommandContext: CommandContext = {
              ...commandContext,
              ui: {
                ...commandContext.ui,
                addItem: addItemWithRecording,
              },
              invocation: {
                raw: trimmed,
                name: commandToExecute.name,
                args,
              },
              abortSignal: abortController.signal,
              overwriteConfirmed,
            };

            // If a one-time list is provided for a "Proceed" action, temporarily
            // augment the session allowlist for this single execution.
            if (oneTimeShellAllowlist && oneTimeShellAllowlist.size > 0) {
              fullCommandContext.session = {
                ...fullCommandContext.session,
                sessionShellAllowlist: new Set([
                  ...fullCommandContext.session.sessionShellAllowlist,
                  ...oneTimeShellAllowlist,
                ]),
              };
            }

            // Use Promise.race to allow ESC to cancel the command immediately
            const abortPromise = new Promise<undefined>((resolve) => {
              abortController.signal.addEventListener(
                'abort',
                () => resolve(undefined),
                { once: true },
              );
            });
            const result = await Promise.race([
              commandToExecute.action(fullCommandContext, args),
              abortPromise,
            ]);

            // If cancelled via ESC, the cancelSlashCommand callback already handled cleanup
            if (abortController.signal.aborted) {
              return { type: 'handled' };
            }

            if (result) {
              switch (result.type) {
                case 'tool':
                  return {
                    type: 'schedule_tool',
                    toolName: result.toolName,
                    toolArgs: result.toolArgs,
                  };
                case 'message':
                  if (result.messageType === 'info') {
                    addMessage({
                      type: MessageType.INFO,
                      content: result.content,
                      timestamp: new Date(),
                    });
                  } else {
                    addMessage({
                      type: MessageType.ERROR,
                      content: result.content,
                      timestamp: new Date(),
                    });
                  }
                  return { type: 'handled' };
                case 'dialog':
                  switch (result.dialog) {
                    case 'auth':
                      actions.openAuthDialog();
                      return { type: 'handled' };
                    case 'theme':
                      actions.openThemeDialog();
                      return { type: 'handled' };
                    case 'editor':
                      actions.openEditorDialog();
                      return { type: 'handled' };
                    case 'settings':
                      actions.openSettingsDialog();
                      return { type: 'handled' };
                    case 'model':
                      actions.openModelDialog();
                      return { type: 'handled' };
                    case 'permissions':
                      actions.openPermissionsDialog();
                      return { type: 'handled' };
                    case 'subagent_create':
                      actions.openSubagentCreateDialog();
                      return { type: 'handled' };
                    case 'subagent_list':
                      actions.openAgentsManagerDialog();
                      return { type: 'handled' };
                    case 'approval-mode':
                      actions.openApprovalModeDialog();
                      return { type: 'handled' };
                    case 'resume':
                      actions.openResumeDialog();
                      return { type: 'handled' };
                    case 'skills':
                      actions.openSkillsDialog();
                      return { type: 'handled' };
                    case 'help':
                      return { type: 'handled' };
                    default: {
                      const unhandled: never = result.dialog;
                      throw new Error(
                        `Unhandled slash command result: ${unhandled}`,
                      );
                    }
                  }
                case 'load_history': {
                  config?.getGeminiClient()?.setHistory(result.clientHistory);
                  config?.getGeminiClient()?.stripThoughtsFromHistory();
                  fullCommandContext.ui.clear();
                  result.history.forEach((item, index) => {
                    fullCommandContext.ui.addItem(item, index);
                  });
                  return { type: 'handled' };
                }

                case 'quit':
                  actions.quit(result.messages);
                  return { type: 'handled' };

                case 'submit_prompt':
                  return {
                    type: 'submit_prompt',
                    content: result.content,
                  };
                case 'confirm_shell_commands': {
                  const { outcome, approvedCommands } = await new Promise<{
                    outcome: ToolConfirmationOutcome;
                    approvedCommands?: string[];
                  }>((resolve) => {
                    setShellConfirmationRequest({
                      commands: result.commandsToConfirm,
                      onConfirm: (
                        resolvedOutcome,
                        resolvedApprovedCommands,
                      ) => {
                        setShellConfirmationRequest(null); // Close the dialog
                        resolve({
                          outcome: resolvedOutcome,
                          approvedCommands: resolvedApprovedCommands,
                        });
                      },
                    });
                  });

                  if (
                    outcome === ToolConfirmationOutcome.Cancel ||
                    !approvedCommands ||
                    approvedCommands.length === 0
                  ) {
                    return { type: 'handled' };
                  }

                  if (outcome === ToolConfirmationOutcome.ProceedAlways) {
                    setSessionShellAllowlist(
                      (prev) => new Set([...prev, ...approvedCommands]),
                    );
                  }

                  return await handleSlashCommand(
                    result.originalInvocation.raw,
                    // Pass the approved commands as a one-time grant for this execution.
                    new Set(approvedCommands),
                  );
                }
                case 'confirm_action': {
                  const { confirmed } = await new Promise<{
                    confirmed: boolean;
                  }>((resolve) => {
                    setConfirmationRequest({
                      prompt: result.prompt,
                      onConfirm: (resolvedConfirmed) => {
                        setConfirmationRequest(null);
                        resolve({ confirmed: resolvedConfirmed });
                      },
                    });
                  });

                  if (!confirmed) {
                    addItemWithRecording(
                      {
                        type: MessageType.INFO,
                        text: 'Operation cancelled.',
                      },
                      Date.now(),
                    );
                    return { type: 'handled' };
                  }

                  return await handleSlashCommand(
                    result.originalInvocation.raw,
                    undefined,
                    true,
                  );
                }
                case 'multi_select_hooks': {
                  const { selected } = await new Promise<{
                    selected: string[];
                  }>((resolve) => {
                    setHookMultiSelectRequest({
                      hookNames: result.hookNames,
                      title: result.title,
                      onSelect: (selectedNames) => {
                        setHookMultiSelectRequest(null);
                        resolve({ selected: selectedNames });
                      },
                      onCancel: () => {
                        setHookMultiSelectRequest(null);
                        resolve({ selected: [] });
                      },
                    });
                  });

                  if (selected.length === 0) {
                    addItemWithRecording(
                      {
                        type: MessageType.INFO,
                        text: 'Operation cancelled.',
                      },
                      Date.now(),
                    );
                    return { type: 'handled' };
                  }

                  const actionResult = result.onSelected(selected);
                  addItemWithRecording(
                    {
                      type:
                        actionResult.messageType === 'error'
                          ? MessageType.ERROR
                          : MessageType.INFO,
                      text: actionResult.content,
                    },
                    Date.now(),
                  );
                  return { type: 'handled' };
                }
                case 'spawn_shell': {
                  appEvents.emit(AppEvent.SpawnShell, result.shell ?? 'bash');
                  return { type: 'handled' };
                }
                case 'stream_messages': {
                  // stream_messages is only used in ACP/Zed integration mode
                  // and should not be returned in interactive UI mode
                  throw new Error(
                    'stream_messages result type is not supported in interactive mode',
                  );
                }
                default: {
                  const unhandled: never = result;
                  throw new Error(
                    `Unhandled slash command result: ${unhandled}`,
                  );
                }
              }
            }

            return { type: 'handled' };
          } else if (commandToExecute.subCommands) {
            const helpText = `Command '/${commandToExecute.name}' requires a subcommand. Available:\n${commandToExecute.subCommands
              .map((sc) => `  - ${sc.name}: ${sc.description || ''}`)
              .join('\n')}`;
            addMessage({
              type: MessageType.INFO,
              content: helpText,
              timestamp: new Date(),
            });
            return { type: 'handled' };
          }
        }

        addMessage({
          type: MessageType.ERROR,
          content: `Unknown command: ${trimmed}`,
          timestamp: new Date(),
        });

        return { type: 'handled' };
      } catch (e: unknown) {
        // If cancelled via ESC, the cancelSlashCommand callback already handled cleanup
        if (abortController.signal.aborted) {
          return { type: 'handled' };
        }
        hasError = true;
        if (config) {
          const event = makeSlashCommandEvent({
            command: resolvedCommandPath[0],
            subcommand,
            status: SlashCommandStatus.ERROR,
          });
          logSlashCommand(config, event);
        }
        addItemWithRecording(
          {
            type: MessageType.ERROR,
            text: e instanceof Error ? e.message : String(e),
          },
          Date.now(),
        );
        return { type: 'handled' };
      } finally {
        if (config?.getChatRecordingService) {
          const chatRecorder = config.getChatRecordingService();
          const primaryCommand =
            resolvedCommandPath[0] ||
            trimmed.replace(/^[/?]/, '').split(/\s+/)[0] ||
            trimmed;
          const shouldRecord =
            !SLASH_COMMANDS_SKIP_RECORDING.has(primaryCommand);
          try {
            if (shouldRecord) {
              chatRecorder?.recordSlashCommand({
                phase: 'invocation',
                rawCommand: trimmed,
              });
              const outputItems = recordedItems
                .filter((item) => item.type !== 'user')
                .map(serializeHistoryItemForRecording);
              chatRecorder?.recordSlashCommand({
                phase: 'result',
                rawCommand: trimmed,
                outputHistoryItems: outputItems,
              });
            }
          } catch (recordError) {
            if (config.getDebugMode()) {
              console.error(
                '[slashCommand] Failed to record slash command:',
                recordError,
              );
            }
          }
        }
        if (
          config &&
          resolvedCommandPath[0] &&
          !hasError &&
          !abortController.signal.aborted
        ) {
          const event = makeSlashCommandEvent({
            command: resolvedCommandPath[0],
            subcommand,
            status: SlashCommandStatus.SUCCESS,
          });
          logSlashCommand(config, event);
        }
        setIsProcessing(false);
      }
    },
    [
      config,
      addItem,
      actions,
      commands,
      commandContext,
      addMessage,
      setShellConfirmationRequest,
      setSessionShellAllowlist,
      setIsProcessing,
      setConfirmationRequest,
    ],
  );

  return {
    handleSlashCommand,
    slashCommands: commands,
    pendingHistoryItems,
    commandContext,
    shellConfirmationRequest,
    confirmationRequest,
    hookMultiSelectRequest,
  };
};
