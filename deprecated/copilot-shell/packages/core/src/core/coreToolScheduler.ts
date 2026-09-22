/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type {
  ToolCallRequestInfo,
  ToolCallResponseInfo,
  ToolCallConfirmationDetails,
  ToolResult,
  ToolResultDisplay,
  ToolRegistry,
  EditorType,
  Config,
  ToolConfirmationPayload,
  AnyDeclarativeTool,
  AnyToolInvocation,
  ChatRecordingService,
} from '../index.js';
import {
  ToolConfirmationOutcome,
  ApprovalMode,
  logToolCall,
  ReadFileTool,
  ToolErrorType,
  ToolCallEvent,
  ShellTool,
  logToolOutputTruncated,
  ToolOutputTruncatedEvent,
  InputFormat,
  SkillTool,
} from '../index.js';
import { uiTelemetryService } from '../telemetry/uiTelemetry.js';
import type { SandboxBypassApprovalRequest } from '../index.js';
import type { HookDecision } from '../hooks/types.js';
import type {
  FunctionResponse,
  FunctionResponsePart,
  Part,
  PartListUnion,
} from '@google/genai';
import { ToolNames } from '../tools/tool-names.js';
import { getResponseTextFromParts } from '../utils/generateContentResponseUtilities.js';
import type { ModifyContext } from '../tools/modifiable-tool.js';
import {
  isModifiableDeclarativeTool,
  modifyWithEditor,
} from '../tools/modifiable-tool.js';
import * as Diff from 'diff';
import * as fs from 'node:fs/promises';
import * as path from 'node:path';
import { doesToolInvocationMatch } from '../utils/tool-utils.js';
import levenshtein from 'fast-levenshtein';
import { getPlanModeSystemReminder } from './prompts.js';
import { ShellToolInvocation } from '../tools/shell.js';
import {
  redactSecrets,
  redactPartListUnion,
  redactAnsiOutput,
} from '../utils/secretRedactor.js';
import type { FileDiff } from '../tools/tools.js';

export type ValidatingToolCall = {
  status: 'validating';
  request: ToolCallRequestInfo;
  tool: AnyDeclarativeTool;
  invocation: AnyToolInvocation;
  startTime?: number;
  outcome?: ToolConfirmationOutcome;
};

export type ScheduledToolCall = {
  status: 'scheduled';
  request: ToolCallRequestInfo;
  tool: AnyDeclarativeTool;
  invocation: AnyToolInvocation;
  startTime?: number;
  outcome?: ToolConfirmationOutcome;
};

export type ErroredToolCall = {
  status: 'error';
  request: ToolCallRequestInfo;
  response: ToolCallResponseInfo;
  tool?: AnyDeclarativeTool;
  durationMs?: number;
  outcome?: ToolConfirmationOutcome;
};

export type SuccessfulToolCall = {
  status: 'success';
  request: ToolCallRequestInfo;
  tool: AnyDeclarativeTool;
  response: ToolCallResponseInfo;
  invocation: AnyToolInvocation;
  durationMs?: number;
  outcome?: ToolConfirmationOutcome;
};

export type ExecutingToolCall = {
  status: 'executing';
  request: ToolCallRequestInfo;
  tool: AnyDeclarativeTool;
  invocation: AnyToolInvocation;
  liveOutput?: ToolResultDisplay;
  startTime?: number;
  outcome?: ToolConfirmationOutcome;
  pid?: number;
};

export type CancelledToolCall = {
  status: 'cancelled';
  request: ToolCallRequestInfo;
  response: ToolCallResponseInfo;
  tool: AnyDeclarativeTool;
  invocation: AnyToolInvocation;
  durationMs?: number;
  outcome?: ToolConfirmationOutcome;
};

export type WaitingToolCall = {
  status: 'awaiting_approval';
  request: ToolCallRequestInfo;
  tool: AnyDeclarativeTool;
  invocation: AnyToolInvocation;
  confirmationDetails: ToolCallConfirmationDetails;
  /**
   * True when this call was forced into awaiting_approval by a hook 'ask'
   * decision (regardless of whether the tool itself requires confirmation).
   * Used by autoApproveCompatiblePendingTools to prevent batch silent
   * approval of calls that the hook explicitly wants to gate one-by-one.
   */
  hookForceAsk?: boolean;
  startTime?: number;
  /**
   * Timestamp when the tool call entered awaiting_approval state.
   * Used to accurately measure user approval wait duration (excludes
   * validation and hook execution time).
   */
  awaitStartTime?: number;
  outcome?: ToolConfirmationOutcome;
};

export type Status = ToolCall['status'];

export type ToolCall =
  | ValidatingToolCall
  | ScheduledToolCall
  | ErroredToolCall
  | SuccessfulToolCall
  | ExecutingToolCall
  | CancelledToolCall
  | WaitingToolCall;

export type CompletedToolCall =
  | SuccessfulToolCall
  | CancelledToolCall
  | ErroredToolCall;

export type ConfirmHandler = (
  toolCall: WaitingToolCall,
) => Promise<ToolConfirmationOutcome>;

export type OutputUpdateHandler = (
  toolCallId: string,
  outputChunk: ToolResultDisplay,
) => void;

export type AllToolCallsCompleteHandler = (
  completedToolCalls: CompletedToolCall[],
) => Promise<void>;

export type ToolCallsUpdateHandler = (toolCalls: ToolCall[]) => void;

/**
 * Formats tool output for a Gemini FunctionResponse.
 */
function createFunctionResponsePart(
  callId: string,
  toolName: string,
  output: string,
  mediaParts?: FunctionResponsePart[],
): Part {
  const functionResponse: FunctionResponse = {
    id: callId,
    name: toolName,
    response: { output },
    ...(mediaParts && mediaParts.length > 0 ? { parts: mediaParts } : {}),
  };

  return {
    functionResponse,
  };
}

export function convertToFunctionResponse(
  toolName: string,
  callId: string,
  llmContent: PartListUnion,
): Part[] {
  const contentToProcess =
    Array.isArray(llmContent) && llmContent.length === 1
      ? llmContent[0]
      : llmContent;

  if (typeof contentToProcess === 'string') {
    return [createFunctionResponsePart(callId, toolName, contentToProcess)];
  }

  if (Array.isArray(contentToProcess)) {
    const functionResponse = createFunctionResponsePart(
      callId,
      toolName,
      'Tool execution succeeded.',
    );
    return [functionResponse, ...toParts(contentToProcess)];
  }

  // After this point, contentToProcess is a single Part object.
  if (contentToProcess.functionResponse) {
    if (contentToProcess.functionResponse.response?.['content']) {
      const stringifiedOutput =
        getResponseTextFromParts(
          contentToProcess.functionResponse.response['content'] as Part[],
        ) || '';
      return [createFunctionResponsePart(callId, toolName, stringifiedOutput)];
    }
    // It's a functionResponse that we should pass through as is.
    return [contentToProcess];
  }

  if (contentToProcess.inlineData || contentToProcess.fileData) {
    const mediaParts: FunctionResponsePart[] = [];
    if (contentToProcess.inlineData) {
      mediaParts.push({ inlineData: contentToProcess.inlineData });
    }
    if (contentToProcess.fileData) {
      mediaParts.push({ fileData: contentToProcess.fileData });
    }

    const functionResponse = createFunctionResponsePart(
      callId,
      toolName,
      '',
      mediaParts,
    );
    return [functionResponse];
  }

  if (contentToProcess.text !== undefined) {
    return [
      createFunctionResponsePart(callId, toolName, contentToProcess.text),
    ];
  }

  // Default case for other kinds of parts.
  return [
    createFunctionResponsePart(callId, toolName, 'Tool execution succeeded.'),
  ];
}

function toParts(input: PartListUnion): Part[] {
  const parts: Part[] = [];
  for (const part of Array.isArray(input) ? input : [input]) {
    if (typeof part === 'string') {
      parts.push({ text: part });
    } else if (part) {
      parts.push(part);
    }
  }
  return parts;
}

const createErrorResponse = (
  request: ToolCallRequestInfo,
  error: Error,
  errorType: ToolErrorType | undefined,
): ToolCallResponseInfo => ({
  callId: request.callId,
  error,
  responseParts: [
    {
      functionResponse: {
        id: request.callId,
        name: request.name,
        response: { error: error.message },
      },
    },
  ],
  resultDisplay: error.message,
  errorType,
  contentLength: error.message.length,
});

export async function truncateAndSaveToFile(
  content: string,
  callId: string,
  projectTempDir: string,
  threshold: number,
  truncateLines: number,
): Promise<{ content: string; outputFile?: string }> {
  if (content.length <= threshold) {
    return { content };
  }

  let lines = content.split('\n');
  let fileContent = content;

  // If the content is long but has few lines, wrap it to enable line-based truncation.
  if (lines.length <= truncateLines) {
    const wrapWidth = 120; // A reasonable width for wrapping.
    const wrappedLines: string[] = [];
    for (const line of lines) {
      if (line.length > wrapWidth) {
        for (let i = 0; i < line.length; i += wrapWidth) {
          wrappedLines.push(line.substring(i, i + wrapWidth));
        }
      } else {
        wrappedLines.push(line);
      }
    }
    lines = wrappedLines;
    fileContent = lines.join('\n');
  }

  const head = Math.floor(truncateLines / 5);
  const beginning = lines.slice(0, head);
  const end = lines.slice(-(truncateLines - head));
  const truncatedContent =
    beginning.join('\n') + '\n... [CONTENT TRUNCATED] ...\n' + end.join('\n');

  // Sanitize callId to prevent path traversal.
  const safeFileName = `${path.basename(callId)}.output`;
  const outputFile = path.join(projectTempDir, safeFileName);
  try {
    await fs.writeFile(outputFile, fileContent);

    return {
      content: `Tool output was too large and has been truncated.
The full output has been saved to: ${outputFile}
To read the complete output, use the ${ReadFileTool.Name} tool with the absolute file path above.
The truncated output below shows the beginning and end of the content. The marker '... [CONTENT TRUNCATED] ...' indicates where content was removed.
This allows you to efficiently examine different parts of the output without loading the entire file.
Truncated part of the output:
${truncatedContent}`,
      outputFile,
    };
  } catch (_error) {
    return {
      content:
        truncatedContent + `\n[Note: Could not save full output to file]`,
    };
  }
}

interface CoreToolSchedulerOptions {
  config: Config;
  outputUpdateHandler?: OutputUpdateHandler;
  onAllToolCallsComplete?: AllToolCallsCompleteHandler;
  onToolCallsUpdate?: ToolCallsUpdateHandler;
  getPreferredEditor: () => EditorType | undefined;
  onEditorClose: () => void;
  /**
   * Optional recording service. If provided, tool results will be recorded.
   */
  chatRecordingService?: ChatRecordingService;
  /**
   * Optional callback invoked when a password prompt (e.g. sudo) is detected.
   */
  onPasswordPrompt?: () => void;
  /**
   * Optional callback invoked when a sandbox-guarded command fails and the
   * sandbox-failure-handler hook requests a bypass approval dialog.
   * Returns true if the user approved the bypass, false otherwise.
   */
  onSandboxBypassRequested?: (
    request: SandboxBypassApprovalRequest,
  ) => Promise<boolean>;
}

export class CoreToolScheduler {
  private toolRegistry: ToolRegistry;
  private toolCalls: ToolCall[] = [];
  private outputUpdateHandler?: OutputUpdateHandler;
  private onAllToolCallsComplete?: AllToolCallsCompleteHandler;
  private onToolCallsUpdate?: ToolCallsUpdateHandler;
  private getPreferredEditor: () => EditorType | undefined;
  private config: Config;
  private onEditorClose: () => void;
  private chatRecordingService?: ChatRecordingService;
  private onPasswordPrompt?: () => void;
  private onSandboxBypassRequested?: (
    request: SandboxBypassApprovalRequest,
  ) => Promise<boolean>;
  private isFinalizingToolCalls = false;
  private isScheduling = false;
  private requestQueue: Array<{
    request: ToolCallRequestInfo | ToolCallRequestInfo[];
    signal: AbortSignal;
    resolve: () => void;
    reject: (reason?: Error) => void;
  }> = [];

  constructor(options: CoreToolSchedulerOptions) {
    this.config = options.config;
    this.toolRegistry = options.config.getToolRegistry();
    this.outputUpdateHandler = options.outputUpdateHandler;
    this.onAllToolCallsComplete = options.onAllToolCallsComplete;
    this.onToolCallsUpdate = options.onToolCallsUpdate;
    this.getPreferredEditor = options.getPreferredEditor;
    this.onEditorClose = options.onEditorClose;
    this.chatRecordingService = options.chatRecordingService;
    this.onPasswordPrompt = options.onPasswordPrompt;
    this.onSandboxBypassRequested = options.onSandboxBypassRequested;
  }

  private setStatusInternal(
    targetCallId: string,
    status: 'success',
    response: ToolCallResponseInfo,
  ): void;
  private setStatusInternal(
    targetCallId: string,
    status: 'awaiting_approval',
    confirmationDetails: ToolCallConfirmationDetails,
    hookForceAsk?: boolean,
  ): void;
  private setStatusInternal(
    targetCallId: string,
    status: 'error',
    response: ToolCallResponseInfo,
  ): void;
  private setStatusInternal(
    targetCallId: string,
    status: 'cancelled',
    reason: string,
  ): void;
  private setStatusInternal(
    targetCallId: string,
    status: 'executing' | 'scheduled' | 'validating',
  ): void;
  private setStatusInternal(
    targetCallId: string,
    newStatus: Status,
    auxiliaryData?: unknown,
    hookForceAsk?: boolean,
  ): void {
    this.toolCalls = this.toolCalls.map((currentCall) => {
      if (
        currentCall.request.callId !== targetCallId ||
        currentCall.status === 'success' ||
        currentCall.status === 'error' ||
        currentCall.status === 'cancelled'
      ) {
        return currentCall;
      }

      // currentCall is a non-terminal state here and should have startTime and tool.
      const existingStartTime = currentCall.startTime;
      const toolInstance = currentCall.tool;
      const invocation = currentCall.invocation;

      const outcome = currentCall.outcome;

      switch (newStatus) {
        case 'success': {
          const durationMs = existingStartTime
            ? Date.now() - existingStartTime
            : undefined;
          return {
            request: currentCall.request,
            tool: toolInstance,
            invocation,
            status: 'success',
            response: auxiliaryData as ToolCallResponseInfo,
            durationMs,
            outcome,
          } as SuccessfulToolCall;
        }
        case 'error': {
          const durationMs = existingStartTime
            ? Date.now() - existingStartTime
            : undefined;
          return {
            request: currentCall.request,
            status: 'error',
            tool: toolInstance,
            response: auxiliaryData as ToolCallResponseInfo,
            durationMs,
            outcome,
          } as ErroredToolCall;
        }
        case 'awaiting_approval':
          return {
            request: currentCall.request,
            tool: toolInstance,
            status: 'awaiting_approval',
            confirmationDetails: auxiliaryData as ToolCallConfirmationDetails,
            hookForceAsk,
            startTime: existingStartTime,
            awaitStartTime:
              (currentCall as WaitingToolCall).awaitStartTime ?? Date.now(),
            outcome,
            invocation,
          } as WaitingToolCall;
        case 'scheduled':
          return {
            request: currentCall.request,
            tool: toolInstance,
            status: 'scheduled',
            startTime: existingStartTime,
            outcome,
            invocation,
          } as ScheduledToolCall;
        case 'cancelled': {
          const durationMs = existingStartTime
            ? Date.now() - existingStartTime
            : undefined;

          // Preserve diff for cancelled edit operations
          let resultDisplay: ToolResultDisplay | undefined = undefined;
          if (currentCall.status === 'awaiting_approval') {
            const waitingCall = currentCall as WaitingToolCall;
            if (waitingCall.confirmationDetails.type === 'edit') {
              resultDisplay = {
                fileDiff: waitingCall.confirmationDetails.fileDiff,
                fileName: waitingCall.confirmationDetails.fileName,
                originalContent:
                  waitingCall.confirmationDetails.originalContent,
                newContent: waitingCall.confirmationDetails.newContent,
              };
            }
          } else if (currentCall.status === 'executing') {
            // If the tool was streaming live output, preserve the latest
            // output so the UI can continue to show it after cancellation.
            const executingCall = currentCall as ExecutingToolCall;
            if (executingCall.liveOutput !== undefined) {
              resultDisplay = executingCall.liveOutput;
            }
          }

          const errorMessage = `[Operation Cancelled] Reason: ${auxiliaryData}`;
          return {
            request: currentCall.request,
            tool: toolInstance,
            invocation,
            status: 'cancelled',
            response: {
              callId: currentCall.request.callId,
              responseParts: [
                {
                  functionResponse: {
                    id: currentCall.request.callId,
                    name: currentCall.request.name,
                    response: {
                      error: errorMessage,
                    },
                  },
                },
              ],
              resultDisplay,
              error: undefined,
              errorType: undefined,
              contentLength: errorMessage.length,
            },
            durationMs,
            outcome,
          } as CancelledToolCall;
        }
        case 'validating':
          return {
            request: currentCall.request,
            tool: toolInstance,
            status: 'validating',
            startTime: existingStartTime,
            outcome,
            invocation,
          } as ValidatingToolCall;
        case 'executing':
          return {
            request: currentCall.request,
            tool: toolInstance,
            status: 'executing',
            startTime: existingStartTime,
            outcome,
            invocation,
          } as ExecutingToolCall;
        default: {
          const exhaustiveCheck: never = newStatus;
          return exhaustiveCheck;
        }
      }
    });
    this.notifyToolCallsUpdate();
    this.checkAndNotifyCompletion();
  }

  private setArgsInternal(targetCallId: string, args: unknown): void {
    this.toolCalls = this.toolCalls.map((call) => {
      // We should never be asked to set args on an ErroredToolCall, but
      // we guard for the case anyways.
      if (call.request.callId !== targetCallId || call.status === 'error') {
        return call;
      }

      const invocationOrError = this.buildInvocation(
        call.tool,
        args as Record<string, unknown>,
      );
      if (invocationOrError instanceof Error) {
        const response = createErrorResponse(
          call.request,
          invocationOrError,
          ToolErrorType.INVALID_TOOL_PARAMS,
        );
        return {
          request: { ...call.request, args: args as Record<string, unknown> },
          status: 'error',
          tool: call.tool,
          response,
        } as ErroredToolCall;
      }

      return {
        ...call,
        request: { ...call.request, args: args as Record<string, unknown> },
        invocation: invocationOrError,
      };
    });
  }

  private isRunning(): boolean {
    return (
      this.isFinalizingToolCalls ||
      this.toolCalls.some(
        (call) =>
          call.status === 'executing' || call.status === 'awaiting_approval',
      )
    );
  }

  private buildInvocation(
    tool: AnyDeclarativeTool,
    args: object,
  ): AnyToolInvocation | Error {
    try {
      return tool.build(args);
    } catch (e) {
      if (e instanceof Error) {
        return e;
      }
      return new Error(String(e));
    }
  }

  /**
   * Generates error message for unknown tool. Returns early with skill-specific
   * message if the name matches a skill, otherwise uses Levenshtein suggestions.
   */
  private getToolNotFoundMessage(unknownToolName: string, topN = 3): string {
    // Check if the unknown tool name matches an available skill name.
    // This handles the case where the model tries to invoke a skill as a tool
    // (e.g., Tool: "pdf" instead of Tool: "Skill" with skill: "pdf")
    const skillTool = this.toolRegistry.getTool(ToolNames.SKILL);
    if (skillTool instanceof SkillTool) {
      const availableSkillNames = skillTool.getAvailableSkillNames();
      if (availableSkillNames.includes(unknownToolName)) {
        return `"${unknownToolName}" is a skill name, not a tool name. To use this skill, invoke the "${ToolNames.SKILL}" tool with parameter: skill: "${unknownToolName}"`;
      }
    }

    // Standard "not found" message with Levenshtein suggestions
    const suggestion = this.getToolSuggestion(unknownToolName, topN);
    return `Tool "${unknownToolName}" not found in registry. Tools must use the exact names that are registered.${suggestion}`;
  }

  /** Suggests similar tool names using Levenshtein distance. */
  private getToolSuggestion(unknownToolName: string, topN = 3): string {
    const allToolNames = this.toolRegistry.getAllToolNames();

    const matches = allToolNames.map((toolName) => ({
      name: toolName,
      distance: levenshtein.get(unknownToolName, toolName),
    }));

    matches.sort((a, b) => a.distance - b.distance);

    const topNResults = matches.slice(0, topN);

    if (topNResults.length === 0) {
      return '';
    }

    const suggestedNames = topNResults
      .map((match) => `"${match.name}"`)
      .join(', ');

    if (topNResults.length > 1) {
      return ` Did you mean one of: ${suggestedNames}?`;
    } else {
      return ` Did you mean ${suggestedNames}?`;
    }
  }

  schedule(
    request: ToolCallRequestInfo | ToolCallRequestInfo[],
    signal: AbortSignal,
  ): Promise<void> {
    if (this.isRunning() || this.isScheduling) {
      return new Promise((resolve, reject) => {
        const abortHandler = () => {
          // Find and remove the request from the queue
          const index = this.requestQueue.findIndex(
            (item) => item.request === request,
          );
          if (index > -1) {
            this.requestQueue.splice(index, 1);
            reject(new Error('Tool call cancelled while in queue.'));
          }
        };

        signal.addEventListener('abort', abortHandler, { once: true });

        this.requestQueue.push({
          request,
          signal,
          resolve: () => {
            signal.removeEventListener('abort', abortHandler);
            resolve();
          },
          reject: (reason?: Error) => {
            signal.removeEventListener('abort', abortHandler);
            reject(reason);
          },
        });
      });
    }
    return this._schedule(request, signal);
  }

  private async _schedule(
    request: ToolCallRequestInfo | ToolCallRequestInfo[],
    signal: AbortSignal,
  ): Promise<void> {
    this.isScheduling = true;
    try {
      if (this.isRunning()) {
        throw new Error(
          'Cannot schedule new tool calls while other tool calls are actively running (executing or awaiting approval).',
        );
      }
      const requestsToProcess = Array.isArray(request) ? request : [request];

      const newToolCalls: ToolCall[] = requestsToProcess.map(
        (reqInfo): ToolCall => {
          // Check if the tool is excluded due to permissions/environment restrictions
          // This check should happen before registry lookup to provide a clear permission error
          const excludeTools = this.config.getExcludeTools?.() ?? undefined;
          if (excludeTools && excludeTools.length > 0) {
            const normalizedToolName = reqInfo.name.toLowerCase().trim();
            const excludedMatch = excludeTools.find(
              (excludedTool) =>
                excludedTool.toLowerCase().trim() === normalizedToolName,
            );

            if (excludedMatch) {
              // The tool exists but is excluded - return permission error directly
              const permissionErrorMessage = `copilot-shell requires permission to use ${excludedMatch}, but that permission was declined.`;
              return {
                status: 'error',
                request: reqInfo,
                response: createErrorResponse(
                  reqInfo,
                  new Error(permissionErrorMessage),
                  ToolErrorType.EXECUTION_DENIED,
                ),
                durationMs: 0,
              };
            }
          }

          const toolInstance = this.toolRegistry.getTool(reqInfo.name);
          if (!toolInstance) {
            // Tool is not in registry and not excluded - likely hallucinated or typo
            const errorMessage = this.getToolNotFoundMessage(reqInfo.name);
            return {
              status: 'error',
              request: reqInfo,
              response: createErrorResponse(
                reqInfo,
                new Error(errorMessage),
                ToolErrorType.TOOL_NOT_REGISTERED,
              ),
              durationMs: 0,
            };
          }

          const invocationOrError = this.buildInvocation(
            toolInstance,
            reqInfo.args,
          );
          if (invocationOrError instanceof Error) {
            return {
              status: 'error',
              request: reqInfo,
              tool: toolInstance,
              response: createErrorResponse(
                reqInfo,
                invocationOrError,
                ToolErrorType.INVALID_TOOL_PARAMS,
              ),
              durationMs: 0,
            };
          }

          return {
            status: 'validating',
            request: reqInfo,
            tool: toolInstance,
            invocation: invocationOrError,
            startTime: Date.now(),
          };
        },
      );

      this.toolCalls = this.toolCalls.concat(newToolCalls);
      this.notifyToolCallsUpdate();

      for (const toolCall of newToolCalls) {
        if (toolCall.status !== 'validating') {
          continue;
        }

        let { request: reqInfo, invocation } = toolCall;

        // Fire PreToolUse hook BEFORE confirmation — allows hooks to
        // modify/block the command before the user sees the approval dialog.
        let hookForceAsk = false;
        let hookAskMessage: string | undefined;
        if (this.config.getEnableHooks()) {
          const hookSystem = this.config.getHookSystem();
          if (hookSystem) {
            try {
              // For Skill tool, resolve the skill file path so hooks can
              // locate the skill on disk even when the SKILL.md `name`
              // differs from the directory name.
              let skillContext:
                | import('../hooks/types.js').SkillToolContext
                | undefined;
              if (
                reqInfo.name === ToolNames.SKILL &&
                typeof reqInfo.args['skill'] === 'string'
              ) {
                const skillTool = this.toolRegistry.getTool(ToolNames.SKILL);
                if (skillTool instanceof SkillTool) {
                  const skillConfig = await skillTool.resolveSkillConfig(
                    reqInfo.args['skill'],
                  );
                  if (skillConfig) {
                    skillContext = {
                      skill_name: skillConfig.name,
                      file_path: skillConfig.filePath,
                    };
                  }
                }
              }

              const hookOutput = await hookSystem.firePreToolUseEvent(
                reqInfo.name,
                reqInfo.args,
                skillContext,
                reqInfo.callId,
                signal,
              );

              if (hookOutput) {
                // Compute the merged decision once so every notification shares
                // the same context. 'block'/'deny'/'ask' come from the hook
                // decision itself; shouldStopExecution() (continue=false) is
                // also treated as a blocking context for UI dimming purposes.
                const isBlocking =
                  hookOutput.isBlockingDecision() ||
                  hookOutput.shouldStopExecution();
                const isAsk = hookOutput.isAskDecision();
                const mergedDecision: HookDecision | undefined = isBlocking
                  ? hookOutput.decision === 'deny'
                    ? 'deny'
                    : 'block'
                  : isAsk
                    ? 'ask'
                    : hookOutput.decision;

                // Emit hook notifications FIRST, regardless of the final
                // decision, so every hook's voice is preserved in the UI
                // (principle: “expose as much as possible unless denied”).
                // UI uses mergedDecision to dim per-hook boxes when the
                // overall outcome is block/deny, avoiding “green allow box
                // + rejected execution” visual conflict.
                if (
                  this.outputUpdateHandler &&
                  hookOutput.notifications?.length
                ) {
                  for (const n of hookOutput.notifications) {
                    this.outputUpdateHandler(reqInfo.callId, {
                      hookName: n.hookName,
                      hookMessage: n.message,
                      decision: n.decision,
                      mergedDecision,
                    });
                  }
                }

                // Block: abort the tool call immediately. Execution rule
                // block > ask > allow is preserved (this branch runs after
                // notifications are already emitted above).
                if (isBlocking) {
                  const reason = hookOutput.getEffectiveReason();
                  this.setStatusInternal(
                    reqInfo.callId,
                    'error',
                    createErrorResponse(
                      reqInfo,
                      new Error(`PreToolUse hook blocked execution: ${reason}`),
                      ToolErrorType.EXECUTION_DENIED,
                    ),
                  );
                  continue;
                }

                // Ask: force user confirmation regardless of YOLO/allowlist
                if (isAsk) {
                  hookForceAsk = true;
                  hookAskMessage = hookOutput.systemMessage;
                }

                // Modify: replace tool_input and rebuild invocation
                const modifiedInput = hookOutput.getModifiedToolInput();
                if (modifiedInput) {
                  const newArgs = { ...reqInfo.args, ...modifiedInput };
                  const newInvocation = this.buildInvocation(
                    toolCall.tool,
                    newArgs,
                  );
                  if (!(newInvocation instanceof Error)) {
                    reqInfo = { ...reqInfo, args: newArgs };
                    invocation = newInvocation;
                    // Update the tool call in-place so confirmation dialog
                    // shows the rewritten command.
                    const idx = this.toolCalls.indexOf(toolCall);
                    if (idx !== -1) {
                      this.toolCalls[idx] = {
                        ...toolCall,
                        request: reqInfo,
                        invocation,
                      };
                    }
                    this.notifyToolCallsUpdate();
                  }
                }
              }
            } catch (error) {
              // Hook errors should not block tool execution
              if (this.config.getDebugMode()) {
                console.debug(`PreToolUse hook error (non-fatal): ${error}`);
              }
            }
          }
        }

        try {
          if (signal.aborted) {
            this.setStatusInternal(
              reqInfo.callId,
              'cancelled',
              'Tool call cancelled by user.',
            );
            continue;
          }

          const confirmationDetails =
            await invocation.shouldConfirmExecute(signal);

          // If hook requested 'ask', ensure the user always sees a confirmation
          // dialog — regardless of YOLO/allowlist.
          //
          // When the tool already has its own confirmation details (e.g. exec/edit)
          // we AUGMENT the existing dialog by appending the hook warning to the
          // title, so the tool's original onConfirm side-effects (IDE diff result
          // back-fill, allowlist update, AUTO_EDIT mode) are preserved.
          //
          // When the tool has NO native confirmation we synthesize an info dialog.
          // In that case we use an empty onConfirm intentionally: ProceedAlways
          // must NOT add the command to the allowlist — the hook wants to be
          // consulted on every future invocation.
          let effectiveConfirmationDetails:
            | ToolCallConfirmationDetails
            | false = confirmationDetails;
          if (hookForceAsk) {
            // Fixed prompt text. The actual per-hook messages are already
            // rendered above the dialog as structured HookNotificationDisplay
            // boxes (emitted before this point, regardless of decision). The
            // dialog only needs to ask the user to make a decision; showing
            // the merged systemMessage here would duplicate those boxes.
            // `hookAskMessage` is intentionally unused in the prompt body.
            void hookAskMessage;
            // NOTE: This English string is used as a stable i18n key on the UI
            // side (see ToolConfirmationMessage.tsx, which wraps `prompt` with
            // t()). Keep both locales/en.js and locales/zh.js entries in sync
            // when editing this text.
            const askPrompt = 'A hook requires your confirmation to proceed.';
            // For 'edit'-type confirmations, also copy fileName/fileDiff so the
            // UI can render a diff preview below the hook message.
            // For 'exec'-type confirmations, copy command/rootCommand so the UI
            // can render the full shell command below the hook message.
            // If the tool needs NO native confirmation (e.g. echo, safe commands)
            // but the hook still wants to ask, extract the command directly from
            // reqInfo.args so the user can still see what they're approving.
            const shellCommandFromArgs =
              !confirmationDetails &&
              reqInfo.name === ToolNames.SHELL &&
              typeof reqInfo.args['command'] === 'string'
                ? (reqInfo.args['command'] as string)
                : undefined;
            effectiveConfirmationDetails = {
              type: 'info',
              title: 'Hook Requires Confirmation',
              prompt: askPrompt,
              ...(confirmationDetails && confirmationDetails.type === 'edit'
                ? {
                    fileName: confirmationDetails.fileName,
                    fileDiff: confirmationDetails.fileDiff,
                  }
                : {}),
              ...(confirmationDetails && confirmationDetails.type === 'exec'
                ? {
                    command: confirmationDetails.command,
                    rootCommand: confirmationDetails.rootCommand,
                  }
                : shellCommandFromArgs
                  ? { command: shellCommandFromArgs }
                  : {}),
              onConfirm: confirmationDetails
                ? (
                    outcome: ToolConfirmationOutcome,
                    payload?: ToolConfirmationPayload,
                  ) => confirmationDetails.onConfirm(outcome, payload)
                : async () => {},
            };
          }

          if (!effectiveConfirmationDetails) {
            this.setToolCallOutcome(
              reqInfo.callId,
              ToolConfirmationOutcome.ProceedAlways,
            );
            this.setStatusInternal(reqInfo.callId, 'scheduled');
            continue;
          }

          const allowedTools = this.config.getAllowedTools() || [];
          const isPlanMode =
            this.config.getApprovalMode() === ApprovalMode.PLAN;
          const isExitPlanModeTool = reqInfo.name === 'exit_plan_mode';

          if (isPlanMode && !isExitPlanModeTool) {
            if (effectiveConfirmationDetails) {
              this.setStatusInternal(reqInfo.callId, 'error', {
                callId: reqInfo.callId,
                responseParts: convertToFunctionResponse(
                  reqInfo.name,
                  reqInfo.callId,
                  getPlanModeSystemReminder(),
                ),
                resultDisplay: 'Plan mode blocked a non-read-only tool call.',
                error: undefined,
                errorType: undefined,
              });
            } else {
              this.setStatusInternal(reqInfo.callId, 'scheduled');
            }
          } else if (
            // hookForceAsk overrides YOLO/allowlist — hook explicitly requested user confirmation
            !hookForceAsk &&
            (this.config.getApprovalMode() === ApprovalMode.YOLO ||
              doesToolInvocationMatch(toolCall.tool, invocation, allowedTools))
          ) {
            this.setToolCallOutcome(
              reqInfo.callId,
              ToolConfirmationOutcome.ProceedAlways,
            );
            this.setStatusInternal(reqInfo.callId, 'scheduled');
          } else {
            /**
             * In non-interactive mode where no user will respond to approval prompts,
             * and not running as IDE companion or Zed integration, automatically deny approval.
             * This is intended to create an explicit denial of the tool call,
             * rather than silently waiting for approval and hanging forever.
             */
            const shouldAutoDeny =
              !this.config.isInteractive() &&
              !this.config.getExperimentalZedIntegration() &&
              this.config.getInputFormat() !== InputFormat.STREAM_JSON;

            if (shouldAutoDeny) {
              const errorMessage = `copilot-shell requires permission to use "${reqInfo.name}", but that permission was declined.`;
              this.setStatusInternal(
                reqInfo.callId,
                'error',
                createErrorResponse(
                  reqInfo,
                  new Error(errorMessage),
                  ToolErrorType.EXECUTION_DENIED,
                ),
              );
              continue;
            }

            // Allow IDE to resolve confirmation
            if (
              effectiveConfirmationDetails &&
              effectiveConfirmationDetails.type === 'edit' &&
              effectiveConfirmationDetails.ideConfirmation
            ) {
              const ideDetails = effectiveConfirmationDetails;
              ideDetails.ideConfirmation!.then((resolution) => {
                if (resolution.status === 'accepted') {
                  this.handleConfirmationResponse(
                    reqInfo.callId,
                    ideDetails.onConfirm,
                    ToolConfirmationOutcome.ProceedOnce,
                    signal,
                  );
                } else {
                  this.handleConfirmationResponse(
                    reqInfo.callId,
                    ideDetails.onConfirm,
                    ToolConfirmationOutcome.Cancel,
                    signal,
                  );
                }
              });
            }

            const activeDetails =
              effectiveConfirmationDetails as ToolCallConfirmationDetails;
            const originalOnConfirm = activeDetails.onConfirm;
            const wrappedConfirmationDetails: ToolCallConfirmationDetails = {
              ...activeDetails,
              onConfirm: (
                outcome: ToolConfirmationOutcome,
                payload?: ToolConfirmationPayload,
              ) =>
                this.handleConfirmationResponse(
                  reqInfo.callId,
                  originalOnConfirm,
                  outcome,
                  signal,
                  payload,
                ),
            };
            this.setStatusInternal(
              reqInfo.callId,
              'awaiting_approval',
              wrappedConfirmationDetails,
              hookForceAsk,
            );
          }
        } catch (error) {
          if (signal.aborted) {
            this.setStatusInternal(
              reqInfo.callId,
              'cancelled',
              'Tool call cancelled by user.',
            );
            continue;
          }

          this.setStatusInternal(
            reqInfo.callId,
            'error',
            createErrorResponse(
              reqInfo,
              error instanceof Error ? error : new Error(String(error)),
              ToolErrorType.UNHANDLED_EXCEPTION,
            ),
          );
        }
      }
      await this.attemptExecutionOfScheduledCalls(signal);
      void this.checkAndNotifyCompletion();
    } finally {
      this.isScheduling = false;
    }
  }

  async handleConfirmationResponse(
    callId: string,
    originalOnConfirm: (
      outcome: ToolConfirmationOutcome,
      payload?: ToolConfirmationPayload,
    ) => Promise<void>,
    outcome: ToolConfirmationOutcome,
    signal: AbortSignal,
    payload?: ToolConfirmationPayload,
  ): Promise<void> {
    const toolCall = this.toolCalls.find(
      (c) => c.request.callId === callId && c.status === 'awaiting_approval',
    );

    // Record await duration for SLS telemetry (use awaitStartTime for accuracy)
    if (toolCall) {
      const awaitStart =
        ('awaitStartTime' in toolCall && toolCall.awaitStartTime) ||
        ('startTime' in toolCall && toolCall.startTime);
      if (awaitStart) {
        const awaitDuration = Date.now() - awaitStart;
        uiTelemetryService.recordAwaitDuration(awaitDuration);
      }
    }

    await originalOnConfirm(outcome, payload);

    if (outcome === ToolConfirmationOutcome.ProceedAlways) {
      await this.autoApproveCompatiblePendingTools(signal, callId);
    }

    this.setToolCallOutcome(callId, outcome);

    if (outcome === ToolConfirmationOutcome.Cancel || signal.aborted) {
      // Use custom cancel message from payload if provided, otherwise use default
      const cancelMessage =
        payload?.cancelMessage || 'User did not allow tool call';
      this.setStatusInternal(callId, 'cancelled', cancelMessage);
    } else if (outcome === ToolConfirmationOutcome.ModifyWithEditor) {
      const waitingToolCall = toolCall as WaitingToolCall;
      if (isModifiableDeclarativeTool(waitingToolCall.tool)) {
        const modifyContext = waitingToolCall.tool.getModifyContext(signal);
        const editorType = this.getPreferredEditor();
        if (!editorType) {
          return;
        }

        this.setStatusInternal(callId, 'awaiting_approval', {
          ...waitingToolCall.confirmationDetails,
          isModifying: true,
        } as ToolCallConfirmationDetails);

        const { updatedParams, updatedDiff } = await modifyWithEditor<
          typeof waitingToolCall.request.args
        >(
          waitingToolCall.request.args,
          modifyContext as ModifyContext<typeof waitingToolCall.request.args>,
          editorType,
          signal,
          this.onEditorClose,
        );
        this.setArgsInternal(callId, updatedParams);
        this.setStatusInternal(callId, 'awaiting_approval', {
          ...waitingToolCall.confirmationDetails,
          fileDiff: updatedDiff,
          isModifying: false,
        } as ToolCallConfirmationDetails);
      }
    } else {
      // If the client provided new content, apply it before scheduling.
      if (payload?.newContent && toolCall) {
        await this._applyInlineModify(
          toolCall as WaitingToolCall,
          payload,
          signal,
        );
      }
      this.setStatusInternal(callId, 'scheduled');
    }
    await this.attemptExecutionOfScheduledCalls(signal);
  }

  /**
   * Applies user-provided content changes to a tool call that is awaiting confirmation.
   * This method updates the tool's arguments and refreshes the confirmation prompt with a new diff
   * before the tool is scheduled for execution.
   * @private
   */
  private async _applyInlineModify(
    toolCall: WaitingToolCall,
    payload: ToolConfirmationPayload,
    signal: AbortSignal,
  ): Promise<void> {
    if (
      toolCall.confirmationDetails.type !== 'edit' ||
      !isModifiableDeclarativeTool(toolCall.tool) ||
      !payload.newContent
    ) {
      return;
    }

    const modifyContext = toolCall.tool.getModifyContext(signal);
    const currentContent = await modifyContext.getCurrentContent(
      toolCall.request.args,
    );

    const updatedParams = modifyContext.createUpdatedParams(
      currentContent,
      payload.newContent,
      toolCall.request.args,
    );
    const updatedDiff = Diff.createPatch(
      modifyContext.getFilePath(toolCall.request.args),
      currentContent,
      payload.newContent,
      'Current',
      'Proposed',
    );

    this.setArgsInternal(toolCall.request.callId, updatedParams);
    this.setStatusInternal(toolCall.request.callId, 'awaiting_approval', {
      ...toolCall.confirmationDetails,
      fileDiff: updatedDiff,
    });
  }

  private async attemptExecutionOfScheduledCalls(
    signal: AbortSignal,
  ): Promise<void> {
    const allCallsFinalOrScheduled = this.toolCalls.every(
      (call) =>
        call.status === 'scheduled' ||
        call.status === 'cancelled' ||
        call.status === 'success' ||
        call.status === 'error',
    );

    if (allCallsFinalOrScheduled) {
      const callsToExecute = this.toolCalls.filter(
        (call) => call.status === 'scheduled',
      );

      for (const toolCall of callsToExecute) {
        if (toolCall.status !== 'scheduled') continue;

        const scheduledCall = toolCall;
        const { callId, name: toolName } = scheduledCall.request;

        const invocation = scheduledCall.invocation;
        this.setStatusInternal(callId, 'executing');

        const liveOutputCallback = scheduledCall.tool.canUpdateOutput
          ? (outputChunk: ToolResultDisplay) => {
              if (this.outputUpdateHandler) {
                this.outputUpdateHandler(callId, outputChunk);
              }
              this.toolCalls = this.toolCalls.map((tc) =>
                tc.request.callId === callId && tc.status === 'executing'
                  ? { ...tc, liveOutput: outputChunk }
                  : tc,
              );
              this.notifyToolCallsUpdate();
            }
          : undefined;

        const shellExecutionConfig = this.config.getShellExecutionConfig();

        // TODO: Refactor to remove special casing for ShellToolInvocation.
        // Introduce a generic callbacks object for the execute method to handle
        // things like `onPid` and `onLiveOutput`. This will make the scheduler
        // agnostic to the invocation type.
        let promise: Promise<ToolResult>;
        if (invocation instanceof ShellToolInvocation) {
          const setPidCallback = (pid: number) => {
            this.toolCalls = this.toolCalls.map((tc) =>
              tc.request.callId === callId && tc.status === 'executing'
                ? { ...tc, pid }
                : tc,
            );
            this.notifyToolCallsUpdate();
          };
          promise = invocation.execute(
            signal,
            liveOutputCallback,
            shellExecutionConfig,
            setPidCallback,
            this.onPasswordPrompt,
          );
        } else {
          promise = invocation.execute(
            signal,
            liveOutputCallback,
            shellExecutionConfig,
          );
        }

        try {
          const toolResult: ToolResult = await promise;

          // Redact secrets from tool result before further processing
          toolResult.llmContent = redactPartListUnion(toolResult.llmContent);
          if (typeof toolResult.returnDisplay === 'string') {
            toolResult.returnDisplay = redactSecrets(toolResult.returnDisplay);
          } else if (
            toolResult.returnDisplay &&
            typeof toolResult.returnDisplay === 'object' &&
            'ansiOutput' in toolResult.returnDisplay
          ) {
            const ansiDisplay = toolResult.returnDisplay as {
              ansiOutput: import('../utils/terminalSerializer.js').AnsiOutput;
            };
            ansiDisplay.ansiOutput = redactAnsiOutput(ansiDisplay.ansiOutput);
          } else if (
            toolResult.returnDisplay &&
            typeof toolResult.returnDisplay === 'object' &&
            'fileDiff' in toolResult.returnDisplay
          ) {
            // Redact secrets from FileDiff display (WriteFile / EditFile results)
            const diffDisplay = toolResult.returnDisplay as FileDiff;
            diffDisplay.fileDiff = redactSecrets(diffDisplay.fileDiff);
            diffDisplay.newContent = redactSecrets(diffDisplay.newContent);
            if (diffDisplay.originalContent !== null) {
              diffDisplay.originalContent = redactSecrets(
                diffDisplay.originalContent,
              );
            }
          }

          if (signal.aborted) {
            this.setStatusInternal(
              callId,
              'cancelled',
              'User cancelled tool execution.',
            );
            continue;
          }

          if (toolResult.error === undefined) {
            let content = toolResult.llmContent;
            let outputFile: string | undefined = undefined;
            const contentLength =
              typeof content === 'string' ? content.length : undefined;
            if (
              typeof content === 'string' &&
              toolName === ShellTool.Name &&
              this.config.getEnableToolOutputTruncation() &&
              this.config.getTruncateToolOutputThreshold() > 0 &&
              this.config.getTruncateToolOutputLines() > 0
            ) {
              const originalContentLength = content.length;
              const threshold = this.config.getTruncateToolOutputThreshold();
              const lines = this.config.getTruncateToolOutputLines();
              const truncatedResult = await truncateAndSaveToFile(
                content,
                callId,
                this.config.storage.getProjectTempDir(),
                threshold,
                lines,
              );
              content = truncatedResult.content;
              outputFile = truncatedResult.outputFile;

              if (outputFile) {
                logToolOutputTruncated(
                  this.config,
                  new ToolOutputTruncatedEvent(
                    scheduledCall.request.prompt_id,
                    {
                      toolName,
                      originalContentLength,
                      truncatedContentLength: content.length,
                      threshold,
                      lines,
                    },
                  ),
                );
              }
            }

            const response = convertToFunctionResponse(
              toolName,
              callId,
              content,
            );
            const successResponse: ToolCallResponseInfo = {
              callId,
              responseParts: response,
              resultDisplay: toolResult.returnDisplay,
              error: undefined,
              errorType: undefined,
              outputFile,
              contentLength,
            };

            // Fire PostToolUse hook after successful tool execution
            if (this.config.getEnableHooks()) {
              const hookSystem = this.config.getHookSystem();
              if (hookSystem) {
                try {
                  // Convert content to string for hook input
                  // content can be string or PartListUnion (array of parts)
                  let contentForHook: string;
                  if (typeof content === 'string') {
                    contentForHook = content;
                  } else if (Array.isArray(content)) {
                    // Extract text from PartListUnion array
                    contentForHook = content
                      .map((part: unknown) => {
                        if (typeof part === 'string') return part;
                        if (
                          part &&
                          typeof part === 'object' &&
                          'text' in part &&
                          typeof (part as { text?: unknown }).text === 'string'
                        ) {
                          return (part as { text: string }).text;
                        }
                        return '';
                      })
                      .join('');
                  } else {
                    contentForHook = '';
                  }

                  const postToolOutput = await hookSystem.firePostToolUseEvent(
                    toolName,
                    scheduledCall.request.args,
                    {
                      llmContent: contentForHook,
                      returnDisplay:
                        typeof toolResult.returnDisplay === 'string'
                          ? toolResult.returnDisplay
                          : undefined,
                    },
                    undefined,
                    undefined,
                    callId,
                    signal,
                  );

                  if (postToolOutput) {
                    // Compute mergedDecision once so every notification shares
                    // the same context. Mirrors the PreToolUse path: blocking
                    // (block/deny or continue=false) wins over ask, which
                    // wins over the per-hook decision used for individual
                    // notification dimming.
                    const isBlockingPost =
                      postToolOutput.isBlockingDecision() ||
                      postToolOutput.shouldStopExecution();
                    const isAskPost = postToolOutput.isAskDecision();
                    const mergedDecisionPost: HookDecision | undefined =
                      isBlockingPost
                        ? postToolOutput.decision === 'deny'
                          ? 'deny'
                          : 'block'
                        : isAskPost
                          ? 'ask'
                          : postToolOutput.decision;

                    // Emit per-hook notifications BEFORE applying the merged
                    // decision so every hook's reason/systemMessage surfaces
                    // in the UI even when the overall outcome later replaces
                    // the tool response.
                    if (
                      this.outputUpdateHandler &&
                      postToolOutput.notifications?.length
                    ) {
                      for (const n of postToolOutput.notifications) {
                        this.outputUpdateHandler(callId, {
                          hookName: n.hookName,
                          hookMessage: n.message,
                          decision: n.decision,
                          mergedDecision: mergedDecisionPost,
                        });
                      }
                    }

                    // If hook denies, replace tool result with reason
                    if (postToolOutput.isBlockingDecision()) {
                      const reason = postToolOutput.getEffectiveReason();
                      const blockedResponse = convertToFunctionResponse(
                        toolName,
                        callId,
                        reason,
                      );
                      successResponse.responseParts = blockedResponse;
                    }

                    // Append additional context to tool result
                    const additionalContext =
                      postToolOutput.getAdditionalContext();
                    if (
                      additionalContext &&
                      !postToolOutput.isBlockingDecision()
                    ) {
                      const augmented = convertToFunctionResponse(
                        toolName,
                        callId,
                        (typeof content === 'string' ? content : '') +
                          '\n' +
                          additionalContext,
                      );
                      successResponse.responseParts = augmented;
                    }

                    // Handle continue=false
                    if (postToolOutput.shouldStopExecution()) {
                      this.setStatusInternal(
                        callId,
                        'success',
                        successResponse,
                      );
                      continue;
                    }
                  }
                } catch (hookError) {
                  console.error(
                    `PostToolUse hook error for ${toolName}: ${hookError}`,
                  );
                }
              }
            }

            this.setStatusInternal(callId, 'success', successResponse);
          } else {
            // It is a failure
            const error = new Error(toolResult.error.message);

            // Fire PostToolUseFailure hook for all tool failures (not
            // just shell/sandbox) so that allow+reason notifications are
            // always surfaced to the UI.
            let handledBySandboxBypass = false;
            let failureOutput:
              | Awaited<
                  ReturnType<
                    NonNullable<
                      ReturnType<Config['getHookSystem']>
                    >['firePostToolUseFailureEvent']
                  >
                >
              | undefined;
            if (this.config.getEnableHooks()) {
              const hookSystem = this.config.getHookSystem();
              if (hookSystem) {
                try {
                  failureOutput = await hookSystem.firePostToolUseFailureEvent(
                    callId,
                    toolName,
                    scheduledCall.request.args,
                    toolResult.error.message,
                    toolResult.error.type,
                  );
                  if (
                    this.outputUpdateHandler &&
                    failureOutput?.notifications?.length
                  ) {
                    const isBlockingFailure =
                      failureOutput.isBlockingDecision() ||
                      failureOutput.shouldStopExecution();
                    const isAskFailure = failureOutput.isAskDecision();
                    const mergedDecisionFailure: HookDecision | undefined =
                      isBlockingFailure
                        ? failureOutput.decision === 'deny'
                          ? 'deny'
                          : 'block'
                        : isAskFailure
                          ? 'ask'
                          : failureOutput.decision;
                    for (const n of failureOutput.notifications) {
                      this.outputUpdateHandler(callId, {
                        hookName: n.hookName,
                        hookMessage: n.message,
                        decision: n.decision,
                        mergedDecision: mergedDecisionFailure,
                      });
                    }
                  }

                  // Sandbox bypass: only for shell tools with a registered bypass callback
                  if (
                    this.onSandboxBypassRequested &&
                    toolName === ShellTool.Name
                  ) {
                    const bypassRequest =
                      failureOutput?.getSandboxBypassRequest?.();
                    if (bypassRequest) {
                      const approved =
                        await this.onSandboxBypassRequested(bypassRequest);
                      if (approved) {
                        hookSystem.setHookEnabled('sandbox-guard', false);
                        try {
                          const originalArgs = {
                            ...scheduledCall.request.args,
                            command: bypassRequest.original_command,
                          };
                          const originalInvocation = this.buildInvocation(
                            scheduledCall.tool,
                            originalArgs,
                          );
                          if (!(originalInvocation instanceof Error)) {
                            let retryPromise: Promise<ToolResult>;
                            if (
                              originalInvocation instanceof ShellToolInvocation
                            ) {
                              // Provide a setPidCallback so the bypass process pid
                              // is tracked in tool call state. Without this, the
                              // ShellInputPrompt would keep referencing the dead
                              // linux-sandbox PTY and sudo password input would fail.
                              const bypassSetPidCallback = (pid: number) => {
                                this.toolCalls = this.toolCalls.map((tc) =>
                                  tc.request.callId === callId &&
                                  tc.status === 'executing'
                                    ? { ...tc, pid }
                                    : tc,
                                );
                                this.notifyToolCallsUpdate();
                              };
                              retryPromise = originalInvocation.execute(
                                signal,
                                liveOutputCallback,
                                shellExecutionConfig,
                                bypassSetPidCallback,
                                this.onPasswordPrompt,
                              );
                            } else {
                              retryPromise = originalInvocation.execute(
                                signal,
                                liveOutputCallback,
                                shellExecutionConfig,
                              );
                            }
                            const retryResult = await retryPromise;
                            if (retryResult.error === undefined) {
                              const retryContent = retryResult.llmContent;
                              const retryResponse = convertToFunctionResponse(
                                toolName,
                                callId,
                                retryContent,
                              );
                              this.setStatusInternal(callId, 'success', {
                                callId,
                                responseParts: retryResponse,
                                resultDisplay: retryResult.returnDisplay,
                                error: undefined,
                                errorType: undefined,
                              });
                              handledBySandboxBypass = true;
                            }
                          }
                        } finally {
                          hookSystem.setHookEnabled('sandbox-guard', true);
                        }
                      }
                    }
                  }
                } catch (hookErr) {
                  // Hook errors are non-fatal
                  if (this.config.getDebugMode()) {
                    console.debug(
                      `PostToolUseFailure hook error (non-fatal): ${hookErr}`,
                    );
                  }
                }
              }
            }

            if (!handledBySandboxBypass) {
              const errorResponse = createErrorResponse(
                scheduledCall.request,
                error,
                toolResult.error.type,
              );
              this.setStatusInternal(callId, 'error', errorResponse);
            }
          }
        } catch (executionError: unknown) {
          if (signal.aborted) {
            this.setStatusInternal(
              callId,
              'cancelled',
              'User cancelled tool execution.',
            );
          } else {
            this.setStatusInternal(
              callId,
              'error',
              createErrorResponse(
                scheduledCall.request,
                executionError instanceof Error
                  ? executionError
                  : new Error(String(executionError)),
                ToolErrorType.UNHANDLED_EXCEPTION,
              ),
            );
          }
        }
      }
    }
  }

  private async checkAndNotifyCompletion(): Promise<void> {
    const allCallsAreTerminal = this.toolCalls.every(
      (call) =>
        call.status === 'success' ||
        call.status === 'error' ||
        call.status === 'cancelled',
    );

    if (this.toolCalls.length > 0 && allCallsAreTerminal) {
      const completedCalls = [...this.toolCalls] as CompletedToolCall[];
      this.toolCalls = [];

      for (const call of completedCalls) {
        logToolCall(this.config, new ToolCallEvent(call));
      }

      // Record tool results before notifying completion
      this.recordToolResults(completedCalls);

      if (this.onAllToolCallsComplete) {
        this.isFinalizingToolCalls = true;
        await this.onAllToolCallsComplete(completedCalls);
        this.isFinalizingToolCalls = false;
      }
      this.notifyToolCallsUpdate();
      // After completion, process the next item in the queue.
      if (this.requestQueue.length > 0) {
        const next = this.requestQueue.shift()!;
        this._schedule(next.request, next.signal)
          .then(next.resolve)
          .catch(next.reject);
      }
    }
  }

  /**
   * Records tool results to the chat recording service.
   * This captures both the raw Content (for API reconstruction) and
   * enriched metadata (for UI recovery).
   */
  private recordToolResults(completedCalls: CompletedToolCall[]): void {
    if (!this.chatRecordingService) return;

    // Collect all response parts from completed calls
    const responseParts: Part[] = completedCalls.flatMap(
      (call) => call.response.responseParts,
    );

    if (responseParts.length === 0) return;

    // Record each tool result individually
    for (const call of completedCalls) {
      this.chatRecordingService.recordToolResult(call.response.responseParts, {
        callId: call.request.callId,
        status: call.status,
        resultDisplay: call.response.resultDisplay,
        error: call.response.error,
        errorType: call.response.errorType,
      });
    }
  }

  private notifyToolCallsUpdate(): void {
    if (this.onToolCallsUpdate) {
      this.onToolCallsUpdate([...this.toolCalls]);
    }
  }

  private setToolCallOutcome(callId: string, outcome: ToolConfirmationOutcome) {
    this.toolCalls = this.toolCalls.map((call) => {
      if (call.request.callId !== callId) return call;
      return {
        ...call,
        outcome,
      };
    });
  }

  private async autoApproveCompatiblePendingTools(
    signal: AbortSignal,
    triggeringCallId: string,
  ): Promise<void> {
    const pendingTools = this.toolCalls.filter(
      (call) =>
        call.status === 'awaiting_approval' &&
        call.request.callId !== triggeringCallId,
    ) as WaitingToolCall[];

    for (const pendingTool of pendingTools) {
      // Never silently approve a tool that was forced into awaiting_approval by
      // a hook 'ask' decision — the hook explicitly requested per-invocation
      // confirmation.  Each such call must be presented to the user individually
      // so the hook warning is never silently skipped within a batch.
      if (pendingTool.hookForceAsk) {
        continue;
      }

      try {
        const stillNeedsConfirmation =
          await pendingTool.invocation.shouldConfirmExecute(signal);

        if (!stillNeedsConfirmation) {
          this.setToolCallOutcome(
            pendingTool.request.callId,
            ToolConfirmationOutcome.ProceedAlways,
          );
          this.setStatusInternal(pendingTool.request.callId, 'scheduled');
        }
      } catch (error) {
        console.error(
          `Error checking confirmation for tool ${pendingTool.request.callId}:`,
          error,
        );
      }
    }
  }
}
