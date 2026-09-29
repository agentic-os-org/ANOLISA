/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import type { Mock } from 'vitest';
import type {
  Config,
  ToolCallConfirmationDetails,
  ToolConfirmationPayload,
  ToolInvocation,
  ToolResult,
  ToolResultDisplay,
  ToolRegistry,
} from '../index.js';
import {
  ApprovalMode,
  BaseDeclarativeTool,
  BaseToolInvocation,
  Kind,
  ToolConfirmationOutcome,
  DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
  DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
  SkillTool,
} from '../index.js';
import type { ToolCall, WaitingToolCall } from './coreToolScheduler.js';
import {
  CoreToolScheduler,
  convertToFunctionResponse,
  truncateAndSaveToFile,
} from './coreToolScheduler.js';
import type { Part, PartListUnion } from '@google/genai';
import {
  MockModifiableTool,
  MockTool,
  MOCK_TOOL_SHOULD_CONFIRM_EXECUTE,
} from '../test-utils/mock-tool.js';
import * as fs from 'node:fs/promises';
import * as path from 'node:path';

vi.mock('fs/promises', () => ({
  writeFile: vi.fn(),
}));

class TestApprovalTool extends BaseDeclarativeTool<{ id: string }, ToolResult> {
  static readonly Name = 'testApprovalTool';

  constructor(private config: Config) {
    super(
      TestApprovalTool.Name,
      'TestApprovalTool',
      'A tool for testing approval logic',
      Kind.Edit,
      {
        properties: { id: { type: 'string' } },
        required: ['id'],
        type: 'object',
      },
    );
  }

  protected createInvocation(params: {
    id: string;
  }): ToolInvocation<{ id: string }, ToolResult> {
    return new TestApprovalInvocation(this.config, params);
  }
}

class TestApprovalInvocation extends BaseToolInvocation<
  { id: string },
  ToolResult
> {
  constructor(
    private config: Config,
    params: { id: string },
  ) {
    super(params);
  }

  getDescription(): string {
    return `Test tool ${this.params.id}`;
  }

  override async shouldConfirmExecute(): Promise<
    ToolCallConfirmationDetails | false
  > {
    // Need confirmation unless approval mode is AUTO_EDIT
    if (this.config.getApprovalMode() === ApprovalMode.AUTO_EDIT) {
      return false;
    }

    return {
      type: 'edit',
      title: `Confirm Test Tool ${this.params.id}`,
      fileName: `test-${this.params.id}.txt`,
      filePath: `/test-${this.params.id}.txt`,
      fileDiff: 'Test diff content',
      originalContent: '',
      newContent: 'Test content',
      onConfirm: async (outcome: ToolConfirmationOutcome) => {
        if (outcome === ToolConfirmationOutcome.ProceedAlways) {
          this.config.setApprovalMode(ApprovalMode.AUTO_EDIT);
        }
      },
    };
  }

  async execute(): Promise<ToolResult> {
    return {
      llmContent: `Executed test tool ${this.params.id}`,
      returnDisplay: `Executed test tool ${this.params.id}`,
    };
  }
}

class AbortDuringConfirmationInvocation extends BaseToolInvocation<
  Record<string, unknown>,
  ToolResult
> {
  constructor(
    private readonly abortController: AbortController,
    private readonly abortError: Error,
    params: Record<string, unknown>,
  ) {
    super(params);
  }

  override async shouldConfirmExecute(
    _signal: AbortSignal,
  ): Promise<ToolCallConfirmationDetails | false> {
    this.abortController.abort();
    throw this.abortError;
  }

  async execute(_abortSignal: AbortSignal): Promise<ToolResult> {
    throw new Error('execute should not be called when confirmation fails');
  }

  getDescription(): string {
    return 'Abort during confirmation invocation';
  }
}

class AbortDuringConfirmationTool extends BaseDeclarativeTool<
  Record<string, unknown>,
  ToolResult
> {
  constructor(
    private readonly abortController: AbortController,
    private readonly abortError: Error,
  ) {
    super(
      'abortDuringConfirmationTool',
      'Abort During Confirmation Tool',
      'A tool that aborts while confirming execution.',
      Kind.Other,
      {
        type: 'object',
        properties: {},
      },
    );
  }

  protected createInvocation(
    params: Record<string, unknown>,
  ): ToolInvocation<Record<string, unknown>, ToolResult> {
    return new AbortDuringConfirmationInvocation(
      this.abortController,
      this.abortError,
      params,
    );
  }
}

async function waitForStatus(
  onToolCallsUpdate: Mock,
  status: 'awaiting_approval' | 'executing' | 'success' | 'error' | 'cancelled',
  timeout = 5000,
): Promise<ToolCall> {
  return new Promise((resolve, reject) => {
    const startTime = Date.now();
    const check = () => {
      if (Date.now() - startTime > timeout) {
        const seenStatuses = onToolCallsUpdate.mock.calls
          .flatMap((call) => call[0])
          .map((toolCall: ToolCall) => toolCall.status);
        reject(
          new Error(
            `Timed out waiting for status "${status}". Seen statuses: ${seenStatuses.join(
              ', ',
            )}`,
          ),
        );
        return;
      }

      const foundCall = onToolCallsUpdate.mock.calls
        .flatMap((call) => call[0])
        .find((toolCall: ToolCall) => toolCall.status === status);
      if (foundCall) {
        resolve(foundCall);
      } else {
        setTimeout(check, 10); // Check again in 10ms
      }
    };
    check();
  });
}

describe('CoreToolScheduler', () => {
  it('should cancel a tool call if the signal is aborted before confirmation', async () => {
    const mockTool = new MockTool({
      name: 'mockTool',
      shouldConfirmExecute: MOCK_TOOL_SHOULD_CONFIRM_EXECUTE,
    });
    const declarativeTool = mockTool;
    const mockToolRegistry = {
      getTool: () => declarativeTool,
      getFunctionDeclarations: () => [],
      tools: new Map(),
      discovery: {},
      registerTool: () => {},
      getToolByName: () => declarativeTool,
      getToolByDisplayName: () => declarativeTool,
      getTools: () => [],
      discoverTools: async () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
    } as unknown as ToolRegistry;

    const onAllToolCallsComplete = vi.fn();
    const onToolCallsUpdate = vi.fn();

    const mockConfig = {
      getSessionId: () => 'test-session-id',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => ApprovalMode.DEFAULT,
      getAllowedTools: () => [],
      getContentGeneratorConfig: () => ({
        model: 'test-model',
        authType: 'gemini',
      }),
      getShellExecutionConfig: () => ({
        terminalWidth: 90,
        terminalHeight: 30,
      }),
      storage: {
        getProjectTempDir: () => '/tmp',
      },
      getTruncateToolOutputThreshold: () =>
        DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
      getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
      getToolRegistry: () => mockToolRegistry,
      getUseSmartEdit: () => false,
      getUseModelRouter: () => false,
      getGeminiClient: () => null, // No client needed for these tests
      getChatRecordingService: () => undefined,
      getEnableHooks: () => false,
      getHookSystem: () => undefined,
    } as unknown as Config;

    const scheduler = new CoreToolScheduler({
      config: mockConfig,
      onAllToolCallsComplete,
      onToolCallsUpdate,
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
    });

    const abortController = new AbortController();
    const request = {
      callId: '1',
      name: 'mockTool',
      args: {},
      isClientInitiated: false,
      prompt_id: 'prompt-id-1',
    };

    abortController.abort();
    await scheduler.schedule([request], abortController.signal);

    expect(onAllToolCallsComplete).toHaveBeenCalled();
    const completedCalls = onAllToolCallsComplete.mock
      .calls[0][0] as ToolCall[];
    expect(completedCalls[0].status).toBe('cancelled');
  });

  it('should mark tool call as cancelled when abort happens during confirmation error', async () => {
    const abortController = new AbortController();
    const abortError = new Error('Abort requested during confirmation');
    const declarativeTool = new AbortDuringConfirmationTool(
      abortController,
      abortError,
    );

    const mockToolRegistry = {
      getTool: () => declarativeTool,
      getFunctionDeclarations: () => [],
      tools: new Map(),
      discovery: {},
      registerTool: () => {},
      getToolByName: () => declarativeTool,
      getToolByDisplayName: () => declarativeTool,
      getTools: () => [],
      discoverTools: async () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
    } as unknown as ToolRegistry;

    const onAllToolCallsComplete = vi.fn();
    const onToolCallsUpdate = vi.fn();

    const mockConfig = {
      getSessionId: () => 'test-session-id',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => ApprovalMode.DEFAULT,
      getAllowedTools: () => [],
      getContentGeneratorConfig: () => ({
        model: 'test-model',
        authType: 'gemini',
      }),
      getShellExecutionConfig: () => ({
        terminalWidth: 90,
        terminalHeight: 30,
      }),
      storage: {
        getProjectTempDir: () => '/tmp',
      },
      getTruncateToolOutputThreshold: () =>
        DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
      getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
      getToolRegistry: () => mockToolRegistry,
      getUseSmartEdit: () => false,
      getUseModelRouter: () => false,
      getGeminiClient: () => null,
      getChatRecordingService: () => undefined,
      getEnableHooks: () => false,
      getHookSystem: () => undefined,
    } as unknown as Config;

    const scheduler = new CoreToolScheduler({
      config: mockConfig,
      onAllToolCallsComplete,
      onToolCallsUpdate,
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
    });

    const request = {
      callId: 'abort-1',
      name: 'abortDuringConfirmationTool',
      args: {},
      isClientInitiated: false,
      prompt_id: 'prompt-id-abort',
    };

    await scheduler.schedule([request], abortController.signal);

    expect(onAllToolCallsComplete).toHaveBeenCalled();
    const completedCalls = onAllToolCallsComplete.mock
      .calls[0][0] as ToolCall[];
    expect(completedCalls[0].status).toBe('cancelled');
    const statuses = onToolCallsUpdate.mock.calls.flatMap((call) =>
      (call[0] as ToolCall[]).map((toolCall) => toolCall.status),
    );
    expect(statuses).not.toContain('error');
  });

  describe('getToolSuggestion', () => {
    it('should suggest the top N closest tool names for a typo', () => {
      // Create mocked tool registry
      const mockToolRegistry = {
        getAllToolNames: () => ['list_files', 'read_file', 'write_file'],
        getTool: () => undefined, // No SkillTool in this test
      } as unknown as ToolRegistry;
      const mockConfig = {
        getToolRegistry: () => mockToolRegistry,
        getUseSmartEdit: () => false,
        getUseModelRouter: () => false,
        getGeminiClient: () => null, // No client needed for these tests
        getExcludeTools: () => undefined,
        isInteractive: () => true,
      } as unknown as Config;

      // Create scheduler
      const scheduler = new CoreToolScheduler({
        config: mockConfig,
        getPreferredEditor: () => 'vscode',
        onEditorClose: vi.fn(),
      });

      // Test that the right tool is selected, with only 1 result, for typos
      // @ts-expect-error accessing private method
      const misspelledTool = scheduler.getToolSuggestion('list_fils', 1);
      expect(misspelledTool).toBe(' Did you mean "list_files"?');

      // Test that the right tool is selected, with only 1 result, for prefixes
      // @ts-expect-error accessing private method
      const prefixedTool = scheduler.getToolSuggestion('github.list_files', 1);
      expect(prefixedTool).toBe(' Did you mean "list_files"?');

      // Test that the right tool is first
      // @ts-expect-error accessing private method
      const suggestionMultiple = scheduler.getToolSuggestion('list_fils');
      expect(suggestionMultiple).toBe(
        ' Did you mean one of: "list_files", "read_file", "write_file"?',
      );
    });

    it('should use Levenshtein suggestions for excluded tools (getToolSuggestion only handles non-excluded)', () => {
      // Create mocked tool registry
      const mockToolRegistry = {
        getAllToolNames: () => ['list_files', 'read_file'],
        getTool: () => undefined, // No SkillTool in this test
      } as unknown as ToolRegistry;

      // Create mocked config with excluded tools
      const mockConfig = {
        getToolRegistry: () => mockToolRegistry,
        getUseSmartEdit: () => false,
        getUseModelRouter: () => false,
        getGeminiClient: () => null,
        getExcludeTools: () => ['write_file', 'edit', 'run_shell_command'],
        isInteractive: () => false, // Value doesn't matter, but included for completeness
      } as unknown as Config;

      // Create scheduler
      const scheduler = new CoreToolScheduler({
        config: mockConfig,
        getPreferredEditor: () => 'vscode',
        onEditorClose: vi.fn(),
      });

      // getToolSuggestion no longer handles excluded tools - it only handles truly missing tools
      // So excluded tools will use Levenshtein distance to find similar registered tools
      // @ts-expect-error accessing private method
      const excludedTool = scheduler.getToolSuggestion('write_file');
      expect(excludedTool).toContain('Did you mean');
    });

    it('should use Levenshtein suggestions for non-excluded tools', () => {
      // Create mocked tool registry
      const mockToolRegistry = {
        getAllToolNames: () => ['list_files', 'read_file'],
        getTool: () => undefined, // No SkillTool in this test
      } as unknown as ToolRegistry;

      // Create mocked config with excluded tools
      const mockConfig = {
        getToolRegistry: () => mockToolRegistry,
        getUseSmartEdit: () => false,
        getUseModelRouter: () => false,
        getGeminiClient: () => null,
        getExcludeTools: () => ['write_file', 'edit'],
        isInteractive: () => false, // Value doesn't matter
      } as unknown as Config;

      // Create scheduler
      const scheduler = new CoreToolScheduler({
        config: mockConfig,
        getPreferredEditor: () => 'vscode',
        onEditorClose: vi.fn(),
      });

      // Test that non-excluded tool (hallucinated) still uses Levenshtein suggestions
      // @ts-expect-error accessing private method
      const hallucinatedTool = scheduler.getToolSuggestion('list_fils');
      expect(hallucinatedTool).toContain('Did you mean');
      expect(hallucinatedTool).not.toContain(
        'not available in the current environment',
      );
    });

    it('should suggest using Skill tool when unknown tool name matches a skill name', () => {
      // Create a mock that passes instanceof SkillTool check
      const mockSkillTool = Object.create(SkillTool.prototype);
      mockSkillTool.getAvailableSkillNames = () => [
        'pdf',
        'xlsx',
        'frontend-design',
      ];

      // Create mocked tool registry that returns the mock SkillTool
      const mockToolRegistry = {
        getAllToolNames: () => ['skill', 'list_files', 'read_file'],
        getTool: (name: string) =>
          name === 'skill' ? mockSkillTool : undefined,
      } as unknown as ToolRegistry;

      // Create mocked config
      const mockConfig = {
        getToolRegistry: () => mockToolRegistry,
        getUseSmartEdit: () => false,
        getUseModelRouter: () => false,
        getGeminiClient: () => null,
        getExcludeTools: () => undefined,
        isInteractive: () => true,
      } as unknown as Config;

      // Create scheduler
      const scheduler = new CoreToolScheduler({
        config: mockConfig,
        getPreferredEditor: () => 'vscode',
        onEditorClose: vi.fn(),
      });

      // Test that when unknown tool name matches a skill name, we get skill-specific message
      // @ts-expect-error accessing private method
      const skillMessage = scheduler.getToolNotFoundMessage('pdf');
      expect(skillMessage).toContain('is a skill name, not a tool name');
      expect(skillMessage).toContain('skill');
      expect(skillMessage).toContain('skill: "pdf"');
      // Should NOT contain the standard "not found in registry" prefix
      expect(skillMessage).not.toContain('not found in registry');

      // Test another skill name
      // @ts-expect-error accessing private method
      const xlsxMessage = scheduler.getToolNotFoundMessage('xlsx');
      expect(xlsxMessage).toContain('is a skill name, not a tool name');
      expect(xlsxMessage).toContain('skill: "xlsx"');

      // Test that non-skill names still use standard message with Levenshtein suggestions
      // @ts-expect-error accessing private method
      const nonSkillMessage = scheduler.getToolNotFoundMessage('list_fils');
      expect(nonSkillMessage).toContain('not found in registry');
      expect(nonSkillMessage).toContain('Did you mean');
      expect(nonSkillMessage).not.toContain('is a skill name');
    });
  });

  describe('excluded tools handling', () => {
    it('should return permission error for excluded tools instead of "not found" message', async () => {
      const onAllToolCallsComplete = vi.fn();
      const onToolCallsUpdate = vi.fn();

      const mockToolRegistry = {
        getTool: () => undefined, // Tool not in registry
        getAllToolNames: () => ['list_files', 'read_file'],
        getFunctionDeclarations: () => [],
        tools: new Map(),
        discovery: {},
        registerTool: () => {},
        getToolByName: () => undefined,
        getToolByDisplayName: () => undefined,
        getTools: () => [],
        discoverTools: async () => {},
        getAllTools: () => [],
        getToolsByServer: () => [],
      } as unknown as ToolRegistry;

      const mockConfig = {
        getSessionId: () => 'test-session-id',
        getUsageStatisticsEnabled: () => true,
        getDebugMode: () => false,
        getApprovalMode: () => ApprovalMode.DEFAULT,
        getAllowedTools: () => [],
        getExcludeTools: () => ['write_file', 'edit', 'run_shell_command'],
        getContentGeneratorConfig: () => ({
          model: 'test-model',
          authType: 'gemini',
        }),
        getShellExecutionConfig: () => ({
          terminalWidth: 90,
          terminalHeight: 30,
        }),
        storage: {
          getProjectTempDir: () => '/tmp',
        },
        getTruncateToolOutputThreshold: () =>
          DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
        getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
        getToolRegistry: () => mockToolRegistry,
        getUseSmartEdit: () => false,
        getUseModelRouter: () => false,
        getGeminiClient: () => null,
        getChatRecordingService: () => undefined,
        getEnableHooks: () => false,
        getHookSystem: () => undefined,
      } as unknown as Config;

      const scheduler = new CoreToolScheduler({
        config: mockConfig,
        onAllToolCallsComplete,
        onToolCallsUpdate,
        getPreferredEditor: () => 'vscode',
        onEditorClose: vi.fn(),
      });

      const abortController = new AbortController();
      const request = {
        callId: '1',
        name: 'write_file', // Excluded tool
        args: {},
        isClientInitiated: false,
        prompt_id: 'prompt-id-excluded',
      };

      await scheduler.schedule([request], abortController.signal);

      // Wait for completion
      await vi.waitFor(() => {
        expect(onAllToolCallsComplete).toHaveBeenCalled();
      });

      const completedCalls = onAllToolCallsComplete.mock
        .calls[0][0] as ToolCall[];
      expect(completedCalls).toHaveLength(1);
      const completedCall = completedCalls[0];
      expect(completedCall.status).toBe('error');
      if (completedCall.status !== 'error') {
        throw new Error(`Expected error status, got ${completedCall.status}`);
      }
      const errorMessage = completedCall.response.error?.message;
      expect(errorMessage).toBe(
        'copilot-shell requires permission to use write_file, but that permission was declined.',
      );
      // Should NOT contain "not found in registry"
      expect(errorMessage).not.toContain('not found in registry');
    });

    it('should return "not found" message for truly missing tools (not excluded)', async () => {
      const onAllToolCallsComplete = vi.fn();
      const onToolCallsUpdate = vi.fn();

      const mockToolRegistry = {
        getTool: () => undefined, // Tool not in registry
        getAllToolNames: () => ['list_files', 'read_file'],
        getFunctionDeclarations: () => [],
        tools: new Map(),
        discovery: {},
        registerTool: () => {},
        getToolByName: () => undefined,
        getToolByDisplayName: () => undefined,
        getTools: () => [],
        discoverTools: async () => {},
        getAllTools: () => [],
        getToolsByServer: () => [],
      } as unknown as ToolRegistry;

      const mockConfig = {
        getSessionId: () => 'test-session-id',
        getUsageStatisticsEnabled: () => true,
        getDebugMode: () => false,
        getApprovalMode: () => ApprovalMode.DEFAULT,
        getAllowedTools: () => [],
        getExcludeTools: () => ['write_file', 'edit'], // Different excluded tools
        getContentGeneratorConfig: () => ({
          model: 'test-model',
          authType: 'gemini',
        }),
        getShellExecutionConfig: () => ({
          terminalWidth: 90,
          terminalHeight: 30,
        }),
        storage: {
          getProjectTempDir: () => '/tmp',
        },
        getTruncateToolOutputThreshold: () =>
          DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
        getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
        getToolRegistry: () => mockToolRegistry,
        getUseSmartEdit: () => false,
        getUseModelRouter: () => false,
        getGeminiClient: () => null,
        getChatRecordingService: () => undefined,
        getEnableHooks: () => false,
        getHookSystem: () => undefined,
      } as unknown as Config;

      const scheduler = new CoreToolScheduler({
        config: mockConfig,
        onAllToolCallsComplete,
        onToolCallsUpdate,
        getPreferredEditor: () => 'vscode',
        onEditorClose: vi.fn(),
      });

      const abortController = new AbortController();
      const request = {
        callId: '1',
        name: 'nonexistent_tool', // Not excluded, just doesn't exist
        args: {},
        isClientInitiated: false,
        prompt_id: 'prompt-id-missing',
      };

      await scheduler.schedule([request], abortController.signal);

      // Wait for completion
      await vi.waitFor(() => {
        expect(onAllToolCallsComplete).toHaveBeenCalled();
      });

      const completedCalls = onAllToolCallsComplete.mock
        .calls[0][0] as ToolCall[];
      expect(completedCalls).toHaveLength(1);
      const completedCall = completedCalls[0];
      expect(completedCall.status).toBe('error');
      if (completedCall.status !== 'error') {
        throw new Error(`Expected error status, got ${completedCall.status}`);
      }
      const errorMessage = completedCall.response.error?.message;
      // Should contain "not found in registry"
      expect(errorMessage).toContain('not found in registry');
      // Should NOT contain permission message
      expect(errorMessage).not.toContain('requires permission');
    });
  });
});

describe('CoreToolScheduler with payload', () => {
  it('should update args and diff and execute tool when payload is provided', async () => {
    const mockTool = new MockModifiableTool();
    mockTool.executeFn = vi.fn();
    const declarativeTool = mockTool;
    const mockToolRegistry = {
      getTool: () => declarativeTool,
      getFunctionDeclarations: () => [],
      tools: new Map(),
      discovery: {},
      registerTool: () => {},
      getToolByName: () => declarativeTool,
      getToolByDisplayName: () => declarativeTool,
      getTools: () => [],
      discoverTools: async () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
    } as unknown as ToolRegistry;

    const onAllToolCallsComplete = vi.fn();
    const onToolCallsUpdate = vi.fn();

    const mockConfig = {
      getSessionId: () => 'test-session-id',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => ApprovalMode.DEFAULT,
      getAllowedTools: () => [],
      getContentGeneratorConfig: () => ({
        model: 'test-model',
        authType: 'gemini',
      }),
      getShellExecutionConfig: () => ({
        terminalWidth: 90,
        terminalHeight: 30,
      }),
      storage: {
        getProjectTempDir: () => '/tmp',
      },
      getTruncateToolOutputThreshold: () =>
        DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
      getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
      getToolRegistry: () => mockToolRegistry,
      getUseSmartEdit: () => false,
      getUseModelRouter: () => false,
      getGeminiClient: () => null, // No client needed for these tests
      isInteractive: () => true, // Required to prevent auto-denial of tool calls
      getIdeMode: () => false,
      getExperimentalZedIntegration: () => false,
      getChatRecordingService: () => undefined,
      getEnableHooks: () => false,
      getHookSystem: () => undefined,
    } as unknown as Config;

    const scheduler = new CoreToolScheduler({
      config: mockConfig,
      onAllToolCallsComplete,
      onToolCallsUpdate,
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
    });

    const abortController = new AbortController();
    const request = {
      callId: '1',
      name: 'mockModifiableTool',
      args: {},
      isClientInitiated: false,
      prompt_id: 'prompt-id-2',
    };

    await scheduler.schedule([request], abortController.signal);

    const awaitingCall = (await waitForStatus(
      onToolCallsUpdate,
      'awaiting_approval',
    )) as WaitingToolCall;
    const confirmationDetails = awaitingCall.confirmationDetails;

    if (confirmationDetails) {
      const payload: ToolConfirmationPayload = { newContent: 'final version' };
      await confirmationDetails.onConfirm(
        ToolConfirmationOutcome.ProceedOnce,
        payload,
      );
    }

    // Wait for the tool execution to complete
    await vi.waitFor(() => {
      expect(onAllToolCallsComplete).toHaveBeenCalled();
    });

    const completedCalls = onAllToolCallsComplete.mock
      .calls[0][0] as ToolCall[];
    expect(completedCalls[0].status).toBe('success');
    expect(mockTool.executeFn).toHaveBeenCalledWith({
      newContent: 'final version',
    });
  });
});

describe('convertToFunctionResponse', () => {
  const toolName = 'testTool';
  const callId = 'call1';

  it('should handle simple string llmContent', () => {
    const llmContent = 'Simple text output';
    const result = convertToFunctionResponse(toolName, callId, llmContent);
    expect(result).toEqual([
      {
        functionResponse: {
          name: toolName,
          id: callId,
          response: { output: 'Simple text output' },
        },
      },
    ]);
  });

  it('should handle llmContent as a single Part with text', () => {
    const llmContent: Part = { text: 'Text from Part object' };
    const result = convertToFunctionResponse(toolName, callId, llmContent);
    expect(result).toEqual([
      {
        functionResponse: {
          name: toolName,
          id: callId,
          response: { output: 'Text from Part object' },
        },
      },
    ]);
  });

  it('should handle llmContent as a PartListUnion array with a single text Part', () => {
    const llmContent: PartListUnion = [{ text: 'Text from array' }];
    const result = convertToFunctionResponse(toolName, callId, llmContent);
    expect(result).toEqual([
      {
        functionResponse: {
          name: toolName,
          id: callId,
          response: { output: 'Text from array' },
        },
      },
    ]);
  });

  it('should handle llmContent with inlineData', () => {
    const llmContent: Part = {
      inlineData: { mimeType: 'image/png', data: 'base64...' },
    };
    const result = convertToFunctionResponse(toolName, callId, llmContent);
    expect(result).toEqual([
      {
        functionResponse: {
          name: toolName,
          id: callId,
          response: {
            output: '',
          },
          parts: [{ inlineData: { mimeType: 'image/png', data: 'base64...' } }],
        },
      },
    ]);
  });

  it('should handle llmContent with fileData', () => {
    const llmContent: Part = {
      fileData: { mimeType: 'application/pdf', fileUri: 'gs://...' },
    };
    const result = convertToFunctionResponse(toolName, callId, llmContent);
    expect(result).toEqual([
      {
        functionResponse: {
          name: toolName,
          id: callId,
          response: {
            output: '',
          },
          parts: [
            {
              fileData: { mimeType: 'application/pdf', fileUri: 'gs://...' },
            },
          ],
        },
      },
    ]);
  });

  it('should handle llmContent as an array of multiple Parts (text and inlineData)', () => {
    const llmContent: PartListUnion = [
      { text: 'Some textual description' },
      { inlineData: { mimeType: 'image/jpeg', data: 'base64data...' } },
      { text: 'Another text part' },
    ];
    const result = convertToFunctionResponse(toolName, callId, llmContent);
    expect(result).toEqual([
      {
        functionResponse: {
          name: toolName,
          id: callId,
          response: { output: 'Tool execution succeeded.' },
        },
      },
      ...llmContent,
    ]);
  });

  it('should handle llmContent as an array with a single inlineData Part', () => {
    const llmContent: PartListUnion = [
      { inlineData: { mimeType: 'image/gif', data: 'gifdata...' } },
    ];
    const result = convertToFunctionResponse(toolName, callId, llmContent);
    expect(result).toEqual([
      {
        functionResponse: {
          name: toolName,
          id: callId,
          response: {
            output: '',
          },
          parts: [
            { inlineData: { mimeType: 'image/gif', data: 'gifdata...' } },
          ],
        },
      },
    ]);
  });

  it('should handle llmContent as a generic Part (not text, inlineData, or fileData)', () => {
    const llmContent: Part = { functionCall: { name: 'test', args: {} } };
    const result = convertToFunctionResponse(toolName, callId, llmContent);
    expect(result).toEqual([
      {
        functionResponse: {
          name: toolName,
          id: callId,
          response: { output: 'Tool execution succeeded.' },
        },
      },
    ]);
  });

  it('should handle empty string llmContent', () => {
    const llmContent = '';
    const result = convertToFunctionResponse(toolName, callId, llmContent);
    expect(result).toEqual([
      {
        functionResponse: {
          name: toolName,
          id: callId,
          response: { output: '' },
        },
      },
    ]);
  });

  it('should handle llmContent as an empty array', () => {
    const llmContent: PartListUnion = [];
    const result = convertToFunctionResponse(toolName, callId, llmContent);
    expect(result).toEqual([
      {
        functionResponse: {
          name: toolName,
          id: callId,
          response: { output: 'Tool execution succeeded.' },
        },
      },
    ]);
  });

  it('should handle llmContent as a Part with undefined inlineData/fileData/text', () => {
    const llmContent: Part = {}; // An empty part object
    const result = convertToFunctionResponse(toolName, callId, llmContent);
    expect(result).toEqual([
      {
        functionResponse: {
          name: toolName,
          id: callId,
          response: { output: 'Tool execution succeeded.' },
        },
      },
    ]);
  });
});

class MockEditToolInvocation extends BaseToolInvocation<
  Record<string, unknown>,
  ToolResult
> {
  constructor(params: Record<string, unknown>) {
    super(params);
  }

  getDescription(): string {
    return 'A mock edit tool invocation';
  }

  override async shouldConfirmExecute(
    _abortSignal: AbortSignal,
  ): Promise<ToolCallConfirmationDetails | false> {
    return {
      type: 'edit',
      title: 'Confirm Edit',
      fileName: 'test.txt',
      filePath: 'test.txt',
      fileDiff:
        '--- test.txt\n+++ test.txt\n@@ -1,1 +1,1 @@\n-old content\n+new content',
      originalContent: 'old content',
      newContent: 'new content',
      onConfirm: async () => {},
    };
  }

  async execute(_abortSignal: AbortSignal): Promise<ToolResult> {
    return {
      llmContent: 'Edited successfully',
      returnDisplay: 'Edited successfully',
    };
  }
}

class MockEditTool extends BaseDeclarativeTool<
  Record<string, unknown>,
  ToolResult
> {
  constructor() {
    super('mockEditTool', 'mockEditTool', 'A mock edit tool', Kind.Edit, {});
  }

  protected createInvocation(
    params: Record<string, unknown>,
  ): ToolInvocation<Record<string, unknown>, ToolResult> {
    return new MockEditToolInvocation(params);
  }
}

describe('CoreToolScheduler edit cancellation', () => {
  it('should preserve diff when an edit is cancelled', async () => {
    const mockEditTool = new MockEditTool();
    const mockToolRegistry = {
      getTool: () => mockEditTool,
      getFunctionDeclarations: () => [],
      tools: new Map(),
      discovery: {},
      registerTool: () => {},
      getToolByName: () => mockEditTool,
      getToolByDisplayName: () => mockEditTool,
      getTools: () => [],
      discoverTools: async () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
    } as unknown as ToolRegistry;

    const onAllToolCallsComplete = vi.fn();
    const onToolCallsUpdate = vi.fn();

    const mockConfig = {
      getSessionId: () => 'test-session-id',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => ApprovalMode.DEFAULT,
      getAllowedTools: () => [],
      getContentGeneratorConfig: () => ({
        model: 'test-model',
        authType: 'gemini',
      }),
      getShellExecutionConfig: () => ({
        terminalWidth: 90,
        terminalHeight: 30,
      }),
      storage: {
        getProjectTempDir: () => '/tmp',
      },
      getToolRegistry: () => mockToolRegistry,
      getUseSmartEdit: () => false,
      getUseModelRouter: () => false,
      getGeminiClient: () => null, // No client needed for these tests
      isInteractive: () => true, // Required to prevent auto-denial of tool calls
      getIdeMode: () => false,
      getExperimentalZedIntegration: () => false,
      getChatRecordingService: () => undefined,
      getEnableHooks: () => false,
      getHookSystem: () => undefined,
    } as unknown as Config;

    const scheduler = new CoreToolScheduler({
      config: mockConfig,
      onAllToolCallsComplete,
      onToolCallsUpdate,
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
    });

    const abortController = new AbortController();
    const request = {
      callId: '1',
      name: 'mockEditTool',
      args: {},
      isClientInitiated: false,
      prompt_id: 'prompt-id-1',
    };

    await scheduler.schedule([request], abortController.signal);

    const awaitingCall = (await waitForStatus(
      onToolCallsUpdate,
      'awaiting_approval',
    )) as WaitingToolCall;

    // Cancel the edit
    const confirmationDetails = awaitingCall.confirmationDetails;
    if (confirmationDetails) {
      await confirmationDetails.onConfirm(ToolConfirmationOutcome.Cancel);
    }

    expect(onAllToolCallsComplete).toHaveBeenCalled();
    const completedCalls = onAllToolCallsComplete.mock
      .calls[0][0] as ToolCall[];

    expect(completedCalls[0].status).toBe('cancelled');

    // Check that the diff is preserved
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const cancelledCall = completedCalls[0] as any;
    expect(cancelledCall.response.resultDisplay).toBeDefined();
    expect(cancelledCall.response.resultDisplay.fileDiff).toBe(
      '--- test.txt\n+++ test.txt\n@@ -1,1 +1,1 @@\n-old content\n+new content',
    );
    expect(cancelledCall.response.resultDisplay.fileName).toBe('test.txt');
  });
});

describe('CoreToolScheduler YOLO mode', () => {
  it('should execute tool requiring confirmation directly without waiting', async () => {
    // Arrange
    const executeFn = vi.fn().mockResolvedValue({
      llmContent: 'Tool executed',
      returnDisplay: 'Tool executed',
    });
    const mockTool = new MockTool({
      name: 'mockTool',
      execute: executeFn,
      shouldConfirmExecute: MOCK_TOOL_SHOULD_CONFIRM_EXECUTE,
    });
    const declarativeTool = mockTool;

    const mockToolRegistry = {
      getTool: () => declarativeTool,
      getToolByName: () => declarativeTool,
      // Other properties are not needed for this test but are included for type consistency.
      getFunctionDeclarations: () => [],
      tools: new Map(),
      discovery: {},
      registerTool: () => {},
      getToolByDisplayName: () => declarativeTool,
      getTools: () => [],
      discoverTools: async () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
    } as unknown as ToolRegistry;

    const onAllToolCallsComplete = vi.fn();
    const onToolCallsUpdate = vi.fn();

    // Configure the scheduler for YOLO mode.
    const mockConfig = {
      getSessionId: () => 'test-session-id',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => ApprovalMode.YOLO,
      getAllowedTools: () => [],
      getContentGeneratorConfig: () => ({
        model: 'test-model',
        authType: 'gemini',
      }),
      getShellExecutionConfig: () => ({
        terminalWidth: 90,
        terminalHeight: 30,
      }),
      storage: {
        getProjectTempDir: () => '/tmp',
      },
      getToolRegistry: () => mockToolRegistry,
      getTruncateToolOutputThreshold: () =>
        DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
      getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
      getUseSmartEdit: () => false,
      getUseModelRouter: () => false,
      getGeminiClient: () => null, // No client needed for these tests
      getChatRecordingService: () => undefined,
      getEnableHooks: () => false,
      getHookSystem: () => undefined,
    } as unknown as Config;

    const scheduler = new CoreToolScheduler({
      config: mockConfig,
      onAllToolCallsComplete,
      onToolCallsUpdate,
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
    });

    const abortController = new AbortController();
    const request = {
      callId: '1',
      name: 'mockTool',
      args: { param: 'value' },
      isClientInitiated: false,
      prompt_id: 'prompt-id-yolo',
    };

    // Act
    await scheduler.schedule([request], abortController.signal);

    // Wait for the tool execution to complete
    await vi.waitFor(() => {
      expect(onAllToolCallsComplete).toHaveBeenCalled();
    });

    // Assert
    // 1. The tool's execute method was called directly.
    expect(executeFn).toHaveBeenCalledWith({ param: 'value' });

    // 2. The tool call status never entered 'awaiting_approval'.
    const statusUpdates = onToolCallsUpdate.mock.calls
      .map((call) => (call[0][0] as ToolCall)?.status)
      .filter(Boolean);
    expect(statusUpdates).not.toContain('awaiting_approval');
    expect(statusUpdates).toEqual([
      'validating',
      'scheduled',
      'executing',
      'success',
    ]);

    // 3. The final callback indicates the tool call was successful.
    const completedCalls = onAllToolCallsComplete.mock
      .calls[0][0] as ToolCall[];
    expect(completedCalls).toHaveLength(1);
    const completedCall = completedCalls[0];
    expect(completedCall.status).toBe('success');
    if (completedCall.status !== 'success') {
      throw new Error(`Expected success status, got ${completedCall.status}`);
    }
    expect(completedCall.response.resultDisplay).toBe('Tool executed');
  });
});

describe('CoreToolScheduler cancellation during executing with live output', () => {
  it('sets status to cancelled and preserves last output', async () => {
    class StreamingInvocation extends BaseToolInvocation<
      { id: string },
      ToolResult
    > {
      getDescription(): string {
        return `Streaming tool ${this.params.id}`;
      }

      async execute(
        signal: AbortSignal,
        updateOutput?: (output: ToolResultDisplay) => void,
      ): Promise<ToolResult> {
        updateOutput?.('hello');
        // Wait until aborted to emulate a long-running task
        await new Promise<void>((resolve) => {
          if (signal.aborted) return resolve();
          const onAbort = () => {
            signal.removeEventListener('abort', onAbort);
            resolve();
          };
          signal.addEventListener('abort', onAbort, { once: true });
        });
        // Return a normal (non-error) result; scheduler should still mark cancelled
        return { llmContent: 'done', returnDisplay: 'done' };
      }
    }

    class StreamingTool extends BaseDeclarativeTool<
      { id: string },
      ToolResult
    > {
      constructor() {
        super(
          'stream-tool',
          'Stream Tool',
          'Emits live output and waits for abort',
          Kind.Other,
          {
            type: 'object',
            properties: { id: { type: 'string' } },
            required: ['id'],
          },
          true,
          true,
        );
      }
      protected createInvocation(params: { id: string }) {
        return new StreamingInvocation(params);
      }
    }

    const tool = new StreamingTool();
    const mockToolRegistry = {
      getTool: () => tool,
      getFunctionDeclarations: () => [],
      tools: new Map(),
      discovery: {},
      registerTool: () => {},
      getToolByName: () => tool,
      getToolByDisplayName: () => tool,
      getTools: () => [],
      discoverTools: async () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
    } as unknown as ToolRegistry;

    const onAllToolCallsComplete = vi.fn();
    const onToolCallsUpdate = vi.fn();

    const mockConfig = {
      getSessionId: () => 'test-session-id',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => ApprovalMode.DEFAULT,
      getContentGeneratorConfig: () => ({
        model: 'test-model',
        authType: 'gemini',
      }),
      getToolRegistry: () => mockToolRegistry,
      getShellExecutionConfig: () => ({
        terminalWidth: 90,
        terminalHeight: 30,
      }),
      getChatRecordingService: () => undefined,
      getEnableHooks: () => false,
      getHookSystem: () => undefined,
    } as unknown as Config;

    const scheduler = new CoreToolScheduler({
      config: mockConfig,
      onAllToolCallsComplete,
      onToolCallsUpdate,
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
    });

    const abortController = new AbortController();
    const request = {
      callId: '1',
      name: 'stream-tool',
      args: { id: 'x' },
      isClientInitiated: true,
      prompt_id: 'prompt-stream',
    };

    const schedulePromise = scheduler.schedule(
      [request],
      abortController.signal,
    );

    // Wait until executing
    await vi.waitFor(() => {
      const calls = onToolCallsUpdate.mock.calls;
      const last = calls[calls.length - 1]?.[0][0] as ToolCall | undefined;
      expect(last?.status).toBe('executing');
    });

    // Now abort
    abortController.abort();

    await schedulePromise;

    await vi.waitFor(() => {
      expect(onAllToolCallsComplete).toHaveBeenCalled();
    });
    const completedCalls = onAllToolCallsComplete.mock
      .calls[0][0] as ToolCall[];
    expect(completedCalls[0].status).toBe('cancelled');
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const cancelled: any = completedCalls[0];
    expect(cancelled.response.resultDisplay).toBe('hello');
  });
});

describe('CoreToolScheduler request queueing', () => {
  it('should queue a request if another is running', async () => {
    let resolveFirstCall: (result: ToolResult) => void;
    const firstCallPromise = new Promise<ToolResult>((resolve) => {
      resolveFirstCall = resolve;
    });

    const executeFn = vi.fn().mockImplementation(() => firstCallPromise);
    const mockTool = new MockTool({ name: 'mockTool', execute: executeFn });
    const declarativeTool = mockTool;

    const mockToolRegistry = {
      getTool: () => declarativeTool,
      getToolByName: () => declarativeTool,
      getFunctionDeclarations: () => [],
      tools: new Map(),
      discovery: {},
      registerTool: () => {},
      getToolByDisplayName: () => declarativeTool,
      getTools: () => [],
      discoverTools: async () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
    } as unknown as ToolRegistry;

    const onAllToolCallsComplete = vi.fn();
    const onToolCallsUpdate = vi.fn();

    const mockConfig = {
      getSessionId: () => 'test-session-id',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => ApprovalMode.YOLO, // Use YOLO to avoid confirmation prompts
      getAllowedTools: () => [],
      getContentGeneratorConfig: () => ({
        model: 'test-model',
        authType: 'gemini',
      }),
      getShellExecutionConfig: () => ({
        terminalWidth: 90,
        terminalHeight: 30,
      }),
      storage: {
        getProjectTempDir: () => '/tmp',
      },
      getTruncateToolOutputThreshold: () =>
        DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
      getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
      getToolRegistry: () => mockToolRegistry,
      getUseSmartEdit: () => false,
      getUseModelRouter: () => false,
      getGeminiClient: () => null, // No client needed for these tests
      getChatRecordingService: () => undefined,
      getEnableHooks: () => false,
      getHookSystem: () => undefined,
    } as unknown as Config;

    const scheduler = new CoreToolScheduler({
      config: mockConfig,
      onAllToolCallsComplete,
      onToolCallsUpdate,
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
    });

    const abortController = new AbortController();
    const request1 = {
      callId: '1',
      name: 'mockTool',
      args: { a: 1 },
      isClientInitiated: false,
      prompt_id: 'prompt-1',
    };
    const request2 = {
      callId: '2',
      name: 'mockTool',
      args: { b: 2 },
      isClientInitiated: false,
      prompt_id: 'prompt-2',
    };

    // Schedule the first call, which will pause execution.
    scheduler.schedule([request1], abortController.signal);

    // Wait for the first call to be in the 'executing' state.
    await waitForStatus(onToolCallsUpdate, 'executing');

    // Schedule the second call while the first is "running".
    const schedulePromise2 = scheduler.schedule(
      [request2],
      abortController.signal,
    );

    // Ensure the second tool call hasn't been executed yet.
    expect(executeFn).toHaveBeenCalledWith({ a: 1 });

    // Complete the first tool call.
    resolveFirstCall!({
      llmContent: 'First call complete',
      returnDisplay: 'First call complete',
    });

    // Wait for the second schedule promise to resolve.
    await schedulePromise2;

    // Let the second call finish.
    const secondCallResult = {
      llmContent: 'Second call complete',
      returnDisplay: 'Second call complete',
    };
    // Since the mock is shared, we need to resolve the current promise.
    // In a real scenario, a new promise would be created for the second call.
    resolveFirstCall!(secondCallResult);

    await vi.waitFor(() => {
      // Now the second tool call should have been executed.
      expect(executeFn).toHaveBeenCalledTimes(2);
    });
    expect(executeFn).toHaveBeenCalledWith({ b: 2 });

    // Wait for the second completion.
    await vi.waitFor(() => {
      expect(onAllToolCallsComplete).toHaveBeenCalledTimes(2);
    });

    // Verify the completion callbacks were called correctly.
    expect(onAllToolCallsComplete.mock.calls[0][0][0].status).toBe('success');
    expect(onAllToolCallsComplete.mock.calls[1][0][0].status).toBe('success');
  });

  it('should auto-approve a tool call if it is on the allowedTools list', async () => {
    // Arrange
    const executeFn = vi.fn().mockResolvedValue({
      llmContent: 'Tool executed',
      returnDisplay: 'Tool executed',
    });
    const mockTool = new MockTool({
      name: 'mockTool',
      execute: executeFn,
      shouldConfirmExecute: MOCK_TOOL_SHOULD_CONFIRM_EXECUTE,
    });
    const declarativeTool = mockTool;

    const toolRegistry = {
      getTool: () => declarativeTool,
      getToolByName: () => declarativeTool,
      getFunctionDeclarations: () => [],
      tools: new Map(),
      discovery: {},
      registerTool: () => {},
      getToolByDisplayName: () => declarativeTool,
      getTools: () => [],
      discoverTools: async () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
    } as unknown as ToolRegistry;

    const onAllToolCallsComplete = vi.fn();
    const onToolCallsUpdate = vi.fn();

    // Configure the scheduler to auto-approve the specific tool call.
    const mockConfig = {
      getSessionId: () => 'test-session-id',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => ApprovalMode.DEFAULT, // Not YOLO mode
      getAllowedTools: () => ['mockTool'], // Auto-approve this tool
      getToolRegistry: () => toolRegistry,
      getContentGeneratorConfig: () => ({
        model: 'test-model',
        authType: 'gemini',
      }),
      getShellExecutionConfig: () => ({
        terminalWidth: 80,
        terminalHeight: 24,
      }),
      getTerminalWidth: vi.fn(() => 80),
      getTerminalHeight: vi.fn(() => 24),
      storage: {
        getProjectTempDir: () => '/tmp',
      },
      getTruncateToolOutputThreshold: () =>
        DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
      getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
      getUseSmartEdit: () => false,
      getUseModelRouter: () => false,
      getGeminiClient: () => null, // No client needed for these tests
      getChatRecordingService: () => undefined,
      getEnableHooks: () => false,
      getHookSystem: () => undefined,
    } as unknown as Config;

    const scheduler = new CoreToolScheduler({
      config: mockConfig,
      onAllToolCallsComplete,
      onToolCallsUpdate,
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
    });

    const abortController = new AbortController();
    const request = {
      callId: '1',
      name: 'mockTool',
      args: { param: 'value' },
      isClientInitiated: false,
      prompt_id: 'prompt-auto-approved',
    };

    // Act
    await scheduler.schedule([request], abortController.signal);

    // Wait for the tool execution to complete
    await vi.waitFor(() => {
      expect(onAllToolCallsComplete).toHaveBeenCalled();
    });

    // Assert
    // 1. The tool's execute method was called directly.
    expect(executeFn).toHaveBeenCalledWith({ param: 'value' });

    // 2. The tool call status never entered 'awaiting_approval'.
    const statusUpdates = onToolCallsUpdate.mock.calls
      .map((call) => (call[0][0] as ToolCall)?.status)
      .filter(Boolean);
    expect(statusUpdates).not.toContain('awaiting_approval');
    expect(statusUpdates).toEqual([
      'validating',
      'scheduled',
      'executing',
      'success',
    ]);

    // 3. The final callback indicates the tool call was successful.
    expect(onAllToolCallsComplete).toHaveBeenCalled();
    const completedCalls = onAllToolCallsComplete.mock
      .calls[0][0] as ToolCall[];
    expect(completedCalls).toHaveLength(1);
    const completedCall = completedCalls[0];
    expect(completedCall.status).toBe('success');
    if (completedCall.status !== 'success') {
      throw new Error(`Expected success status, got ${completedCall.status}`);
    }
    expect(completedCall.response.resultDisplay).toBe('Tool executed');
  });

  it('should handle two synchronous calls to schedule', async () => {
    const executeFn = vi.fn().mockResolvedValue({
      llmContent: 'Tool executed',
      returnDisplay: 'Tool executed',
    });
    const mockTool = new MockTool({ name: 'mockTool', execute: executeFn });
    const declarativeTool = mockTool;
    const mockToolRegistry = {
      getTool: () => declarativeTool,
      getToolByName: () => declarativeTool,
      getFunctionDeclarations: () => [],
      tools: new Map(),
      discovery: {},
      registerTool: () => {},
      getToolByDisplayName: () => declarativeTool,
      getTools: () => [],
      discoverTools: async () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
    } as unknown as ToolRegistry;
    const onAllToolCallsComplete = vi.fn();
    const onToolCallsUpdate = vi.fn();

    const mockConfig = {
      getSessionId: () => 'test-session-id',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => ApprovalMode.YOLO,
      getAllowedTools: () => [],
      getContentGeneratorConfig: () => ({
        model: 'test-model',
        authType: 'gemini',
      }),
      getShellExecutionConfig: () => ({
        terminalWidth: 90,
        terminalHeight: 30,
      }),
      storage: {
        getProjectTempDir: () => '/tmp',
      },
      getTruncateToolOutputThreshold: () =>
        DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
      getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
      getToolRegistry: () => mockToolRegistry,
      getUseSmartEdit: () => false,
      getUseModelRouter: () => false,
      getGeminiClient: () => null, // No client needed for these tests
      getChatRecordingService: () => undefined,
      getEnableHooks: () => false,
      getHookSystem: () => undefined,
    } as unknown as Config;

    const scheduler = new CoreToolScheduler({
      config: mockConfig,
      onAllToolCallsComplete,
      onToolCallsUpdate,
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
    });

    const abortController = new AbortController();
    const request1 = {
      callId: '1',
      name: 'mockTool',
      args: { a: 1 },
      isClientInitiated: false,
      prompt_id: 'prompt-1',
    };
    const request2 = {
      callId: '2',
      name: 'mockTool',
      args: { b: 2 },
      isClientInitiated: false,
      prompt_id: 'prompt-2',
    };

    // Schedule two calls synchronously.
    const schedulePromise1 = scheduler.schedule(
      [request1],
      abortController.signal,
    );
    const schedulePromise2 = scheduler.schedule(
      [request2],
      abortController.signal,
    );

    // Wait for both promises to resolve.
    await Promise.all([schedulePromise1, schedulePromise2]);

    // Ensure the tool was called twice with the correct arguments.
    expect(executeFn).toHaveBeenCalledTimes(2);
    expect(executeFn).toHaveBeenCalledWith({ a: 1 });
    expect(executeFn).toHaveBeenCalledWith({ b: 2 });

    // Ensure completion callbacks were called twice.
    expect(onAllToolCallsComplete).toHaveBeenCalledTimes(2);
  });

  it('should auto-approve remaining tool calls when first tool call is approved with ProceedAlways', async () => {
    let approvalMode = ApprovalMode.DEFAULT;
    const mockConfig = {
      getSessionId: () => 'test-session-id',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => approvalMode,
      getAllowedTools: () => [],
      setApprovalMode: (mode: ApprovalMode) => {
        approvalMode = mode;
      },
      getShellExecutionConfig: () => ({
        terminalWidth: 90,
        terminalHeight: 30,
      }),
      storage: {
        getProjectTempDir: () => '/tmp',
      },
      getTruncateToolOutputThreshold: () =>
        DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
      getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
      getUseSmartEdit: () => false,
      getUseModelRouter: () => false,
      getGeminiClient: () => null, // No client needed for these tests
      isInteractive: () => true, // Required to prevent auto-denial of tool calls
      getIdeMode: () => false,
      getExperimentalZedIntegration: () => false,
      getChatRecordingService: () => undefined,
      getEnableHooks: () => false,
      getHookSystem: () => undefined,
    } as unknown as Config;

    const testTool = new TestApprovalTool(mockConfig);
    const toolRegistry = {
      getTool: () => testTool,
      getFunctionDeclarations: () => [],
      getFunctionDeclarationsFiltered: () => [],
      registerTool: () => {},
      discoverAllTools: async () => {},
      discoverMcpTools: async () => {},
      discoverToolsForServer: async () => {},
      removeMcpToolsByServer: () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
      tools: new Map(),
      config: mockConfig,
      mcpClientManager: undefined,
      getToolByName: () => testTool,
      getToolByDisplayName: () => testTool,
      getTools: () => [],
      discoverTools: async () => {},
      discovery: {},
    } as unknown as ToolRegistry;

    mockConfig.getToolRegistry = () => toolRegistry;

    const onAllToolCallsComplete = vi.fn();
    const onToolCallsUpdate = vi.fn();
    const pendingConfirmations: Array<
      (
        outcome: ToolConfirmationOutcome,
        payload?: ToolConfirmationPayload,
      ) => Promise<void>
    > = [];

    const scheduler = new CoreToolScheduler({
      config: mockConfig,
      onAllToolCallsComplete,
      onToolCallsUpdate: (toolCalls) => {
        onToolCallsUpdate(toolCalls);
        // Capture confirmation handlers for awaiting_approval tools
        toolCalls.forEach((call) => {
          if (call.status === 'awaiting_approval') {
            const waitingCall = call as WaitingToolCall;
            if (waitingCall.confirmationDetails?.onConfirm) {
              const originalHandler = pendingConfirmations.find(
                (h) => h === waitingCall.confirmationDetails.onConfirm,
              );
              if (!originalHandler) {
                pendingConfirmations.push(
                  waitingCall.confirmationDetails.onConfirm,
                );
              }
            }
          }
        });
      },
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
    });

    const abortController = new AbortController();

    // Schedule multiple tools that need confirmation
    const requests = [
      {
        callId: '1',
        name: 'testApprovalTool',
        args: { id: 'first' },
        isClientInitiated: false,
        prompt_id: 'prompt-1',
      },
      {
        callId: '2',
        name: 'testApprovalTool',
        args: { id: 'second' },
        isClientInitiated: false,
        prompt_id: 'prompt-2',
      },
      {
        callId: '3',
        name: 'testApprovalTool',
        args: { id: 'third' },
        isClientInitiated: false,
        prompt_id: 'prompt-3',
      },
    ];

    await scheduler.schedule(requests, abortController.signal);

    // Wait for all tools to be awaiting approval
    await vi.waitFor(() => {
      const calls = onToolCallsUpdate.mock.calls.at(-1)?.[0] as ToolCall[];
      expect(calls?.length).toBe(3);
      expect(calls?.every((call) => call.status === 'awaiting_approval')).toBe(
        true,
      );
    });

    expect(pendingConfirmations.length).toBe(3);

    // Approve the first tool with ProceedAlways
    const firstConfirmation = pendingConfirmations[0];
    await firstConfirmation(ToolConfirmationOutcome.ProceedAlways);

    // Wait for all tools to be completed
    await vi.waitFor(() => {
      expect(onAllToolCallsComplete).toHaveBeenCalled();
      const completedCalls = onAllToolCallsComplete.mock.calls.at(
        -1,
      )?.[0] as ToolCall[];
      expect(completedCalls?.length).toBe(3);
      expect(completedCalls?.every((call) => call.status === 'success')).toBe(
        true,
      );
    });

    // Verify approval mode was changed
    expect(approvalMode).toBe(ApprovalMode.AUTO_EDIT);
  });
});

describe('CoreToolScheduler Sequential Execution', () => {
  it('should execute tool calls in a batch sequentially', async () => {
    // Arrange
    let firstCallFinished = false;
    const executeFn = vi
      .fn()
      .mockImplementation(async (args: { call: number }) => {
        if (args.call === 1) {
          // First call, wait for a bit to simulate work
          await new Promise((resolve) => setTimeout(resolve, 50));
          firstCallFinished = true;
          return { llmContent: 'First call done' };
        }
        if (args.call === 2) {
          // Second call, should only happen after the first is finished
          if (!firstCallFinished) {
            throw new Error(
              'Second tool call started before the first one finished!',
            );
          }
          return { llmContent: 'Second call done' };
        }
        return { llmContent: 'default' };
      });

    const mockTool = new MockTool({ name: 'mockTool', execute: executeFn });
    const declarativeTool = mockTool;

    const mockToolRegistry = {
      getTool: () => declarativeTool,
      getToolByName: () => declarativeTool,
      getFunctionDeclarations: () => [],
      tools: new Map(),
      discovery: {},
      registerTool: () => {},
      getToolByDisplayName: () => declarativeTool,
      getTools: () => [],
      discoverTools: async () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
    } as unknown as ToolRegistry;

    const onAllToolCallsComplete = vi.fn();
    const onToolCallsUpdate = vi.fn();

    const mockConfig = {
      getSessionId: () => 'test-session-id',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => ApprovalMode.YOLO, // Use YOLO to avoid confirmation prompts
      getAllowedTools: () => [],
      getContentGeneratorConfig: () => ({
        model: 'test-model',
        authType: 'gemini',
      }),
      getShellExecutionConfig: () => ({
        terminalWidth: 90,
        terminalHeight: 30,
      }),
      storage: {
        getProjectTempDir: () => '/tmp',
      },
      getToolRegistry: () => mockToolRegistry,
      getTruncateToolOutputThreshold: () =>
        DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
      getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
      getUseSmartEdit: () => false,
      getUseModelRouter: () => false,
      getGeminiClient: () => null,
      getChatRecordingService: () => undefined,
      getEnableHooks: () => false,
      getHookSystem: () => undefined,
    } as unknown as Config;

    const scheduler = new CoreToolScheduler({
      config: mockConfig,
      onAllToolCallsComplete,
      onToolCallsUpdate,
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
    });

    const abortController = new AbortController();
    const requests = [
      {
        callId: '1',
        name: 'mockTool',
        args: { call: 1 },
        isClientInitiated: false,
        prompt_id: 'prompt-1',
      },
      {
        callId: '2',
        name: 'mockTool',
        args: { call: 2 },
        isClientInitiated: false,
        prompt_id: 'prompt-1',
      },
    ];

    // Act
    await scheduler.schedule(requests, abortController.signal);

    // Assert
    await vi.waitFor(() => {
      expect(onAllToolCallsComplete).toHaveBeenCalled();
    });

    // Check that execute was called twice
    expect(executeFn).toHaveBeenCalledTimes(2);

    // Check the order of calls
    const calls = executeFn.mock.calls;
    expect(calls[0][0]).toEqual({ call: 1 });
    expect(calls[1][0]).toEqual({ call: 2 });

    // The onAllToolCallsComplete should be called once with both results
    const completedCalls = onAllToolCallsComplete.mock
      .calls[0][0] as ToolCall[];
    expect(completedCalls).toHaveLength(2);
    expect(completedCalls[0].status).toBe('success');
    expect(completedCalls[1].status).toBe('success');
  });

  it('should cancel subsequent tools when the signal is aborted.', async () => {
    // Arrange
    const abortController = new AbortController();
    let secondCallStarted = false;

    const executeFn = vi
      .fn()
      .mockImplementation(async (args: { call: number }) => {
        if (args.call === 1) {
          return { llmContent: 'First call done' };
        }
        if (args.call === 2) {
          secondCallStarted = true;
          // This call will be cancelled while it's "running".
          await new Promise((resolve) => setTimeout(resolve, 100));
          // It should not return a value because it will be cancelled.
          return { llmContent: 'Second call should not complete' };
        }
        if (args.call === 3) {
          return { llmContent: 'Third call done' };
        }
        return { llmContent: 'default' };
      });

    const mockTool = new MockTool({ name: 'mockTool', execute: executeFn });
    const declarativeTool = mockTool;

    const mockToolRegistry = {
      getTool: () => declarativeTool,
      getToolByName: () => declarativeTool,
      getFunctionDeclarations: () => [],
      tools: new Map(),
      discovery: {},
      registerTool: () => {},
      getToolByDisplayName: () => declarativeTool,
      getTools: () => [],
      discoverTools: async () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
    } as unknown as ToolRegistry;

    const onAllToolCallsComplete = vi.fn();
    const onToolCallsUpdate = vi.fn();

    const mockConfig = {
      getSessionId: () => 'test-session-id',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => ApprovalMode.YOLO,
      getAllowedTools: () => [],
      getContentGeneratorConfig: () => ({
        model: 'test-model',
        authType: 'gemini',
      }),
      getShellExecutionConfig: () => ({
        terminalWidth: 90,
        terminalHeight: 30,
      }),
      storage: {
        getProjectTempDir: () => '/tmp',
      },
      getToolRegistry: () => mockToolRegistry,
      getTruncateToolOutputThreshold: () =>
        DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
      getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
      getUseSmartEdit: () => false,
      getUseModelRouter: () => false,
      getGeminiClient: () => null,
      getChatRecordingService: () => undefined,
      getEnableHooks: () => false,
      getHookSystem: () => undefined,
    } as unknown as Config;

    const scheduler = new CoreToolScheduler({
      config: mockConfig,
      onAllToolCallsComplete,
      onToolCallsUpdate,
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
    });

    const requests = [
      {
        callId: '1',
        name: 'mockTool',
        args: { call: 1 },
        isClientInitiated: false,
        prompt_id: 'prompt-1',
      },
      {
        callId: '2',
        name: 'mockTool',
        args: { call: 2 },
        isClientInitiated: false,
        prompt_id: 'prompt-1',
      },
      {
        callId: '3',
        name: 'mockTool',
        args: { call: 3 },
        isClientInitiated: false,
        prompt_id: 'prompt-1',
      },
    ];

    // Act
    const schedulePromise = scheduler.schedule(
      requests,
      abortController.signal,
    );

    // Wait for the second call to start, then abort.
    await vi.waitFor(() => {
      expect(secondCallStarted).toBe(true);
    });
    abortController.abort();

    await schedulePromise;

    // Assert
    await vi.waitFor(() => {
      expect(onAllToolCallsComplete).toHaveBeenCalled();
    });

    // Check that execute was called for all three tools initially
    expect(executeFn).toHaveBeenCalledTimes(3);
    expect(executeFn).toHaveBeenCalledWith({ call: 1 });
    expect(executeFn).toHaveBeenCalledWith({ call: 2 });
    expect(executeFn).toHaveBeenCalledWith({ call: 3 });

    const completedCalls = onAllToolCallsComplete.mock
      .calls[0][0] as ToolCall[];
    expect(completedCalls).toHaveLength(3);

    const call1 = completedCalls.find((c) => c.request.callId === '1');
    const call2 = completedCalls.find((c) => c.request.callId === '2');
    const call3 = completedCalls.find((c) => c.request.callId === '3');

    expect(call1?.status).toBe('success');
    expect(call2?.status).toBe('cancelled');
    expect(call3?.status).toBe('cancelled');
  });

  // ─────────────────────────────────────────────────────────────────────────
  // Regression: Bug 1 — ask + ProceedAlways must NOT batch-approve other
  //   pending hookForceAsk calls in the same scheduling round.
  // ─────────────────────────────────────────────────────────────────────────
  it('ask + ProceedAlways should NOT auto-approve other hookForceAsk-pending calls in the same batch', async () => {
    // A tool with NO native confirmation requirement (shouldConfirmExecute
    // returns false).  The hook turns every call into a mandatory ask dialog.
    const executeFn = vi.fn().mockResolvedValue({
      llmContent: 'done',
      returnDisplay: 'done',
    });
    const noConfirmTool = new MockTool({
      name: 'hookAskTool',
      execute: executeFn,
      shouldConfirmExecute: vi.fn().mockResolvedValue(false),
    });

    // Hook always returns an 'ask' decision
    const hookAskOutput = {
      isBlockingDecision: () => false,
      shouldStopExecution: () => false,
      isAskDecision: () => true,
      systemMessage: 'Please verify this operation',
      getModifiedToolInput: () => null,
      getEffectiveReason: () => '',
    };
    const mockHookSystemAsk = {
      firePreToolUseEvent: vi.fn().mockResolvedValue(hookAskOutput),
      firePostToolUseEvent: vi.fn().mockResolvedValue(undefined),
    };

    const toolRegistryAsk = {
      getTool: () => noConfirmTool,
      getFunctionDeclarations: () => [],
      tools: new Map(),
      discovery: {},
      registerTool: () => {},
      getToolByName: () => noConfirmTool,
      getToolByDisplayName: () => noConfirmTool,
      getTools: () => [],
      discoverTools: async () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
    } as unknown as ToolRegistry;

    const onAllToolCallsCompleteAsk = vi.fn();
    const onToolCallsUpdateAsk = vi.fn();

    const mockConfigAsk = {
      getSessionId: () => 'test-session-ask',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => ApprovalMode.DEFAULT,
      getAllowedTools: () => [],
      getToolRegistry: () => toolRegistryAsk,
      getContentGeneratorConfig: () => ({
        model: 'test-model',
        authType: 'gemini',
      }),
      getShellExecutionConfig: () => ({
        terminalWidth: 90,
        terminalHeight: 30,
      }),
      storage: { getProjectTempDir: () => '/tmp' },
      getTruncateToolOutputThreshold: () =>
        DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
      getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
      getUseSmartEdit: () => false,
      getUseModelRouter: () => false,
      getGeminiClient: () => null,
      getChatRecordingService: () => undefined,
      isInteractive: () => true, // prevent auto-denial of approval prompts
      getExperimentalZedIntegration: () => false,
      getEnableHooks: () => true,
      getHookSystem: () => mockHookSystemAsk,
    } as unknown as Config;

    const pendingAskConfirmations: Array<
      (
        outcome: ToolConfirmationOutcome,
        payload?: ToolConfirmationPayload,
      ) => Promise<void>
    > = [];

    const schedulerAsk = new CoreToolScheduler({
      config: mockConfigAsk,
      onAllToolCallsComplete: onAllToolCallsCompleteAsk,
      onToolCallsUpdate: (toolCalls) => {
        onToolCallsUpdateAsk(toolCalls);
        toolCalls.forEach((call) => {
          if (call.status === 'awaiting_approval') {
            const waitingCall = call as WaitingToolCall;
            const alreadyCaptured = pendingAskConfirmations.find(
              (h) => h === waitingCall.confirmationDetails.onConfirm,
            );
            if (!alreadyCaptured) {
              pendingAskConfirmations.push(
                waitingCall.confirmationDetails.onConfirm,
              );
            }
          }
        });
      },
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
    });

    const abortControllerAsk = new AbortController();
    const askRequests = [
      {
        callId: 'hook-ask-1',
        name: 'hookAskTool',
        args: {},
        isClientInitiated: false,
        prompt_id: 'p-ask-1',
      },
      {
        callId: 'hook-ask-2',
        name: 'hookAskTool',
        args: {},
        isClientInitiated: false,
        prompt_id: 'p-ask-2',
      },
    ];

    await schedulerAsk.schedule(askRequests, abortControllerAsk.signal);

    // Both calls must be in awaiting_approval because the hook forced ask.
    await vi.waitFor(() => {
      const latestCalls = onToolCallsUpdateAsk.mock.calls.at(
        -1,
      )?.[0] as ToolCall[];
      expect(
        latestCalls?.filter((c) => c.status === 'awaiting_approval').length,
      ).toBe(2);
    });

    expect(pendingAskConfirmations.length).toBe(2);

    // Approve the first call with ProceedAlways.
    await pendingAskConfirmations[0](ToolConfirmationOutcome.ProceedAlways);

    // ── Key regression assertion ──────────────────────────────────────────
    // The second hookForceAsk call must remain awaiting_approval.
    // autoApproveCompatiblePendingTools must NOT silently promote it,
    // because the hook wants to gate every invocation individually.
    await vi.waitFor(() => {
      const latestCalls = onToolCallsUpdateAsk.mock.calls.at(
        -1,
      )?.[0] as ToolCall[];
      const secondCall = latestCalls?.find(
        (c) => c.request.callId === 'hook-ask-2',
      );
      expect(secondCall?.status).toBe('awaiting_approval');
    });

    // Confirm the second call individually so the test can finish cleanly.
    await pendingAskConfirmations[1](ToolConfirmationOutcome.ProceedOnce);

    await vi.waitFor(() => {
      expect(onAllToolCallsCompleteAsk).toHaveBeenCalled();
      const completedCalls = onAllToolCallsCompleteAsk.mock
        .calls[0][0] as ToolCall[];
      expect(completedCalls.every((c) => c.status === 'success')).toBe(true);
    });
  });

  // ─────────────────────────────────────────────────────────────────────────
  // Regression: hookForceAsk must always synthesize an 'info' dialog with a
  //   fixed confirmation prompt, while still forwarding the original
  //   onConfirm so native side-effects (e.g. setApprovalMode, allowlisting)
  //   are preserved. The per-hook messages are rendered separately as
  //   structured HookNotificationDisplay boxes above the dialog, so the
  //   dialog prompt no longer embeds the hook's systemMessage.
  // ─────────────────────────────────────────────────────────────────────────
  it('hookForceAsk should synthesize an info dialog with a fixed prompt, preserving original onConfirm side-effects', async () => {
    let approvalMode = ApprovalMode.DEFAULT;

    // Hook always returns 'ask'
    const hookAugmentOutput = {
      isBlockingDecision: () => false,
      shouldStopExecution: () => false,
      isAskDecision: () => true,
      systemMessage: 'Hook inspection required',
      getModifiedToolInput: () => null,
      getEffectiveReason: () => '',
    };
    const mockHookSystemAugment = {
      firePreToolUseEvent: vi.fn().mockResolvedValue(hookAugmentOutput),
      firePostToolUseEvent: vi.fn().mockResolvedValue(undefined),
    };

    // TestApprovalTool provides a native 'edit'-type confirmation whose
    // onConfirm calls config.setApprovalMode(AUTO_EDIT) on ProceedAlways.
    // toolRegistryAugment is declared first so it can be referenced directly
    // inside the mockConfigAugment literal, avoiding a post-assignment cast.
    // eslint-disable-next-line prefer-const
    let testToolAugment: TestApprovalTool;
    const toolRegistryAugment = {
      getTool: () => testToolAugment,
      getFunctionDeclarations: () => [],
      tools: new Map(),
      discovery: {},
      registerTool: () => {},
      getToolByName: () => testToolAugment,
      getToolByDisplayName: () => testToolAugment,
      getTools: () => [],
      discoverTools: async () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
    } as unknown as ToolRegistry;

    const mockConfigAugment = {
      getSessionId: () => 'test-session-augment',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => approvalMode,
      setApprovalMode: (mode: ApprovalMode) => {
        approvalMode = mode;
      },
      getAllowedTools: () => [],
      getToolRegistry: () => toolRegistryAugment,
      getContentGeneratorConfig: () => ({
        model: 'test-model',
        authType: 'gemini',
      }),
      getShellExecutionConfig: () => ({
        terminalWidth: 90,
        terminalHeight: 30,
      }),
      storage: { getProjectTempDir: () => '/tmp' },
      getTruncateToolOutputThreshold: () =>
        DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
      getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
      getUseSmartEdit: () => false,
      getUseModelRouter: () => false,
      getGeminiClient: () => null,
      getChatRecordingService: () => undefined,
      isInteractive: () => true, // prevent auto-denial of approval prompts
      getExperimentalZedIntegration: () => false,
      getEnableHooks: () => true,
      getHookSystem: () => mockHookSystemAugment,
    } as unknown as Config;

    testToolAugment = new TestApprovalTool(mockConfigAugment);

    const onAllToolCallsCompleteAugment = vi.fn();
    const onToolCallsUpdateAugment = vi.fn();
    let capturedWaitingCall: WaitingToolCall | undefined;

    const schedulerAugment = new CoreToolScheduler({
      config: mockConfigAugment,
      onAllToolCallsComplete: onAllToolCallsCompleteAugment,
      onToolCallsUpdate: (toolCalls) => {
        onToolCallsUpdateAugment(toolCalls);
        if (!capturedWaitingCall) {
          capturedWaitingCall = toolCalls.find(
            (c) => c.status === 'awaiting_approval',
          ) as WaitingToolCall | undefined;
        }
      },
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
    });

    const abortControllerAugment = new AbortController();
    await schedulerAugment.schedule(
      [
        {
          callId: 'augment-1',
          name: 'testApprovalTool',
          args: { id: 'augment-test' },
          isClientInitiated: false,
          prompt_id: 'p-augment',
        },
      ],
      abortControllerAugment.signal,
    );

    // Wait for the tool to enter awaiting_approval
    await vi.waitFor(() => {
      expect(capturedWaitingCall).toBeDefined();
    });

    // ── Key assertions ──────────────────────────────────────────────────────
    // 1. Confirmation type must be 'info' — dialog simply asks the user to
    //    decide; the per-hook messages render separately as notification
    //    boxes above this dialog.
    expect(capturedWaitingCall!.confirmationDetails.type).toBe('info');

    // 2. The prompt should be the fixed confirmation ask, NOT the merged
    //    hook systemMessage (which would duplicate the notification boxes).
    //    The English string here is also used as the i18n key on the UI side.
    expect(
      (capturedWaitingCall!.confirmationDetails as { prompt?: string }).prompt,
    ).toBe('A hook requires your confirmation to proceed.');

    // 3. The title should be the generic hook header.
    expect(capturedWaitingCall!.confirmationDetails.title).toBe(
      'Hook Requires Confirmation',
    );

    // Trigger ProceedAlways through the wrapped onConfirm stored in the call.
    await capturedWaitingCall!.confirmationDetails.onConfirm(
      ToolConfirmationOutcome.ProceedAlways,
    );

    // 4. The original tool onConfirm side-effect must have run:
    //    TestApprovalTool.onConfirm sets config.setApprovalMode(AUTO_EDIT)
    //    on ProceedAlways — if it was replaced by an empty onConfirm this
    //    would remain DEFAULT.
    expect(approvalMode).toBe(ApprovalMode.AUTO_EDIT);

    // 5. Execution should complete successfully.
    await vi.waitFor(() => {
      expect(onAllToolCallsCompleteAugment).toHaveBeenCalled();
      const completedCalls = onAllToolCallsCompleteAugment.mock
        .calls[0][0] as ToolCall[];
      expect(completedCalls[0].status).toBe('success');
    });
  });

  // ─────────────────────────────────────────────────────────────────────────
  // Regression: Bug #421 — PreToolUse allow decision reason field is silently
  //   ignored and not displayed in UI.
  //   After the structured-notification refactor, aggregator always produces
  //   `notifications[]` when any hook supplies systemMessage or reason, and
  //   the scheduler emits those as HookNotificationDisplay objects.
  // ─────────────────────────────────────────────────────────────────────────
  it('allow decision with reason should emit a structured notification to outputUpdateHandler', async () => {
    const executeFn = vi.fn().mockResolvedValue({
      llmContent: 'done',
      returnDisplay: 'done',
    });
    const allowReasonTool = new MockTool({
      name: 'allowReasonTool',
      execute: executeFn,
      shouldConfirmExecute: vi.fn().mockResolvedValue(false),
    });

    // Hook returns allow with reason. Aggregator would forward this as one
    // entry in notifications[]; we mock the aggregated shape directly.
    const hookAllowReasonOutput = {
      decision: 'allow' as const,
      isBlockingDecision: () => false,
      shouldStopExecution: () => false,
      isAskDecision: () => false,
      systemMessage: undefined,
      reason: 'Warning: skill signature could not be verified',
      getModifiedToolInput: () => null,
      getEffectiveReason: () =>
        'Warning: skill signature could not be verified',
      notifications: [
        {
          hookName: 'skill-verifier',
          message: 'Warning: skill signature could not be verified',
          decision: 'allow' as const,
        },
      ],
    };
    const mockHookAllowReason = {
      firePreToolUseEvent: vi.fn().mockResolvedValue(hookAllowReasonOutput),
      firePostToolUseEvent: vi.fn().mockResolvedValue(undefined),
    };

    const toolRegistryAllowReason = {
      getTool: () => allowReasonTool,
      getFunctionDeclarations: () => [],
      tools: new Map(),
      discovery: {},
      registerTool: () => {},
      getToolByName: () => allowReasonTool,
      getToolByDisplayName: () => allowReasonTool,
      getTools: () => [],
      discoverTools: async () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
    } as unknown as ToolRegistry;

    const onAllToolCallsCompleteAllowReason = vi.fn();
    const outputUpdateHandlerMock = vi.fn();

    const mockConfigAllowReason = {
      getSessionId: () => 'test-session-allow-reason',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => ApprovalMode.DEFAULT,
      getAllowedTools: () => [],
      getToolRegistry: () => toolRegistryAllowReason,
      getContentGeneratorConfig: () => ({
        model: 'test-model',
        authType: 'gemini',
      }),
      getShellExecutionConfig: () => ({
        terminalWidth: 90,
        terminalHeight: 30,
      }),
      storage: { getProjectTempDir: () => '/tmp' },
      getTruncateToolOutputThreshold: () =>
        DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
      getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
      getUseSmartEdit: () => false,
      getUseModelRouter: () => false,
      getGeminiClient: () => null,
      getChatRecordingService: () => undefined,
      isInteractive: () => true,
      getExperimentalZedIntegration: () => false,
      getEnableHooks: () => true,
      getHookSystem: () => mockHookAllowReason,
    } as unknown as Config;

    const schedulerAllowReason = new CoreToolScheduler({
      config: mockConfigAllowReason,
      onAllToolCallsComplete: onAllToolCallsCompleteAllowReason,
      onToolCallsUpdate: vi.fn(),
      outputUpdateHandler: outputUpdateHandlerMock,
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
    });

    const abortControllerAllowReason = new AbortController();
    await schedulerAllowReason.schedule(
      [
        {
          callId: 'allow-reason-1',
          name: 'allowReasonTool',
          args: {},
          isClientInitiated: false,
          prompt_id: 'p-allow-reason',
        },
      ],
      abortControllerAllowReason.signal,
    );

    await vi.waitFor(() => {
      expect(onAllToolCallsCompleteAllowReason).toHaveBeenCalled();
    });

    // The notification must have been emitted via outputUpdateHandler as a
    // structured HookNotificationDisplay so that the terminal UI renders it
    // with the hook's name, icon and color.
    expect(outputUpdateHandlerMock).toHaveBeenCalledWith('allow-reason-1', {
      hookName: 'skill-verifier',
      hookMessage: 'Warning: skill signature could not be verified',
      decision: 'allow',
      mergedDecision: 'allow',
    });

    // The tool must still execute successfully — allow means proceed.
    const completedCalls = onAllToolCallsCompleteAllowReason.mock
      .calls[0][0] as ToolCall[];
    expect(completedCalls[0].status).toBe('success');
  });

  // ─────────────────────────────────────────────────────────────────────────
  // Regression: Issue #535 — PostToolUse hook reason was not surfaced to UI.
  //   The aggregator already produces notifications[]; the scheduler must
  //   forward them via outputUpdateHandler so the terminal renders the same
  //   per-hook box style used for PreToolUse.
  // ─────────────────────────────────────────────────────────────────────────
  it('PostToolUse allow decision with reason should emit a structured notification to outputUpdateHandler', async () => {
    const executeFn = vi.fn().mockResolvedValue({
      llmContent: 'tool ran',
      returnDisplay: 'tool ran',
    });
    const postAllowTool = new MockTool({
      name: 'postAllowTool',
      execute: executeFn,
      shouldConfirmExecute: vi.fn().mockResolvedValue(false),
    });

    const postAllowHookOutput = {
      decision: 'allow' as const,
      isBlockingDecision: () => false,
      shouldStopExecution: () => false,
      isAskDecision: () => false,
      systemMessage: undefined,
      reason: 'Tool output passed compliance review',
      getEffectiveReason: () => 'Tool output passed compliance review',
      getAdditionalContext: () => undefined,
      notifications: [
        {
          hookName: 'compliance-reviewer',
          message: 'Tool output passed compliance review',
          decision: 'allow' as const,
        },
      ],
    };
    const mockHookPostAllow = {
      firePreToolUseEvent: vi.fn().mockResolvedValue(undefined),
      firePostToolUseEvent: vi.fn().mockResolvedValue(postAllowHookOutput),
    };

    const toolRegistryPostAllow = {
      getTool: () => postAllowTool,
      getFunctionDeclarations: () => [],
      tools: new Map(),
      discovery: {},
      registerTool: () => {},
      getToolByName: () => postAllowTool,
      getToolByDisplayName: () => postAllowTool,
      getTools: () => [],
      discoverTools: async () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
    } as unknown as ToolRegistry;

    const onAllToolCallsCompletePostAllow = vi.fn();
    const outputUpdateHandlerPostAllow = vi.fn();

    const mockConfigPostAllow = {
      getSessionId: () => 'test-session-post-allow',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => ApprovalMode.DEFAULT,
      getAllowedTools: () => [],
      getToolRegistry: () => toolRegistryPostAllow,
      getContentGeneratorConfig: () => ({
        model: 'test-model',
        authType: 'gemini',
      }),
      getShellExecutionConfig: () => ({
        terminalWidth: 90,
        terminalHeight: 30,
      }),
      storage: { getProjectTempDir: () => '/tmp' },
      getTruncateToolOutputThreshold: () =>
        DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
      getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
      getUseSmartEdit: () => false,
      getUseModelRouter: () => false,
      getGeminiClient: () => null,
      getChatRecordingService: () => undefined,
      isInteractive: () => true,
      getExperimentalZedIntegration: () => false,
      getEnableHooks: () => true,
      getHookSystem: () => mockHookPostAllow,
    } as unknown as Config;

    const schedulerPostAllow = new CoreToolScheduler({
      config: mockConfigPostAllow,
      onAllToolCallsComplete: onAllToolCallsCompletePostAllow,
      onToolCallsUpdate: vi.fn(),
      outputUpdateHandler: outputUpdateHandlerPostAllow,
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
    });

    const abortControllerPostAllow = new AbortController();
    await schedulerPostAllow.schedule(
      [
        {
          callId: 'post-allow-1',
          name: 'postAllowTool',
          args: {},
          isClientInitiated: false,
          prompt_id: 'p-post-allow',
        },
      ],
      abortControllerPostAllow.signal,
    );

    await vi.waitFor(() => {
      expect(onAllToolCallsCompletePostAllow).toHaveBeenCalled();
    });

    // Notification must surface to the UI even though the decision is allow.
    expect(outputUpdateHandlerPostAllow).toHaveBeenCalledWith('post-allow-1', {
      hookName: 'compliance-reviewer',
      hookMessage: 'Tool output passed compliance review',
      decision: 'allow',
      mergedDecision: 'allow',
    });

    const completedCalls = onAllToolCallsCompletePostAllow.mock
      .calls[0][0] as ToolCall[];
    expect(completedCalls[0].status).toBe('success');
  });

  it('PostToolUse block decision should emit notification AND replace tool response', async () => {
    const executeFn = vi.fn().mockResolvedValue({
      llmContent: 'tool ran',
      returnDisplay: 'tool ran',
    });
    const postBlockTool = new MockTool({
      name: 'postBlockTool',
      execute: executeFn,
      shouldConfirmExecute: vi.fn().mockResolvedValue(false),
    });

    const postBlockHookOutput = {
      decision: 'block' as const,
      isBlockingDecision: () => true,
      shouldStopExecution: () => false,
      isAskDecision: () => false,
      systemMessage: undefined,
      reason: 'Output requires follow-up review before continuing',
      getEffectiveReason: () =>
        'Output requires follow-up review before continuing',
      getAdditionalContext: () => undefined,
      notifications: [
        {
          hookName: 'compliance-reviewer',
          message: 'Output requires follow-up review before continuing',
          decision: 'block' as const,
        },
      ],
    };
    const mockHookPostBlock = {
      firePreToolUseEvent: vi.fn().mockResolvedValue(undefined),
      firePostToolUseEvent: vi.fn().mockResolvedValue(postBlockHookOutput),
    };

    const toolRegistryPostBlock = {
      getTool: () => postBlockTool,
      getFunctionDeclarations: () => [],
      tools: new Map(),
      discovery: {},
      registerTool: () => {},
      getToolByName: () => postBlockTool,
      getToolByDisplayName: () => postBlockTool,
      getTools: () => [],
      discoverTools: async () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
    } as unknown as ToolRegistry;

    const onAllToolCallsCompletePostBlock = vi.fn();
    const outputUpdateHandlerPostBlock = vi.fn();

    const mockConfigPostBlock = {
      getSessionId: () => 'test-session-post-block',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => ApprovalMode.DEFAULT,
      getAllowedTools: () => [],
      getToolRegistry: () => toolRegistryPostBlock,
      getContentGeneratorConfig: () => ({
        model: 'test-model',
        authType: 'gemini',
      }),
      getShellExecutionConfig: () => ({
        terminalWidth: 90,
        terminalHeight: 30,
      }),
      storage: { getProjectTempDir: () => '/tmp' },
      getTruncateToolOutputThreshold: () =>
        DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
      getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
      getUseSmartEdit: () => false,
      getUseModelRouter: () => false,
      getGeminiClient: () => null,
      getChatRecordingService: () => undefined,
      isInteractive: () => true,
      getExperimentalZedIntegration: () => false,
      getEnableHooks: () => true,
      getHookSystem: () => mockHookPostBlock,
    } as unknown as Config;

    const schedulerPostBlock = new CoreToolScheduler({
      config: mockConfigPostBlock,
      onAllToolCallsComplete: onAllToolCallsCompletePostBlock,
      onToolCallsUpdate: vi.fn(),
      outputUpdateHandler: outputUpdateHandlerPostBlock,
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
    });

    const abortControllerPostBlock = new AbortController();
    await schedulerPostBlock.schedule(
      [
        {
          callId: 'post-block-1',
          name: 'postBlockTool',
          args: {},
          isClientInitiated: false,
          prompt_id: 'p-post-block',
        },
      ],
      abortControllerPostBlock.signal,
    );

    await vi.waitFor(() => {
      expect(onAllToolCallsCompletePostBlock).toHaveBeenCalled();
    });

    // Block path: mergedDecision is 'block' so the per-hook box can dim.
    expect(outputUpdateHandlerPostBlock).toHaveBeenCalledWith('post-block-1', {
      hookName: 'compliance-reviewer',
      hookMessage: 'Output requires follow-up review before continuing',
      decision: 'block',
      mergedDecision: 'block',
    });

    // Tool execution still completes (status success); only the responseParts
    // sent back to the LLM are replaced with the hook reason.
    const completedCalls = onAllToolCallsCompletePostBlock.mock
      .calls[0][0] as ToolCall[];
    expect(completedCalls[0].status).toBe('success');
    const responseParts = (
      completedCalls[0] as { response: { responseParts: unknown } }
    ).response.responseParts;
    expect(JSON.stringify(responseParts)).toContain(
      'Output requires follow-up review before continuing',
    );
  });

  // ─────────────────────────────────────────────────────────────────────────
  // hook-ask command forwarding: safe shell command (echo)
  //   When shouldConfirmExecute() returns false (safe command like `echo`)
  //   but a hook forces ask, the scheduler must extract the shell command
  //   from reqInfo.args and surface it in the synthesized info dialog so
  //   the user can see what they are approving.
  // ─────────────────────────────────────────────────────────────────────────
  it('hookForceAsk with safe shell command should forward command from reqInfo.args into info dialog', async () => {
    // A shell-named tool whose shouldConfirmExecute returns false (safe cmd).
    const safeCmdTool = new MockTool({
      name: 'run_shell_command',
      shouldConfirmExecute: vi.fn().mockResolvedValue(false),
    });

    const hookAskOutputSafe = {
      isBlockingDecision: () => false,
      shouldStopExecution: () => false,
      isAskDecision: () => true,
      systemMessage: 'Hook verification required',
      getModifiedToolInput: () => null,
      getEffectiveReason: () => '',
    };
    const mockHookSafeCmd = {
      firePreToolUseEvent: vi.fn().mockResolvedValue(hookAskOutputSafe),
      firePostToolUseEvent: vi.fn().mockResolvedValue(undefined),
    };

    const toolRegistrySafeCmd = {
      getTool: () => safeCmdTool,
      getFunctionDeclarations: () => [],
      tools: new Map(),
      discovery: {},
      registerTool: () => {},
      getToolByName: () => safeCmdTool,
      getToolByDisplayName: () => safeCmdTool,
      getTools: () => [],
      discoverTools: async () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
    } as unknown as ToolRegistry;

    const onAllToolCallsCompleteSafeCmd = vi.fn();
    let capturedSafeCmdCall: WaitingToolCall | undefined;

    const mockConfigSafeCmd = {
      getSessionId: () => 'test-session-safe-cmd',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => ApprovalMode.DEFAULT,
      getAllowedTools: () => [],
      getToolRegistry: () => toolRegistrySafeCmd,
      getContentGeneratorConfig: () => ({
        model: 'test-model',
        authType: 'gemini',
      }),
      getShellExecutionConfig: () => ({
        terminalWidth: 90,
        terminalHeight: 30,
      }),
      storage: { getProjectTempDir: () => '/tmp' },
      getTruncateToolOutputThreshold: () =>
        DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
      getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
      getUseSmartEdit: () => false,
      getUseModelRouter: () => false,
      getGeminiClient: () => null,
      getChatRecordingService: () => undefined,
      isInteractive: () => true,
      getExperimentalZedIntegration: () => false,
      getEnableHooks: () => true,
      getHookSystem: () => mockHookSafeCmd,
    } as unknown as Config;

    const schedulerSafeCmd = new CoreToolScheduler({
      config: mockConfigSafeCmd,
      onAllToolCallsComplete: onAllToolCallsCompleteSafeCmd,
      onToolCallsUpdate: (toolCalls) => {
        if (!capturedSafeCmdCall) {
          capturedSafeCmdCall = toolCalls.find(
            (c) => c.status === 'awaiting_approval',
          ) as WaitingToolCall | undefined;
        }
      },
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
    });

    const acSafeCmd = new AbortController();
    await schedulerSafeCmd.schedule(
      [
        {
          callId: 'safe-cmd-1',
          name: 'run_shell_command',
          args: { command: 'echo hi' },
          isClientInitiated: false,
          prompt_id: 'p-safe-cmd',
        },
      ],
      acSafeCmd.signal,
    );

    await vi.waitFor(() => {
      expect(capturedSafeCmdCall).toBeDefined();
    });

    // 1. Dialog type must be 'info'.
    expect(capturedSafeCmdCall!.confirmationDetails.type).toBe('info');

    // 2. command must be extracted from reqInfo.args['command'].
    expect(
      (capturedSafeCmdCall!.confirmationDetails as { command?: string })
        .command,
    ).toBe('echo hi');

    // 3. rootCommand must NOT be set — only the raw command is available
    //    from args; there is no exec confirmation to copy rootCommand from.
    expect(
      (
        capturedSafeCmdCall!.confirmationDetails as {
          rootCommand?: string;
        }
      ).rootCommand,
    ).toBeUndefined();

    // Clean up: confirm so the scheduler can finish.
    await capturedSafeCmdCall!.confirmationDetails.onConfirm(
      ToolConfirmationOutcome.ProceedOnce,
    );
    await vi.waitFor(() => {
      expect(onAllToolCallsCompleteSafeCmd).toHaveBeenCalled();
    });
  });

  // ─────────────────────────────────────────────────────────────────────────
  // hook-ask command forwarding: exec-type confirmation (dangerous command)
  //   When shouldConfirmExecute() returns an exec-type confirmation and a
  //   hook forces ask, the synthesized info dialog must carry both `command`
  //   and `rootCommand` forwarded from the native exec confirmation details.
  // ─────────────────────────────────────────────────────────────────────────
  it('hookForceAsk with exec confirmation should forward command and rootCommand into info dialog', async () => {
    const execCommand = 'rm -rf /tmp/test-dir';
    const execRootCommand = 'rm';

    const execCmdTool = new MockTool({
      name: 'run_shell_command',
      shouldConfirmExecute: vi.fn().mockResolvedValue({
        type: 'exec' as const,
        title: 'Confirm Shell Command',
        command: execCommand,
        rootCommand: execRootCommand,
        onConfirm: async () => {},
      }),
    });

    const hookAskOutputExec = {
      isBlockingDecision: () => false,
      shouldStopExecution: () => false,
      isAskDecision: () => true,
      systemMessage: 'Security hook inspection required',
      getModifiedToolInput: () => null,
      getEffectiveReason: () => '',
    };
    const mockHookExecCmd = {
      firePreToolUseEvent: vi.fn().mockResolvedValue(hookAskOutputExec),
      firePostToolUseEvent: vi.fn().mockResolvedValue(undefined),
    };

    const toolRegistryExecCmd = {
      getTool: () => execCmdTool,
      getFunctionDeclarations: () => [],
      tools: new Map(),
      discovery: {},
      registerTool: () => {},
      getToolByName: () => execCmdTool,
      getToolByDisplayName: () => execCmdTool,
      getTools: () => [],
      discoverTools: async () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
    } as unknown as ToolRegistry;

    const onAllToolCallsCompleteExecCmd = vi.fn();
    let capturedExecCmdCall: WaitingToolCall | undefined;

    const mockConfigExecCmd = {
      getSessionId: () => 'test-session-exec-cmd',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => ApprovalMode.DEFAULT,
      getAllowedTools: () => [],
      getToolRegistry: () => toolRegistryExecCmd,
      getContentGeneratorConfig: () => ({
        model: 'test-model',
        authType: 'gemini',
      }),
      getShellExecutionConfig: () => ({
        terminalWidth: 90,
        terminalHeight: 30,
      }),
      storage: { getProjectTempDir: () => '/tmp' },
      getTruncateToolOutputThreshold: () =>
        DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
      getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
      getUseSmartEdit: () => false,
      getUseModelRouter: () => false,
      getGeminiClient: () => null,
      getChatRecordingService: () => undefined,
      isInteractive: () => true,
      getExperimentalZedIntegration: () => false,
      getEnableHooks: () => true,
      getHookSystem: () => mockHookExecCmd,
    } as unknown as Config;

    const schedulerExecCmd = new CoreToolScheduler({
      config: mockConfigExecCmd,
      onAllToolCallsComplete: onAllToolCallsCompleteExecCmd,
      onToolCallsUpdate: (toolCalls) => {
        if (!capturedExecCmdCall) {
          capturedExecCmdCall = toolCalls.find(
            (c) => c.status === 'awaiting_approval',
          ) as WaitingToolCall | undefined;
        }
      },
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
    });

    const acExecCmd = new AbortController();
    await schedulerExecCmd.schedule(
      [
        {
          callId: 'exec-cmd-1',
          name: 'run_shell_command',
          args: { command: execCommand },
          isClientInitiated: false,
          prompt_id: 'p-exec-cmd',
        },
      ],
      acExecCmd.signal,
    );

    await vi.waitFor(() => {
      expect(capturedExecCmdCall).toBeDefined();
    });

    // 1. Dialog type must be 'info'.
    expect(capturedExecCmdCall!.confirmationDetails.type).toBe('info');

    // 2. command must be forwarded from the native exec confirmation.
    expect(
      (capturedExecCmdCall!.confirmationDetails as { command?: string })
        .command,
    ).toBe(execCommand);

    // 3. rootCommand must be forwarded from the native exec confirmation.
    expect(
      (
        capturedExecCmdCall!.confirmationDetails as {
          rootCommand?: string;
        }
      ).rootCommand,
    ).toBe(execRootCommand);

    // Clean up: confirm so the scheduler can finish.
    await capturedExecCmdCall!.confirmationDetails.onConfirm(
      ToolConfirmationOutcome.ProceedOnce,
    );
    await vi.waitFor(() => {
      expect(onAllToolCallsCompleteExecCmd).toHaveBeenCalled();
    });
  });
});

describe('CoreToolScheduler PostToolUseFailure notifications', () => {
  it('should emit notifications via outputUpdateHandler when PostToolUseFailure hook returns allow+reason', async () => {
    const failingTool = new MockTool({
      name: 'run_shell_command',
      execute: vi.fn().mockResolvedValue({
        llmContent: 'error output',
        returnDisplay: 'error output',
        error: { message: 'Permission denied', type: 'SANDBOX_ERROR' },
      }),
      shouldConfirmExecute: vi.fn().mockResolvedValue(false),
    });

    const failureHookOutput = {
      decision: 'allow' as const,
      isBlockingDecision: () => false,
      shouldStopExecution: () => false,
      isAskDecision: () => false,
      systemMessage: undefined,
      reason: 'Sandbox violation logged',
      getSandboxBypassRequest: () => undefined,
      notifications: [
        {
          hookName: 'sandbox-guard',
          message: 'Sandbox violation logged',
          decision: 'allow' as const,
        },
      ],
    };
    const mockHookSystem = {
      firePreToolUseEvent: vi.fn().mockResolvedValue(undefined),
      firePostToolUseEvent: vi.fn().mockResolvedValue(undefined),
      firePostToolUseFailureEvent: vi.fn().mockResolvedValue(failureHookOutput),
      setHookEnabled: vi.fn(),
    };

    const toolRegistry = {
      getTool: () => failingTool,
      getFunctionDeclarations: () => [],
      tools: new Map(),
      discovery: {},
      registerTool: () => {},
      getToolByName: () => failingTool,
      getToolByDisplayName: () => failingTool,
      getTools: () => [],
      discoverTools: async () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
    } as unknown as ToolRegistry;

    const onAllToolCallsComplete = vi.fn();
    const outputUpdateHandlerMock = vi.fn();

    const mockConfig = {
      getSessionId: () => 'test-session-failure-notif',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => ApprovalMode.DEFAULT,
      getAllowedTools: () => [],
      getToolRegistry: () => toolRegistry,
      getContentGeneratorConfig: () => ({
        model: 'test-model',
        authType: 'gemini',
      }),
      getShellExecutionConfig: () => ({
        terminalWidth: 90,
        terminalHeight: 30,
      }),
      storage: { getProjectTempDir: () => '/tmp' },
      getTruncateToolOutputThreshold: () =>
        DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
      getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
      getUseSmartEdit: () => false,
      getUseModelRouter: () => false,
      getGeminiClient: () => null,
      getChatRecordingService: () => undefined,
      isInteractive: () => true,
      getExperimentalZedIntegration: () => false,
      getEnableHooks: () => true,
      getHookSystem: () => mockHookSystem,
    } as unknown as Config;

    const scheduler = new CoreToolScheduler({
      config: mockConfig,
      onAllToolCallsComplete,
      onToolCallsUpdate: vi.fn(),
      outputUpdateHandler: outputUpdateHandlerMock,
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
      onSandboxBypassRequested: vi.fn().mockResolvedValue(false),
    });

    await scheduler.schedule(
      [
        {
          callId: 'fail-notif-1',
          name: 'run_shell_command',
          args: { command: 'dangerous-cmd' },
          isClientInitiated: false,
          prompt_id: 'p-fail',
        },
      ],
      new AbortController().signal,
    );

    await vi.waitFor(() => {
      expect(onAllToolCallsComplete).toHaveBeenCalled();
    });

    expect(outputUpdateHandlerMock).toHaveBeenCalledWith('fail-notif-1', {
      hookName: 'sandbox-guard',
      hookMessage: 'Sandbox violation logged',
      decision: 'allow',
      mergedDecision: 'allow',
    });
  });

  it('should emit notifications for non-shell tool failures without onSandboxBypassRequested', async () => {
    const failingEditTool = new MockTool({
      name: 'edit',
      execute: vi.fn().mockResolvedValue({
        llmContent: 'error output',
        returnDisplay: 'error output',
        error: { message: 'File not found' },
      }),
      shouldConfirmExecute: vi.fn().mockResolvedValue(false),
    });

    const failureHookOutput = {
      decision: 'allow' as const,
      isBlockingDecision: () => false,
      shouldStopExecution: () => false,
      isAskDecision: () => false,
      systemMessage: undefined,
      reason: 'Edit failure logged',
      getSandboxBypassRequest: () => undefined,
      notifications: [
        {
          hookName: 'audit-hook',
          message: 'Edit failure logged',
          decision: 'allow' as const,
        },
      ],
    };
    const mockHookSystem = {
      firePreToolUseEvent: vi.fn().mockResolvedValue(undefined),
      firePostToolUseEvent: vi.fn().mockResolvedValue(undefined),
      firePostToolUseFailureEvent: vi.fn().mockResolvedValue(failureHookOutput),
    };

    const toolRegistry = {
      getTool: () => failingEditTool,
      getFunctionDeclarations: () => [],
      tools: new Map(),
      discovery: {},
      registerTool: () => {},
      getToolByName: () => failingEditTool,
      getToolByDisplayName: () => failingEditTool,
      getTools: () => [],
      discoverTools: async () => {},
      getAllTools: () => [],
      getToolsByServer: () => [],
    } as unknown as ToolRegistry;

    const onAllToolCallsComplete = vi.fn();
    const outputUpdateHandlerMock = vi.fn();

    const mockConfig = {
      getSessionId: () => 'test-session-edit-failure',
      getUsageStatisticsEnabled: () => true,
      getDebugMode: () => false,
      getApprovalMode: () => ApprovalMode.DEFAULT,
      getAllowedTools: () => [],
      getToolRegistry: () => toolRegistry,
      getContentGeneratorConfig: () => ({
        model: 'test-model',
        authType: 'gemini',
      }),
      getShellExecutionConfig: () => ({
        terminalWidth: 90,
        terminalHeight: 30,
      }),
      storage: { getProjectTempDir: () => '/tmp' },
      getTruncateToolOutputThreshold: () =>
        DEFAULT_TRUNCATE_TOOL_OUTPUT_THRESHOLD,
      getTruncateToolOutputLines: () => DEFAULT_TRUNCATE_TOOL_OUTPUT_LINES,
      getUseSmartEdit: () => false,
      getUseModelRouter: () => false,
      getGeminiClient: () => null,
      getChatRecordingService: () => undefined,
      isInteractive: () => true,
      getExperimentalZedIntegration: () => false,
      getEnableHooks: () => true,
      getHookSystem: () => mockHookSystem,
    } as unknown as Config;

    // No onSandboxBypassRequested provided
    const scheduler = new CoreToolScheduler({
      config: mockConfig,
      onAllToolCallsComplete,
      onToolCallsUpdate: vi.fn(),
      outputUpdateHandler: outputUpdateHandlerMock,
      getPreferredEditor: () => 'vscode',
      onEditorClose: vi.fn(),
    });

    await scheduler.schedule(
      [
        {
          callId: 'edit-fail-1',
          name: 'edit',
          args: { file: '/missing.ts' },
          isClientInitiated: false,
          prompt_id: 'p-edit-fail',
        },
      ],
      new AbortController().signal,
    );

    await vi.waitFor(() => {
      expect(onAllToolCallsComplete).toHaveBeenCalled();
    });

    expect(mockHookSystem.firePostToolUseFailureEvent).toHaveBeenCalledWith(
      'edit-fail-1',
      'edit',
      { file: '/missing.ts' },
      'File not found',
      undefined,
    );

    expect(outputUpdateHandlerMock).toHaveBeenCalledWith('edit-fail-1', {
      hookName: 'audit-hook',
      hookMessage: 'Edit failure logged',
      decision: 'allow',
      mergedDecision: 'allow',
    });

    const completedCalls = onAllToolCallsComplete.mock
      .calls[0][0] as ToolCall[];
    expect(completedCalls[0].status).toBe('error');
  });
});

describe('truncateAndSaveToFile', () => {
  const mockWriteFile = vi.mocked(fs.writeFile);
  const THRESHOLD = 40_000;
  const TRUNCATE_LINES = 1000;

  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('should return content unchanged if below threshold', async () => {
    const content = 'Short content';
    const callId = 'test-call-id';
    const projectTempDir = '/tmp';

    const result = await truncateAndSaveToFile(
      content,
      callId,
      projectTempDir,
      THRESHOLD,
      TRUNCATE_LINES,
    );

    expect(result).toEqual({ content });
    expect(mockWriteFile).not.toHaveBeenCalled();
  });

  it('should truncate content by lines when content has many lines', async () => {
    // Create content that exceeds 100,000 character threshold with many lines
    const lines = Array(2000).fill('x'.repeat(100)); // 100 chars per line * 2000 lines = 200,000 chars
    const content = lines.join('\n');
    const callId = 'test-call-id';
    const projectTempDir = '/tmp';

    mockWriteFile.mockResolvedValue(undefined);

    const result = await truncateAndSaveToFile(
      content,
      callId,
      projectTempDir,
      THRESHOLD,
      TRUNCATE_LINES,
    );

    expect(result.outputFile).toBe(
      path.join(projectTempDir, `${callId}.output`),
    );
    expect(mockWriteFile).toHaveBeenCalledWith(
      path.join(projectTempDir, `${callId}.output`),
      content,
    );

    // Should contain the first and last lines with 1/5 head and 4/5 tail
    const head = Math.floor(TRUNCATE_LINES / 5);
    const beginning = lines.slice(0, head);
    const end = lines.slice(-(TRUNCATE_LINES - head));
    const expectedTruncated =
      beginning.join('\n') + '\n... [CONTENT TRUNCATED] ...\n' + end.join('\n');

    expect(result.content).toContain(
      'Tool output was too large and has been truncated',
    );
    expect(result.content).toContain('Truncated part of the output:');
    expect(result.content).toContain(expectedTruncated);
  });

  it('should wrap and truncate content when content has few but long lines', async () => {
    const content = 'a'.repeat(200_000); // A single very long line
    const callId = 'test-call-id';
    const projectTempDir = '/tmp';
    const wrapWidth = 120;

    mockWriteFile.mockResolvedValue(undefined);

    // Manually wrap the content to generate the expected file content
    const wrappedLines: string[] = [];
    for (let i = 0; i < content.length; i += wrapWidth) {
      wrappedLines.push(content.substring(i, i + wrapWidth));
    }
    const expectedFileContent = wrappedLines.join('\n');

    const result = await truncateAndSaveToFile(
      content,
      callId,
      projectTempDir,
      THRESHOLD,
      TRUNCATE_LINES,
    );

    expect(result.outputFile).toBe(
      path.join(projectTempDir, `${callId}.output`),
    );
    // Check that the file was written with the wrapped content
    expect(mockWriteFile).toHaveBeenCalledWith(
      path.join(projectTempDir, `${callId}.output`),
      expectedFileContent,
    );

    // Should contain the first and last lines with 1/5 head and 4/5 tail of the wrapped content
    const head = Math.floor(TRUNCATE_LINES / 5);
    const beginning = wrappedLines.slice(0, head);
    const end = wrappedLines.slice(-(TRUNCATE_LINES - head));
    const expectedTruncated =
      beginning.join('\n') + '\n... [CONTENT TRUNCATED] ...\n' + end.join('\n');
    expect(result.content).toContain(
      'Tool output was too large and has been truncated',
    );
    expect(result.content).toContain('Truncated part of the output:');
    expect(result.content).toContain(expectedTruncated);
  });

  it('should handle file write errors gracefully', async () => {
    const content = 'a'.repeat(2_000_000);
    const callId = 'test-call-id';
    const projectTempDir = '/tmp';

    mockWriteFile.mockRejectedValue(new Error('File write failed'));

    const result = await truncateAndSaveToFile(
      content,
      callId,
      projectTempDir,
      THRESHOLD,
      TRUNCATE_LINES,
    );

    expect(result.outputFile).toBeUndefined();
    expect(result.content).toContain(
      '[Note: Could not save full output to file]',
    );
    expect(mockWriteFile).toHaveBeenCalled();
  });

  it('should save to correct file path with call ID', async () => {
    const content = 'a'.repeat(200_000);
    const callId = 'unique-call-123';
    const projectTempDir = '/custom/temp/dir';
    const wrapWidth = 120;

    mockWriteFile.mockResolvedValue(undefined);

    // Manually wrap the content to generate the expected file content
    const wrappedLines: string[] = [];
    for (let i = 0; i < content.length; i += wrapWidth) {
      wrappedLines.push(content.substring(i, i + wrapWidth));
    }
    const expectedFileContent = wrappedLines.join('\n');

    const result = await truncateAndSaveToFile(
      content,
      callId,
      projectTempDir,
      THRESHOLD,
      TRUNCATE_LINES,
    );

    const expectedPath = path.join(projectTempDir, `${callId}.output`);
    expect(result.outputFile).toBe(expectedPath);
    expect(mockWriteFile).toHaveBeenCalledWith(
      expectedPath,
      expectedFileContent,
    );
  });

  it('should include helpful instructions in truncated message', async () => {
    const content = 'a'.repeat(2_000_000);
    const callId = 'test-call-id';
    const projectTempDir = '/tmp';

    mockWriteFile.mockResolvedValue(undefined);

    const result = await truncateAndSaveToFile(
      content,
      callId,
      projectTempDir,
      THRESHOLD,
      TRUNCATE_LINES,
    );

    expect(result.content).toContain(
      'Tool output was too large and has been truncated',
    );
    expect(result.content).toContain('The full output has been saved to:');
    expect(result.content).toContain(
      'To read the complete output, use the read_file tool with the absolute file path above',
    );
    expect(result.content).toContain(
      'The truncated output below shows the beginning and end of the content',
    );
  });

  it('should sanitize callId to prevent path traversal', async () => {
    const content = 'a'.repeat(200_000);
    const callId = '../../../../../etc/passwd';
    const projectTempDir = '/tmp/safe_dir';
    const wrapWidth = 120;

    mockWriteFile.mockResolvedValue(undefined);

    // Manually wrap the content to generate the expected file content
    const wrappedLines: string[] = [];
    for (let i = 0; i < content.length; i += wrapWidth) {
      wrappedLines.push(content.substring(i, i + wrapWidth));
    }
    const expectedFileContent = wrappedLines.join('\n');

    await truncateAndSaveToFile(
      content,
      callId,
      projectTempDir,
      THRESHOLD,
      TRUNCATE_LINES,
    );

    const expectedPath = path.join(projectTempDir, 'passwd.output');
    expect(mockWriteFile).toHaveBeenCalledWith(
      expectedPath,
      expectedFileContent,
    );
  });
});
