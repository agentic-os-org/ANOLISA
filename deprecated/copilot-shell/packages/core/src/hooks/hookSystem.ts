/**
 * @license
 * Copyright 2026 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type { Config } from '../config/config.js';
import { HookRegistry } from './hookRegistry.js';
import { HookRunner } from './hookRunner.js';
import { HookAggregator } from './hookAggregator.js';
import { HookPlanner } from './hookPlanner.js';
import { HookEventHandler } from './hookEventHandler.js';
import type { HookRegistryEntry } from './hookRegistry.js';
import { createDebugLogger } from '../utils/debugLogger.js';
import type {
  DefaultHookOutput,
  PreToolUseHookOutput,
  PostToolUseHookOutput,
  PostToolUseFailureHookOutput,
  BeforeModelHookOutput,
  AfterModelHookOutput,
  BeforeToolSelectionHookOutput,
  HookEventName,
  HookConfig,
  McpToolContext,
  NotificationType,
  SessionStartSource,
  SessionEndReason,
  PreCompactTrigger,
} from './types.js';
import { createHookOutput, HooksConfigSource } from './types.js';
import type {
  GenerateContentParameters,
  GenerateContentResponse,
} from '@google/genai';

const debugLogger = createDebugLogger('TRUSTED_HOOKS');

/**
 * Main hook system that coordinates all hook-related functionality
 */

export class HookSystem {
  private readonly hookRegistry: HookRegistry;
  private readonly hookRunner: HookRunner;
  private readonly hookAggregator: HookAggregator;
  private readonly hookPlanner: HookPlanner;
  private readonly hookEventHandler: HookEventHandler;

  constructor(config: Config) {
    // Initialize components
    this.hookRegistry = new HookRegistry(config);
    this.hookRunner = new HookRunner();
    this.hookAggregator = new HookAggregator();
    this.hookPlanner = new HookPlanner(this.hookRegistry);
    this.hookEventHandler = new HookEventHandler(
      config,
      this.hookPlanner,
      this.hookRunner,
      this.hookAggregator,
    );
  }

  /**
   * Initialize the hook system
   */
  async initialize(): Promise<void> {
    await this.hookRegistry.initialize();
    debugLogger.debug('Hook system initialized successfully');
  }

  /**
   * Get the hook event bus for firing events
   */
  getEventHandler(): HookEventHandler {
    return this.hookEventHandler;
  }

  /**
   * Get hook registry for management operations
   */
  getRegistry(): HookRegistry {
    return this.hookRegistry;
  }

  /**
   * Enable or disable a hook
   */
  setHookEnabled(hookName: string, enabled: boolean): void {
    this.hookRegistry.setHookEnabled(hookName, enabled);
  }

  /**
   * Get all registered hooks for display/management
   */
  getAllHooks(): HookRegistryEntry[] {
    return this.hookRegistry.getAllHooks();
  }

  async fireUserPromptSubmitEvent(
    prompt: string,
  ): Promise<DefaultHookOutput | undefined> {
    const result =
      await this.hookEventHandler.fireUserPromptSubmitEvent(prompt);
    return result.finalOutput
      ? createHookOutput('UserPromptSubmit', result.finalOutput)
      : undefined;
  }

  async firePreToolUseEvent(
    toolName: string,
    toolInput: Record<string, unknown>,
    skillContext?: import('./types.js').SkillToolContext,
    toolUseId?: string,
    signal?: AbortSignal,
  ): Promise<PreToolUseHookOutput | undefined> {
    debugLogger.info(
      `[Hook Debug] hookSystem.firePreToolUseEvent: entering facade, tool=${toolName}`,
    );
    const result = await this.hookEventHandler.firePreToolUseEvent(
      toolName,
      toolInput,
      skillContext,
      toolUseId,
      signal,
    );
    const output = result.finalOutput
      ? (createHookOutput(
          'PreToolUse',
          result.finalOutput,
        ) as PreToolUseHookOutput)
      : undefined;
    // Carry per-hook notifications from the aggregator so the scheduler can
    // emit them as structured data to the UI layer.
    if (output && result.notifications?.length) {
      output.notifications = result.notifications;
    }
    debugLogger.info(
      `[Hook Debug] hookSystem.firePreToolUseEvent: facade returning, hasOutput=${!!output}`,
    );
    return output;
  }

  async fireStopEvent(
    stopHookActive: boolean = false,
    lastAssistantMessage: string = '',
  ): Promise<DefaultHookOutput | undefined> {
    const result = await this.hookEventHandler.fireStopEvent(
      stopHookActive,
      lastAssistantMessage,
    );
    return result.finalOutput
      ? createHookOutput('Stop', result.finalOutput)
      : undefined;
  }

  async firePostToolUseFailureEvent(
    toolUseId: string,
    toolName: string,
    toolInput: Record<string, unknown>,
    error: string,
    errorType?: string,
  ): Promise<PostToolUseFailureHookOutput | undefined> {
    const result = await this.hookEventHandler.firePostToolUseFailureEvent(
      toolUseId,
      toolName,
      toolInput,
      error,
      errorType,
    );
    const output = result.finalOutput
      ? (createHookOutput(
          'PostToolUseFailure',
          result.finalOutput,
        ) as PostToolUseFailureHookOutput)
      : undefined;
    if (output && result.notifications?.length) {
      output.notifications = result.notifications;
    }
    return output;
  }

  async firePostToolUseEvent(
    toolName: string,
    toolInput: Record<string, unknown>,
    toolResponse: Record<string, unknown>,
    mcpContext?: McpToolContext,
    originalRequestName?: string,
    toolUseId?: string,
    signal?: AbortSignal,
  ): Promise<PostToolUseHookOutput | undefined> {
    debugLogger.info(
      `[Hook Debug] hookSystem.firePostToolUseEvent: entering facade, tool=${toolName}`,
    );
    const result = await this.hookEventHandler.firePostToolUseEvent(
      toolName,
      toolInput,
      toolResponse,
      mcpContext,
      originalRequestName,
      toolUseId,
      signal,
    );
    const output = result.finalOutput
      ? (createHookOutput(
          'PostToolUse',
          result.finalOutput,
        ) as PostToolUseHookOutput)
      : undefined;
    // Carry per-hook notifications from the aggregator so the scheduler can
    // emit them as structured data to the UI layer (mirrors PreToolUse).
    if (output && result.notifications?.length) {
      output.notifications = result.notifications;
    }
    debugLogger.info(
      `[Hook Debug] hookSystem.firePostToolUseEvent: facade returning, hasOutput=${!!output}`,
    );
    return output;
  }

  async fireNotificationEvent(
    type: NotificationType,
    message: string,
    details: Record<string, unknown>,
  ): Promise<DefaultHookOutput | undefined> {
    const result = await this.hookEventHandler.fireNotificationEvent(
      type,
      message,
      details,
    );
    return result.finalOutput
      ? createHookOutput('Notification', result.finalOutput)
      : undefined;
  }

  async fireSessionStartEvent(
    source: SessionStartSource,
  ): Promise<DefaultHookOutput | undefined> {
    debugLogger.info(
      `[Hook Debug] hookSystem.fireSessionStartEvent: entering facade, source=${source}`,
    );
    const result = await this.hookEventHandler.fireSessionStartEvent(source);
    const output = result.finalOutput
      ? createHookOutput('SessionStart', result.finalOutput)
      : undefined;
    debugLogger.info(
      `[Hook Debug] hookSystem.fireSessionStartEvent: facade returning, hasOutput=${!!output}`,
    );
    return output;
  }

  async fireSessionEndEvent(
    reason: SessionEndReason,
  ): Promise<DefaultHookOutput | undefined> {
    debugLogger.info(
      `[Hook Debug] hookSystem.fireSessionEndEvent: entering facade, reason=${reason}`,
    );
    const result = await this.hookEventHandler.fireSessionEndEvent(reason);
    const output = result.finalOutput
      ? createHookOutput('SessionEnd', result.finalOutput)
      : undefined;
    debugLogger.info(
      `[Hook Debug] hookSystem.fireSessionEndEvent: facade returning, hasOutput=${!!output}`,
    );
    return output;
  }

  async firePreCompactEvent(
    trigger: PreCompactTrigger,
  ): Promise<DefaultHookOutput | undefined> {
    const result = await this.hookEventHandler.firePreCompactEvent(trigger);
    return result.finalOutput
      ? createHookOutput('PreCompact', result.finalOutput)
      : undefined;
  }

  async fireBeforeModelEvent(
    llmRequest: GenerateContentParameters,
  ): Promise<BeforeModelHookOutput | undefined> {
    debugLogger.info(
      '[Hook Debug] hookSystem.fireBeforeModelEvent: entering facade',
    );
    const result = await this.hookEventHandler.fireBeforeModelEvent(llmRequest);
    const output = result.finalOutput
      ? (createHookOutput(
          'BeforeModel',
          result.finalOutput,
        ) as BeforeModelHookOutput)
      : undefined;
    debugLogger.info(
      `[Hook Debug] hookSystem.fireBeforeModelEvent: facade returning, hasOutput=${!!output}`,
    );
    return output;
  }

  async fireAfterModelEvent(
    llmRequest: GenerateContentParameters,
    llmResponse: GenerateContentResponse,
  ): Promise<AfterModelHookOutput | undefined> {
    debugLogger.info(
      '[Hook Debug] hookSystem.fireAfterModelEvent: entering facade',
    );
    const result = await this.hookEventHandler.fireAfterModelEvent(
      llmRequest,
      llmResponse,
    );
    const output = result.finalOutput
      ? (createHookOutput(
          'AfterModel',
          result.finalOutput,
        ) as AfterModelHookOutput)
      : undefined;
    // Carry per-hook notifications from the aggregator so the caller can
    // surface non-blocking AfterModel messages to the UI (mirrors PreToolUse/PostToolUse).
    if (output && result.notifications?.length) {
      output.notifications = result.notifications;
    }
    debugLogger.info(
      `[Hook Debug] hookSystem.fireAfterModelEvent: facade returning, hasOutput=${!!output}`,
    );
    return output;
  }

  async fireBeforeToolSelectionEvent(
    llmRequest: GenerateContentParameters,
  ): Promise<BeforeToolSelectionHookOutput | undefined> {
    debugLogger.info(
      '[Hook Debug] hookSystem.fireBeforeToolSelectionEvent: entering facade',
    );
    const result =
      await this.hookEventHandler.fireBeforeToolSelectionEvent(llmRequest);
    const output = result.finalOutput
      ? (createHookOutput(
          'BeforeToolSelection',
          result.finalOutput,
        ) as BeforeToolSelectionHookOutput)
      : undefined;
    debugLogger.info(
      `[Hook Debug] hookSystem.fireBeforeToolSelectionEvent: facade returning, hasOutput=${!!output}`,
    );
    return output;
  }

  /**
   * Dynamically register a hook for the current session.
   * Used by /hooks install to activate hooks immediately without restart.
   */
  registerHook(
    eventName: HookEventName,
    hookConfig: HookConfig,
    source: HooksConfigSource = HooksConfigSource.User,
  ): void {
    this.hookRegistry.registerHook(eventName, hookConfig, source);
  }
}
