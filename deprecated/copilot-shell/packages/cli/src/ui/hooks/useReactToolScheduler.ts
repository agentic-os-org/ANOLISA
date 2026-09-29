/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type {
  Config,
  ToolCallRequestInfo,
  ExecutingToolCall,
  ScheduledToolCall,
  ValidatingToolCall,
  WaitingToolCall,
  CompletedToolCall,
  CancelledToolCall,
  OutputUpdateHandler,
  AllToolCallsCompleteHandler,
  ToolCallsUpdateHandler,
  ToolCall,
  Status as CoreStatus,
  EditorType,
  SandboxBypassApprovalRequest,
  HookNotificationDisplay,
} from '@copilot-shell/core';
import { CoreToolScheduler } from '@copilot-shell/core';
import { useCallback, useState, useMemo, useRef } from 'react';
import type {
  HistoryItemToolGroup,
  IndividualToolCallDisplay,
  HookNotificationItem,
} from '../types.js';
import { ToolCallStatus } from '../types.js';

export type ScheduleFn = (
  request: ToolCallRequestInfo | ToolCallRequestInfo[],
  signal: AbortSignal,
) => void;
export type MarkToolsAsSubmittedFn = (callIds: string[]) => void;

export type TrackedScheduledToolCall = ScheduledToolCall & {
  responseSubmittedToGemini?: boolean;
  hookSystemMessage?: HookNotificationItem[];
};
export type TrackedValidatingToolCall = ValidatingToolCall & {
  responseSubmittedToGemini?: boolean;
  hookSystemMessage?: HookNotificationItem[];
};
export type TrackedWaitingToolCall = WaitingToolCall & {
  responseSubmittedToGemini?: boolean;
  hookSystemMessage?: HookNotificationItem[];
};
export type TrackedExecutingToolCall = ExecutingToolCall & {
  responseSubmittedToGemini?: boolean;
  pid?: number;
  hookSystemMessage?: HookNotificationItem[];
};
export type TrackedCompletedToolCall = CompletedToolCall & {
  responseSubmittedToGemini?: boolean;
  hookSystemMessage?: HookNotificationItem[];
};
export type TrackedCancelledToolCall = CancelledToolCall & {
  responseSubmittedToGemini?: boolean;
  hookSystemMessage?: HookNotificationItem[];
};

export type TrackedToolCall =
  | TrackedScheduledToolCall
  | TrackedValidatingToolCall
  | TrackedWaitingToolCall
  | TrackedExecutingToolCall
  | TrackedCompletedToolCall
  | TrackedCancelledToolCall;

/**
 * Type guard: checks if an outputChunk is a structured HookNotificationDisplay.
 */
function isHookNotification(chunk: unknown): chunk is HookNotificationDisplay {
  return (
    typeof chunk === 'object' &&
    chunk !== null &&
    'hookName' in chunk &&
    'hookMessage' in chunk
  );
}

export function useReactToolScheduler(
  onComplete: (tools: CompletedToolCall[]) => Promise<void>,
  config: Config,
  getPreferredEditor: () => EditorType | undefined,
  onEditorClose: () => void,
  onPasswordPrompt?: () => void,
  onSandboxBypassRequested?: (
    request: SandboxBypassApprovalRequest,
  ) => Promise<boolean>,
): [TrackedToolCall[], ScheduleFn, MarkToolsAsSubmittedFn] {
  const [toolCallsForDisplay, setToolCallsForDisplay] = useState<
    TrackedToolCall[]
  >([]);

  // Synchronous registry of hook notifications keyed by callId.
  // React state updates (setToolCallsForDisplay) are asynchronous, so when
  // allToolCallsCompleteHandler fires it may see stale state. This ref is
  // updated synchronously inside outputUpdateHandler and is always current
  // when the history snapshot is created.
  const hookNotificationsRef = useRef<Map<string, HookNotificationItem[]>>(
    new Map(),
  );

  const outputUpdateHandler: OutputUpdateHandler = useCallback(
    (toolCallId, outputChunk) => {
      // Structured hook notification from the core scheduler.
      if (isHookNotification(outputChunk)) {
        const item: HookNotificationItem = {
          hookName: outputChunk.hookName,
          message: outputChunk.hookMessage,
          decision: outputChunk.decision,
          mergedDecision: outputChunk.mergedDecision,
        };
        // Synchronous ref update for history snapshots.
        const existing = hookNotificationsRef.current.get(toolCallId) ?? [];
        hookNotificationsRef.current.set(toolCallId, [...existing, item]);
        // React state update for live display.
        setToolCallsForDisplay((prevCalls) =>
          prevCalls.map((tc) => {
            if (tc.request.callId === toolCallId) {
              return {
                ...tc,
                hookSystemMessage: [...(tc.hookSystemMessage ?? []), item],
              } as typeof tc;
            }
            return tc;
          }),
        );
        return;
      }

      // Live streaming output (executing phase).
      setToolCallsForDisplay((prevCalls) =>
        prevCalls.map((tc) => {
          if (tc.request.callId === toolCallId && tc.status === 'executing') {
            const executingTc = tc as TrackedExecutingToolCall;
            return { ...executingTc, liveOutput: outputChunk };
          }
          return tc;
        }),
      );
    },
    [],
  );

  const allToolCallsCompleteHandler: AllToolCallsCompleteHandler = useCallback(
    async (completedToolCalls) => {
      // completedToolCalls comes from the core scheduler (CompletedToolCall[])
      // and does NOT carry the UI-only hookSystemMessage field. Enrich each
      // entry from the synchronous ref before handing off to onComplete so
      // that the history snapshot preserves hook notifications.
      const enriched = completedToolCalls.map((tc) => ({
        ...tc,
        hookSystemMessage: hookNotificationsRef.current.get(tc.request.callId),
      }));
      // Clean up processed entries to avoid unbounded growth.
      completedToolCalls.forEach((tc) =>
        hookNotificationsRef.current.delete(tc.request.callId),
      );
      await onComplete(enriched as CompletedToolCall[]);
    },
    [onComplete],
  );

  const toolCallsUpdateHandler: ToolCallsUpdateHandler = useCallback(
    (updatedCoreToolCalls: ToolCall[]) => {
      setToolCallsForDisplay((prevTrackedCalls) =>
        updatedCoreToolCalls.map((coreTc) => {
          const existingTrackedCall = prevTrackedCalls.find(
            (ptc) => ptc.request.callId === coreTc.request.callId,
          );
          // Start with the new core state, then layer on the existing UI state
          // to ensure UI-only properties like pid are preserved.
          const responseSubmittedToGemini =
            existingTrackedCall?.responseSubmittedToGemini ?? false;

          if (coreTc.status === 'executing') {
            return {
              ...coreTc,
              responseSubmittedToGemini,
              liveOutput: (existingTrackedCall as TrackedExecutingToolCall)
                ?.liveOutput,
              pid: (coreTc as ExecutingToolCall).pid,
              // Carry forward hook notification from pre-execution phase
              hookSystemMessage: existingTrackedCall?.hookSystemMessage,
            };
          }

          // For other statuses, explicitly set liveOutput and pid to undefined
          // to ensure they are not carried over from a previous executing state.
          return {
            ...coreTc,
            responseSubmittedToGemini,
            liveOutput: undefined,
            pid: undefined,
            // hookSystemMessage is a sticky UI field: preserve it across transitions
            hookSystemMessage: existingTrackedCall?.hookSystemMessage,
          };
        }),
      );
    },
    [setToolCallsForDisplay],
  );

  const scheduler = useMemo(
    () =>
      new CoreToolScheduler({
        config,
        chatRecordingService: config.getChatRecordingService(),
        outputUpdateHandler,
        onAllToolCallsComplete: allToolCallsCompleteHandler,
        onToolCallsUpdate: toolCallsUpdateHandler,
        getPreferredEditor,
        onEditorClose,
        onPasswordPrompt,
        onSandboxBypassRequested,
      }),
    [
      config,
      outputUpdateHandler,
      allToolCallsCompleteHandler,
      toolCallsUpdateHandler,
      getPreferredEditor,
      onEditorClose,
      onPasswordPrompt,
      onSandboxBypassRequested,
    ],
  );

  const schedule: ScheduleFn = useCallback(
    (
      request: ToolCallRequestInfo | ToolCallRequestInfo[],
      signal: AbortSignal,
    ) => {
      void scheduler.schedule(request, signal);
    },
    [scheduler],
  );

  const markToolsAsSubmitted: MarkToolsAsSubmittedFn = useCallback(
    (callIdsToMark: string[]) => {
      setToolCallsForDisplay((prevCalls) =>
        prevCalls.map((tc) =>
          callIdsToMark.includes(tc.request.callId)
            ? { ...tc, responseSubmittedToGemini: true }
            : tc,
        ),
      );
    },
    [],
  );

  return [toolCallsForDisplay, schedule, markToolsAsSubmitted];
}

/**
 * Maps a CoreToolScheduler status to the UI's ToolCallStatus enum.
 */
function mapCoreStatusToDisplayStatus(coreStatus: CoreStatus): ToolCallStatus {
  switch (coreStatus) {
    case 'validating':
      return ToolCallStatus.Executing;
    case 'awaiting_approval':
      return ToolCallStatus.Confirming;
    case 'executing':
      return ToolCallStatus.Executing;
    case 'success':
      return ToolCallStatus.Success;
    case 'cancelled':
      return ToolCallStatus.Canceled;
    case 'error':
      return ToolCallStatus.Error;
    case 'scheduled':
      return ToolCallStatus.Pending;
    default: {
      const exhaustiveCheck: never = coreStatus;
      console.warn(`Unknown core status encountered: ${exhaustiveCheck}`);
      return ToolCallStatus.Error;
    }
  }
}

/**
 * Transforms `TrackedToolCall` objects into `HistoryItemToolGroup` objects for UI display.
 */
export function mapToDisplay(
  toolOrTools: TrackedToolCall[] | TrackedToolCall,
): HistoryItemToolGroup {
  const toolCalls = Array.isArray(toolOrTools) ? toolOrTools : [toolOrTools];

  const toolDisplays = toolCalls.map(
    (trackedCall): IndividualToolCallDisplay => {
      let displayName: string;
      let description: string;
      let renderOutputAsMarkdown = false;

      if (trackedCall.status === 'error') {
        displayName =
          trackedCall.tool === undefined
            ? trackedCall.request.name
            : trackedCall.tool.displayName;
        description = JSON.stringify(trackedCall.request.args);
      } else {
        displayName = trackedCall.tool.displayName;
        description = trackedCall.invocation.getDescription();
        renderOutputAsMarkdown = trackedCall.tool.isOutputMarkdown;
      }

      const baseDisplayProperties: Omit<
        IndividualToolCallDisplay,
        'status' | 'resultDisplay' | 'confirmationDetails'
      > = {
        callId: trackedCall.request.callId,
        name: displayName,
        description,
        renderOutputAsMarkdown,
      };

      switch (trackedCall.status) {
        case 'success':
          return {
            ...baseDisplayProperties,
            status: mapCoreStatusToDisplayStatus(trackedCall.status),
            resultDisplay: trackedCall.response.resultDisplay,
            confirmationDetails: undefined,
            outputFile: trackedCall.response.outputFile,
            hookNotification: trackedCall.hookSystemMessage?.length
              ? trackedCall.hookSystemMessage
              : undefined,
          };
        case 'error':
          return {
            ...baseDisplayProperties,
            status: mapCoreStatusToDisplayStatus(trackedCall.status),
            resultDisplay: trackedCall.response.resultDisplay,
            confirmationDetails: undefined,
            hookNotification: trackedCall.hookSystemMessage?.length
              ? trackedCall.hookSystemMessage
              : undefined,
          };
        case 'cancelled':
          return {
            ...baseDisplayProperties,
            status: mapCoreStatusToDisplayStatus(trackedCall.status),
            resultDisplay: trackedCall.response.resultDisplay,
            confirmationDetails: undefined,
            hookNotification: trackedCall.hookSystemMessage?.length
              ? trackedCall.hookSystemMessage
              : undefined,
          };
        case 'awaiting_approval':
          return {
            ...baseDisplayProperties,
            status: mapCoreStatusToDisplayStatus(trackedCall.status),
            resultDisplay: undefined,
            confirmationDetails: trackedCall.confirmationDetails,
            hookNotification: (trackedCall as TrackedWaitingToolCall)
              .hookSystemMessage?.length
              ? (trackedCall as TrackedWaitingToolCall).hookSystemMessage
              : undefined,
          };
        case 'executing':
          return {
            ...baseDisplayProperties,
            status: mapCoreStatusToDisplayStatus(trackedCall.status),
            // Show live streaming output if available. The hook pre-execution
            // notification is rendered separately via hookNotification so it
            // remains visible alongside live output.
            resultDisplay:
              (trackedCall as TrackedExecutingToolCall).liveOutput ?? undefined,
            confirmationDetails: undefined,
            ptyId: (trackedCall as TrackedExecutingToolCall).pid,
            hookNotification: trackedCall.hookSystemMessage?.length
              ? trackedCall.hookSystemMessage
              : undefined,
          };
        case 'validating': // Fallthrough
        case 'scheduled':
          return {
            ...baseDisplayProperties,
            status: mapCoreStatusToDisplayStatus(trackedCall.status),
            resultDisplay: undefined,
            confirmationDetails: undefined,
            // Hook pre-execution notification shown via hookNotification so it
            // persists through all subsequent states.
            hookNotification: trackedCall.hookSystemMessage?.length
              ? trackedCall.hookSystemMessage
              : undefined,
          };
        default: {
          const exhaustiveCheck: never = trackedCall;
          return {
            callId: (exhaustiveCheck as TrackedToolCall).request.callId,
            name: 'Unknown Tool',
            description: 'Encountered an unknown tool call state.',
            status: ToolCallStatus.Error,
            resultDisplay: 'Unknown tool call state',
            confirmationDetails: undefined,
            renderOutputAsMarkdown: false,
          };
        }
      }
    },
  );

  return {
    type: 'tool_group',
    tools: toolDisplays,
  };
}
