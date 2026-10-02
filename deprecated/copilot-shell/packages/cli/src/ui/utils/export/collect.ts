/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { randomUUID } from 'node:crypto';
import type { Config, ChatRecord } from '@copilot-shell/core';
import type { SessionContext } from '../../../acp-integration/session/types.js';
import type * as acp from '../../../acp-integration/acp.js';
import { HistoryReplayer } from '../../../acp-integration/session/HistoryReplayer.js';
import type { ExportMessage, ExportSessionData } from './types.js';

/**
 * Export session context that captures session updates into export messages.
 * Implements SessionContext to work with HistoryReplayer.
 */
class ExportSessionContext implements SessionContext {
  readonly sessionId: string;
  readonly config: Config;
  private messages: ExportMessage[] = [];
  private currentMessage: {
    type: 'user' | 'assistant';
    role: 'user' | 'assistant' | 'thinking';
    parts: Array<{ text: string }>;
    timestamp: number;
  } | null = null;
  private activeRecordId: string | null = null;
  private toolCallMap: Map<string, ExportMessage['toolCall']> = new Map();

  constructor(sessionId: string, config: Config) {
    this.sessionId = sessionId;
    this.config = config;
  }

  async sendUpdate(update: acp.SessionUpdate): Promise<void> {
    switch (update.sessionUpdate) {
      case 'user_message_chunk':
        this.handleMessageChunk('user', update.content);
        break;
      case 'agent_message_chunk':
        this.handleMessageChunk('assistant', update.content);
        break;
      case 'agent_thought_chunk':
        this.handleMessageChunk('assistant', update.content, 'thinking');
        break;
      case 'tool_call':
        this.flushCurrentMessage();
        this.handleToolCallStart(update);
        break;
      case 'tool_call_update':
        this.handleToolCallUpdate(update);
        break;
      default:
        // Ignore other update types
        break;
    }
  }

  setActiveRecordId(recordId: string | null): void {
    this.activeRecordId = recordId;
  }

  private getMessageUuid(): string {
    return this.activeRecordId ?? randomUUID();
  }

  private handleMessageChunk(
    role: 'user' | 'assistant',
    content: { type: string; text?: string },
    messageRole: 'user' | 'assistant' | 'thinking' = role,
  ): void {
    if (content.type !== 'text' || !content.text) return;

    // If we're starting a new message type, flush the previous one
    if (
      this.currentMessage &&
      (this.currentMessage.type !== role ||
        this.currentMessage.role !== messageRole)
    ) {
      this.flushCurrentMessage();
    }

    // Add to current message or create new one
    if (
      this.currentMessage &&
      this.currentMessage.type === role &&
      this.currentMessage.role === messageRole
    ) {
      this.currentMessage.parts.push({ text: content.text });
    } else {
      this.currentMessage = {
        type: role,
        role: messageRole,
        parts: [{ text: content.text }],
        timestamp: Date.now(),
      };
    }
  }

  private flushCurrentMessage(): void {
    if (!this.currentMessage) return;

    const message: ExportMessage = {
      uuid: this.getMessageUuid(),
      timestamp: new Date(this.currentMessage.timestamp).toISOString(),
      type: this.currentMessage.type,
      message: {
        role: this.currentMessage.role,
        parts: this.currentMessage.parts,
      },
    };

    this.messages.push(message);
    this.currentMessage = null;
  }

  private handleToolCallStart(update: acp.SessionUpdate): void {
    if (update.sessionUpdate !== 'tool_call') return;

    const toolCall: ExportMessage['toolCall'] = {
      toolCallId: update.toolCallId,
      kind: update.kind,
      title: update.title,
      status: 'pending',
      // Cast rawInput from unknown to string | object | undefined
      rawInput: update.rawInput as string | object | undefined,
      locations: update.locations,
    };

    this.toolCallMap.set(update.toolCallId, toolCall);

    const message: ExportMessage = {
      uuid: randomUUID(),
      timestamp: new Date().toISOString(),
      type: 'tool_call',
      toolCall,
    };

    this.messages.push(message);
  }

  private handleToolCallUpdate(update: acp.SessionUpdate): void {
    if (update.sessionUpdate !== 'tool_call_update') return;

    const existingToolCall = this.toolCallMap.get(update.toolCallId);
    if (!existingToolCall) return;

    // Update status
    if (update.status) {
      existingToolCall.status = update.status;
    }

    // Update content
    if (update.content && update.content.length > 0) {
      if (!existingToolCall.content) {
        existingToolCall.content = [];
      }
      // update.content is an array, push each item individually
      for (const item of update.content) {
        existingToolCall.content.push(item);
      }
    }
  }

  getExportSessionData(startTime: string): ExportSessionData {
    // Flush any remaining message
    this.flushCurrentMessage();

    return {
      sessionId: this.sessionId,
      startTime,
      messages: this.messages,
    };
  }
}

/**
 * Collects session data from ChatRecords using HistoryReplayer.
 * This produces the initial ExportSessionData (SSOT) which is then normalized.
 */
export async function collectSessionData(
  conversation: {
    sessionId: string;
    startTime: string;
    messages: ChatRecord[];
  },
  config: Config,
): Promise<ExportSessionData> {
  const context = new ExportSessionContext(conversation.sessionId, config);
  const replayer = new HistoryReplayer(context);

  // Replay all records through the context
  await replayer.replay(conversation.messages);

  return context.getExportSessionData(conversation.startTime);
}
