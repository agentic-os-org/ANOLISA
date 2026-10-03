/**
 * @license
 * Copyright 2026 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type { Config } from '../config/config.js';
import type { HookPlanner, HookEventContext } from './hookPlanner.js';
import type { HookRunner } from './hookRunner.js';
import type { HookAggregator, AggregatedHookResult } from './hookAggregator.js';
import { HookEventName } from './types.js';
import type {
  HookConfig,
  HookInput,
  HookExecutionResult,
  UserPromptSubmitInput,
  StopInput,
  PreToolUseInput,
  PostToolUseFailureInput,
  PostToolUseInput,
  NotificationInput,
  SessionStartInput,
  SessionEndInput,
  PreCompactInput,
  BeforeModelInput,
  AfterModelInput,
  BeforeToolSelectionInput,
  McpToolContext,
} from './types.js';
import type {
  NotificationType,
  SessionStartSource,
  SessionEndReason,
  PreCompactTrigger,
} from './types.js';
import { createDebugLogger } from '../utils/debugLogger.js';
import { defaultHookTranslator } from './hookTranslator.js';
import type {
  GenerateContentParameters,
  GenerateContentResponse,
} from '@google/genai';

const debugLogger = createDebugLogger('TRUSTED_HOOKS');

/**
 * Hook event bus that coordinates hook execution across the system
 */
export class HookEventHandler {
  private readonly config: Config;
  private readonly hookPlanner: HookPlanner;
  private readonly hookRunner: HookRunner;
  private readonly hookAggregator: HookAggregator;

  constructor(
    config: Config,
    hookPlanner: HookPlanner,
    hookRunner: HookRunner,
    hookAggregator: HookAggregator,
  ) {
    this.config = config;
    this.hookPlanner = hookPlanner;
    this.hookRunner = hookRunner;
    this.hookAggregator = hookAggregator;
  }

  /**
   * Fire a UserPromptSubmit event
   * Called by handleHookExecutionRequest - executes hooks directly
   */
  async fireUserPromptSubmitEvent(
    prompt: string,
  ): Promise<AggregatedHookResult> {
    const input: UserPromptSubmitInput = {
      ...this.createBaseInput(HookEventName.UserPromptSubmit),
      prompt,
    };

    return this.executeHooks(HookEventName.UserPromptSubmit, input);
  }

  /**
   * Fire a PreToolUse event
   * Called before tool execution to allow hooks to inspect/modify/block tool calls
   */
  async firePreToolUseEvent(
    toolName: string,
    toolInput: Record<string, unknown>,
    skillContext?: import('./types.js').SkillToolContext,
    toolUseId?: string,
    signal?: AbortSignal,
  ): Promise<AggregatedHookResult> {
    debugLogger.info(
      `[Hook Debug] hookEventHandler.firePreToolUseEvent: tool=${toolName}`,
    );
    const input: PreToolUseInput = {
      ...this.createBaseInput(HookEventName.PreToolUse),
      tool_name: toolName,
      tool_input: toolInput,
      ...(skillContext && { skill_context: skillContext }),
      ...(toolUseId && { tool_use_id: toolUseId }),
    };

    const result = await this.executeHooks(
      HookEventName.PreToolUse,
      input,
      undefined,
      signal,
    );
    debugLogger.info(
      `[Hook Debug] hookEventHandler.firePreToolUseEvent: completed, outputs=${result.allOutputs.length}, errors=${result.errors.length}`,
    );
    return result;
  }

  /**
   * Fire a Stop event
   * Called by handleHookExecutionRequest - executes hooks directly
   */
  async fireStopEvent(
    stopHookActive: boolean = false,
    lastAssistantMessage: string = '',
  ): Promise<AggregatedHookResult> {
    const input: StopInput = {
      ...this.createBaseInput(HookEventName.Stop),
      stop_hook_active: stopHookActive,
      last_assistant_message: lastAssistantMessage,
    };

    return this.executeHooks(HookEventName.Stop, input);
  }

  /**
   * Fire a PostToolUseFailure event
   * Called after a tool execution fails, allowing hooks to react and request bypasses
   */
  async firePostToolUseFailureEvent(
    toolUseId: string,
    toolName: string,
    toolInput: Record<string, unknown>,
    error: string,
    errorType?: string,
  ): Promise<AggregatedHookResult> {
    const input: PostToolUseFailureInput = {
      ...this.createBaseInput(HookEventName.PostToolUseFailure),
      tool_use_id: toolUseId,
      tool_name: toolName,
      tool_input: toolInput,
      error,
      error_type: errorType,
    };

    return this.executeHooks(HookEventName.PostToolUseFailure, input);
  }

  /**
   * Fire a PostToolUse event
   * Called after a tool executes successfully, for result auditing, context injection,
   * or hiding sensitive output from the agent
   */
  async firePostToolUseEvent(
    toolName: string,
    toolInput: Record<string, unknown>,
    toolResponse: Record<string, unknown>,
    mcpContext?: McpToolContext,
    originalRequestName?: string,
    toolUseId?: string,
    signal?: AbortSignal,
  ): Promise<AggregatedHookResult> {
    debugLogger.info(
      `[Hook Debug] hookEventHandler.firePostToolUseEvent: tool=${toolName}`,
    );
    const input: PostToolUseInput = {
      ...this.createBaseInput(HookEventName.PostToolUse),
      tool_name: toolName,
      tool_input: toolInput,
      tool_response: toolResponse,
      ...(mcpContext && { mcp_context: mcpContext }),
      ...(originalRequestName && {
        original_request_name: originalRequestName,
      }),
      ...(toolUseId && { tool_use_id: toolUseId }),
    };

    const context: HookEventContext = { toolName };
    const result = await this.executeHooks(
      HookEventName.PostToolUse,
      input,
      context,
      signal,
    );
    debugLogger.info(
      `[Hook Debug] hookEventHandler.firePostToolUseEvent: completed, outputs=${result.allOutputs.length}, errors=${result.errors.length}`,
    );
    return result;
  }

  /**
   * Fire a Notification event
   * Fires when the CLI emits a system alert (e.g., Tool Permissions).
   * Observability only - cannot block alerts or grant permissions automatically.
   */
  async fireNotificationEvent(
    type: NotificationType,
    message: string,
    details: Record<string, unknown>,
  ): Promise<AggregatedHookResult> {
    const input: NotificationInput = {
      ...this.createBaseInput(HookEventName.Notification),
      notification_type: type,
      message,
      details,
    };

    return this.executeHooks(HookEventName.Notification, input);
  }

  /**
   * Fire a SessionStart event
   * Fires on application startup, resuming a session, or after a /clear command.
   * Advisory only - continue and decision fields are ignored.
   */
  async fireSessionStartEvent(
    source: SessionStartSource,
  ): Promise<AggregatedHookResult> {
    debugLogger.info(
      `[Hook Debug] hookEventHandler.fireSessionStartEvent: source=${source}`,
    );
    const input: SessionStartInput = {
      ...this.createBaseInput(HookEventName.SessionStart),
      source,
    };

    const context: HookEventContext = { trigger: source };
    const result = await this.executeHooks(
      HookEventName.SessionStart,
      input,
      context,
    );
    debugLogger.info(
      `[Hook Debug] hookEventHandler.fireSessionStartEvent: completed, outputs=${result.allOutputs.length}, errors=${result.errors.length}`,
    );
    return result;
  }

  /**
   * Fire a SessionEnd event
   * Fires when the CLI exits or a session is cleared.
   * Best effort - the CLI will not wait for this hook to complete.
   */
  async fireSessionEndEvent(
    reason: SessionEndReason,
  ): Promise<AggregatedHookResult> {
    debugLogger.info(
      `[Hook Debug] hookEventHandler.fireSessionEndEvent: reason=${reason}`,
    );
    const input: SessionEndInput = {
      ...this.createBaseInput(HookEventName.SessionEnd),
      reason,
    };

    const context: HookEventContext = { trigger: reason };
    const result = await this.executeHooks(
      HookEventName.SessionEnd,
      input,
      context,
    );
    debugLogger.info(
      `[Hook Debug] hookEventHandler.fireSessionEndEvent: completed, outputs=${result.allOutputs.length}, errors=${result.errors.length}`,
    );
    return result;
  }

  /**
   * Fire a PreCompact event
   * Fires before the CLI summarizes history to save tokens.
   * Advisory only - cannot block or modify the compression process.
   */
  async firePreCompactEvent(
    trigger: PreCompactTrigger,
  ): Promise<AggregatedHookResult> {
    const input: PreCompactInput = {
      ...this.createBaseInput(HookEventName.PreCompact),
      trigger,
    };

    const context: HookEventContext = { trigger };
    return this.executeHooks(HookEventName.PreCompact, input, context);
  }

  /**
   * Fire a BeforeModel event
   * Called before sending a request to the LLM.
   * Can modify the request, provide a synthetic response, or block the call.
   */
  async fireBeforeModelEvent(
    llmRequest: GenerateContentParameters,
  ): Promise<AggregatedHookResult> {
    debugLogger.info(
      '[Hook Debug] hookEventHandler.fireBeforeModelEvent: translating SDK request to hook format',
    );
    const input: BeforeModelInput = {
      ...this.createBaseInput(HookEventName.BeforeModel),
      llm_request: defaultHookTranslator.toHookLLMRequest(llmRequest),
    };
    debugLogger.debug(
      `[Hook Debug] hookEventHandler.fireBeforeModelEvent: input.llm_request.model=${input.llm_request.model}`,
    );

    const result = await this.executeHooks(HookEventName.BeforeModel, input);
    debugLogger.info(
      `[Hook Debug] hookEventHandler.fireBeforeModelEvent: completed, outputs=${result.allOutputs.length}, errors=${result.errors.length}`,
    );
    return result;
  }

  /**
   * Fire an AfterModel event
   * Called after receiving a response from the LLM.
   * Can modify the response, stop execution, or observe for logging.
   */
  async fireAfterModelEvent(
    llmRequest: GenerateContentParameters,
    llmResponse: GenerateContentResponse,
  ): Promise<AggregatedHookResult> {
    debugLogger.info(
      '[Hook Debug] hookEventHandler.fireAfterModelEvent: translating SDK request/response to hook format',
    );
    const input: AfterModelInput = {
      ...this.createBaseInput(HookEventName.AfterModel),
      llm_request: defaultHookTranslator.toHookLLMRequest(llmRequest),
      llm_response: defaultHookTranslator.toHookLLMResponse(llmResponse),
    };
    debugLogger.debug(
      `[Hook Debug] hookEventHandler.fireAfterModelEvent: response.text.length=${input.llm_response.text?.length ?? 0}`,
    );

    const result = await this.executeHooks(HookEventName.AfterModel, input);
    debugLogger.info(
      `[Hook Debug] hookEventHandler.fireAfterModelEvent: completed, outputs=${result.allOutputs.length}, errors=${result.errors.length}`,
    );
    return result;
  }

  /**
   * Fire a BeforeToolSelection event
   * Called before selecting tools for the LLM request.
   * Can modify tool configuration (mode, allowed function names).
   */
  async fireBeforeToolSelectionEvent(
    llmRequest: GenerateContentParameters,
  ): Promise<AggregatedHookResult> {
    debugLogger.info(
      '[Hook Debug] hookEventHandler.fireBeforeToolSelectionEvent: translating SDK request to hook format',
    );
    const input: BeforeToolSelectionInput = {
      ...this.createBaseInput(HookEventName.BeforeToolSelection),
      llm_request: defaultHookTranslator.toHookLLMRequest(llmRequest),
    };

    const result = await this.executeHooks(
      HookEventName.BeforeToolSelection,
      input,
    );
    debugLogger.info(
      `[Hook Debug] hookEventHandler.fireBeforeToolSelectionEvent: completed, outputs=${result.allOutputs.length}, errors=${result.errors.length}`,
    );
    return result;
  }

  /**
   * Execute hooks for a specific event (direct execution without MessageBus)
   * Used as fallback when MessageBus is not available
   *
   * The optional `signal` propagates caller cancellation (e.g. Ctrl+C) so
   * hook process trees are terminated instead of being orphaned.
   * Only the tool-use call sites (PreToolUse/PostToolUse) currently supply
   * a signal; other events accept one but have no cancellation source yet,
   * so their hooks are still bounded only by the per-hook timeout.
   */
  private async executeHooks(
    eventName: HookEventName,
    input: HookInput,
    context?: HookEventContext,
    signal?: AbortSignal,
  ): Promise<AggregatedHookResult> {
    try {
      // Create execution plan
      const plan = this.hookPlanner.createExecutionPlan(eventName, context);

      if (!plan || plan.hookConfigs.length === 0) {
        return {
          success: true,
          allOutputs: [],
          errors: [],
          totalDuration: 0,
        };
      }

      const onHookStart = (_config: HookConfig, _index: number) => {
        // Hook start event (telemetry removed)
      };

      const onHookEnd = (_config: HookConfig, _result: HookExecutionResult) => {
        // Hook end event (telemetry removed)
      };

      // Execute hooks according to the plan's strategy
      const results = plan.sequential
        ? await this.hookRunner.executeHooksSequential(
            plan.hookConfigs,
            eventName,
            input,
            onHookStart,
            onHookEnd,
            signal,
          )
        : await this.hookRunner.executeHooksParallel(
            plan.hookConfigs,
            eventName,
            input,
            onHookStart,
            onHookEnd,
            signal,
          );

      // Aggregate results
      const aggregated = this.hookAggregator.aggregateResults(
        results,
        eventName,
      );

      // Process common hook output fields centrally
      this.processCommonHookOutputFields(aggregated);

      return aggregated;
    } catch (error) {
      debugLogger.error(`Hook event bus error for ${eventName}: ${error}`);

      return {
        success: false,
        allOutputs: [],
        errors: [error instanceof Error ? error : new Error(String(error))],
        totalDuration: 0,
      };
    }
  }

  /**
   * Create base hook input with common fields
   */
  private createBaseInput(eventName: HookEventName): HookInput {
    // Get the transcript path from the Config
    const transcriptPath = this.config.getTranscriptPath();

    return {
      session_id: this.config.getSessionId(),
      run_id: this.config.getCurrentRunId(),
      transcript_path: transcriptPath,
      cwd: this.config.getWorkingDir(),
      hook_event_name: eventName,
      timestamp: new Date().toISOString(),
    };
  }

  /**
   * Process common hook output fields centrally
   */
  private processCommonHookOutputFields(
    aggregated: AggregatedHookResult,
  ): void {
    if (!aggregated.finalOutput) {
      return;
    }

    // Handle systemMessage - show to user in transcript mode (not to agent)
    const systemMessage = aggregated.finalOutput.systemMessage;
    if (systemMessage && !aggregated.finalOutput.suppressOutput) {
      debugLogger.warn(`Hook system message: ${systemMessage}`);
    }

    // Handle suppressOutput - already handled by not logging above when true

    // Handle continue=false - this should stop the entire agent execution
    if (aggregated.finalOutput.continue === false) {
      const stopReason =
        aggregated.finalOutput.stopReason ||
        aggregated.finalOutput.reason ||
        'No reason provided';
      debugLogger.debug(`Hook requested to stop execution: ${stopReason}`);

      // Note: The actual stopping of execution must be handled by integration points
      // as they need to interpret this signal in the context of their specific workflow
      // This is just logging the request centrally
    }
  }
}
