/**
 * @license
 * Copyright 2026 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import type {
  GenerateContentParameters,
  GenerateContentResponse,
} from '@google/genai';
import { HookSystem } from './hookSystem.js';
import { HookRegistry } from './hookRegistry.js';
import { HookRunner } from './hookRunner.js';
import { HookAggregator } from './hookAggregator.js';
import { HookPlanner } from './hookPlanner.js';
import { HookEventHandler } from './hookEventHandler.js';
import {
  HookType,
  HooksConfigSource,
  HookEventName,
  type HookDecision,
} from './types.js';
import type { Config } from '../config/config.js';

vi.mock('./hookRegistry.js');
vi.mock('./hookRunner.js');
vi.mock('./hookAggregator.js');
vi.mock('./hookPlanner.js');
vi.mock('./hookEventHandler.js');

describe('HookSystem', () => {
  let mockConfig: Config;
  let mockHookRegistry: HookRegistry;
  let mockHookRunner: HookRunner;
  let mockHookAggregator: HookAggregator;
  let mockHookPlanner: HookPlanner;
  let mockHookEventHandler: HookEventHandler;
  let hookSystem: HookSystem;

  beforeEach(() => {
    mockConfig = {
      getSessionId: vi.fn().mockReturnValue('test-session-id'),
      getTranscriptPath: vi.fn().mockReturnValue('/test/transcript'),
      getWorkingDir: vi.fn().mockReturnValue('/test/cwd'),
    } as unknown as Config;

    mockHookRegistry = {
      initialize: vi.fn().mockResolvedValue(undefined),
      setHookEnabled: vi.fn(),
      getAllHooks: vi.fn().mockReturnValue([]),
    } as unknown as HookRegistry;

    mockHookRunner = {
      executeHooksSequential: vi.fn(),
      executeHooksParallel: vi.fn(),
    } as unknown as HookRunner;

    mockHookAggregator = {
      aggregateResults: vi.fn(),
    } as unknown as HookAggregator;

    mockHookPlanner = {
      createExecutionPlan: vi.fn(),
    } as unknown as HookPlanner;

    mockHookEventHandler = {
      fireUserPromptSubmitEvent: vi.fn(),
      firePreToolUseEvent: vi.fn(),
      fireStopEvent: vi.fn(),
      firePostToolUseEvent: vi.fn(),
      fireAfterModelEvent: vi.fn(),
      firePostToolUseFailureEvent: vi.fn(),
    } as unknown as HookEventHandler;

    vi.mocked(HookRegistry).mockImplementation(() => mockHookRegistry);
    vi.mocked(HookRunner).mockImplementation(() => mockHookRunner);
    vi.mocked(HookAggregator).mockImplementation(() => mockHookAggregator);
    vi.mocked(HookPlanner).mockImplementation(() => mockHookPlanner);
    vi.mocked(HookEventHandler).mockImplementation(() => mockHookEventHandler);

    hookSystem = new HookSystem(mockConfig);
  });

  describe('constructor', () => {
    it('should create instance with all dependencies', () => {
      expect(HookRegistry).toHaveBeenCalledWith(mockConfig);
      expect(HookRunner).toHaveBeenCalled();
      expect(HookAggregator).toHaveBeenCalled();
      expect(HookPlanner).toHaveBeenCalledWith(mockHookRegistry);
      expect(HookEventHandler).toHaveBeenCalledWith(
        mockConfig,
        mockHookPlanner,
        mockHookRunner,
        mockHookAggregator,
      );
    });
  });

  describe('initialize', () => {
    it('should initialize hook registry', async () => {
      await hookSystem.initialize();

      expect(mockHookRegistry.initialize).toHaveBeenCalled();
    });
  });

  describe('getEventHandler', () => {
    it('should return the hook event handler', () => {
      const eventHandler = hookSystem.getEventHandler();

      expect(eventHandler).toBe(mockHookEventHandler);
    });
  });

  describe('getRegistry', () => {
    it('should return the hook registry', () => {
      const registry = hookSystem.getRegistry();

      expect(registry).toBe(mockHookRegistry);
    });
  });

  describe('setHookEnabled', () => {
    it('should enable a hook', () => {
      hookSystem.setHookEnabled('test-hook', true);

      expect(mockHookRegistry.setHookEnabled).toHaveBeenCalledWith(
        'test-hook',
        true,
      );
    });

    it('should disable a hook', () => {
      hookSystem.setHookEnabled('test-hook', false);

      expect(mockHookRegistry.setHookEnabled).toHaveBeenCalledWith(
        'test-hook',
        false,
      );
    });
  });

  describe('getAllHooks', () => {
    it('should return all registered hooks', () => {
      const mockHooks = [
        {
          config: {
            type: HookType.Command,
            command: 'echo test',
            source: HooksConfigSource.Project,
          },
          source: HooksConfigSource.Project,
          eventName: HookEventName.PreToolUse,
          enabled: true,
        },
      ];
      vi.mocked(mockHookRegistry.getAllHooks).mockReturnValue(mockHooks);

      const hooks = hookSystem.getAllHooks();

      expect(hooks).toEqual(mockHooks);
      expect(mockHookRegistry.getAllHooks).toHaveBeenCalled();
    });
  });

  describe('firePreToolUseEvent', () => {
    it('should pass toolUseId through to the event handler', async () => {
      const mockResult = {
        success: true,
        allOutputs: [],
        errors: [],
        totalDuration: 12,
        finalOutput: {
          decision: 'allow' as HookDecision,
        },
      };
      vi.mocked(mockHookEventHandler.firePreToolUseEvent).mockResolvedValue(
        mockResult,
      );

      await hookSystem.firePreToolUseEvent(
        'shell',
        { command: 'ls' },
        undefined,
        'tool-call-1',
      );

      expect(mockHookEventHandler.firePreToolUseEvent).toHaveBeenCalledWith(
        'shell',
        { command: 'ls' },
        undefined,
        'tool-call-1',
        undefined,
      );
    });
  });

  describe('fireStopEvent', () => {
    it('should fire stop event and return output', async () => {
      const mockResult = {
        success: true,
        allOutputs: [],
        errors: [],
        totalDuration: 50,
        finalOutput: {
          continue: false,
          stopReason: 'user_stop',
        },
      };
      vi.mocked(mockHookEventHandler.fireStopEvent).mockResolvedValue(
        mockResult,
      );

      const result = await hookSystem.fireStopEvent(true, 'last message');

      expect(mockHookEventHandler.fireStopEvent).toHaveBeenCalledWith(
        true,
        'last message',
      );
      expect(result).toBeDefined();
    });

    it('should use default parameters when not provided', async () => {
      const mockResult = {
        success: true,
        allOutputs: [],
        errors: [],
        totalDuration: 0,
        finalOutput: undefined,
      };
      vi.mocked(mockHookEventHandler.fireStopEvent).mockResolvedValue(
        mockResult,
      );

      await hookSystem.fireStopEvent();

      expect(mockHookEventHandler.fireStopEvent).toHaveBeenCalledWith(
        false,
        '',
      );
    });

    it('should return undefined when no final output', async () => {
      const mockResult = {
        success: true,
        allOutputs: [],
        errors: [],
        totalDuration: 0,
        finalOutput: undefined,
      };
      vi.mocked(mockHookEventHandler.fireStopEvent).mockResolvedValue(
        mockResult,
      );

      const result = await hookSystem.fireStopEvent();

      expect(result).toBeUndefined();
    });
  });

  describe('fireUserPromptSubmitEvent', () => {
    it('should fire UserPromptSubmit event and return output', async () => {
      const mockResult = {
        success: true,
        allOutputs: [],
        errors: [],
        totalDuration: 50,
        finalOutput: {
          continue: true,
          decision: 'allow' as HookDecision,
        },
      };
      vi.mocked(
        mockHookEventHandler.fireUserPromptSubmitEvent,
      ).mockResolvedValue(mockResult);

      const result = await hookSystem.fireUserPromptSubmitEvent('test prompt');

      expect(
        mockHookEventHandler.fireUserPromptSubmitEvent,
      ).toHaveBeenCalledWith('test prompt');
      expect(result).toBeDefined();
    });

    it('should pass prompt to event handler', async () => {
      const mockResult = {
        success: true,
        allOutputs: [],
        errors: [],
        totalDuration: 0,
        finalOutput: {
          decision: 'allow' as HookDecision,
        },
      };
      vi.mocked(
        mockHookEventHandler.fireUserPromptSubmitEvent,
      ).mockResolvedValue(mockResult);

      await hookSystem.fireUserPromptSubmitEvent('my custom prompt');

      expect(
        mockHookEventHandler.fireUserPromptSubmitEvent,
      ).toHaveBeenCalledWith('my custom prompt');
    });

    it('should return undefined when no final output', async () => {
      const mockResult = {
        success: true,
        allOutputs: [],
        errors: [],
        totalDuration: 0,
        finalOutput: undefined,
      };
      vi.mocked(
        mockHookEventHandler.fireUserPromptSubmitEvent,
      ).mockResolvedValue(mockResult);

      const result = await hookSystem.fireUserPromptSubmitEvent('test');

      expect(result).toBeUndefined();
    });

    it('should return DefaultHookOutput with blocking decision', async () => {
      const mockResult = {
        success: true,
        allOutputs: [],
        errors: [],
        totalDuration: 50,
        finalOutput: {
          decision: 'block' as HookDecision,
          reason: 'Blocked by policy',
        },
      };
      vi.mocked(
        mockHookEventHandler.fireUserPromptSubmitEvent,
      ).mockResolvedValue(mockResult);

      const result = await hookSystem.fireUserPromptSubmitEvent('test');

      expect(result).toBeDefined();
      expect(result?.isBlockingDecision()).toBe(true);
    });

    it('should return DefaultHookOutput with additional context', async () => {
      const mockResult = {
        success: true,
        allOutputs: [],
        errors: [],
        totalDuration: 50,
        finalOutput: {
          decision: 'allow' as HookDecision,
          hookSpecificOutput: {
            additionalContext: 'Some additional context',
          },
        },
      };
      vi.mocked(
        mockHookEventHandler.fireUserPromptSubmitEvent,
      ).mockResolvedValue(mockResult);

      const result = await hookSystem.fireUserPromptSubmitEvent('test');

      expect(result).toBeDefined();
      expect(result?.getAdditionalContext()).toBe('Some additional context');
    });
  });

  describe('firePostToolUseEvent', () => {
    it('should attach aggregator notifications to the returned output', async () => {
      const notifications = [
        {
          hookName: 'audit-hook',
          message: 'Tool output flagged for follow-up review',
          decision: 'allow' as HookDecision,
        },
      ];
      const mockResult = {
        success: true,
        allOutputs: [],
        errors: [],
        totalDuration: 12,
        finalOutput: {
          decision: 'allow' as HookDecision,
          reason: 'Tool output flagged for follow-up review',
        },
        notifications,
      };
      vi.mocked(mockHookEventHandler.firePostToolUseEvent).mockResolvedValue(
        mockResult,
      );

      const result = await hookSystem.firePostToolUseEvent(
        'shell',
        { command: 'ls' },
        { llmContent: 'output' },
      );

      expect(result).toBeDefined();
      expect(result?.notifications).toEqual(notifications);
    });

    it('should leave notifications undefined when aggregator emits none', async () => {
      const mockResult = {
        success: true,
        allOutputs: [],
        errors: [],
        totalDuration: 12,
        finalOutput: {
          decision: 'allow' as HookDecision,
        },
      };
      vi.mocked(mockHookEventHandler.firePostToolUseEvent).mockResolvedValue(
        mockResult,
      );

      const result = await hookSystem.firePostToolUseEvent(
        'shell',
        { command: 'ls' },
        { llmContent: 'output' },
      );

      expect(result).toBeDefined();
      expect(result?.notifications).toBeUndefined();
    });

    it('should return undefined when no final output', async () => {
      const mockResult = {
        success: true,
        allOutputs: [],
        errors: [],
        totalDuration: 0,
        finalOutput: undefined,
        notifications: [
          {
            hookName: 'audit-hook',
            message: 'noise that should be discarded',
            decision: 'allow' as HookDecision,
          },
        ],
      };
      vi.mocked(mockHookEventHandler.firePostToolUseEvent).mockResolvedValue(
        mockResult,
      );

      const result = await hookSystem.firePostToolUseEvent(
        'shell',
        { command: 'ls' },
        { llmContent: 'output' },
      );

      expect(result).toBeUndefined();
    });
  });

  describe('fireAfterModelEvent', () => {
    const dummyRequest = { model: 'test' } as GenerateContentParameters;
    const dummyResponse = {
      candidates: [],
    } as unknown as GenerateContentResponse;

    it('should attach aggregator notifications to the returned output', async () => {
      const notifications = [
        {
          hookName: 'pii-hook',
          message: 'PII detected in model response',
          decision: 'allow' as HookDecision,
        },
      ];
      const mockResult = {
        success: true,
        allOutputs: [],
        errors: [],
        totalDuration: 5,
        finalOutput: {
          decision: 'allow' as HookDecision,
          reason: 'PII detected in model response',
        },
        notifications,
      };
      vi.mocked(mockHookEventHandler.fireAfterModelEvent).mockResolvedValue(
        mockResult,
      );

      const result = await hookSystem.fireAfterModelEvent(
        dummyRequest,
        dummyResponse,
      );

      expect(result).toBeDefined();
      expect(result?.notifications).toEqual(notifications);
    });

    it('should leave notifications undefined when aggregator emits none', async () => {
      const mockResult = {
        success: true,
        allOutputs: [],
        errors: [],
        totalDuration: 5,
        finalOutput: {
          decision: 'allow' as HookDecision,
        },
      };
      vi.mocked(mockHookEventHandler.fireAfterModelEvent).mockResolvedValue(
        mockResult,
      );

      const result = await hookSystem.fireAfterModelEvent(
        dummyRequest,
        dummyResponse,
      );

      expect(result).toBeDefined();
      expect(result?.notifications).toBeUndefined();
    });

    it('should return undefined when no final output', async () => {
      const mockResult = {
        success: true,
        allOutputs: [],
        errors: [],
        totalDuration: 0,
        finalOutput: undefined,
        notifications: [
          {
            hookName: 'pii-hook',
            message: 'discarded when no output',
            decision: 'allow' as HookDecision,
          },
        ],
      };
      vi.mocked(mockHookEventHandler.fireAfterModelEvent).mockResolvedValue(
        mockResult,
      );

      const result = await hookSystem.fireAfterModelEvent(
        dummyRequest,
        dummyResponse,
      );

      expect(result).toBeUndefined();
    });
  });

  describe('firePostToolUseFailureEvent', () => {
    it('should attach aggregator notifications to the returned output', async () => {
      const notifications = [
        {
          hookName: 'sandbox-guard',
          message: 'Command blocked by sandbox',
          decision: 'allow' as HookDecision,
        },
      ];
      const mockResult = {
        success: true,
        allOutputs: [],
        errors: [],
        totalDuration: 3,
        finalOutput: {
          decision: 'allow' as HookDecision,
          reason: 'Command blocked by sandbox',
        },
        notifications,
      };
      vi.mocked(
        mockHookEventHandler.firePostToolUseFailureEvent,
      ).mockResolvedValue(mockResult);

      const result = await hookSystem.firePostToolUseFailureEvent(
        'call-1',
        'shell',
        { command: 'rm -rf /' },
        'Permission denied',
      );

      expect(result).toBeDefined();
      expect(result?.notifications).toEqual(notifications);
    });

    it('should leave notifications undefined when aggregator emits none', async () => {
      const mockResult = {
        success: true,
        allOutputs: [],
        errors: [],
        totalDuration: 3,
        finalOutput: {
          decision: 'allow' as HookDecision,
        },
      };
      vi.mocked(
        mockHookEventHandler.firePostToolUseFailureEvent,
      ).mockResolvedValue(mockResult);

      const result = await hookSystem.firePostToolUseFailureEvent(
        'call-1',
        'shell',
        { command: 'ls' },
        'error',
      );

      expect(result).toBeDefined();
      expect(result?.notifications).toBeUndefined();
    });
  });
});
