/**
 * @license
 * Copyright 2026 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import {
  HookEventName,
  DefaultHookOutput,
  PreToolUseHookOutput,
  PostToolUseHookOutput,
  StopHookOutput,
  PermissionRequestHookOutput,
  BeforeModelHookOutput,
  AfterModelHookOutput,
  BeforeToolSelectionHookOutput,
} from './types.js';
import type {
  HookOutput,
  HookConfig,
  HookExecutionResult,
  BeforeToolSelectionOutput,
  HookDecision,
} from './types.js';

/**
 * A hook output paired with the display name of the hook that produced it.
 * Used by merge strategies that need to attribute messages to their source
 * (e.g. prefixing systemMessage with `[name]` when multiple hooks contribute).
 */
interface NamedHookOutput {
  name: string;
  output: HookOutput;
}

/**
 * Derive a human-readable display name for a hook. Prefers the explicit `name`
 * field; falls back to the command basename if name is absent.
 */
function getHookDisplayName(config: HookConfig | undefined): string {
  if (!config) return 'hook';
  if (config.name && config.name.trim().length > 0) return config.name;
  const cmd = (config.command || '').trim();
  if (!cmd) return 'hook';
  // Take the first token (command binary) and strip path.
  const firstToken = cmd.split(/\s+/)[0] ?? '';
  const basename = firstToken.split('/').pop() ?? '';
  return basename || 'hook';
}

/**
 * Per-hook notification for UI display (hookName + message pair).
 * The optional `decision` field lets the UI choose color and icon based on
 * the hook's outcome (allow / approve / ask / block / deny / undefined).
 */
export interface HookNotification {
  hookName: string;
  message: string;
  decision?: HookDecision;
}

/**
 * Aggregated result from multiple hook executions
 */
export interface AggregatedHookResult {
  success: boolean;
  allOutputs: HookOutput[];
  errors: Error[];
  totalDuration: number;
  finalOutput?: HookOutput;
  /** Per-hook UI notifications (hookName + message). */
  notifications?: HookNotification[];
}

/**
 * HookAggregator merges multiple hook outputs using event-specific rules.
 *
 * Different events have different merging strategies:
 * - PreToolUse/PostToolUse: OR logic for decisions, concatenation for messages
 */
export class HookAggregator {
  /**
   * Aggregate results from multiple hook executions
   */
  aggregateResults(
    results: HookExecutionResult[],
    eventName: HookEventName,
  ): AggregatedHookResult {
    const allOutputs: HookOutput[] = [];
    const namedOutputs: NamedHookOutput[] = [];
    const errors: Error[] = [];
    let totalDuration = 0;

    for (const result of results) {
      totalDuration += result.duration;

      if (!result.success && result.error) {
        errors.push(result.error);
      }

      if (result.output) {
        allOutputs.push(result.output);
        namedOutputs.push({
          name: getHookDisplayName(result.hookConfig),
          output: result.output,
        });
      }
    }

    const success = errors.length === 0;
    const finalOutput = this.mergeOutputs(namedOutputs, eventName);

    // Build per-hook notifications for UI display. Each hook that provides a
    // systemMessage (or falls back to reason) gets its own notification entry
    // so the UI can render them as independent visual elements with the hook
    // name clearly attributed.
    const notifications: HookNotification[] = [];
    for (const { name, output } of namedOutputs) {
      const msg = output.systemMessage ?? output.reason;
      if (msg) {
        notifications.push({
          hookName: name,
          message: msg,
          decision: output.decision,
        });
      }
    }

    return {
      success,
      allOutputs,
      errors,
      totalDuration,
      finalOutput,
      notifications: notifications.length > 0 ? notifications : undefined,
    };
  }

  /**
   * Merge multiple hook outputs based on event type
   */
  private mergeOutputs(
    named: NamedHookOutput[],
    eventName: HookEventName,
  ): HookOutput | undefined {
    if (named.length === 0) {
      return undefined;
    }

    if (named.length === 1) {
      // Single-hook fast path: pass through without `[name]` prefixing to
      // preserve backward-compatible output for the common case.
      return this.createSpecificHookOutput(named[0].output, eventName);
    }

    // For merge strategies that don't need hook names, strip them once.
    const outputs = named.map((n) => n.output);

    let merged: HookOutput;

    switch (eventName) {
      case HookEventName.UserPromptSubmit:
      case HookEventName.PreToolUse:
      case HookEventName.PostToolUse:
      case HookEventName.PostToolUseFailure:
      case HookEventName.Stop:
        merged = this.mergeWithOrLogic(named);
        break;
      case HookEventName.PermissionRequest:
        merged = this.mergePermissionRequestOutputs(outputs);
        break;
      case HookEventName.BeforeModel:
      case HookEventName.AfterModel:
        merged = this.mergeWithFieldReplacement(outputs);
        break;
      case HookEventName.BeforeToolSelection:
        merged = this.mergeToolSelectionOutputs(
          outputs as BeforeToolSelectionOutput[],
        );
        break;
      default:
        merged = this.mergeSimple(outputs);
    }

    return this.createSpecificHookOutput(merged, eventName);
  }

  /**
   * Merge outputs using OR logic for decisions and concatenation for messages.
   *
   * Rules:
   * - Any "block" or "deny" decision results in blocking (most restrictive wins)
   * - Reasons are concatenated with newlines
   * - systemMessage values from multiple hooks are concatenated as
   *   `[hookName] message` on separate lines, so users can see which hook
   *   contributed which message (fixes the pre-existing "last write wins"
   *   ambiguity where the triggering hook's message could be silently
   *   overridden by a later allow hook).
   * - continue=false takes precedence over continue=true
   * - Additional context is concatenated
   */
  private mergeWithOrLogic(named: NamedHookOutput[]): HookOutput {
    const merged: HookOutput = {};
    const reasons: string[] = [];
    const additionalContexts: string[] = [];
    const systemMessages: string[] = [];
    let hasBlock = false;
    let hasAsk = false;
    let hasContinueFalse = false;
    let stopReason: string | undefined;
    const otherHookSpecificFields: Record<string, unknown> = {};

    for (const { name, output } of named) {
      // Check for blocking decisions
      if (output.decision === 'block' || output.decision === 'deny') {
        hasBlock = true;
      } else if (output.decision === 'ask') {
        // ask decision is only tracked if no blocking decision found yet
        if (!hasBlock) {
          hasAsk = true;
        }
      }

      // Collect reasons
      if (output.reason) {
        reasons.push(output.reason);
      }

      // Check continue flag
      if (output.continue === false) {
        hasContinueFalse = true;
        if (output.stopReason) {
          stopReason = output.stopReason;
        }
      }

      // Extract additional context
      this.extractAdditionalContext(output, additionalContexts);

      // Collect other hookSpecificOutput fields (later values win)
      if (output.hookSpecificOutput) {
        for (const [key, value] of Object.entries(output.hookSpecificOutput)) {
          if (key !== 'additionalContext') {
            otherHookSpecificFields[key] = value;
          }
        }
      }

      // Copy other fields (later values win for simple fields)
      if (output.suppressOutput !== undefined) {
        merged.suppressOutput = output.suppressOutput;
      }
      // systemMessage is accumulated with hook-name attribution rather than
      // last-write-wins, so users see every hook's voice.
      if (output.systemMessage !== undefined && output.systemMessage !== '') {
        systemMessages.push(`[${name}] ${output.systemMessage}`);
      }
    }

    // Set merged decision
    // Note: 'approve' is a Claude-Code-compatible single-hook value that no
    // merge branch produces here; it is treated as equivalent to 'allow' at
    // the scheduler level (neither blocking nor ask). Per-hook 'approve' is
    // still forwarded verbatim in notifications so the UI can render it.
    if (hasBlock) {
      merged.decision = 'block';
    } else if (hasAsk) {
      merged.decision = 'ask';
    } else if (named.some(({ output }) => output.decision === 'allow')) {
      merged.decision = 'allow';
    }

    // Set merged reason
    if (reasons.length > 0) {
      merged.reason = reasons.join('\n');
    }

    // Set merged systemMessage (concatenated with hook name prefixes)
    if (systemMessages.length > 0) {
      merged.systemMessage = systemMessages.join('\n');
    }

    // Set continue flag
    if (hasContinueFalse) {
      merged.continue = false;
      if (stopReason) {
        merged.stopReason = stopReason;
      }
    }

    // Build hookSpecificOutput
    const hookSpecificOutput: Record<string, unknown> = {
      ...otherHookSpecificFields,
    };
    if (additionalContexts.length > 0) {
      hookSpecificOutput['additionalContext'] = additionalContexts.join('\n');
    }

    if (Object.keys(hookSpecificOutput).length > 0) {
      merged.hookSpecificOutput = hookSpecificOutput;
    }

    return merged;
  }

  /**
   * Merge outputs for mergePermissionRequestOutputs events.
   *
   * Rules:
   * - behavior: deny wins over allow (security priority)
   * - message: concatenated with newlines
   * - updatedInput: later values win
   * - updatedPermissions: concatenated
   * - interrupt: true wins over false
   */
  private mergePermissionRequestOutputs(outputs: HookOutput[]): HookOutput {
    const merged: HookOutput = {};
    const messages: string[] = [];
    let hasDeny = false;
    let hasAllow = false;
    let interrupt = false;
    let updatedInput: Record<string, unknown> | undefined;
    const allUpdatedPermissions: Array<{ type: string; tool?: string }> = [];

    for (const output of outputs) {
      const specific = output.hookSpecificOutput;
      if (!specific) continue;

      const decision = specific['decision'] as
        | {
            behavior?: string;
            message?: string;
            updatedInput?: Record<string, unknown>;
            updatedPermissions?: Array<{ type: string; tool?: string }>;
            interrupt?: boolean;
          }
        | undefined;

      if (!decision) continue;

      // Check behavior
      if (decision['behavior'] === 'deny') {
        hasDeny = true;
      } else if (decision['behavior'] === 'allow') {
        hasAllow = true;
      }

      // Collect message
      if (decision['message']) {
        messages.push(decision['message'] as string);
      }

      // Check interrupt - true wins
      if (decision['interrupt'] === true) {
        interrupt = true;
      }

      // Collect updatedInput - use last non-empty
      if (decision['updatedInput']) {
        updatedInput = decision['updatedInput'] as Record<string, unknown>;
      }

      // Collect updatedPermissions
      if (decision['updatedPermissions']) {
        allUpdatedPermissions.push(
          ...(decision['updatedPermissions'] as Array<{
            type: string;
            tool?: string;
          }>),
        );
      }

      // Copy other fields
      if (output.continue !== undefined) {
        merged.continue = output.continue;
      }
      if (output.reason !== undefined) {
        merged.reason = output.reason;
      }
    }

    // Build merged decision
    const mergedDecision: Record<string, unknown> = {};

    if (hasDeny) {
      mergedDecision['behavior'] = 'deny';
    } else if (hasAllow) {
      mergedDecision['behavior'] = 'allow';
    }

    if (messages.length > 0) {
      mergedDecision['message'] = messages.join('\n');
    }

    if (interrupt) {
      mergedDecision['interrupt'] = true;
    }

    if (updatedInput) {
      mergedDecision['updatedInput'] = updatedInput;
    }

    if (allUpdatedPermissions.length > 0) {
      mergedDecision['updatedPermissions'] = allUpdatedPermissions;
    }

    merged.hookSpecificOutput = {
      ...merged.hookSpecificOutput,
      decision: mergedDecision,
    };

    return merged;
  }

  /**
   * Simple merge for events without special logic
   */
  private mergeSimple(outputs: HookOutput[]): HookOutput {
    const additionalContexts: string[] = [];
    let merged: HookOutput = {};

    for (const output of outputs) {
      // Collect additionalContext for concatenation
      this.extractAdditionalContext(output, additionalContexts);
      merged = { ...merged, ...output };
    }

    // Merge additionalContext with concatenation
    if (additionalContexts.length > 0) {
      merged.hookSpecificOutput = {
        ...merged.hookSpecificOutput,
        additionalContext: additionalContexts.join('\n'),
      };
    }

    return merged;
  }

  /**
   * Merge outputs with later fields replacing earlier fields.
   * Used for BeforeModel and AfterModel events where later hooks override earlier ones.
   */
  private mergeWithFieldReplacement(outputs: HookOutput[]): HookOutput {
    let merged: HookOutput = {};

    for (const output of outputs) {
      // Later outputs override earlier ones
      merged = {
        ...merged,
        ...output,
        hookSpecificOutput: {
          ...merged.hookSpecificOutput,
          ...output.hookSpecificOutput,
        },
      };
    }

    return merged;
  }

  /**
   * Merge tool selection outputs with union strategy.
   *
   * Rules:
   * - allowedFunctionNames: union of all hooks (sorted for deterministic caching)
   * - mode: NONE wins (most restrictive), then ANY > AUTO
   * - This means hooks can only add/enable tools, not filter them out individually
   */
  private mergeToolSelectionOutputs(
    outputs: BeforeToolSelectionOutput[],
  ): BeforeToolSelectionOutput {
    const merged: BeforeToolSelectionOutput = {};

    const allFunctionNames = new Set<string>();
    let hasNoneMode = false;
    let hasAnyMode = false;

    for (const output of outputs) {
      const toolConfig = output.hookSpecificOutput?.toolConfig;
      if (!toolConfig) {
        continue;
      }

      // Check mode
      if (toolConfig.mode === 'NONE') {
        hasNoneMode = true;
      } else if (toolConfig.mode === 'ANY') {
        hasAnyMode = true;
      }

      // Collect function names (union of all hooks)
      if (toolConfig.allowedFunctionNames) {
        for (const name of toolConfig.allowedFunctionNames) {
          allFunctionNames.add(name);
        }
      }
    }

    // Determine final mode and function names
    let finalMode: 'AUTO' | 'ANY' | 'NONE';
    let finalFunctionNames: string[] = [];

    if (hasNoneMode) {
      // NONE mode wins - most restrictive
      finalMode = 'NONE';
      finalFunctionNames = [];
    } else if (hasAnyMode) {
      // ANY mode if present (and no NONE)
      finalMode = 'ANY';
      finalFunctionNames = Array.from(allFunctionNames).sort();
    } else {
      // Default to AUTO mode
      finalMode = 'AUTO';
      finalFunctionNames = Array.from(allFunctionNames).sort();
    }

    merged.hookSpecificOutput = {
      hookEventName: 'BeforeToolSelection',
      toolConfig: {
        mode: finalMode,
        allowedFunctionNames: finalFunctionNames,
      },
    };

    return merged;
  }

  /**
   * Create the appropriate specific hook output class based on event type
   */
  private createSpecificHookOutput(
    output: HookOutput,
    eventName: HookEventName,
  ): DefaultHookOutput {
    switch (eventName) {
      case HookEventName.PreToolUse:
        return new PreToolUseHookOutput(output);
      case HookEventName.PostToolUse:
        return new PostToolUseHookOutput(output);
      case HookEventName.Stop:
        return new StopHookOutput(output);
      case HookEventName.PermissionRequest:
        return new PermissionRequestHookOutput(output);
      case HookEventName.BeforeModel:
        return new BeforeModelHookOutput(output);
      case HookEventName.AfterModel:
        return new AfterModelHookOutput(output);
      case HookEventName.BeforeToolSelection:
        return new BeforeToolSelectionHookOutput(output);
      default:
        return new DefaultHookOutput(output);
    }
  }

  /**
   * Extract additional context from hook-specific outputs
   */
  private extractAdditionalContext(
    output: HookOutput,
    contexts: string[],
  ): void {
    const specific = output.hookSpecificOutput;
    if (!specific) {
      return;
    }

    // Extract additionalContext from various hook types
    if (
      'additionalContext' in specific &&
      typeof specific['additionalContext'] === 'string'
    ) {
      contexts.push(specific['additionalContext']);
    }
  }
}
