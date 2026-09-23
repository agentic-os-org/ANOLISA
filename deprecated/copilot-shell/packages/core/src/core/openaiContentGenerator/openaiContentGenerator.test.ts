/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

// Mock the request tokenizer module BEFORE importing the class that uses it
const mockTokenizer = {
  calculateTokens: vi.fn().mockResolvedValue({
    totalTokens: 50,
    breakdown: {
      textTokens: 50,
      imageTokens: 0,
      audioTokens: 0,
      otherTokens: 0,
    },
    processingTime: 1,
  }),
  dispose: vi.fn(),
};

vi.mock('../../../utils/request-tokenizer/index.js', () => ({
  RequestTokenEstimator: vi.fn(() => mockTokenizer),
}));

// Now import the modules that depend on the mocked modules
import { OpenAIContentGenerator } from './openaiContentGenerator.js';
import type { Config } from '../../config/config.js';
import { AuthType } from '../contentGenerator.js';
import type {
  GenerateContentParameters,
  CountTokensParameters,
} from '@google/genai';
import type { OpenAICompatibleProvider } from './provider/index.js';
import OpenAI from 'openai';

describe('OpenAIContentGenerator (Refactored)', () => {
  let generator: OpenAIContentGenerator;
  let mockConfig: Config;

  beforeEach(() => {
    // Reset mocks
    vi.clearAllMocks();

    // Mock config
    mockConfig = {
      getContentGeneratorConfig: vi.fn().mockReturnValue({
        authType: 'openai',
        enableOpenAILogging: false,
        timeout: 120000,
        maxRetries: 3,
        samplingParams: {
          temperature: 0.7,
          max_tokens: 1000,
          top_p: 0.9,
        },
      }),
      getCliVersion: vi.fn().mockReturnValue('1.0.0'),
    } as unknown as Config;

    // Create generator instance
    const contentGeneratorConfig = {
      model: 'gpt-4',
      apiKey: 'test-key',
      authType: AuthType.USE_OPENAI,
      enableOpenAILogging: false,
      timeout: 120000,
      maxRetries: 3,
      samplingParams: {
        temperature: 0.7,
        max_tokens: 1000,
        top_p: 0.9,
      },
    };

    // Create a minimal mock provider
    const mockProvider: OpenAICompatibleProvider = {
      buildHeaders: vi.fn().mockReturnValue({}),
      buildClient: vi.fn().mockReturnValue({
        chat: {
          completions: {
            create: vi.fn(),
          },
        },
        embeddings: {
          create: vi.fn(),
        },
      } as unknown as OpenAI),
      buildRequest: vi.fn().mockImplementation((req) => req),
      getDefaultGenerationConfig: vi.fn().mockReturnValue({}),
    };

    generator = new OpenAIContentGenerator(
      contentGeneratorConfig,
      mockConfig,
      mockProvider,
    );
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  describe('constructor', () => {
    it('should initialize with basic configuration', () => {
      expect(generator).toBeDefined();
    });
  });

  describe('generateContent', () => {
    it('should delegate to pipeline.execute', async () => {
      // This test verifies the method exists and can be called
      expect(typeof generator.generateContent).toBe('function');
    });
  });

  describe('generateContentStream', () => {
    it('should delegate to pipeline.executeStream', async () => {
      // This test verifies the method exists and can be called
      expect(typeof generator.generateContentStream).toBe('function');
    });
  });

  describe('countTokens', () => {
    it('should count tokens using character-based estimation', async () => {
      const request: CountTokensParameters = {
        contents: [{ role: 'user', parts: [{ text: 'Hello world' }] }],
        model: 'gpt-4',
      };

      const result = await generator.countTokens(request);

      // 'Hello world' = 11 ASCII chars
      // 11 / 4 = 2.75 -> ceil = 3 tokens
      expect(result.totalTokens).toBe(3);
    });

    it('should handle multimodal content', async () => {
      const request: CountTokensParameters = {
        contents: [
          {
            role: 'user',
            parts: [{ text: 'Hello' }, { text: ' world' }],
          },
        ],
        model: 'gpt-4',
      };

      const result = await generator.countTokens(request);

      // Parts are combined for estimation:
      // 'Hello world' = 11 ASCII chars -> 11/4 = 2.75 -> ceil = 3 tokens
      expect(result.totalTokens).toBe(3);
    });
  });

  describe('embedContent', () => {
    it('should delegate to pipeline.client.embeddings.create', async () => {
      // This test verifies the method exists and can be called
      expect(typeof generator.embedContent).toBe('function');
    });
  });

  describe('validateApiKey', () => {
    let mockOpenAIClient: {
      models: {
        retrieve: ReturnType<typeof vi.fn>;
      };
    };

    beforeEach(() => {
      // Create mock OpenAI client with models.retrieve
      mockOpenAIClient = {
        models: {
          retrieve: vi.fn(),
        },
      };

      // Update mockProvider.buildClient to return the mock client
      const mockProvider = generator['pipeline']['config']['provider'];
      mockProvider.buildClient = vi.fn().mockReturnValue(mockOpenAIClient);
    });

    it('should resolve when API key and model are valid', async () => {
      mockOpenAIClient.models.retrieve.mockResolvedValue({
        id: 'gpt-4',
        object: 'model',
        created: 1234567890,
        owned_by: 'openai',
      });

      await expect(generator.validateApiKey()).resolves.toBeUndefined();
      expect(mockOpenAIClient.models.retrieve).toHaveBeenCalledWith('gpt-4');
    });

    it('should reject with model-not-found error when model returns 404', async () => {
      // Create a real OpenAI.APIError instance for 404
      const mockError = new OpenAI.APIError(
        404,
        {},
        'Model not found',
        undefined,
      );
      mockOpenAIClient.models.retrieve.mockRejectedValue(mockError);

      await expect(generator.validateApiKey()).rejects.toThrow(
        'Model "gpt-4" is not available. Please check if the model name is correct.',
      );
    });

    it('should reject with invalid API key error when model returns 401', async () => {
      // Create a real OpenAI.APIError instance for 401
      const mockError = new OpenAI.APIError(401, {}, 'Unauthorized', undefined);
      mockOpenAIClient.models.retrieve.mockRejectedValue(mockError);

      await expect(generator.validateApiKey()).rejects.toThrow(
        'Invalid API key. Please check your API key and try again.',
      );
    });

    it('should reject with permission error when model returns 403', async () => {
      // Create a real OpenAI.APIError instance for 403
      const mockError = new OpenAI.APIError(403, {}, 'Forbidden', undefined);
      mockOpenAIClient.models.retrieve.mockRejectedValue(mockError);

      await expect(generator.validateApiKey()).rejects.toThrow(
        'API key does not have permission to access this resource.',
      );
    });

    it('should reject with rate limit error when model returns 429', async () => {
      // Create a real OpenAI.APIError instance for 429
      const mockError = new OpenAI.APIError(
        429,
        {},
        'Rate limit exceeded',
        undefined,
      );
      mockOpenAIClient.models.retrieve.mockRejectedValue(mockError);

      await expect(generator.validateApiKey()).rejects.toThrow(
        'Rate limit exceeded. Please check your quota.',
      );
    });

    it('should reject with generic error for other API errors', async () => {
      // Create a real OpenAI.APIError instance for 500
      // Note: OpenAI.APIError message is auto-generated as "${status} ${JSON.stringify(error)}"
      const mockError = new OpenAI.APIError(500, {}, undefined, undefined);
      mockOpenAIClient.models.retrieve.mockRejectedValue(mockError);

      await expect(generator.validateApiKey()).rejects.toThrow(
        'API validation failed: 500 {}',
      );
    });

    it('should reject with generic error for non-API errors', async () => {
      const mockError = new Error('Network connection failed');
      mockOpenAIClient.models.retrieve.mockRejectedValue(mockError);

      await expect(generator.validateApiKey()).rejects.toThrow(
        'Authentication failed: Network connection failed',
      );
    });

    it('should skip validation for non-DashScope providers', async () => {
      // Create a generator with a custom (non-DashScope) baseUrl
      const customConfig = {
        model: 'custom-model',
        apiKey: 'test-key',
        baseUrl: 'https://api.custom-provider.com/v1',
        authType: AuthType.USE_OPENAI,
        enableOpenAILogging: false,
        timeout: 120000,
        maxRetries: 3,
        samplingParams: {
          temperature: 0.7,
          max_tokens: 1000,
          top_p: 0.9,
        },
      };
      const modelsRetrieve = vi.fn();
      const mockProvider: OpenAICompatibleProvider = {
        buildHeaders: vi.fn().mockReturnValue({}),
        buildClient: vi.fn().mockReturnValue({
          chat: { completions: { create: vi.fn() } },
          embeddings: { create: vi.fn() },
          models: { retrieve: modelsRetrieve },
        } as unknown as OpenAI),
        buildRequest: vi.fn().mockImplementation((req) => req),
        getDefaultGenerationConfig: vi.fn().mockReturnValue({}),
      };
      const customGenerator = new OpenAIContentGenerator(
        customConfig,
        mockConfig,
        mockProvider,
      );

      // Should resolve immediately without calling models.retrieve
      await expect(customGenerator.validateApiKey()).resolves.toBeUndefined();
      expect(modelsRetrieve).not.toHaveBeenCalled();
    });

    it('should still validate for DashScope providers', async () => {
      // Default generator has no baseUrl, which isDashScopeProvider returns true for
      mockOpenAIClient.models.retrieve.mockResolvedValue({
        id: 'gpt-4',
        object: 'model',
        created: 1234567890,
        owned_by: 'dashscope',
      });

      await expect(generator.validateApiKey()).resolves.toBeUndefined();
      expect(mockOpenAIClient.models.retrieve).toHaveBeenCalledWith('gpt-4');
    });
  });

  describe('shouldSuppressErrorLogging', () => {
    // Create a test subclass to access the protected method
    class TestGenerator extends OpenAIContentGenerator {
      testShouldSuppressErrorLogging(
        error: unknown,
        request: GenerateContentParameters,
      ): boolean {
        return this.shouldSuppressErrorLogging(error, request);
      }
    }

    let testGenerator: TestGenerator;

    beforeEach(() => {
      const contentGeneratorConfig = {
        model: 'gpt-4',
        apiKey: 'test-key',
        authType: AuthType.USE_OPENAI,
        enableOpenAILogging: false,
        timeout: 120000,
        maxRetries: 3,
        samplingParams: {
          temperature: 0.7,
          max_tokens: 1000,
          top_p: 0.9,
        },
      };

      // Create a minimal mock provider
      const mockProvider: OpenAICompatibleProvider = {
        buildHeaders: vi.fn().mockReturnValue({}),
        buildClient: vi.fn().mockReturnValue({
          chat: {
            completions: {
              create: vi.fn(),
            },
          },
          embeddings: {
            create: vi.fn(),
          },
        } as unknown as OpenAI),
        buildRequest: vi.fn().mockImplementation((req) => req),
        getDefaultGenerationConfig: vi.fn().mockReturnValue({}),
      };

      testGenerator = new TestGenerator(
        contentGeneratorConfig,
        mockConfig,
        mockProvider,
      );
    });

    it('should return false for regular errors', () => {
      const request: GenerateContentParameters = {
        contents: [{ role: 'user', parts: [{ text: 'Hello' }] }],
        model: 'gpt-4',
      };

      const result = testGenerator.testShouldSuppressErrorLogging(
        new Error('Test error'),
        request,
      );

      expect(result).toBe(false);
    });

    it('should return true for AbortError when signal is also aborted (user cancellation)', () => {
      const abortController = new AbortController();
      abortController.abort();

      const request: GenerateContentParameters = {
        contents: [{ role: 'user', parts: [{ text: 'Hello' }] }],
        model: 'gpt-4',
        config: {
          abortSignal: abortController.signal,
        },
      };

      // Create an AbortError with aborted signal - this is user-initiated
      const abortError = new Error('The operation was aborted');
      abortError.name = 'AbortError';

      const result = testGenerator.testShouldSuppressErrorLogging(
        abortError,
        request,
      );

      expect(result).toBe(true);
    });

    it('should return false for AbortError when signal is NOT aborted (network abort)', () => {
      const abortController = new AbortController();
      // Signal is NOT aborted - this simulates a network abort

      const request: GenerateContentParameters = {
        contents: [{ role: 'user', parts: [{ text: 'Hello' }] }],
        model: 'gpt-4',
        config: {
          abortSignal: abortController.signal,
        },
      };

      // AbortError but signal not aborted - could be network issue
      const abortError = new Error('The operation was aborted');
      abortError.name = 'AbortError';

      const result = testGenerator.testShouldSuppressErrorLogging(
        abortError,
        request,
      );

      expect(result).toBe(false);
    });

    it('should return false for AbortError without any signal', () => {
      const request: GenerateContentParameters = {
        contents: [{ role: 'user', parts: [{ text: 'Hello' }] }],
        model: 'gpt-4',
      };

      // AbortError but no signal at all - unknown source
      const abortError = new Error('The operation was aborted');
      abortError.name = 'AbortError';

      const result = testGenerator.testShouldSuppressErrorLogging(
        abortError,
        request,
      );

      expect(result).toBe(false);
    });

    it('should return false for non-AbortError even when signal is aborted', () => {
      const abortController = new AbortController();
      abortController.abort();

      const request: GenerateContentParameters = {
        contents: [{ role: 'user', parts: [{ text: 'Hello' }] }],
        model: 'gpt-4',
        config: {
          abortSignal: abortController.signal,
        },
      };

      // Regular error even though signal is aborted - should still be logged
      const result = testGenerator.testShouldSuppressErrorLogging(
        new Error('Network error'),
        request,
      );

      expect(result).toBe(false);
    });

    it('should return false for errors with non-aborted signal', () => {
      const abortController = new AbortController();

      const request: GenerateContentParameters = {
        contents: [{ role: 'user', parts: [{ text: 'Hello' }] }],
        model: 'gpt-4',
        config: {
          abortSignal: abortController.signal,
        },
      };

      const result = testGenerator.testShouldSuppressErrorLogging(
        new Error('Network error'),
        request,
      );

      expect(result).toBe(false);
    });

    it('should allow subclasses to override error suppression behavior', async () => {
      class CustomTestGenerator extends OpenAIContentGenerator {
        testShouldSuppressErrorLogging(
          error: unknown,
          request: GenerateContentParameters,
        ): boolean {
          return this.shouldSuppressErrorLogging(error, request);
        }

        protected override shouldSuppressErrorLogging(
          _error: unknown,
          _request: GenerateContentParameters,
        ): boolean {
          return true; // Always suppress for this test
        }
      }

      const contentGeneratorConfig = {
        model: 'gpt-4',
        apiKey: 'test-key',
        authType: AuthType.USE_OPENAI,
        enableOpenAILogging: false,
        timeout: 120000,
        maxRetries: 3,
        samplingParams: {
          temperature: 0.7,
          max_tokens: 1000,
          top_p: 0.9,
        },
      };

      // Create a minimal mock provider
      const mockProvider: OpenAICompatibleProvider = {
        buildHeaders: vi.fn().mockReturnValue({}),
        buildClient: vi.fn().mockReturnValue({
          chat: {
            completions: {
              create: vi.fn(),
            },
          },
          embeddings: {
            create: vi.fn(),
          },
        } as unknown as OpenAI),
        buildRequest: vi.fn().mockImplementation((req) => req),
        getDefaultGenerationConfig: vi.fn().mockReturnValue({}),
      };

      const customGenerator = new CustomTestGenerator(
        contentGeneratorConfig,
        mockConfig,
        mockProvider,
      );

      const request: GenerateContentParameters = {
        contents: [{ role: 'user', parts: [{ text: 'Hello' }] }],
        model: 'gpt-4',
      };

      const result = customGenerator.testShouldSuppressErrorLogging(
        new Error('Test error'),
        request,
      );

      expect(result).toBe(true);
    });
  });
});
