/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import type { Part } from '@google/genai';
import type {
  ChatRecord,
  Config,
  ToolResultDisplay,
} from '@copilot-shell/core';
import type { ExportMessage, ExportSessionData } from './types.js';

/**
 * Normalizes export session data by merging tool call information from tool_result records.
 * This ensures the SSOT contains complete tool call metadata.
 */
export function normalizeSessionData(
  sessionData: ExportSessionData,
  originalRecords: ChatRecord[],
  config: Config,
): ExportSessionData {
  const normalized = [...sessionData.messages];
  const toolCallIndexById = new Map<string, number>();

  // Build index of tool call messages
  normalized.forEach((message, index) => {
    if (message.type === 'tool_call' && message.toolCall?.toolCallId) {
      toolCallIndexById.set(message.toolCall.toolCallId, index);
    }
  });

  // Merge tool result information into tool call messages
  for (const record of originalRecords) {
    if (record.type !== 'tool_result') continue;

    const toolCallMessage = buildToolCallMessageFromResult(record, config);
    if (!toolCallMessage?.toolCall) continue;

    const existingIndex = toolCallIndexById.get(
      toolCallMessage.toolCall.toolCallId,
    );

    if (existingIndex === undefined) {
      // No existing tool call, add this one
      toolCallIndexById.set(
        toolCallMessage.toolCall.toolCallId,
        normalized.length,
      );
      normalized.push(toolCallMessage);
      continue;
    }

    // Merge into existing tool call
    const existingMessage = normalized[existingIndex];
    if (existingMessage.type !== 'tool_call' || !existingMessage.toolCall) {
      continue;
    }

    mergeToolCallData(existingMessage.toolCall, toolCallMessage.toolCall);
  }

  return {
    ...sessionData,
    messages: normalized,
  };
}

/**
 * Merges incoming tool call data into existing tool call.
 */
function mergeToolCallData(
  existing: NonNullable<ExportMessage['toolCall']>,
  incoming: NonNullable<ExportMessage['toolCall']>,
): void {
  if (!existing.content || existing.content.length === 0) {
    existing.content = incoming.content;
  }
  if (existing.status === 'pending' || existing.status === 'in_progress') {
    existing.status = incoming.status;
  }
  if (!existing.rawInput && incoming.rawInput) {
    existing.rawInput = incoming.rawInput;
  }
  if (!existing.kind || existing.kind === 'other') {
    existing.kind = incoming.kind;
  }
  if ((!existing.title || existing.title === '') && incoming.title) {
    existing.title = incoming.title;
  }
  if (
    (!existing.locations || existing.locations.length === 0) &&
    incoming.locations &&
    incoming.locations.length > 0
  ) {
    existing.locations = incoming.locations;
  }
}

/**
 * Builds a tool call message from a tool_result ChatRecord.
 */
function buildToolCallMessageFromResult(
  record: ChatRecord,
  _config: Config,
): ExportMessage | null {
  if (!record.toolCallResult) return null;

  const toolCallId = record.toolCallResult.callId;
  if (!toolCallId) return null;

  // Extract tool name from functionResponse if available
  let toolName = 'Unknown Tool';
  if (record.message?.parts) {
    for (const part of record.message.parts) {
      if ('functionResponse' in part && part.functionResponse?.name) {
        toolName = part.functionResponse.name;
        break;
      }
    }
  }

  // Build tool call data
  const toolCall: ExportMessage['toolCall'] = {
    toolCallId,
    kind: 'other',
    title: toolName,
    status: record.toolCallResult.error ? 'failed' : 'completed',
    content: [],
  };

  // Add response content
  if (record.toolCallResult.responseParts && toolCall.content) {
    toolCall.content.push({
      type: 'content',
      content: {
        type: 'text',
        text: partsToText(record.toolCallResult.responseParts),
      },
    });
  }

  // Add result display if available
  if (record.toolCallResult.resultDisplay && toolCall.content) {
    const displayContent = formatResultDisplay(
      record.toolCallResult.resultDisplay,
    );
    if (displayContent) {
      toolCall.content.push(displayContent);
    }
  }

  return {
    uuid: record.uuid,
    parentUuid: record.parentUuid,
    sessionId: record.sessionId,
    timestamp: record.timestamp,
    type: 'tool_call',
    toolCall,
  };
}

/**
 * Converts Part[] to text string.
 */
function partsToText(parts: Part[]): string {
  const textParts: string[] = [];
  for (const part of parts) {
    if ('text' in part && part.text) {
      textParts.push(part.text);
    }
  }
  return textParts.join('\n');
}

/**
 * Content item type for tool call display.
 */
type ToolCallContentItem = {
  type: string;
  [key: string]: unknown;
};

/**
 * Formats ToolResultDisplay into export content format.
 */
function formatResultDisplay(
  display: ToolResultDisplay,
): ToolCallContentItem | null {
  if (typeof display === 'string') {
    return {
      type: 'content',
      content: {
        type: 'text',
        text: display,
      },
    };
  }

  // Handle different display types
  if ('fileDiff' in display) {
    return {
      type: 'diff',
      path: display.fileName,
      oldText: display.originalContent || '',
      newText: display.newContent,
    };
  }

  if ('type' in display) {
    if (display.type === 'todo_list') {
      return {
        type: 'content',
        content: {
          type: 'text',
          text: JSON.stringify(display.todos, null, 2),
        },
      };
    }

    if (display.type === 'plan_summary') {
      return {
        type: 'content',
        content: {
          type: 'text',
          text: display.message + '\n\n' + display.plan,
        },
      };
    }

    if (display.type === 'task_execution') {
      // TaskResultDisplay from TaskTool
      return {
        type: 'content',
        content: {
          type: 'text',
          text: JSON.stringify(display, null, 2),
        },
      };
    }
  }

  // Unknown display type
  return {
    type: 'content',
    content: {
      type: 'text',
      text: JSON.stringify(display, null, 2),
    },
  };
}
